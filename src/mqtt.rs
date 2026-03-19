//! MQTT live-feed page: background subscriber task + SSE fan-out.

use std::{
    collections::VecDeque,
    convert::Infallible,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use time::format_description::well_known::Rfc3339;

use askama::Template;
use axum::{
    extract::State,
    response::{
        Html,
        sse::{Event, KeepAlive, Sse},
    },
};
use futures::StreamExt as _;
use rumqttc::{AsyncClient, Event as MqttEvent, MqttOptions, Packet, QoS};
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, broadcast};

use crate::{
    auth::{AuthUserInfo, GmUser},
    error::Error,
    ServerState,
};

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

/// Shared MQTT fan-out state stored in [`ServerState`].
#[derive(Debug)]
pub struct MqttState {
    /// Broadcast sender; SSE handlers subscribe by calling `tx.subscribe()`.
    pub tx: broadcast::Sender<BrokerEvent>,
    /// Last known broker status (`"connected"`, `"error"`, `"connecting"`).
    /// Sent immediately to new SSE clients so they don't wait for the next event.
    pub last_status: Arc<RwLock<String>>,
    /// Ring buffer of recent messages replayed to new SSE clients on connect.
    pub recent_messages: Arc<RwLock<VecDeque<MqttMessage>>>,
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
    last_status: &Arc<RwLock<String>>,
    tx: &broadcast::Sender<BrokerEvent>,
    host: &str,
    port: u16,
) {
    tracing::info!(host, port, "MQTT connected");
    *last_status.write().await = "connected".into();
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
    recent_messages: &Arc<RwLock<VecDeque<MqttMessage>>>,
) {
    let msg = MqttMessage {
        topic,
        payload: String::from_utf8_lossy(payload).into_owned(),
        received_at: utc_now(),
    };
    tracing::debug!(topic = %msg.topic, "MQTT message received");
    {
        let mut buf = recent_messages.write().await;
        if buf.len() == scrollback {
            let _ = buf.pop_front();
        }
        buf.push_back(msg.clone());
    }
    let _ = tx.send(BrokerEvent::Message(msg));
}

/// Handle an event loop error: update status and broadcast.
/// Extracted for testability — the retry sleep stays in [`run_mqtt_task`].
async fn handle_error(
    err: &rumqttc::ConnectionError,
    last_status: &Arc<RwLock<String>>,
    tx: &broadcast::Sender<BrokerEvent>,
) {
    tracing::warn!(%err, "MQTT eventloop error, will retry");
    *last_status.write().await = "error".into();
    let _ = tx.send(BrokerEvent::Status { status: "error".into() });
}

/// Spawn the MQTT subscriber task. Runs forever, reconnecting automatically.
pub async fn run_mqtt_task(
    config: MqttConfig,
    tx: broadcast::Sender<BrokerEvent>,
    last_status: Arc<RwLock<String>>,
    recent_messages: Arc<RwLock<VecDeque<MqttMessage>>>,
) {
    let mut opts = MqttOptions::new("green-mqtt", &config.host, config.port);
    let _ = opts.set_keep_alive(Duration::from_secs(30));
    // Some topics (e.g. Frigate snapshots, zigbee2mqtt device lists) send large
    // payloads. Raise the limit to 1 MiB to avoid repeated reconnect loops.
    let _ = opts.set_max_packet_size(1024 * 1024, 1024 * 1024);
    if let (Some(user), Some(pass)) = (config.username.as_deref(), config.password.as_deref()) {
        let _ = opts.set_credentials(user, pass);
    }

    let (client, mut eventloop) = AsyncClient::new(opts, 64);

    loop {
        match eventloop.poll().await {
            Ok(MqttEvent::Incoming(Packet::Publish(publish))) => {
                handle_publish(publish.topic, &publish.payload, config.scrollback, &tx, &recent_messages).await;
            }
            Ok(MqttEvent::Incoming(Packet::ConnAck(_))) => {
                handle_conn_ack(&client, &config.topics, &last_status, &tx, &config.host, config.port).await;
            }
            Ok(_) => {}
            Err(err) => {
                handle_error(&err, &last_status, &tx).await;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
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

    // ── MqttConfig defaults ───────────────────────────────────────────────────

    #[test]
    fn mqtt_config_default_host() {
        let cfg: MqttConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.host, "localhost");
    }

    #[test]
    fn mqtt_config_default_port() {
        let cfg: MqttConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.port, 1883);
    }

    #[test]
    fn mqtt_config_default_topics_is_wildcard() {
        let cfg: MqttConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.topics, vec!["#"]);
    }

    #[test]
    fn mqtt_config_default_credentials_are_none() {
        let cfg: MqttConfig = toml::from_str("").unwrap();
        assert!(cfg.username.is_none());
        assert!(cfg.password.is_none());
    }

    #[test]
    fn mqtt_config_explicit_values() {
        let cfg: MqttConfig = toml::from_str(r#"
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
        let last_status = Arc::new(RwLock::new("connecting".to_string()));
        handle_conn_ack(&MockSubscriber::default(), &[], &last_status, &tx, "h", 1883).await;
        assert_eq!(&*last_status.read().await, "connected");
        assert!(matches!(rx.try_recv(), Ok(BrokerEvent::Status { status }) if status == "connected"));
    }

    #[tokio::test]
    async fn conn_ack_subscribes_to_all_configured_topics() {
        let (tx, _rx) = broadcast::channel(16);
        let last_status = Arc::new(RwLock::new("connecting".to_string()));
        let subscribed = Arc::new(std::sync::Mutex::new(vec![]));
        let mock = MockSubscriber { subscribed: Arc::clone(&subscribed) };
        handle_conn_ack(
            &mock,
            &["home/#".to_string(), "sensors/+".to_string()],
            &last_status,
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
        let recent = Arc::new(RwLock::new(VecDeque::new()));
        handle_publish("home/temp".to_string(), b"21.5", 200, &tx, &recent).await;
        let buf = recent.read().await;
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].topic, "home/temp");
        assert_eq!(buf[0].payload, "21.5");
        drop(buf);
        assert!(matches!(rx.try_recv(), Ok(BrokerEvent::Message(m)) if m.topic == "home/temp"));
    }

    #[tokio::test]
    async fn publish_caps_buffer_at_scrollback_limit() {
        let (tx, _rx) = broadcast::channel(256);
        let recent = Arc::new(RwLock::new(VecDeque::new()));
        let cap = 3;
        for i in 0..5u8 {
            handle_publish(format!("t/{i}"), &[i], cap, &tx, &recent).await;
        }
        let buf = recent.read().await;
        assert_eq!(buf.len(), cap);
        assert_eq!(buf[0].topic, "t/2");
        assert_eq!(buf[cap - 1].topic, "t/4");
    }

    #[tokio::test]
    async fn error_sets_status_error_and_broadcasts() {
        let (tx, mut rx) = broadcast::channel(16);
        let last_status = Arc::new(RwLock::new("connected".to_string()));
        let err = rumqttc::ConnectionError::Io(std::io::Error::from(
            std::io::ErrorKind::ConnectionReset,
        ));
        handle_error(&err, &last_status, &tx).await;
        assert_eq!(&*last_status.read().await, "error");
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
    let time = format_time(&msg.received_at);
    let topic_esc = html_escape(&msg.topic);
    let body = render_payload_body(&msg.payload);
    format!(
        r#"<div class="mqtt-msg mqtt-msg-new" data-topic="{topic_esc}"><div class="mqtt-msg-header"><span class="mqtt-msg-topic" title="{topic_esc}">{topic_esc}</span><span class="mqtt-msg-time">{time}</span></div>{body}</div>"#
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

    let current_status = mqtt
        .last_status
        .read()
        .await
        .clone();
    let backlog: Vec<MqttMessage> = mqtt
        .recent_messages
        .read()
        .await
        .iter()
        .cloned()
        .collect();

    Ok(Sse::new(build_sse_stream(current_status, backlog, rx)).keep_alive(KeepAlive::default()))
}
