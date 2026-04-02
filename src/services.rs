//! Systemd service monitoring dashboard.

use askama::Template;
use axum::{Json, extract::State, response::Html};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::{
    ServerState, VERSION,
    auth::{AuthUser, AuthUserInfo, GmUser},
    error::Error,
};

// ─── Config ───────────────────────────────────────────────────────────────────

/// Systemd unit monitoring configuration.
///
/// Configured under `[systemd]` in the TOML config. If absent, the services
/// route returns 404.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SystemdConfig {
    /// Systemd unit names to monitor (e.g. `"postgresql"`, `"mosquitto.service"`).
    /// Names without a suffix are treated as `.service` units by systemd.
    pub units: Vec<String>,
}

// ─── Status types ────────────────────────────────────────────────────────────

/// Derived health bucket — coarser than raw systemd states, used for CSS and
/// the JSON API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
pub fn parse_systemctl_output(name: &str, output: &str) -> ServiceStatus {
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
                    if let Ok(n) = value.parse::<u32>() {
                        if n > 0 {
                            pid = Some(n);
                        }
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
    ServiceStatus { name: name.to_owned(), description, load_state, active_state, sub_state, pid, since, health }
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

async fn query_unit(name: &str) -> ServiceStatus {
    let result = Command::new("systemctl")
        .args(["show", name, "--property", PROPERTIES, "--no-pager"])
        .output()
        .await;

    match result {
        Ok(out) => parse_systemctl_output(name, &String::from_utf8_lossy(&out.stdout)),
        Err(e) => {
            tracing::warn!(unit = name, error = %e, "systemctl query failed");
            ServiceStatus {
                name: name.to_owned(),
                description: String::new(),
                load_state: "error".into(),
                active_state: "unknown".into(),
                sub_state: "unknown".into(),
                pid: None,
                since: None,
                health: Health::Failed,
            }
        }
    }
}

/// Query all configured units concurrently.
pub async fn query_all(config: &SystemdConfig) -> Vec<ServiceStatus> {
    futures::future::join_all(config.units.iter().map(|n| query_unit(n))).await
}

// ─── Templates ───────────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "services.html")]
struct ServicesPage {
    version: &'static str,
    auth_user: Option<AuthUserInfo>,
    services: Vec<ServiceStatus>,
}

// ─── Handlers ────────────────────────────────────────────────────────────────

fn auth_user_info(user: &AuthUser) -> AuthUserInfo {
    AuthUserInfo { username: user.username.clone(), role: user.role.clone() }
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
    };
    Ok(Html(page.render()?))
}

/// `GET /api/services` — JSON list of current service statuses (GM only).
pub async fn services_api_route(
    _user: GmUser,
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
        parse_systemctl_output(name, output)
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
