//! MQTT live-feed page: background subscriber task + SSE fan-out.

use std::{convert::Infallible, time::Duration};

use time::format_description::well_known::Rfc3339;

use askama::Template;
use axum::{
    extract::State,
    response::{
        Html,
        sse::{Event, KeepAlive, Sse},
    },
};
use rumqttc::{AsyncClient, Event as MqttEvent, MqttOptions, Packet, QoS};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

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

/// Shared MQTT fan-out state stored in [`ServerState`].
#[derive(Debug)]
pub struct MqttState {
    /// Broadcast sender; SSE handlers subscribe by calling `tx.subscribe()`.
    pub tx: broadcast::Sender<MqttMessage>,
}

/// Spawn the MQTT subscriber task. Runs forever, reconnecting automatically.
pub async fn run_mqtt_task(config: MqttConfig, tx: broadcast::Sender<MqttMessage>) {
    let mut opts = MqttOptions::new("green-mqtt", &config.host, config.port);
    let _ = opts.set_keep_alive(Duration::from_secs(30));
    if let (Some(user), Some(pass)) = (config.username.as_deref(), config.password.as_deref()) {
        let _ = opts.set_credentials(user, pass);
    }

    let (client, mut eventloop) = AsyncClient::new(opts, 64);

    // Topics are subscribed in the ConnAck handler so that both the initial
    // connection and automatic reconnects use the same code path.

    loop {
        match eventloop.poll().await {
            Ok(MqttEvent::Incoming(Packet::Publish(publish))) => {
                let payload = String::from_utf8_lossy(&publish.payload).into_owned();
                let msg = MqttMessage {
                    topic: publish.topic,
                    payload,
                    received_at: utc_now(),
                };
                // Ignore send errors — no subscribers is fine.
                let _ = tx.send(msg);
            }
            Ok(MqttEvent::Incoming(Packet::ConnAck(_))) => {
                tracing::info!(host = %config.host, port = config.port, "MQTT connected");
                // Re-subscribe after reconnect.
                for topic in &config.topics {
                    if let Err(err) = client.subscribe(topic, QoS::AtMostOnce).await {
                        tracing::warn!(%err, %topic, "failed to re-subscribe after reconnect");
                    }
                }
            }
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(%err, "MQTT eventloop error, will retry");
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

/// GET `/api/mqtt/stream` — SSE stream of live MQTT messages (GM only).
pub async fn mqtt_stream_route(
    _user: GmUser,
    State(state): State<ServerState>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, Error> {
    let mqtt = state.mqtt_state.as_ref().ok_or(Error::MqttNotConfigured)?;
    let rx = mqtt.tx.subscribe();

    let stream = futures::stream::unfold(rx, |mut rx| async move {
        loop {
            match rx.recv().await {
                Ok(msg) => {
                    let event = Event::default().json_data(&msg).ok()?;
                    return Some((Ok::<_, Infallible>(event), rx));
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(n, "mqtt sse client lagged, skipping messages");
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
