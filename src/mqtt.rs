//! MQTT live-feed page: background subscriber task + SSE fan-out.

use std::{
    collections::{HashMap, VecDeque},
    convert::Infallible,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use time::format_description::well_known::Rfc3339;

use askama::Template;
use axum::{
    extract::{Query, State},
    response::{
        Html,
        sse::{Event, KeepAlive, Sse},
    },
    Json,
};
use futures::StreamExt as _;
use rumqttc::{AsyncClient, Event as MqttEvent, EventLoop, MqttOptions, Packet, QoS};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch, Mutex as TokioMutex};

use crate::{
    auth::{AuthUserInfo, GmUser},
    error::Error,
    ServerState,
};

// ─── Integration pattern types ───────────────────────────────────────────────

/// Parsed segment of an integration topic pattern.
#[derive(Debug)]
pub(crate) enum PatternSegment {
    /// Matches only the given literal string.
    Literal(String),
    /// Captures this segment as the device ID.
    Capture,
    /// Matches any single segment (no capture).
    Any,
    /// Matches zero or more remaining segments; terminates parsing.
    Glob,
}

/// Per-integration device-tracking configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IntegrationConfig {
    /// Topic pattern with `{device}`, `*`/`+` (any segment), and `**` (glob) tokens.
    pub pattern: String,
    /// Optional display name; defaults to the first literal segment of the pattern.
    #[serde(default)]
    pub name: Option<String>,
}

/// Configuration for the MQTT broker connection.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MqttConfig {
    /// Broker hostname or IP address.
    #[serde(default = "default_host")]
    pub host: String,
    /// Broker port.
    #[serde(default = "default_port")]
    pub port: u16,
    /// Optional broker username.
    #[serde(default)]
    pub username: Option<String>,
    /// Optional broker password.
    #[serde(default)]
    pub password: Option<String>,
    /// Topics to subscribe to. Defaults to `["#"]` (all topics).
    #[serde(default = "default_topics")]
    pub topics: Vec<String>,
    /// Number of recent messages to replay to new SSE clients. Defaults to 200.
    #[serde(default = "default_scrollback")]
    pub scrollback: usize,
    /// Integration configs for device tracking. Empty = no tracking.
    #[serde(default)]
    pub integrations: Vec<IntegrationConfig>,
    /// MQTT client ID sent to the broker. Must be unique per connected instance.
    pub client_id: String,
}

fn default_host() -> String {
    "localhost".to_string()
}


fn default_port() -> u16 {
    1883
}

fn default_topics() -> Vec<String> {
    vec!["#".to_string()]
}

fn default_scrollback() -> usize {
    200
}

// ─── Internal integration representation ─────────────────────────────────────

/// A parsed integration used for device extraction from MQTT topics.
#[derive(Debug)]
pub(crate) struct Integration {
    /// Human-readable name for this integration (shown in the devices table).
    display_name: String,
    /// Original pattern string — stored in the DB to detect stale entries after config changes.
    pattern: String,
    segments: Vec<PatternSegment>,
}

/// Parse [`IntegrationConfig`] slices into [`Integration`] values ready for matching.
pub(crate) fn parse_integrations(cfgs: &[IntegrationConfig]) -> Vec<Integration> {
    cfgs.iter()
        .map(|cfg| {
            let segments: Vec<PatternSegment> = cfg
                .pattern
                .split('/')
                .map(|seg| match seg {
                    "{device}" => PatternSegment::Capture,
                    "**" => PatternSegment::Glob,
                    "*" | "+" => PatternSegment::Any,
                    lit => PatternSegment::Literal(lit.to_string()),
                })
                .collect();

            let display_name = cfg.name.clone().unwrap_or_else(|| {
                segments
                    .iter()
                    .find_map(|s| {
                        if let PatternSegment::Literal(l) = s {
                            Some(l.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| "unknown".to_string())
            });

            Integration { display_name, pattern: cfg.pattern.clone(), segments }
        })
        .collect()
}

/// Try to match `topic` against `segments`, returning the captured device ID.
///
/// Returns `None` if the pattern does not match or contains no `{device}` capture.
fn match_topic<'a>(segments: &[PatternSegment], topic: &'a str) -> Option<&'a str> {
    let topic_segs: Vec<&str> = topic.split('/').collect();
    let mut device_id: Option<&str> = None;
    let mut pi = 0usize;
    let mut ti = 0usize;

    while pi < segments.len() {
        match &segments[pi] {
            PatternSegment::Glob => {
                // ** matches the remainder — consume all topic segs and stop.
                ti = topic_segs.len();
                break;
            }
            PatternSegment::Literal(l) => {
                if ti >= topic_segs.len() || topic_segs[ti] != l {
                    return None;
                }
                ti += 1;
            }
            PatternSegment::Capture => {
                if ti >= topic_segs.len() {
                    return None;
                }
                device_id = Some(topic_segs[ti]);
                ti += 1;
            }
            PatternSegment::Any => {
                if ti >= topic_segs.len() {
                    return None;
                }
                ti += 1;
            }
        }
        pi += 1;
    }

    // All pattern segments consumed — topic must also be fully consumed.
    if ti != topic_segs.len() {
        return None;
    }

    device_id
}

/// Try each integration in order; return the first `(integration, device_id)` match.
fn match_integrations<'i, 't>(
    integrations: &'i [Integration],
    topic: &'t str,
) -> Option<(&'i Integration, &'t str)> {
    for integration in integrations {
        if let Some(device_id) = match_topic(&integration.segments, topic) {
            return Some((integration, device_id));
        }
    }
    None
}

/// A single MQTT publish received from the broker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MqttMessage {
    /// The topic the message was published to.
    pub topic: String,
    /// UTF-8 payload (non-UTF-8 bytes are replaced with the replacement character).
    pub payload: String,
    /// RFC 3339 timestamp of when the message was received by this server.
    pub received_at: String,
}

/// Fan-out channel payload: either a received MQTT message or a broker status change.
#[derive(Debug, Clone)]
pub enum BrokerEvent {
    /// A publish message received from the broker.
    Message(MqttMessage),
    /// The broker connection status changed (e.g. `"connected"`, `"error"`).
    Status { status: String },
}

/// Prometheus registry and counter for MQTT device messages.
pub struct PrometheusState {
    /// Prometheus scrape registry (not the global default).
    pub registry: prometheus::Registry,
    /// `mqtt_messages_total{integration, device}` counter.
    pub messages_total: prometheus::IntCounterVec,
}

impl std::fmt::Debug for PrometheusState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrometheusState").finish_non_exhaustive()
    }
}

/// Shared MQTT fan-out state stored in [`ServerState`].
#[derive(Debug)]
pub struct MqttState {
    /// Broadcast sender; SSE handlers subscribe by calling `tx.subscribe()`.
    pub tx: broadcast::Sender<BrokerEvent>,
    /// Last known broker status (`"connected"`, `"error"`, `"connecting"`).
    /// New SSE clients call `.borrow()` for an immediate snapshot without waiting.
    pub status_tx: Arc<watch::Sender<String>>,
    /// Ring buffer of recent messages replayed to new SSE clients on connect.
    pub recent_messages: Arc<TokioMutex<VecDeque<MqttMessage>>>,
    /// Prometheus metrics, present only when integrations are configured.
    pub prometheus: Option<PrometheusState>,
    /// Parsed integrations, shared with the device tracker task and message filter handler.
    pub(crate) integrations: Arc<Vec<Integration>>,
    /// Cloned client handle used for publishing outbound messages (e.g. device commands).
    pub publish_client: AsyncClient,
}

/// Abstraction over the MQTT client's subscribe call, injected into
/// [`handle_conn_ack`] so it can be mocked in tests.
pub(crate) trait MqttSubscriber: Send + Sync {
    fn subscribe<'a>(
        &'a self,
        topic: &'a str,
        qos: QoS,
    ) -> Pin<Box<dyn Future<Output = Result<(), rumqttc::ClientError>> + Send + 'a>>;
}

impl MqttSubscriber for AsyncClient {
    fn subscribe<'a>(
        &'a self,
        topic: &'a str,
        qos: QoS,
    ) -> Pin<Box<dyn Future<Output = Result<(), rumqttc::ClientError>> + Send + 'a>> {
        Box::pin(AsyncClient::subscribe(self, topic, qos))
    }
}

/// Handle a ConnAck packet: update status, broadcast, re-subscribe to topics.
/// Extracted for testability — called by [`run_mqtt_task`] on every (re)connect.
async fn handle_conn_ack(
    client: &impl MqttSubscriber,
    topics: &[String],
    status_tx: &Arc<watch::Sender<String>>,
    tx: &broadcast::Sender<BrokerEvent>,
    host: &str,
    port: u16,
) {
    tracing::info!(host, port, "MQTT connected");
    let _ = status_tx.send_replace("connected".into());
    let _ = tx.send(BrokerEvent::Status { status: "connected".into() });
    // Re-subscribe after every (re)connect so reconnects pick up the same topics.
    for topic in topics {
        if let Err(err) = client.subscribe(topic, QoS::AtMostOnce).await {
            tracing::warn!(%err, %topic, "failed to re-subscribe after reconnect");
        }
    }
}

/// Handle a Publish packet: store in the ring buffer and broadcast to SSE clients.
/// Extracted for testability — called by [`run_mqtt_task`] on every received message.
async fn handle_publish(
    topic: String,
    payload: &[u8],
    scrollback: usize,
    tx: &broadcast::Sender<BrokerEvent>,
    recent_messages: &Arc<TokioMutex<VecDeque<MqttMessage>>>,
) {
    let msg = MqttMessage {
        topic,
        payload: String::from_utf8_lossy(payload).into_owned(),
        received_at: utc_now(),
    };
    tracing::trace!(topic = %msg.topic, "MQTT message received");
    {
        let mut buf = recent_messages.lock().await;
        if buf.len() == scrollback {
            let _ = buf.pop_front();
        }
        buf.push_back(msg.clone());
    }
    let _ = tx.send(BrokerEvent::Message(msg));
}

/// Handle an event loop error: update status and broadcast.
/// Extracted for testability — the retry sleep stays in [`run_mqtt_task`].
fn handle_error(
    err: &rumqttc::ConnectionError,
    status_tx: &Arc<watch::Sender<String>>,
    tx: &broadcast::Sender<BrokerEvent>,
) {
    tracing::warn!(%err, "MQTT eventloop error, will retry");
    let _ = status_tx.send_replace("error".into());
    let _ = tx.send(BrokerEvent::Status { status: "error".into() });
}

/// Create and configure an MQTT client from config without connecting.
/// The returned `AsyncClient` can be cloned for publishing; pass the `EventLoop`
/// to [`run_mqtt_task`] to drive the connection.
pub fn setup_mqtt_client(config: &MqttConfig) -> (AsyncClient, EventLoop) {
    let mut opts = MqttOptions::new(&config.client_id, &config.host, config.port);
    let _ = opts.set_keep_alive(Duration::from_secs(10));
    // Some topics (e.g. Frigate snapshots, zigbee2mqtt device lists) send large
    // payloads. Raise the limit to 1 MiB to avoid repeated reconnect loops.
    let _ = opts.set_max_packet_size(1024 * 1024, 1024 * 1024);
    if let (Some(user), Some(pass)) = (config.username.as_deref(), config.password.as_deref()) {
        let _ = opts.set_credentials(user, pass);
    }
    AsyncClient::new(opts, 64)
}

/// Spawn the MQTT subscriber task. Runs forever, reconnecting automatically.
pub async fn run_mqtt_task(
    config: MqttConfig,
    client: AsyncClient,
    mut eventloop: EventLoop,
    tx: broadcast::Sender<BrokerEvent>,
    status_tx: Arc<watch::Sender<String>>,
    recent_messages: Arc<TokioMutex<VecDeque<MqttMessage>>>,
) {
    loop {
        match eventloop.poll().await {
            Ok(MqttEvent::Incoming(Packet::Publish(publish))) => {
                handle_publish(publish.topic, &publish.payload, config.scrollback, &tx, &recent_messages).await;
            }
            Ok(MqttEvent::Incoming(Packet::ConnAck(_))) => {
                handle_conn_ack(&client, &config.topics, &status_tx, &tx, &config.host, config.port).await;
            }
            Ok(_) => {}
            Err(err) => {
                handle_error(&err, &status_tx, &tx);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

// ─── Device tracker task ─────────────────────────────────────────────────────

/// A row returned from `mqtt_devices` for display in the devices page.
pub struct DeviceRow {
    /// Integration display name.
    pub integration: String,
    /// Device identifier as seen in the topic.
    pub device_id: String,
    /// ISO 8601 timestamp of the first message from this device.
    pub first_seen: String,
    /// ISO 8601 timestamp of the most recent message from this device.
    pub last_seen: String,
    /// Total number of messages seen from this device.
    pub message_count: i64,
}

/// Background task: listen for MQTT messages, extract device IDs, persist to DB, and
/// increment the Prometheus counter.
///
/// On startup, purges any rows whose stored pattern no longer matches the current config
/// so that pattern changes are automatically reflected without manual DB cleanup.
///
/// DB writes are debounced: the first message from each device is written immediately
/// (to record `first_seen`), then subsequent writes are batched for up to
/// [`DB_WRITE_INTERVAL`] so that high-frequency devices don't generate one query
/// per message.
pub async fn run_device_tracker_task(
    integrations: Arc<Vec<Integration>>,
    db: sqlx::PgPool,
    metrics: Option<prometheus::IntCounterVec>,
    mut rx: broadcast::Receiver<BrokerEvent>,
) {
    // Purge stale rows for each integration before processing live messages.
    for integration in integrations.as_ref() {
        cleanup_stale_pattern(&db, &integration.display_name, &integration.pattern).await;
    }

    // (integration, pattern, device_id) → (pending_count, last_write)
    // `last_write = None` means the device has never been written to the DB.
    let mut pending: HashMap<(String, String, String), (i64, Option<Instant>)> = HashMap::new();

    loop {
        match rx.recv().await {
            Ok(BrokerEvent::Message(ref msg)) => {
                if let Some((integration, device_id)) =
                    match_integrations(&integrations, &msg.topic)
                {
                    let key = (
                        integration.display_name.clone(),
                        integration.pattern.clone(),
                        device_id.to_string(),
                    );
                    let (count, last_write) = pending.entry(key.clone()).or_insert((0, None));
                    *count += 1;

                    let should_write = match last_write {
                        None => true,
                        Some(t) => t.elapsed() >= DB_WRITE_INTERVAL,
                    };
                    if should_write {
                        upsert_device(&db, &key.0, &key.1, &key.2, *count).await;
                        *count = 0;
                        *last_write = Some(Instant::now());
                    }

                    if let Some(ref counter) = metrics {
                        counter.with_label_values(&[&integration.display_name, device_id]).inc();
                    }
                }
            }
            Ok(BrokerEvent::Status { .. }) => {}
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(n, "device tracker lagged, skipping messages");
            }
            Err(broadcast::error::RecvError::Closed) => {
                tracing::warn!("device tracker broadcast channel closed, task exiting");
                break;
            }
        }
    }
}

/// How long to accumulate message counts before flushing to the database.
const DB_WRITE_INTERVAL: Duration = Duration::from_secs(60);

/// Delete rows for `integration` whose pattern no longer matches the current config.
/// Called once at tracker startup so pattern changes self-heal on restart.
async fn cleanup_stale_pattern(db: &sqlx::PgPool, integration: &str, pattern: &str) {
    let result = sqlx::query(
        "DELETE FROM mqtt_devices WHERE integration = $1 AND pattern != $2",
    )
    .bind(integration)
    .bind(pattern)
    .execute(db)
    .await;

    match result {
        Ok(r) if r.rows_affected() > 0 => {
            tracing::info!(
                integration,
                pattern,
                rows = r.rows_affected(),
                "purged stale mqtt_devices rows after pattern change"
            );
        }
        Ok(_) => {}
        Err(err) => {
            tracing::warn!(%err, integration, "failed to purge stale mqtt_devices rows");
        }
    }
}

async fn upsert_device(db: &sqlx::PgPool, integration: &str, pattern: &str, device_id: &str, count: i64) {
    let result = sqlx::query(
        "INSERT INTO mqtt_devices (integration, pattern, device_id, message_count)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (integration, pattern, device_id)
         DO UPDATE SET last_seen = NOW(),
                       message_count = mqtt_devices.message_count + $4",
    )
    .bind(integration)
    .bind(pattern)
    .bind(device_id)
    .bind(count)
    .execute(db)
    .await;

    if let Err(err) = result {
        tracing::warn!(%err, integration, device_id, "failed to upsert mqtt device");
    }
}

/// Returns the current UTC time formatted as RFC 3339 (e.g. `2026-03-15T12:00:00Z`).
fn utc_now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_integrations / match_topic / match_integrations ─────────────────

    fn parsed(pattern: &str) -> Vec<PatternSegment> {
        parse_integrations(&[IntegrationConfig { pattern: pattern.to_string(), name: None }])
            .into_iter()
            .next()
            .unwrap()
            .segments
    }

    #[test]
    fn zigbee_pattern_captures_device() {
        let segs = parsed("zigbee2mqtt/{device}/**");
        assert_eq!(
            match_topic(&segs, "zigbee2mqtt/0x1234/some/state"),
            Some("0x1234")
        );
    }

    #[test]
    fn zigbee_pattern_no_match_different_prefix() {
        let segs = parsed("zigbee2mqtt/{device}/**");
        assert_eq!(match_topic(&segs, "other/0x1234"), None);
    }

    #[test]
    fn homeassistant_pattern_captures_device() {
        let segs = parsed("homeassistant/*/{device}/**");
        assert_eq!(
            match_topic(&segs, "homeassistant/light/my_dev/state"),
            Some("my_dev")
        );
    }

    #[test]
    fn no_device_segment_returns_none() {
        let segs = parsed("some/literal/path");
        assert_eq!(match_topic(&segs, "some/literal/path"), None);
    }

    #[test]
    fn pattern_shorter_than_topic_without_glob_returns_none() {
        let segs = parsed("a/b");
        assert_eq!(match_topic(&segs, "a/b/c"), None);
    }

    #[test]
    fn topic_shorter_than_pattern_returns_none() {
        let segs = parsed("a/{device}/c");
        assert_eq!(match_topic(&segs, "a/dev"), None);
    }

    #[test]
    fn glob_matches_zero_remaining_segments() {
        // ** at end — topic ends right at the device segment (no trailing segments)
        let segs = parsed("prefix/{device}/**");
        assert_eq!(match_topic(&segs, "prefix/dev"), Some("dev"));
    }

    #[test]
    fn match_integrations_returns_first_match_with_name() {
        let cfgs = vec![
            IntegrationConfig { pattern: "other/#".to_string(), name: None },
            IntegrationConfig {
                pattern: "z2m/{device}/**".to_string(),
                name: Some("Zigbee".to_string()),
            },
        ];
        let integrations = parse_integrations(&cfgs);
        let result = match_integrations(&integrations, "z2m/abc/state");
        assert!(result.is_some());
        let (integration, device) = result.unwrap();
        assert_eq!(integration.display_name, "Zigbee");
        assert_eq!(device, "abc");
    }

    #[test]
    fn match_integrations_no_match_returns_none() {
        let cfgs = vec![IntegrationConfig {
            pattern: "zigbee2mqtt/{device}/**".to_string(),
            name: None,
        }];
        let integrations = parse_integrations(&cfgs);
        assert!(match_integrations(&integrations, "homeassistant/sensor/state").is_none());
    }

    #[test]
    fn integration_stores_original_pattern() {
        let cfgs = vec![IntegrationConfig {
            pattern: "zigbee2mqtt/{device}/**".to_string(),
            name: None,
        }];
        let integrations = parse_integrations(&cfgs);
        assert_eq!(integrations[0].pattern, "zigbee2mqtt/{device}/**");
    }

    #[test]
    fn match_integrations_returns_integration_with_pattern() {
        let cfgs = vec![IntegrationConfig {
            pattern: "z2m/{device}/**".to_string(),
            name: Some("Zigbee".to_string()),
        }];
        let integrations = parse_integrations(&cfgs);
        let (integration, _device) =
            match_integrations(&integrations, "z2m/bulb/state").unwrap();
        assert_eq!(integration.pattern, "z2m/{device}/**");
    }

    #[test]
    fn integration_display_name_defaults_to_first_literal() {
        let cfgs = vec![IntegrationConfig {
            pattern: "mybridge/{device}/**".to_string(),
            name: None,
        }];
        let integrations = parse_integrations(&cfgs);
        assert_eq!(integrations[0].display_name, "mybridge");
    }

    #[test]
    fn integration_display_name_falls_back_to_unknown_when_no_literals() {
        let cfgs = vec![IntegrationConfig {
            pattern: "{device}/**".to_string(),
            name: None,
        }];
        let integrations = parse_integrations(&cfgs);
        assert_eq!(integrations[0].display_name, "unknown");
    }

    #[test]
    fn raw_zigbee_address_starts_with_0x() {
        // The template flags device_ids starting with "0x" as unnamed.
        // Verify the condition matches typical zigbee hardware addresses.
        assert!("0x00158d0001234567".starts_with("0x"));
        assert!(!"living_room_lamp".starts_with("0x"));
        assert!(!"0".starts_with("0x")); // bare zero is not a hex address
    }

    #[test]
    fn mqtt_config_integrations_defaults_to_empty() {
        let cfg: MqttConfig = toml::from_str(r#"client_id = "test""#).unwrap();
        assert!(cfg.integrations.is_empty());
    }

    #[test]
    fn mqtt_config_integrations_parses() {
        let cfg: MqttConfig = toml::from_str(
            r#"client_id = "test"
               [[integrations]]
               pattern = "zigbee2mqtt/{device}/**"
               name = "Zigbee"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.integrations.len(), 1);
        assert_eq!(cfg.integrations[0].pattern, "zigbee2mqtt/{device}/**");
        assert_eq!(cfg.integrations[0].name.as_deref(), Some("Zigbee"));
    }

    // ── MqttConfig defaults ───────────────────────────────────────────────────

    #[test]
    fn mqtt_config_default_host() {
        let cfg: MqttConfig = toml::from_str(r#"client_id = "test""#).unwrap();
        assert_eq!(cfg.host, "localhost");
    }

    #[test]
    fn mqtt_config_default_port() {
        let cfg: MqttConfig = toml::from_str(r#"client_id = "test""#).unwrap();
        assert_eq!(cfg.port, 1883);
    }

    #[test]
    fn mqtt_config_default_topics_is_wildcard() {
        let cfg: MqttConfig = toml::from_str(r#"client_id = "test""#).unwrap();
        assert_eq!(cfg.topics, vec!["#"]);
    }

    #[test]
    fn mqtt_config_default_credentials_are_none() {
        let cfg: MqttConfig = toml::from_str(r#"client_id = "test""#).unwrap();
        assert!(cfg.username.is_none());
        assert!(cfg.password.is_none());
    }

    #[test]
    fn mqtt_config_explicit_values() {
        let cfg: MqttConfig = toml::from_str(r#"
            client_id = "my-client"
            host = "broker.example.com"
            port = 8883
            username = "user"
            password = "pass"
            topics = ["home/#", "sensors/#"]
        "#)
        .unwrap();
        assert_eq!(cfg.host, "broker.example.com");
        assert_eq!(cfg.port, 8883);
        assert_eq!(cfg.username.as_deref(), Some("user"));
        assert_eq!(cfg.password.as_deref(), Some("pass"));
        assert_eq!(cfg.topics, vec!["home/#", "sensors/#"]);
    }

    // ── MqttMessage serde ─────────────────────────────────────────────────────

    #[test]
    fn mqtt_message_round_trips_json() {
        let msg = MqttMessage {
            topic: "home/temp".into(),
            payload: "21.5".into(),
            received_at: "2026-03-15T12:00:00Z".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: MqttMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.topic, msg.topic);
        assert_eq!(decoded.payload, msg.payload);
        assert_eq!(decoded.received_at, msg.received_at);
    }

    #[test]
    fn mqtt_message_serialized_field_names() {
        let msg = MqttMessage {
            topic: "t".into(),
            payload: "p".into(),
            received_at: "r".into(),
        };
        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert!(v.get("topic").is_some());
        assert!(v.get("payload").is_some());
        assert!(v.get("received_at").is_some());
    }

    // ── utc_now ───────────────────────────────────────────────────────────────

    #[test]
    fn utc_now_is_rfc3339_format() {
        let ts = utc_now();
        // Must contain the date/time separator and end with Z (UTC).
        assert!(ts.contains('T'), "timestamp should contain T separator: {ts}");
        assert!(ts.ends_with('Z'), "timestamp should end with Z (UTC): {ts}");
        // Year must be plausible (2020+).
        let year: u32 = ts[..4].parse().expect("first 4 chars should be year");
        assert!(year >= 2020, "year {year} looks wrong");
    }

    #[test]
    fn utc_now_changes_over_time() {
        let t1 = utc_now();
        std::thread::sleep(Duration::from_millis(1_100));
        let t2 = utc_now();
        assert_ne!(t1, t2, "successive calls should differ by at least 1 second");
    }

    // ── handler unit tests ────────────────────────────────────────────────────

    /// Mock that records which topics were subscribed.
    #[derive(Default)]
    struct MockSubscriber {
        subscribed: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl MqttSubscriber for MockSubscriber {
        fn subscribe<'a>(
            &'a self,
            topic: &'a str,
            _qos: QoS,
        ) -> Pin<Box<dyn Future<Output = Result<(), rumqttc::ClientError>> + Send + 'a>> {
            self.subscribed.lock().unwrap().push(topic.to_owned());
            Box::pin(std::future::ready(Ok(())))
        }
    }

    #[tokio::test]
    async fn conn_ack_sets_status_connected_and_broadcasts() {
        let (tx, mut rx) = broadcast::channel(16);
        let (status_tx, _) = watch::channel("connecting".to_string());
        let status_tx = Arc::new(status_tx);
        handle_conn_ack(&MockSubscriber::default(), &[], &status_tx, &tx, "h", 1883).await;
        assert_eq!(*status_tx.borrow(), "connected");
        assert!(matches!(rx.try_recv(), Ok(BrokerEvent::Status { status }) if status == "connected"));
    }

    #[tokio::test]
    async fn conn_ack_subscribes_to_all_configured_topics() {
        let (tx, _rx) = broadcast::channel(16);
        let (status_tx, _) = watch::channel("connecting".to_string());
        let status_tx = Arc::new(status_tx);
        let subscribed = Arc::new(std::sync::Mutex::new(vec![]));
        let mock = MockSubscriber { subscribed: Arc::clone(&subscribed) };
        handle_conn_ack(
            &mock,
            &["home/#".to_string(), "sensors/+".to_string()],
            &status_tx,
            &tx,
            "h",
            1883,
        )
        .await;
        assert_eq!(*subscribed.lock().unwrap(), vec!["home/#", "sensors/+"]);
    }

    #[tokio::test]
    async fn publish_stored_in_buffer_and_broadcast() {
        let (tx, mut rx) = broadcast::channel(16);
        let recent = Arc::new(TokioMutex::new(VecDeque::new()));
        handle_publish("home/temp".to_string(), b"21.5", 200, &tx, &recent).await;
        let buf = recent.lock().await;
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].topic, "home/temp");
        assert_eq!(buf[0].payload, "21.5");
        drop(buf);
        assert!(matches!(rx.try_recv(), Ok(BrokerEvent::Message(m)) if m.topic == "home/temp"));
    }

    #[tokio::test]
    async fn publish_caps_buffer_at_scrollback_limit() {
        let (tx, _rx) = broadcast::channel(256);
        let recent = Arc::new(TokioMutex::new(VecDeque::new()));
        let cap = 3;
        for i in 0..5u8 {
            handle_publish(format!("t/{i}"), &[i], cap, &tx, &recent).await;
        }
        let buf = recent.lock().await;
        assert_eq!(buf.len(), cap);
        assert_eq!(buf[0].topic, "t/2");
        assert_eq!(buf[cap - 1].topic, "t/4");
    }

    #[test]
    fn error_sets_status_error_and_broadcasts() {
        let (tx, mut rx) = broadcast::channel(16);
        let (status_tx, _) = watch::channel("connected".to_string());
        let status_tx = Arc::new(status_tx);
        let err = rumqttc::ConnectionError::Io(std::io::Error::from(
            std::io::ErrorKind::ConnectionReset,
        ));
        handle_error(&err, &status_tx, &tx);
        assert_eq!(*status_tx.borrow(), "error");
        assert!(matches!(rx.try_recv(), Ok(BrokerEvent::Status { status }) if status == "error"));
    }

    // ── build_event_stream ────────────────────────────────────────────────────

    fn msg(topic: &str) -> MqttMessage {
        MqttMessage {
            topic: topic.into(),
            payload: "p".into(),
            received_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[tokio::test]
    async fn event_stream_first_event_is_initial_status() {
        let (tx, rx) = broadcast::channel(16);
        let stream = build_event_stream("connected".into(), vec![], rx);
        let events: Vec<_> = futures::StreamExt::take(stream, 1).collect().await;
        match &events[0] {
            BrokerEvent::Status { status } => assert_eq!(status, "connected"),
            _ => panic!("expected Status event first"),
        }
        drop(tx);
    }

    #[tokio::test]
    async fn event_stream_backlog_follows_status() {
        let (tx, rx) = broadcast::channel(16);
        let backlog = vec![msg("a"), msg("b")];
        let stream = build_event_stream("connected".into(), backlog, rx);
        // Take status + 2 history events
        let events: Vec<_> = futures::StreamExt::take(stream, 3).collect().await;
        assert!(matches!(&events[0], BrokerEvent::Status { .. }));
        assert!(matches!(&events[1], BrokerEvent::Message(m) if m.topic == "a"));
        assert!(matches!(&events[2], BrokerEvent::Message(m) if m.topic == "b"));
        drop(tx);
    }

    #[tokio::test]
    async fn event_stream_backlog_order_is_oldest_first() {
        let (tx, rx) = broadcast::channel(16);
        let backlog = vec![msg("first"), msg("second"), msg("third")];
        let stream = build_event_stream("connected".into(), backlog, rx);
        let events: Vec<_> = futures::StreamExt::take(stream, 4).collect().await;
        // events[0] = status; events[1..] = history in order
        let topics: Vec<&str> = events[1..]
            .iter()
            .map(|e| match e {
                BrokerEvent::Message(m) => m.topic.as_str(),
                _ => "",
            })
            .collect();
        assert_eq!(topics, ["first", "second", "third"]);
        drop(tx);
    }

    #[tokio::test]
    async fn event_stream_live_message_forwarded() {
        let (tx, rx) = broadcast::channel(16);
        // Send a live message before draining the stream past history
        let _ = tx.send(BrokerEvent::Message(msg("live")));
        // stream: 1 status (no backlog) + 1 live message
        let stream = build_event_stream("connected".into(), vec![], rx);
        let events: Vec<_> = futures::StreamExt::take(stream, 2).collect().await;
        assert!(matches!(&events[1], BrokerEvent::Message(m) if m.topic == "live"));
        drop(tx);
    }

    #[tokio::test]
    async fn event_stream_live_status_forwarded() {
        let (tx, rx) = broadcast::channel(16);
        let _ = tx.send(BrokerEvent::Status { status: "error".into() });
        let stream = build_event_stream("connected".into(), vec![], rx);
        let events: Vec<_> = futures::StreamExt::take(stream, 2).collect().await;
        assert!(matches!(&events[1], BrokerEvent::Status { status } if status == "error"));
        drop(tx);
    }

    #[tokio::test]
    async fn event_stream_ends_when_channel_closed() {
        let (tx, rx) = broadcast::channel(16);
        drop(tx);
        let stream = build_event_stream("connecting".into(), vec![], rx);
        // Only the initial status; live part immediately returns None
        let events: Vec<_> = stream.collect().await;
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], BrokerEvent::Status { status } if status == "connecting"));
    }

    // ── html_escape ───────────────────────────────────────────────────────────

    #[test]
    fn html_escape_ampersand() {
        assert_eq!(html_escape("a&b"), "a&amp;b");
    }

    #[test]
    fn html_escape_lt_gt() {
        assert_eq!(html_escape("<tag>"), "&lt;tag&gt;");
    }

    #[test]
    fn html_escape_quote() {
        assert_eq!(html_escape(r#"say "hi""#), "say &quot;hi&quot;");
    }

    #[test]
    fn html_escape_no_special_chars_unchanged() {
        assert_eq!(html_escape("hello world"), "hello world");
    }

    #[test]
    fn html_escape_all_specials() {
        assert_eq!(html_escape(r#"<a href="x&y">z</a>"#), "&lt;a href=&quot;x&amp;y&quot;&gt;z&lt;/a&gt;");
    }

    // ── truncate_at_char ──────────────────────────────────────────────────────

    #[test]
    fn truncate_at_char_ascii_within_limit() {
        assert_eq!(truncate_at_char("hello", 10), "hello");
    }

    #[test]
    fn truncate_at_char_ascii_at_limit() {
        assert_eq!(truncate_at_char("hello", 5), "hello");
    }

    #[test]
    fn truncate_at_char_ascii_over_limit() {
        assert_eq!(truncate_at_char("hello world", 5), "hello");
    }

    #[test]
    fn truncate_at_char_unicode_over_limit() {
        // "héllo" is 5 chars but 6 bytes; truncate at 3 chars → "hél"
        assert_eq!(truncate_at_char("héllo", 3), "hél");
    }

    #[test]
    fn truncate_at_char_empty_string() {
        assert_eq!(truncate_at_char("", 5), "");
    }

    // ── format_time ───────────────────────────────────────────────────────────

    #[test]
    fn format_time_extracts_hhmmsss() {
        assert_eq!(format_time("2026-03-17T23:15:24Z"), "23:15:24");
    }

    #[test]
    fn format_time_fallback_on_non_iso() {
        assert_eq!(format_time("not-a-date"), "not-a-date");
    }

    #[test]
    fn format_time_fallback_when_t_near_end() {
        // "T" exists but not enough chars after it
        assert_eq!(format_time("20260317T23"), "20260317T23");
    }

    // ── format_entry_value ────────────────────────────────────────────────────

    #[test]
    fn format_entry_value_null() {
        assert_eq!(format_entry_value(&serde_json::Value::Null), "null");
    }

    #[test]
    fn format_entry_value_bool_true() {
        assert_eq!(format_entry_value(&serde_json::Value::Bool(true)), "true");
    }

    #[test]
    fn format_entry_value_number() {
        assert_eq!(format_entry_value(&serde_json::json!(42)), "42");
    }

    #[test]
    fn format_entry_value_string() {
        assert_eq!(format_entry_value(&serde_json::json!("hello")), "hello");
    }

    #[test]
    fn format_entry_value_object_serialized() {
        let v = serde_json::json!({"a": 1});
        let s = format_entry_value(&v);
        assert!(s.contains("\"a\""));
    }

    // ── render_payload_body ───────────────────────────────────────────────────

    #[test]
    fn render_payload_body_json_object_produces_dl() {
        let html = render_payload_body(r#"{"state":"ON","brightness":200}"#);
        assert!(html.contains("mqtt-msg-kv"), "should produce kv grid");
        assert!(html.contains("state"), "key present");
        assert!(html.contains("ON"), "value present");
    }

    #[test]
    fn render_payload_body_plain_text_produces_pre() {
        let html = render_payload_body("21.5");
        assert!(html.contains("<pre"), "plain text gets pre tag");
        assert!(html.contains("21.5"));
    }

    #[test]
    fn render_payload_body_plain_text_is_escaped() {
        let html = render_payload_body("<b>bold</b>");
        assert!(html.contains("&lt;b&gt;"), "HTML is escaped");
    }

    #[test]
    fn render_payload_body_long_plain_text_uses_details() {
        let long = "x".repeat(TRUNCATE_LIMIT + 1);
        let html = render_payload_body(&long);
        assert!(html.contains("<details"), "long payload uses details element");
    }

    #[test]
    fn render_payload_body_json_array_treated_as_non_object() {
        let html = render_payload_body("[1,2,3]");
        assert!(!html.contains("mqtt-msg-kv"), "array should not produce kv grid");
    }

    // ── render_message_card ───────────────────────────────────────────────────

    #[test]
    fn render_message_card_contains_topic() {
        let msg = MqttMessage { topic: "home/temp".to_owned(), payload: "21.5".to_owned(), received_at: "2026-01-01T12:00:00Z".to_owned() };
        let html = render_message_card(&msg);
        assert!(html.contains("home/temp"));
        assert!(html.contains("mqtt-msg"));
    }

    #[test]
    fn render_message_card_topic_is_escaped() {
        let msg = MqttMessage { topic: "home/<test>".to_owned(), payload: "".to_owned(), received_at: "2026-01-01T00:00:00Z".to_owned() };
        let html = render_message_card(&msg);
        assert!(html.contains("&lt;test&gt;"), "topic is HTML-escaped");
    }

    #[test]
    fn render_message_card_includes_time() {
        let msg = MqttMessage { topic: "t".to_owned(), payload: "p".to_owned(), received_at: "2026-03-17T23:15:24Z".to_owned() };
        let html = render_message_card(&msg);
        assert!(html.contains("23:15:24"), "formatted time present");
    }

    // ── render_status_html ────────────────────────────────────────────────────

    #[test]
    fn render_status_html_connected() {
        let html = render_status_html("connected");
        assert!(html.contains("mqtt-dot-connected"));
        assert!(html.contains("connected"));
    }

    #[test]
    fn render_status_html_escapes_special_chars() {
        let html = render_status_html("err<or>");
        assert!(html.contains("err&lt;or&gt;"));
    }

    // ── publish_route / device_messages_route handler tests ───────────────────

    use crate::{
        auth::{AuthConfig, AuthState, Role, SessionData},
        breaker::BreakerContent,
        breaker_detail::{BreakerData, BreakerStore},
        index::Index,
        route::Routes,
    };
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::{get, post},
        Router,
    };
    use std::{collections::HashMap, path::Path, time::Instant};
    use tower::ServiceExt;
    use uuid::Uuid;

    async fn state_with_mqtt() -> ServerState {
        let auth_state = AuthState::new_for_testing(AuthConfig {
            rp_id: "localhost".to_string(),
            rp_origin: "http://localhost".to_string(),
            db_url: "postgres://localhost/nonexistent".to_string(),
            gm_users: vec!["gm".to_string()],
            ntfy_url: None,
        })
        .unwrap();

        let (tx, _) = broadcast::channel(16);
        let (publish_client, eventloop) =
            AsyncClient::new(MqttOptions::new("green-test", "localhost", 1883), 64);
        // Hold the EventLoop alive in a spawned task so the internal channel
        // stays open, allowing publish() to enqueue without error.
        let _ = tokio::spawn(async move {
            let _hold = eventloop;
            std::future::pending::<()>().await
        });
        let integrations = parse_integrations(&[IntegrationConfig {
            pattern: "zigbee2mqtt/{device}/**".to_string(),
            name: None,
        }]);
        let mqtt_state = Arc::new(MqttState {
            tx,
            status_tx: Arc::new(watch::channel("connecting".to_string()).0),
            recent_messages: Arc::new(TokioMutex::new(VecDeque::new())),
            prometheus: None,
            integrations: Arc::new(integrations),
            publish_client,
        });

        let store = Arc::new(
            BreakerStore::from_data(BreakerData {
                todos: vec![],
                slots: HashMap::new(),
                couples: vec![],
            })
            .unwrap(),
        );
        let breaker_content = Arc::new(BreakerContent::new(store.as_ref()));
        ServerState {
            certificate: Arc::from(""),
            breaker_content,
            breaker_detail_store: store,
            index: Index::new(Routes::default(), false, false, false, false).await.unwrap(),
            tailscale_socket: Arc::from(Path::new("/tmp/fake.sock")),
            notes_store: None,
            auth_state: Some(Arc::new(auth_state)),
            mqtt_state: Some(mqtt_state),
            log_config: None,
        }
    }

    async fn insert_gm_session(state: &ServerState) -> String {
        let auth = state.auth_state.as_ref().unwrap();
        let token = Uuid::new_v4().to_string();
        let _ = auth.session_store.write().await.insert(
            token.clone(),
            SessionData {
                user_id: Uuid::new_v4(),
                username: "gm".to_string(),
                role: Role::Gm,
                created_at: Instant::now(),
            },
        );
        token
    }

    #[test]
    fn publish_request_deserializes_from_json() {
        let json = r#"{"topic":"zigbee2mqtt/bulb/set","payload":"{\"state\":\"ON\"}"}"#;
        let req: MqttPublishRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.topic, "zigbee2mqtt/bulb/set");
        assert!(req.payload.contains("ON"));
    }

    #[tokio::test]
    async fn publish_route_returns_204_for_gm() {
        let state = state_with_mqtt().await;
        let token = insert_gm_session(&state).await;
        let app = Router::new()
            .route("/api/mqtt/publish", post(publish_route))
            .with_state(state);
        let req = Request::builder()
            .method("POST")
            .uri("/api/mqtt/publish")
            .header("content-type", "application/json")
            .header("cookie", format!("green_session={token}"))
            .body(Body::from(
                r#"{"topic":"test/bulb/set","payload":"{\"state\":\"ON\"}"}"#,
            ))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn device_messages_route_includes_cmd_form() {
        let state = state_with_mqtt().await;
        let token = insert_gm_session(&state).await;
        let app = Router::new()
            .route("/api/mqtt/device-messages", get(device_messages_route))
            .with_state(state);
        let req = Request::builder()
            .method("GET")
            .uri("/api/mqtt/device-messages?integration=zigbee2mqtt&device=0xABCD")
            .header("cookie", format!("green_session={token}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let html = std::str::from_utf8(&bytes).unwrap();
        assert!(html.contains("device-cmd-form"), "panel should include the command form");
        assert!(
            html.contains("no recent messages") || html.contains("leet-muted"),
            "empty buffer should show muted message"
        );
    }

    #[tokio::test]
    async fn mqtt_page_route_returns_html_for_gm() {
        let state = state_with_mqtt().await;
        let token = insert_gm_session(&state).await;
        let app = Router::new()
            .route("/mqtt", get(mqtt_page_route))
            .with_state(state);
        let req = Request::builder()
            .uri("/mqtt")
            .header("cookie", format!("green_session={token}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let html = std::str::from_utf8(&bytes).unwrap();
        assert!(html.contains("mqtt"), "page contains mqtt content");
    }

    #[tokio::test]
    async fn mqtt_stream_route_returns_sse_for_gm() {
        let state = state_with_mqtt().await;
        let token = insert_gm_session(&state).await;
        let app = Router::new()
            .route("/api/mqtt/stream", get(mqtt_stream_route))
            .with_state(state);
        let req = Request::builder()
            .uri("/api/mqtt/stream")
            .header("cookie", format!("green_session={token}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
        assert!(ct.contains("text/event-stream"), "SSE content-type");
    }

    #[tokio::test]
    async fn metrics_route_returns_text_plain_with_prometheus() {
        // Build a state with Prometheus configured.
        let (tx, _) = broadcast::channel(16);
        let (publish_client, eventloop) =
            AsyncClient::new(MqttOptions::new("green-test-metrics", "localhost", 1883), 64);
        let _ = tokio::spawn(async move {
            let _hold = eventloop;
            std::future::pending::<()>().await
        });
        let registry = prometheus::Registry::new();
        let messages_total = prometheus::IntCounterVec::new(
            prometheus::opts!("mqtt_messages_total", "test"),
            &["integration", "device"],
        )
        .unwrap();
        let _ = registry.register(Box::new(messages_total.clone()));
        messages_total.with_label_values(&["zigbee2mqtt", "0xABCD"]).inc();
        let mqtt_state = Arc::new(MqttState {
            tx,
            status_tx: Arc::new(watch::channel("connecting".to_string()).0),
            recent_messages: Arc::new(TokioMutex::new(VecDeque::new())),
            prometheus: Some(PrometheusState { registry, messages_total }),
            integrations: Arc::new(vec![]),
            publish_client,
        });
        let store = Arc::new(
            BreakerStore::from_data(BreakerData { todos: vec![], slots: HashMap::new(), couples: vec![] })
                .unwrap(),
        );
        let state = ServerState {
            certificate: Arc::from(""),
            breaker_content: Arc::new(BreakerContent::new(store.as_ref())),
            breaker_detail_store: store,
            index: Index::new(Routes::default(), false, false, false, false).await.unwrap(),
            tailscale_socket: Arc::from(Path::new("/tmp/fake.sock")),
            notes_store: None,
            auth_state: None,
            mqtt_state: Some(mqtt_state),
            log_config: None,
        };
        let app = Router::new()
            .route("/metrics", get(metrics_route))
            .with_state(state);
        let req = Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp.headers().get("content-type").unwrap().to_str().unwrap();
        assert!(ct.contains("text/plain"), "metrics uses text/plain");
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let body = std::str::from_utf8(&bytes).unwrap();
        assert!(body.contains("mqtt_messages_total"), "prometheus metric present");
    }
}

#[derive(Template)]
#[template(path = "mqtt.html")]
struct MqttPage {
    version: &'static str,
    auth_user: Option<AuthUserInfo>,
}

/// GET `/mqtt` — renders the MQTT live-feed page (GM only).
pub async fn mqtt_page_route(
    user: GmUser,
    State(_state): State<ServerState>,
) -> Result<Html<String>, Error> {
    let auth_user = Some(AuthUserInfo {
        username: user.0.username.clone(),
        role: user.0.role.clone(),
    });
    let page = MqttPage {
        version: crate::VERSION,
        auth_user,
    };
    Ok(Html(page.render()?))
}

/// Build the logical event stream for a new SSE client.
///
/// Emits:
/// 1. A [`BrokerEvent::Status`] with `current_status` (sent immediately so the
///    client doesn't have to wait for the next real event).
/// 2. One [`BrokerEvent::Message`] per `backlog` entry (oldest first).
/// 3. Live [`BrokerEvent`]s from `rx` as they arrive.
///
/// The stream ends when `rx`'s broadcast channel is closed.
fn build_event_stream(
    current_status: String,
    backlog: Vec<MqttMessage>,
    rx: broadcast::Receiver<BrokerEvent>,
) -> impl futures::Stream<Item = BrokerEvent> {
    let status_stream =
        futures::stream::once(std::future::ready(BrokerEvent::Status { status: current_status }));
    let history_stream = futures::stream::iter(backlog.into_iter().map(BrokerEvent::Message));
    let live_stream = futures::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(event) => return Some((event, rx)),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(n, "mqtt sse client lagged, skipping messages");
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    status_stream.chain(history_stream).chain(live_stream)
}

// ─── Server-side card rendering ──────────────────────────────────────────────

/// Maximum characters shown in the payload preview (non-object payloads).
const TRUNCATE_LIMIT: usize = 280;
/// Maximum characters shown per key-value entry in object payloads.
const VALUE_TRUNCATE: usize = 120;

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Return a `&str` sub-slice of at most `max_chars` Unicode scalar values.
/// Always splits at a valid char boundary.
fn truncate_at_char(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => &s[..byte_idx],
        None => s,
    }
}

/// Extract HH:MM:SS from an ISO 8601 timestamp (e.g. `"2026-03-17T23:15:24Z"` → `"23:15:24"`).
fn format_time(iso: &str) -> String {
    iso.find('T')
        .filter(|&t| iso.len() >= t + 9)
        .map(|t| iso[t + 1..t + 9].to_owned())
        .unwrap_or_else(|| iso.to_owned())
}

/// Render a single JSON value as a display string.
fn format_entry_value(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::Null => "null".to_owned(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.clone(),
        _ => val.to_string(),
    }
}

/// Render the body of an MQTT card:
/// - JSON objects → `<dl>` key-value grid
/// - Anything else → `<pre>` (with `<details>` expand/collapse if truncated)
fn render_payload_body(raw: &str) -> String {
    if let Ok(serde_json::Value::Object(obj)) = serde_json::from_str::<serde_json::Value>(raw) {
        let mut rows = String::new();
        for (key, val) in &obj {
            let key_esc = html_escape(key);
            let value_str = format_entry_value(val);
            let (display, title_attr) = if value_str.chars().count() > VALUE_TRUNCATE {
                let preview = html_escape(&format!("{}…", truncate_at_char(&value_str, VALUE_TRUNCATE)));
                let title = html_escape(&value_str);
                (preview, format!(r#" title="{title}""#))
            } else {
                (html_escape(&value_str), String::new())
            };
            rows.push_str(&format!(
                r#"<dt class="mqtt-kv-key">{key_esc}</dt><dd class="mqtt-kv-value"{title_attr}>{display}</dd>"#
            ));
        }
        return format!(r#"<dl class="mqtt-msg-kv">{rows}</dl>"#);
    }

    let full = serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| raw.to_owned());

    if full.chars().count() > TRUNCATE_LIMIT {
        let preview = html_escape(truncate_at_char(&full, TRUNCATE_LIMIT));
        let full_esc = html_escape(&full);
        format!(r#"<details class="mqtt-msg-expand"><summary class="mqtt-msg-body">{preview}…</summary><pre class="mqtt-msg-body">{full_esc}</pre></details>"#)
    } else {
        format!(r#"<pre class="mqtt-msg-body">{}</pre>"#, html_escape(&full))
    }
}

/// Render an MQTT message as an HTML card fragment for SSE delivery.
fn render_message_card(msg: &MqttMessage) -> String {
    let topic_esc = html_escape(&msg.topic);
    let body = render_payload_body(&msg.payload);
    let time = format_time(&msg.received_at);
    format!(
        r#"<div class="mqtt-msg mqtt-msg-new" data-topic="{topic_esc}" data-received-at="{received_at}"><div class="mqtt-msg-header"><span class="mqtt-msg-topic" title="{topic_esc}">{topic_esc}</span><span class="mqtt-msg-time">{time}</span></div>{body}</div>"#,
        received_at = msg.received_at,
    )
}

/// Render the status bar inner HTML for SSE delivery.
fn render_status_html(status: &str) -> String {
    let status_esc = html_escape(status);
    format!(
        r#"<span class="mqtt-dot mqtt-dot-{status_esc}"></span><span class="mqtt-status-text">{status_esc}</span>"#
    )
}

/// Map a [`BrokerEvent`] stream to SSE wire events.
/// Messages are sent as pre-rendered HTML card fragments (event name `message`).
/// Status changes are sent as pre-rendered HTML status bar fragments (event name `broker`).
fn build_sse_stream(
    current_status: String,
    backlog: Vec<MqttMessage>,
    rx: broadcast::Receiver<BrokerEvent>,
) -> impl futures::Stream<Item = Result<Event, Infallible>> {
    build_event_stream(current_status, backlog, rx).map(|ev| {
        Ok(match ev {
            BrokerEvent::Message(msg) => Event::default().data(render_message_card(&msg)),
            BrokerEvent::Status { status } => {
                Event::default().event("broker").data(render_status_html(&status))
            }
        })
    })
}

/// GET `/api/mqtt/stream` — SSE stream of live MQTT messages (GM only).
pub async fn mqtt_stream_route(
    _user: GmUser,
    State(state): State<ServerState>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, Error> {
    let mqtt = state.mqtt_state.as_ref().ok_or(Error::MqttNotConfigured)?;
    let rx = mqtt.tx.subscribe();

    let current_status = mqtt.status_tx.borrow().clone();
    let backlog: Vec<MqttMessage> = mqtt.recent_messages.lock().await.iter().cloned().collect();

    Ok(Sse::new(build_sse_stream(current_status, backlog, rx)).keep_alive(KeepAlive::default()))
}

// ─── Publish endpoint ────────────────────────────────────────────────────────

/// Request body for `POST /api/mqtt/publish`.
#[derive(Debug, Deserialize)]
pub struct MqttPublishRequest {
    /// Full MQTT topic to publish to.
    pub topic: String,
    /// Payload string (typically JSON for smart-home integrations).
    pub payload: String,
}

/// POST `/api/mqtt/publish` — publish a message to the broker (GM only).
pub async fn publish_route(
    _user: GmUser,
    State(state): State<ServerState>,
    Json(req): Json<MqttPublishRequest>,
) -> Result<axum::http::StatusCode, Error> {
    let mqtt = state.mqtt_state.as_ref().ok_or(Error::MqttNotConfigured)?;
    mqtt.publish_client
        .publish(&req.topic, QoS::AtLeastOnce, false, req.payload.as_bytes().to_vec())
        .await
        .map_err(|e| Error::Database(format!("mqtt publish: {e}")))?;
    tracing::info!(topic = %req.topic, "published mqtt message");
    Ok(axum::http::StatusCode::NO_CONTENT)
}

// ─── Device messages panel ───────────────────────────────────────────────────

/// Query parameters for the device messages endpoint.
#[derive(Debug, Deserialize)]
pub struct DeviceMessagesQuery {
    /// Integration display name (must match a configured integration).
    pub integration: String,
    /// Device ID to filter messages for.
    pub device: String,
}

/// GET `/api/mqtt/device-messages` — returns recent ring-buffer messages for one device
/// as pre-rendered HTML card fragments (GM only; no DB required).
pub async fn device_messages_route(
    _user: GmUser,
    State(state): State<ServerState>,
    Query(params): Query<DeviceMessagesQuery>,
) -> Result<Html<String>, Error> {
    let mqtt = state.mqtt_state.as_ref().ok_or(Error::MqttNotConfigured)?;

    let integration = mqtt
        .integrations
        .iter()
        .find(|i| i.display_name == params.integration)
        .ok_or(Error::NotFound)?;

    let messages: Vec<MqttMessage> = mqtt
        .recent_messages
        .lock()
        .await
        .iter()
        .filter(|msg| {
            match_topic(&integration.segments, &msg.topic).as_deref()
                == Some(params.device.as_str())
        })
        .cloned()
        .collect();

    let messages_html: String = if messages.is_empty() {
        r#"<p class="leet-muted">no recent messages in buffer</p>"#.to_owned()
    } else {
        messages.iter().map(render_message_card).collect()
    };

    let form_html = r#"<form class="device-cmd-form">
<div class="device-cmd-fields">
<input class="device-cmd-topic" name="topic" type="text" placeholder="topic  e.g. zigbee2mqtt/device/set" autocomplete="off" spellcheck="false">
<textarea class="device-cmd-payload" name="payload" rows="2" placeholder='payload  e.g. {"state":"ON","brightness":200}'></textarea>
</div>
<div class="device-cmd-actions">
<button class="leet-btn" type="submit">send</button>
<span class="device-cmd-status"></span>
</div>
</form>"#;

    Ok(Html(format!(
        r#"{messages_html}<hr class="device-cmd-sep">{form_html}"#
    )))
}

// ─── Devices page ─────────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "mqtt_devices.html")]
struct MqttDevicesPage {
    devices: Vec<DeviceRow>,
    auth_user: Option<AuthUserInfo>,
    version: &'static str,
}

/// GET `/mqtt/devices` — MQTT device inventory table (GM only).
pub async fn mqtt_devices_route(
    user: GmUser,
    State(state): State<ServerState>,
) -> Result<Html<String>, Error> {
    let auth = state.auth_state.as_ref().ok_or(Error::MqttNotConfigured)?;
    let auth_user = Some(AuthUserInfo {
        username: user.0.username.clone(),
        role: user.0.role.clone(),
    });

    let rows = sqlx::query(
        "SELECT integration, device_id,
                to_char(first_seen AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS') AS first_seen,
                to_char(last_seen  AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS') AS last_seen,
                message_count
         FROM mqtt_devices
         ORDER BY integration, device_id",
    )
    .fetch_all(&auth.db)
    .await
    .map_err(|e| Error::Database(e.to_string()))?;

    let devices: Vec<DeviceRow> = rows
        .into_iter()
        .map(|row| {
            use sqlx::Row as _;
            DeviceRow {
                integration: row.get("integration"),
                device_id: row.get("device_id"),
                first_seen: row.get("first_seen"),
                last_seen: row.get("last_seen"),
                message_count: row.get("message_count"),
            }
        })
        .collect();

    let page = MqttDevicesPage { devices, auth_user, version: crate::VERSION };
    Ok(Html(page.render()?))
}

// ─── Prometheus metrics endpoint ─────────────────────────────────────────────

/// GET `/metrics` — Prometheus text exposition format (no auth; Prometheus scrapers
/// can't do cookie auth). Expose only when integrations are configured.
pub async fn metrics_route(
    State(state): State<ServerState>,
) -> Result<([(axum::http::HeaderName, &'static str); 1], String), Error> {
    let mqtt = state.mqtt_state.as_ref().ok_or(Error::MqttNotConfigured)?;
    let ps = mqtt.prometheus.as_ref().ok_or(Error::MqttNotConfigured)?;

    let encoder = prometheus::TextEncoder::new();
    let metric_families = ps.registry.gather();
    let body = encoder
        .encode_to_string(&metric_families)
        .map_err(|e| Error::PrometheusEncode(e.to_string()))?;

    Ok(([(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4")], body))
}
