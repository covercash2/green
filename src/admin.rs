use std::{collections::HashSet, sync::Arc};

use askama::Template;
use axum::{
    extract::{FromRef, State},
    response::Html,
};

use crate::{
    PeerInfo, Routes, ServerState,
    auth::{AdminUser, AuthUserInfo},
    error::Error,
    index::NavLink,
    services::{PeerServiceGroup, ServiceStatus, SystemdConfig},
};

/// A link shown in the admin dashboard's routes list.
#[derive(Debug, Clone, bon::Builder)]
#[builder(on(String, into), start_fn = href)]
pub struct AdminEntry {
    #[builder(start_fn)]
    pub href: String,
    pub name: Option<String>,
    pub description: String,
    pub icon_url: Option<String>,
}

impl AdminEntry {
    /// Display name: explicit `name` if set, otherwise the `href` as-is.
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.href)
    }
}

/// Optional dashboard entries that are only shown when the corresponding
/// feature is configured. Pass any subset to [`AdminDashboard::new`].
#[derive(Debug)]
pub enum OptionalEntry {
    Mqtt,
    MqttDevices,
    Logs,
}

impl From<OptionalEntry> for AdminEntry {
    fn from(e: OptionalEntry) -> Self {
        match e {
            OptionalEntry::Mqtt => AdminEntry::href("/admin/mqtt")
                .description("Live MQTT message feed")
                .build(),
            OptionalEntry::MqttDevices => AdminEntry::href("/admin/mqtt/devices")
                .description("MQTT device inventory")
                .build(),
            OptionalEntry::Logs => AdminEntry::href("/admin/logs/app")
                .description("Dev server log viewer")
                .build(),
        }
    }
}

#[derive(Debug, Clone, Template)]
#[template(path = "admin.html")]
pub struct AdminDashboard {
    pub routes: Vec<AdminEntry>,
    /// Local systemd service statuses, queried per-request.
    pub services: Vec<ServiceStatus>,
    /// Remote peer service groups, fetched in parallel per-request.
    pub peer_groups: Vec<PeerServiceGroup>,
    pub version: &'static str,
    pub auth_user: Option<AuthUserInfo>,
    pub logo_url: Option<String>,
    pub nav_links: Arc<[NavLink]>,
}

impl AdminDashboard {
    pub async fn new(
        routes: Routes,
        optional_entries: impl IntoIterator<Item = impl Into<AdminEntry>>,
        service_urls: &HashSet<String>,
        logo_url: Option<String>,
        nav_links: Arc<[NavLink]>,
    ) -> Result<Self, Error> {
        let static_entries = [
            AdminEntry::href("/admin/breaker")
                .description("Electrical circuit layout")
                .build(),
            AdminEntry::href("/admin/tailscale")
                .description("Tailscale peer list")
                .build(),
        ];

        let mut routes: Vec<AdminEntry> = routes
            .into_iter()
            .filter(|(_, info)| !service_urls.contains(&format!("https://{}", info.url)))
            .map(|(name, info)| {
                AdminEntry::href(format!("https://{}", info.url))
                    .name(name)
                    .description(info.description)
                    .maybe_icon_url(info.icon_url)
                    .build()
            })
            .chain(static_entries)
            .chain(optional_entries.into_iter().map(Into::into))
            .collect();

        routes.sort_by(|a, b| a.display_name().cmp(b.display_name()));

        Ok(AdminDashboard {
            routes,
            services: Vec::new(),
            peer_groups: Vec::new(),
            version: crate::VERSION,
            auth_user: None,
            logo_url,
            nav_links,
        })
    }
}

/// Resolve just the pre-rendered admin dashboard out of `ServerState`, so
/// `admin_route` doesn't need to depend on the rest of the app's state.
impl FromRef<ServerState> for Arc<AdminDashboard> {
    fn from_ref(state: &ServerState) -> Self {
        state.admin_dashboard.clone()
    }
}

/// Resolve just the configured peers out of `ServerState`, so `admin_route`
/// doesn't need to depend on the rest of the app's state.
impl FromRef<ServerState> for Arc<[PeerInfo]> {
    fn from_ref(state: &ServerState) -> Self {
        state.peers.clone()
    }
}

/// Resolve just the shared HTTP client out of `ServerState`, so
/// `admin_route` doesn't need to depend on the rest of the app's state.
impl FromRef<ServerState> for reqwest::Client {
    fn from_ref(state: &ServerState) -> Self {
        state.http_client.clone()
    }
}

/// `GET /admin` — consolidated admin dashboard (admin only): local systemd
/// services, peer service groups, and every homelab-infra link (config-driven
/// external services plus breaker/tailscale/mqtt/logs).
pub async fn admin_route(
    AdminUser(user): AdminUser,
    State(dashboard): State<Arc<AdminDashboard>>,
    State(systemd_config): State<Option<SystemdConfig>>,
    State(peers): State<Arc<[PeerInfo]>>,
    State(http_client): State<reqwest::Client>,
) -> Result<Html<String>, Error> {
    // Local services — always fetched when systemd is configured.
    let local_services = if let Some(ref config) = systemd_config {
        crate::services::query_all(config).await
    } else {
        Vec::new()
    };

    // Peer services — fetched in parallel. `AdminUser` already gates this
    // whole route, so unlike the old public index page there's no separate
    // per-request admin check needed here.
    //
    // Only peers with `api_key` configured are contacted. Peers without a
    // key appear in the nav drawer but not in the services section.
    //
    // All fetches run concurrently via `futures::future::join_all`. Each
    // individual fetch has its own timeout (see `PEER_FETCH_TIMEOUT` in
    // services.rs), so the overall latency is bounded by the slowest peer,
    // not the sum of all peers.
    // See: https://docs.rs/futures/latest/futures/future/fn.join_all.html
    let peers_with_keys: Vec<_> = peers.iter().filter(|p| p.api_key.is_some()).collect();
    let peer_groups = futures::future::join_all(
        peers_with_keys
            .iter()
            .map(|p| crate::services::fetch_peer_services(p, &http_client)),
    )
    .await;

    let auth_user = Some(AuthUserInfo {
        username: user.username.clone(),
        role: user.role.clone(),
    });

    let page = AdminDashboard {
        auth_user,
        services: local_services,
        peer_groups,
        ..(*dashboard).clone()
    };
    Ok(Html(page.render()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route::Routes;

    async fn make_dashboard(entries: impl IntoIterator<Item = OptionalEntry>) -> AdminDashboard {
        AdminDashboard::new(
            Routes::default(),
            entries,
            &HashSet::new(),
            None,
            Arc::new([]),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn dashboard_always_has_static_entries() {
        let dashboard = make_dashboard([]).await;
        assert!(dashboard.routes.iter().any(|r| r.href == "/admin/breaker"));
        assert!(
            dashboard
                .routes
                .iter()
                .any(|r| r.href == "/admin/tailscale")
        );
    }

    #[tokio::test]
    async fn dashboard_entries_sorted_alphabetically() {
        let dashboard = make_dashboard([OptionalEntry::Mqtt]).await;
        let names: Vec<&str> = dashboard.routes.iter().map(|r| r.display_name()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(
            names, sorted,
            "dashboard entries should be in alphabetical order"
        );
    }

    #[tokio::test]
    async fn dashboard_without_mqtt_devices_has_no_devices_entry() {
        let dashboard = make_dashboard([OptionalEntry::Mqtt]).await;
        assert!(
            !dashboard
                .routes
                .iter()
                .any(|r| r.href == "/admin/mqtt/devices"),
            "mqtt devices entry should be absent when MqttDevices not passed"
        );
    }

    #[tokio::test]
    async fn dashboard_with_mqtt_devices_has_devices_entry() {
        let dashboard = make_dashboard([OptionalEntry::Mqtt, OptionalEntry::MqttDevices]).await;
        assert!(
            dashboard
                .routes
                .iter()
                .any(|r| r.href == "/admin/mqtt/devices"),
            "mqtt devices entry should be present when MqttDevices passed"
        );
    }

    #[tokio::test]
    async fn dashboard_mqtt_devices_entry_has_expected_fields() {
        let dashboard = make_dashboard([OptionalEntry::Mqtt, OptionalEntry::MqttDevices]).await;
        let entry = dashboard
            .routes
            .iter()
            .find(|r| r.href == "/admin/mqtt/devices")
            .unwrap();
        assert_eq!(entry.display_name(), "/admin/mqtt/devices");
        assert!(!entry.description.is_empty());
    }

    #[tokio::test]
    async fn dashboard_mqtt_devices_sorted_adjacent_to_mqtt() {
        let dashboard = make_dashboard([OptionalEntry::Mqtt, OptionalEntry::MqttDevices]).await;
        let names: Vec<&str> = dashboard.routes.iter().map(|r| r.display_name()).collect();
        let mqtt_pos = names.iter().position(|n| *n == "/admin/mqtt").unwrap();
        let devices_pos = names
            .iter()
            .position(|n| *n == "/admin/mqtt/devices")
            .unwrap();
        assert_eq!(
            devices_pos,
            mqtt_pos + 1,
            "mqtt devices should immediately follow mqtt"
        );
    }

    #[tokio::test]
    async fn dashboard_deduplicates_routes_matching_service_urls() {
        let routes: Routes =
            toml::from_str("[grafana]\nurl = \"grafana.example.com\"\ndescription = \"Grafana\"\n")
                .unwrap();
        let service_urls: HashSet<String> = ["https://grafana.example.com".to_string()].into();
        let dashboard = AdminDashboard::new(
            routes,
            std::iter::empty::<OptionalEntry>(),
            &service_urls,
            None,
            Arc::new([]),
        )
        .await
        .unwrap();
        assert!(
            !dashboard
                .routes
                .iter()
                .any(|r| r.href == "https://grafana.example.com"),
            "route matching a service url should be filtered out"
        );
    }
}
