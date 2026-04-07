//! Systemd service monitoring dashboard — local and remote.
//!
//! # Local monitoring
//!
//! Service status is queried by calling `systemctl show --property=… <unit>` as
//! a subprocess.  Results are parsed from the newline-delimited `key=value`
//! output format.  See [`query_all`] and [`parse_systemctl_output`].
//!
//! # Remote monitoring (server-side proxy)
//!
//! Each configured peer (see [`crate::PeerInfo`]) may include an `api_key`.
//! When present, this instance calls the peer's `/api/services` endpoint and
//! merges the results into the home page under a labelled group.
//!
//! ```text
//! Browser (GM user on A)
//!     │
//!     │  GET /  (session cookie for A)
//!     ▼
//! Green A  ──── local systemctl ───────────────► Vec<ServiceStatus>
//!     │
//!     │  GET https://B/api/services
//!     │  Header: X-Green-Api-Key: <peer.api_key from A's config>
//!     │  Timeout: 5 s
//!     ▼
//! Green B  ──── validates header ──────────────► Vec<ServiceStatus> (JSON)
//!     │
//!     │  (if unreachable / timeout / error)
//!     └──────────────────────────────────────── PeerServiceGroup { online: false }
//! ```
//!
//! ## Authentication model
//!
//! Two authentication paths reach `/api/services`:
//!
//! 1. **Browser (GM session cookie)** — the existing `GmUser` extractor; only
//!    for direct browser access.
//!
//! 2. **Peer server (API key)** — machine A sends `X-Green-Api-Key: <token>` in
//!    an HTTPS request.  Machine B validates the token against its own
//!    `peer_api_key` config value.
//!
//! The two-step check is implemented in the [`GmOrPeer`] extractor and applied
//! only to `services_api_route`.  All other routes continue to use `GmUser`.
//!
//! ### Why a custom header rather than `Authorization`?
//!
//! The standard `Authorization` header (RFC 7235) would work, but `GmUser`
//! reads cookies rather than `Authorization`, so using a distinct header makes
//! the two auth paths unambiguous and avoids coupling the cookie and API-key
//! paths together.  See:
//! - <https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Authorization>
//! - <https://www.rfc-editor.org/rfc/rfc7235>
//!
//! ### Why not share the session cookie across machines?
//!
//! Cookies are scoped to the origin (`Set-Cookie: Domain=.chrash.net` would
//! share them across all subdomains, but that exposes every machine's session
//! store to every other machine).  The server-side proxy keeps credentials
//! server-to-server, never involving the browser in cross-origin requests.
//!
//! ### Security considerations
//!
//! - The `api_key` / `peer_api_key` values are pre-shared secrets.  They must
//!   be stored encrypted (e.g. via sops-nix) and injected via the
//!   `GREEN_PEER_API_KEY` environment variable.  They must **not** appear in
//!   config.toml in the Nix store.
//! - All communication is over HTTPS (enforced by Caddy + mkcert/Tailscale TLS).
//! - The comparison is a plain `==` on `&str`.  For this internal threat model
//!   (LAN-only, already behind TLS) timing-safe comparison is not required, but
//!   if you expose green to the internet you should switch to a constant-time
//!   compare such as [`subtle::ConstantTimeEq`](https://docs.rs/subtle).
//! - Rotate keys by updating both sides' configs and redeploying.

use askama::Template;
use axum::{
    Json,
    extract::{FromRequestParts, State},
    http::{StatusCode, request::Parts},
    response::{Html, IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use std::sync::Arc;

use crate::{
    PeerInfo, ServerState, VERSION,
    auth::{AuthUser, AuthUserInfo, GmUser},
    error::Error,
    index::NavLink,
};

// ─── Peer auth header ─────────────────────────────────────────────────────────

/// HTTP request header used for server-to-server authentication.
///
/// A peer instance (machine A) sets this header to its configured `api_key`
/// when calling another instance's (machine B's) `/api/services` endpoint.
/// Machine B validates it against its own `peer_api_key` config value.
///
/// Example (machine A calling machine B):
/// ```http
/// GET /api/services HTTP/1.1
/// Host: green.b.chrash.net
/// X-Green-Api-Key: <token from [[peers]] api_key on A>
/// ```
pub const PEER_AUTH_HEADER: &str = "X-Green-Api-Key";

// ─── Timeout ──────────────────────────────────────────────────────────────────

/// Maximum time to wait for a peer's `/api/services` response.
///
/// Chosen to be long enough for a slow host on the LAN but short enough to keep
/// the home page load fast even when a peer is down.  If a peer exceeds this
/// deadline the home page renders it as "offline" rather than blocking
/// indefinitely.
///
/// See: <https://docs.rs/tokio/latest/tokio/time/fn.timeout.html>
const PEER_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

// ─── Config ───────────────────────────────────────────────────────────────────

/// Per-unit configuration within [`SystemdConfig`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UnitConfig {
    /// Systemd unit name (e.g. `"postgresql"`, `"mosquitto.service"`).
    /// Names without a suffix are treated as `.service` units by systemd.
    pub name: String,
    /// Optional URL for an icon image shown on the services dashboard.
    #[serde(default)]
    pub icon_url: Option<String>,
    /// Optional URL linking the service card to its web UI.
    #[serde(default)]
    pub url: Option<String>,
}

/// Systemd unit monitoring configuration.
///
/// Configured under `[systemd]` in the TOML config. If absent, the services
/// route returns 404.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SystemdConfig {
    pub units: Vec<UnitConfig>,
}

// ─── Status types ────────────────────────────────────────────────────────────

/// Derived health bucket — coarser than raw systemd states, used for CSS and
/// the JSON API.
///
/// `Deserialize` is derived so that peer responses (which arrive as JSON) can
/// be round-tripped back into this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Health {
    /// `active/running` — the unit is up and running normally.
    Healthy,
    /// `active/exited` — a oneshot unit completed successfully.
    Degraded,
    /// `inactive/dead` — not running, not failed.
    Inactive,
    /// `failed` state, `not-found`, `masked`, or query error.
    Failed,
}

impl Health {
    /// CSS class applied to the service card.
    pub fn css_class(self) -> &'static str {
        match self {
            Health::Healthy => "svc-healthy",
            Health::Degraded => "svc-degraded",
            Health::Inactive => "svc-inactive",
            Health::Failed => "svc-failed",
        }
    }

    /// Short human-readable label shown in the status badge.
    pub fn label(self) -> &'static str {
        match self {
            Health::Healthy => "● running",
            Health::Degraded => "● exited",
            Health::Inactive => "○ inactive",
            Health::Failed => "✕ failed",
        }
    }
}

/// Runtime status of a single systemd unit, as returned by the JSON API and
/// used by the Askama template.
///
/// `Deserialize` is derived so that the type can be parsed from a peer's JSON
/// response when aggregating remote services on the home page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    /// Unit name as given in config (e.g. `"postgresql"`).
    pub name: String,
    /// `Description=` from unit file.
    pub description: String,
    /// `LoadState=` — `loaded`, `not-found`, `masked`, etc.
    pub load_state: String,
    /// `ActiveState=` — `active`, `inactive`, `failed`, etc.
    pub active_state: String,
    /// `SubState=` — `running`, `dead`, `exited`, `failed`, etc.
    pub sub_state: String,
    /// `MainPID=`, omitted when zero (not running).
    pub pid: Option<u32>,
    /// `ExecMainStartTimestamp=`, omitted when empty.
    pub since: Option<String>,
    /// Derived health bucket.
    pub health: Health,
    /// Optional icon URL from config.
    pub icon_url: Option<String>,
    /// Optional URL linking to the service's web UI.
    pub url: Option<String>,
}

// ─── Peer service group ───────────────────────────────────────────────────────

/// Service statuses aggregated from one peer instance, shown as a labelled
/// group below the local services on the home page (GM view only).
///
/// `online: false` means the peer was unreachable, timed out, or returned an
/// error; in that case `services` is empty and the template shows an "offline"
/// banner instead.
#[derive(Debug, Clone)]
pub struct PeerServiceGroup {
    /// Human-readable name of the peer (from `[[peers]] name`).
    pub name: String,
    /// Base URL of the peer's Green instance (from `[[peers]] url`).
    pub url: String,
    /// Whether the peer responded successfully within [`PEER_FETCH_TIMEOUT`].
    pub online: bool,
    /// Services returned by the peer, or empty when `online` is false.
    pub services: Vec<ServiceStatus>,
}

// ─── Extractor: GmOrPeer ─────────────────────────────────────────────────────

/// Axum extractor that permits access to `/api/services` via **either** of two
/// authentication paths:
///
/// 1. **GM session cookie** — the standard `GmUser` path; used by the browser.
/// 2. **Peer API key** — the `X-Green-Api-Key` header sent by another Green
///    instance when it proxies this machine's service list.
///
/// The header is checked first.  If it is present but invalid the request is
/// rejected with 403 immediately (no cookie fallback).  If the header is absent
/// the extractor falls through to the normal `GmUser` cookie check.
///
/// For background on axum extractors, see:
/// <https://docs.rs/axum/latest/axum/extract/trait.FromRequestParts.html>
pub struct GmOrPeer;

impl FromRequestParts<ServerState> for GmOrPeer {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ServerState,
    ) -> Result<Self, Self::Rejection> {
        // ── Path 1: peer server-to-server (X-Green-Api-Key) ──────────────────
        if let Some(key_hdr) = parts.headers.get(PEER_AUTH_HEADER) {
            return match (state.peer_api_key.as_deref(), key_hdr.to_str().ok()) {
                (Some(expected), Some(provided)) if expected == provided => Ok(GmOrPeer),
                _ => Err(StatusCode::FORBIDDEN.into_response()),
            };
        }

        // ── Path 2: browser GM session cookie ────────────────────────────────
        GmUser::from_request_parts(parts, state)
            .await
            .map(|_| GmOrPeer)
            .map_err(IntoResponse::into_response)
    }
}

// ─── Peer fetch ───────────────────────────────────────────────────────────────

/// Call a peer's `/api/services` endpoint and return a [`PeerServiceGroup`].
///
/// The request is authenticated with the peer's `api_key` (sent as
/// [`PEER_AUTH_HEADER`]).  If the peer has no `api_key` configured, the
/// function returns immediately with `online: false` — callers should skip
/// peers without keys rather than calling this function.
///
/// Any network error, non-2xx status, JSON parse failure, or timeout longer
/// than [`PEER_FETCH_TIMEOUT`] results in `online: false` and an empty
/// `services` list.  Errors are logged at WARN level with the peer name so they
/// are visible in the structured log without crashing the request.
///
/// The `client` parameter is a shared [`reqwest::Client`] stored in
/// [`ServerState`].  Reusing a single client is important for connection
/// pooling and keep-alive; never create a new client per request.
///
/// See: <https://docs.rs/reqwest/latest/reqwest/struct.Client.html>
pub async fn fetch_peer_services(peer: &PeerInfo, client: &reqwest::Client) -> PeerServiceGroup {
    let Some(ref api_key) = peer.api_key else {
        // No key configured — caller should not have called us, but be safe.
        return PeerServiceGroup {
            name: peer.name.clone(),
            url: peer.url.clone(),
            online: false,
            services: Vec::new(),
        };
    };

    let endpoint = format!("{}/api/services", peer.url.trim_end_matches('/'));

    // Wrap the entire request+parse chain in a timeout so a slow or hung peer
    // does not block the home page indefinitely.
    // See: https://docs.rs/tokio/latest/tokio/time/fn.timeout.html
    let result = tokio::time::timeout(
        PEER_FETCH_TIMEOUT,
        client
            .get(&endpoint)
            .header(PEER_AUTH_HEADER, api_key.as_str())
            .send(),
    )
    .await;

    let offline = |reason: &str| {
        tracing::warn!(peer = %peer.name, reason, "peer service fetch failed");
        PeerServiceGroup {
            name: peer.name.clone(),
            url: peer.url.clone(),
            online: false,
            services: Vec::new(),
        }
    };

    match result {
        Err(_timeout) => offline("timeout"),
        Ok(Err(e)) => {
            tracing::warn!(peer = %peer.name, error = %e, "peer service request error");
            PeerServiceGroup {
                name: peer.name.clone(),
                url: peer.url.clone(),
                online: false,
                services: Vec::new(),
            }
        }
        Ok(Ok(response)) if !response.status().is_success() => {
            tracing::warn!(peer = %peer.name, status = %response.status(), "peer service non-2xx response");
            PeerServiceGroup {
                name: peer.name.clone(),
                url: peer.url.clone(),
                online: false,
                services: Vec::new(),
            }
        }
        Ok(Ok(response)) => match response.json::<Vec<ServiceStatus>>().await {
            Ok(services) => PeerServiceGroup {
                name: peer.name.clone(),
                url: peer.url.clone(),
                online: true,
                services,
            },
            Err(e) => {
                tracing::warn!(peer = %peer.name, error = %e, "peer service JSON parse error");
                PeerServiceGroup {
                    name: peer.name.clone(),
                    url: peer.url.clone(),
                    online: false,
                    services: Vec::new(),
                }
            }
        },
    }
}

// ─── Parsing ─────────────────────────────────────────────────────────────────

/// Properties requested from `systemctl show`.
const PROPERTIES: &str =
    "Description,LoadState,ActiveState,SubState,MainPID,ExecMainStartTimestamp";

/// Parse the newline-delimited `key=value` output of `systemctl show --property=...`
/// into a [`ServiceStatus`].
///
/// Unknown keys are silently ignored so this stays forward-compatible with
/// additional properties being added to the query.
pub fn parse_systemctl_output(
    name: &str,
    output: &str,
    icon_url: Option<String>,
    url: Option<String>,
) -> ServiceStatus {
    let mut description = String::new();
    let mut load_state = String::new();
    let mut active_state = String::new();
    let mut sub_state = String::new();
    let mut pid: Option<u32> = None;
    let mut since: Option<String> = None;

    for line in output.lines() {
        if let Some((key, value)) = line.split_once('=') {
            match key {
                "Description" => description = value.to_owned(),
                "LoadState" => load_state = value.to_owned(),
                "ActiveState" => active_state = value.to_owned(),
                "SubState" => sub_state = value.to_owned(),
                "MainPID" => {
                    if let Ok(n) = value.parse::<u32>()
                        && n > 0
                    {
                        pid = Some(n);
                    }
                }
                "ExecMainStartTimestamp" if !value.is_empty() => {
                    since = Some(value.to_owned());
                }
                _ => {}
            }
        }
    }

    let health = derive_health(&load_state, &active_state, &sub_state);
    ServiceStatus {
        name: name.to_owned(),
        description,
        load_state,
        active_state,
        sub_state,
        pid,
        since,
        health,
        icon_url,
        url,
    }
}

fn derive_health(load_state: &str, active_state: &str, sub_state: &str) -> Health {
    if matches!(load_state, "not-found" | "masked" | "error") {
        return Health::Failed;
    }
    match (active_state, sub_state) {
        ("active", "running") => Health::Healthy,
        ("active", "exited") => Health::Degraded,
        ("failed", _) => Health::Failed,
        _ => Health::Inactive,
    }
}

// ─── systemctl query ─────────────────────────────────────────────────────────

async fn query_unit(unit: &UnitConfig) -> ServiceStatus {
    let result = Command::new("systemctl")
        .args(["show", &unit.name, "--property", PROPERTIES, "--no-pager"])
        .output()
        .await;

    match result {
        Ok(out) => parse_systemctl_output(
            &unit.name,
            &String::from_utf8_lossy(&out.stdout),
            unit.icon_url.clone(),
            unit.url.clone(),
        ),
        Err(e) => {
            tracing::warn!(unit = %unit.name, error = %e, "systemctl query failed");
            ServiceStatus {
                name: unit.name.clone(),
                description: String::new(),
                load_state: "error".into(),
                active_state: "unknown".into(),
                sub_state: "unknown".into(),
                pid: None,
                since: None,
                health: Health::Failed,
                icon_url: unit.icon_url.clone(),
                url: unit.url.clone(),
            }
        }
    }
}

/// Query all configured units concurrently.
pub async fn query_all(config: &SystemdConfig) -> Vec<ServiceStatus> {
    futures::future::join_all(config.units.iter().map(query_unit)).await
}

// ─── Templates ───────────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "services.html")]
struct ServicesPage {
    version: &'static str,
    auth_user: Option<AuthUserInfo>,
    services: Vec<ServiceStatus>,
    nav_links: Arc<[NavLink]>,
}

// ─── Handlers ────────────────────────────────────────────────────────────────

fn auth_user_info(user: &AuthUser) -> AuthUserInfo {
    AuthUserInfo {
        username: user.username.clone(),
        role: user.role.clone(),
    }
}

/// `GET /services` — service status dashboard (GM only).
pub async fn services_route(
    GmUser(user): GmUser,
    State(state): State<ServerState>,
) -> Result<Html<String>, Error> {
    let config = state.systemd_config.as_ref().ok_or(Error::NotFound)?;
    let services = query_all(config).await;
    let page = ServicesPage {
        version: VERSION,
        auth_user: Some(auth_user_info(&user)),
        services,
        nav_links: state.nav_links.clone(),
    };
    Ok(Html(page.render()?))
}

/// `GET /api/services` — JSON list of current service statuses.
///
/// Accessible by either a GM browser session (cookie) or a peer Green instance
/// (via the [`PEER_AUTH_HEADER`] header + a matching [`ServerState::peer_api_key`]).
///
/// This is the endpoint that other Green instances call when aggregating remote
/// service status on their own home page.  The [`GmOrPeer`] extractor handles
/// both authentication paths.
pub async fn services_api_route(
    _caller: GmOrPeer,
    State(state): State<ServerState>,
) -> Result<Json<Vec<ServiceStatus>>, Error> {
    let config = state.systemd_config.as_ref().ok_or(Error::NotFound)?;
    Ok(Json(query_all(config).await))
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(name: &str, output: &str) -> ServiceStatus {
        parse_systemctl_output(name, output, None, None)
    }

    #[test]
    fn parse_running_service() {
        let out = "\
Description=PostgreSQL Server\n\
LoadState=loaded\n\
ActiveState=active\n\
SubState=running\n\
MainPID=1261\n\
ExecMainStartTimestamp=Sun 2026-03-29 10:48:26 CDT\n";
        let s = parse("postgresql", out);
        assert_eq!(s.name, "postgresql");
        assert_eq!(s.description, "PostgreSQL Server");
        assert_eq!(s.active_state, "active");
        assert_eq!(s.sub_state, "running");
        assert_eq!(s.pid, Some(1261));
        assert_eq!(s.since.as_deref(), Some("Sun 2026-03-29 10:48:26 CDT"));
        assert_eq!(s.health, Health::Healthy);
    }

    #[test]
    fn parse_inactive_service() {
        let out = "\
Description=Some Service\n\
LoadState=loaded\n\
ActiveState=inactive\n\
SubState=dead\n\
MainPID=0\n\
ExecMainStartTimestamp=\n";
        let s = parse("some-service", out);
        assert_eq!(s.health, Health::Inactive);
        assert_eq!(s.pid, None);
        assert_eq!(s.since, None);
    }

    #[test]
    fn parse_failed_service() {
        let out = "\
Description=Broken Service\n\
LoadState=loaded\n\
ActiveState=failed\n\
SubState=failed\n\
MainPID=0\n\
ExecMainStartTimestamp=\n";
        let s = parse("broken", out);
        assert_eq!(s.health, Health::Failed);
    }

    #[test]
    fn parse_not_found_unit() {
        let out = "\
Description=nonexistent.service\n\
LoadState=not-found\n\
ActiveState=inactive\n\
SubState=dead\n\
MainPID=0\n\
ExecMainStartTimestamp=\n";
        let s = parse("nonexistent", out);
        assert_eq!(s.health, Health::Failed);
    }

    #[test]
    fn parse_oneshot_exited() {
        let out = "\
Description=One Shot Task\n\
LoadState=loaded\n\
ActiveState=active\n\
SubState=exited\n\
MainPID=0\n\
ExecMainStartTimestamp=Mon 2026-03-30 08:00:00 CDT\n";
        let s = parse("oneshot", out);
        assert_eq!(s.health, Health::Degraded);
    }

    #[test]
    fn parse_masked_unit() {
        let out = "\
Description=Masked Unit\n\
LoadState=masked\n\
ActiveState=inactive\n\
SubState=dead\n\
MainPID=0\n\
ExecMainStartTimestamp=\n";
        let s = parse("masked-unit", out);
        assert_eq!(s.health, Health::Failed);
    }

    #[test]
    fn parse_ignores_unknown_keys() {
        let out = "\
Description=Test\n\
LoadState=loaded\n\
ActiveState=active\n\
SubState=running\n\
SomeOtherProperty=whatever\n\
MainPID=42\n\
ExecMainStartTimestamp=\n";
        let s = parse("test", out);
        assert_eq!(s.health, Health::Healthy);
        assert_eq!(s.pid, Some(42));
    }
}
