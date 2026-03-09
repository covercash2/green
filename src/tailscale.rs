use std::{collections::HashMap, path::Path};

use askama::Template;
use axum::{extract::State, response::Html};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{error::Error, ServerState};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct TailscaleStatus {
    pub version: String,
    pub backend_state: String,
    #[serde(rename = "TailscaleIPs")]
    pub _tailscale_ips: Option<Vec<String>>,
    #[serde(rename = "Self")]
    pub self_peer: TailscalePeer,
    #[serde(default)]
    pub peer: HashMap<String, TailscalePeer>,
}

#[derive(Debug, Deserialize)]
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
    stream
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

pub async fn tailscale_route(State(state): State<ServerState>) -> Result<Html<String>, Error> {
    let mut status = fetch_status(&state.tailscale_socket).await?;

    let mut peers: Vec<TailscalePeer> = status.peer.drain().map(|(_, v)| v).collect();
    peers.sort_by(|a, b| a.host_name.cmp(&b.host_name));

    let page = TailscalePage {
        version: crate::VERSION,
        ts_version: status.version,
        backend_state: status.backend_state,
        self_peer: status.self_peer,
        peers,
    };

    Ok(Html(page.render()?))
}
