use std::{collections::HashMap, path::Path, sync::Arc};

use askama::Template;
use axum::{extract::State, response::Html};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{
    ServerState,
    auth::{AuthUserInfo, GmUser},
    error::Error,
    index::NavLink,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct TailscaleStatus {
    pub version: String,
    pub backend_state: String,
    #[serde(rename = "Self")]
    pub self_peer: TailscalePeer,
    #[serde(default)]
    pub peer: HashMap<String, TailscalePeer>,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(Default))]
#[serde(rename_all = "PascalCase")]
pub struct TailscalePeer {
    pub host_name: String,
    #[serde(rename = "DNSName")]
    pub dns_name: String,
    #[serde(rename = "OS")]
    pub os: String,
    #[serde(rename = "TailscaleIPs")]
    pub tailscale_ips: Option<Vec<String>>,
    pub online: Option<bool>,
    pub active: Option<bool>,
    pub relay: Option<String>,
    pub rx_bytes: Option<u64>,
    pub tx_bytes: Option<u64>,
    pub last_seen: Option<String>,
    pub last_handshake: Option<String>,
    pub exit_node: Option<bool>,
    pub exit_node_option: Option<bool>,
    pub keep_alive: Option<bool>,
    pub tags: Option<Vec<String>>,
}

impl TailscalePeer {
    fn fmt_bytes(n: u64) -> String {
        const KIB: u64 = 1024;
        const MIB: u64 = KIB * 1024;
        const GIB: u64 = MIB * 1024;
        if n < KIB {
            format!("{n} B")
        } else if n < MIB {
            format!("{:.1} KiB", n as f64 / KIB as f64)
        } else if n < GIB {
            format!("{:.1} MiB", n as f64 / MIB as f64)
        } else {
            format!("{:.1} GiB", n as f64 / GIB as f64)
        }
    }

    pub fn rx_str(&self) -> String {
        self.rx_bytes.map(Self::fmt_bytes).unwrap_or_default()
    }

    pub fn tx_str(&self) -> String {
        self.tx_bytes.map(Self::fmt_bytes).unwrap_or_default()
    }

    pub fn ips_str(&self) -> String {
        self.tailscale_ips
            .as_ref()
            .map(|ips| ips.join(", "))
            .unwrap_or_default()
    }

    pub fn is_online(&self) -> bool {
        self.online.unwrap_or(false)
    }

    pub fn flags(&self) -> Vec<&'static str> {
        let mut flags = Vec::new();
        if self.active.unwrap_or(false) {
            flags.push("active");
        }
        if self.exit_node.unwrap_or(false) {
            flags.push("exit node");
        }
        if self.exit_node_option.unwrap_or(false) {
            flags.push("exit node option");
        }
        if self.keep_alive.unwrap_or(false) {
            flags.push("keep alive");
        }
        flags
    }

    /// Returns `last_seen` unless it's Go's zero time.
    pub fn last_seen_str(&self) -> Option<&str> {
        self.last_seen
            .as_deref()
            .filter(|s| !s.starts_with("0001-01-01"))
    }

    /// Returns `last_handshake` unless it's Go's zero time.
    pub fn last_handshake_str(&self) -> Option<&str> {
        self.last_handshake
            .as_deref()
            .filter(|s| !s.starts_with("0001-01-01"))
    }

    pub fn relay_str(&self) -> Option<&str> {
        self.relay.as_deref().filter(|s| !s.is_empty())
    }
}

#[derive(Template)]
#[template(path = "tailscale.html")]
pub struct TailscalePage {
    pub version: &'static str,
    pub ts_version: String,
    pub backend_state: String,
    pub self_peer: TailscalePeer,
    pub peers: Vec<TailscalePeer>,
    pub auth_user: Option<AuthUserInfo>,
    pub nav_links: Arc<[NavLink]>,
}

async fn fetch_status(socket_path: &Path) -> Result<TailscaleStatus, Error> {
    let mut stream = tokio::net::UnixStream::connect(socket_path)
        .await
        .map_err(|source| Error::TailscaleConnect { source })?;

    stream
        .write_all(b"GET /localapi/v0/status HTTP/1.0\r\nHost: local-tailscaled.sock\r\n\r\n")
        .await
        .map_err(|source| Error::TailscaleConnect { source })?;

    let mut response = Vec::new();
    let _ = stream
        .read_to_end(&mut response)
        .await
        .map_err(|source| Error::TailscaleConnect { source })?;

    let body_start = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| Error::TailscaleParse("no HTTP body separator".into()))?;

    serde_json::from_slice(&response[body_start + 4..])
        .map_err(|source| Error::TailscaleDeserialize { source })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helpers ─────────────────────────────────────────────────────────────────

    fn peer_with_bytes(rx: u64, tx: u64) -> TailscalePeer {
        TailscalePeer {
            rx_bytes: Some(rx),
            tx_bytes: Some(tx),
            ..Default::default()
        }
    }

    // ── fmt_bytes ─────────────────────────────────────────────────────────────

    #[test]
    fn fmt_bytes_under_kib() {
        assert_eq!(TailscalePeer::fmt_bytes(0), "0 B");
        assert_eq!(TailscalePeer::fmt_bytes(1023), "1023 B");
    }

    #[test]
    fn fmt_bytes_kib_range() {
        assert_eq!(TailscalePeer::fmt_bytes(1024), "1.0 KiB");
        assert_eq!(TailscalePeer::fmt_bytes(1024 * 1024 - 1), "1024.0 KiB");
    }

    #[test]
    fn fmt_bytes_mib_range() {
        assert_eq!(TailscalePeer::fmt_bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(
            TailscalePeer::fmt_bytes(1024 * 1024 * 1024 - 1),
            "1024.0 MiB"
        );
    }

    #[test]
    fn fmt_bytes_gib_range() {
        assert_eq!(TailscalePeer::fmt_bytes(1024 * 1024 * 1024), "1.0 GiB");
        assert_eq!(
            TailscalePeer::fmt_bytes(10 * 1024 * 1024 * 1024),
            "10.0 GiB"
        );
    }

    // ── rx_str / tx_str ───────────────────────────────────────────────────────

    #[test]
    fn rx_str_with_value() {
        let peer = peer_with_bytes(2048, 0);
        assert_eq!(peer.rx_str(), "2.0 KiB");
    }

    #[test]
    fn tx_str_with_value() {
        let peer = peer_with_bytes(0, 3 * 1024 * 1024);
        assert_eq!(peer.tx_str(), "3.0 MiB");
    }

    #[test]
    fn rx_tx_str_none_returns_empty() {
        let peer = TailscalePeer::default();
        assert_eq!(peer.rx_str(), "");
        assert_eq!(peer.tx_str(), "");
    }

    // ── ips_str ───────────────────────────────────────────────────────────────

    #[test]
    fn ips_str_with_multiple_ips() {
        let peer = TailscalePeer {
            tailscale_ips: Some(vec!["100.64.0.1".into(), "fd7a::1".into()]),
            ..Default::default()
        };
        assert_eq!(peer.ips_str(), "100.64.0.1, fd7a::1");
    }

    #[test]
    fn ips_str_none_returns_empty() {
        assert_eq!(TailscalePeer::default().ips_str(), "");
    }

    // ── is_online ─────────────────────────────────────────────────────────────

    #[test]
    fn is_online_true() {
        let peer = TailscalePeer {
            online: Some(true),
            ..Default::default()
        };
        assert!(peer.is_online());
    }

    #[test]
    fn is_online_false() {
        let peer = TailscalePeer {
            online: Some(false),
            ..Default::default()
        };
        assert!(!peer.is_online());
    }

    #[test]
    fn is_online_none_defaults_false() {
        assert!(!TailscalePeer::default().is_online());
    }

    // ── flags ─────────────────────────────────────────────────────────────────

    #[test]
    fn flags_empty_by_default() {
        assert!(TailscalePeer::default().flags().is_empty());
    }

    #[test]
    fn flags_active() {
        let peer = TailscalePeer {
            active: Some(true),
            ..Default::default()
        };
        assert!(peer.flags().contains(&"active"));
    }

    #[test]
    fn flags_exit_node() {
        let peer = TailscalePeer {
            exit_node: Some(true),
            ..Default::default()
        };
        assert!(peer.flags().contains(&"exit node"));
    }

    #[test]
    fn flags_exit_node_option() {
        let peer = TailscalePeer {
            exit_node_option: Some(true),
            ..Default::default()
        };
        assert!(peer.flags().contains(&"exit node option"));
    }

    #[test]
    fn flags_keep_alive() {
        let peer = TailscalePeer {
            keep_alive: Some(true),
            ..Default::default()
        };
        assert!(peer.flags().contains(&"keep alive"));
    }

    #[test]
    fn flags_multiple() {
        let peer = TailscalePeer {
            active: Some(true),
            keep_alive: Some(true),
            ..Default::default()
        };
        let flags = peer.flags();
        assert_eq!(flags.len(), 2);
        assert!(flags.contains(&"active"));
        assert!(flags.contains(&"keep alive"));
    }

    // ── last_seen_str / last_handshake_str ────────────────────────────────────

    #[test]
    fn last_seen_str_normal_timestamp() {
        let peer = TailscalePeer {
            last_seen: Some("2026-03-15T12:00:00Z".into()),
            ..Default::default()
        };
        assert_eq!(peer.last_seen_str(), Some("2026-03-15T12:00:00Z"));
    }

    #[test]
    fn last_seen_str_go_zero_time_is_none() {
        let peer = TailscalePeer {
            last_seen: Some("0001-01-01T00:00:00Z".into()),
            ..Default::default()
        };
        assert_eq!(peer.last_seen_str(), None);
    }

    #[test]
    fn last_seen_str_none_field_is_none() {
        assert_eq!(TailscalePeer::default().last_seen_str(), None);
    }

    #[test]
    fn last_handshake_str_normal() {
        let peer = TailscalePeer {
            last_handshake: Some("2026-03-15T11:00:00Z".into()),
            ..Default::default()
        };
        assert_eq!(peer.last_handshake_str(), Some("2026-03-15T11:00:00Z"));
    }

    #[test]
    fn last_handshake_str_go_zero_time_is_none() {
        let peer = TailscalePeer {
            last_handshake: Some("0001-01-01T00:00:00Z".into()),
            ..Default::default()
        };
        assert_eq!(peer.last_handshake_str(), None);
    }

    // ── relay_str ─────────────────────────────────────────────────────────────

    #[test]
    fn relay_str_with_relay() {
        let peer = TailscalePeer {
            relay: Some("nyc".into()),
            ..Default::default()
        };
        assert_eq!(peer.relay_str(), Some("nyc"));
    }

    #[test]
    fn relay_str_empty_string_is_none() {
        let peer = TailscalePeer {
            relay: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(peer.relay_str(), None);
    }

    #[test]
    fn relay_str_none_field_is_none() {
        assert_eq!(TailscalePeer::default().relay_str(), None);
    }

    // ── TailscaleStatus deserialization ───────────────────────────────────────

    #[test]
    fn tailscale_status_deserializes_from_json() {
        let json = r#"{
            "Version": "1.60.0",
            "BackendState": "Running",
            "TailscaleIPs": ["100.64.0.1"],
            "Self": {
                "HostName": "green",
                "DNSName": "green.tail.ts.net.",
                "OS": "linux",
                "TailscaleIPs": ["100.64.0.1"],
                "Online": true
            },
            "Peer": {}
        }"#;
        let status: TailscaleStatus = serde_json::from_str(json).unwrap();
        assert_eq!(status.version, "1.60.0");
        assert_eq!(status.backend_state, "Running");
        assert_eq!(status.self_peer.host_name, "green");
        assert!(status.self_peer.is_online());
        assert!(status.peer.is_empty());
    }

    #[test]
    fn tailscale_status_deserializes_peers() {
        let json = r#"{
            "Version": "1.60.0",
            "BackendState": "Running",
            "Self": {
                "HostName": "green",
                "DNSName": "green.tail.ts.net.",
                "OS": "linux"
            },
            "Peer": {
                "abc123": {
                    "HostName": "laptop",
                    "DNSName": "laptop.tail.ts.net.",
                    "OS": "macos",
                    "Online": false,
                    "Active": true
                }
            }
        }"#;
        let status: TailscaleStatus = serde_json::from_str(json).unwrap();
        assert_eq!(status.peer.len(), 1);
        let peer = status.peer.values().next().unwrap();
        assert_eq!(peer.host_name, "laptop");
        assert!(peer.flags().contains(&"active"));
    }
}

pub async fn tailscale_route(
    user: GmUser,
    State(state): State<ServerState>,
) -> Result<Html<String>, Error> {
    let mut status = fetch_status(&state.tailscale_socket).await?;

    let mut peers: Vec<TailscalePeer> = status.peer.drain().map(|(_, v)| v).collect();
    peers.sort_by(|a, b| a.host_name.cmp(&b.host_name));

    let auth_user = Some(AuthUserInfo {
        username: user.0.username.clone(),
        role: user.0.role.clone(),
    });

    let page = TailscalePage {
        version: crate::VERSION,
        ts_version: status.version,
        backend_state: status.backend_state,
        self_peer: status.self_peer,
        peers,
        auth_user,
        nav_links: state.nav_links.clone(),
    };

    Ok(Html(page.render()?))
}
