use std::{collections::HashSet, sync::Arc};

use askama::Template;
use axum::{extract::State, response::Html};

use crate::{
    Routes, ServerState,
    auth::{AuthUserInfo, MaybeAuthUser},
    error::Error,
    services::{PeerServiceGroup, ServiceStatus},
};

/// A navigation link shown in the site-wide nav bar.
#[derive(Debug, Clone)]
pub struct NavLink {
    pub name: String,
    pub href: String,
    /// When true, the link is only rendered for GM users.
    pub is_gm: bool,
}

#[derive(Debug, Clone, bon::Builder)]
#[builder(on(String, into), start_fn = href)]
pub struct IndexEntry {
    #[builder(start_fn)]
    pub href: String,
    pub name: Option<String>,
    pub description: String,
    pub icon_url: Option<String>,
}

impl IndexEntry {
    /// Display name: explicit `name` if set, otherwise the `href` as-is.
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.href)
    }
}

#[derive(Debug, Clone, Template)]
#[template(path = "index.html")]
pub struct Index {
    pub routes: Vec<IndexEntry>,
    /// Local systemd service statuses, queried per-request.
    pub services: Vec<ServiceStatus>,
    /// Remote peer service groups, fetched in parallel per-request (GM only).
    /// Empty for non-GM users — peers are only shown to GMs.
    pub peer_groups: Vec<PeerServiceGroup>,
    pub version: &'static str,
    pub auth_user: Option<AuthUserInfo>,
    pub logo_url: Option<String>,
    pub nav_links: Arc<[NavLink]>,
}

/// Optional index entries that are only shown when the corresponding feature
/// is configured. Pass any subset to [`Index::new`].
#[derive(Debug)]
pub enum OptionalEntry {
    Notes,
    Recipes,
    Mqtt,
    MqttDevices,
    Logs,
}

impl From<OptionalEntry> for IndexEntry {
    fn from(e: OptionalEntry) -> Self {
        match e {
            OptionalEntry::Notes => IndexEntry::href("/notes")
                .description("D&D campaign notes")
                .build(),
            OptionalEntry::Recipes => IndexEntry::href("/recipes")
                .description("Recipe collection")
                .build(),
            OptionalEntry::Mqtt => IndexEntry::href("/mqtt")
                .description("Live MQTT message feed")
                .build(),
            OptionalEntry::MqttDevices => IndexEntry::href("/mqtt/devices")
                .description("MQTT device inventory")
                .build(),
            OptionalEntry::Logs => IndexEntry::href("/logs/app")
                .description("Dev server log viewer")
                .build(),
        }
    }
}

impl Index {
    pub async fn new(
        routes: Routes,
        optional_entries: impl IntoIterator<Item = impl Into<IndexEntry>>,
        service_urls: &HashSet<String>,
        logo_url: Option<String>,
        nav_links: Arc<[NavLink]>,
    ) -> Result<Self, Error> {
        let static_entries = [
            IndexEntry::href("/breaker")
                .description("Electrical circuit layout")
                .build(),
            IndexEntry::href("/qr")
                .description("Generate a QR code")
                .build(),
            IndexEntry::href("/tailscale")
                .description("Tailscale peer list")
                .build(),
        ];

        let mut routes: Vec<IndexEntry> = routes
            .into_iter()
            .filter(|(_, info)| !service_urls.contains(&format!("https://{}", info.url)))
            .map(|(name, info)| {
                IndexEntry::href(format!("https://{}", info.url))
                    .name(name)
                    .description(info.description)
                    .maybe_icon_url(info.icon_url)
                    .build()
            })
            .chain(static_entries)
            .chain(optional_entries.into_iter().map(Into::into))
            .collect();

        routes.sort_by(|a, b| a.display_name().cmp(b.display_name()));

        Ok(Index {
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

pub async fn index(
    MaybeAuthUser(auth_user): MaybeAuthUser,
    State(state): State<ServerState>,
) -> Result<Html<String>, Error> {
    // Local services — always fetched when systemd is configured.
    let local_services = if let Some(ref config) = state.systemd_config {
        crate::services::query_all(config).await
    } else {
        Vec::new()
    };

    // Peer services — fetched in parallel, but only for GM users.
    // Non-GMs never see peer data; skip the network calls entirely so a GM
    // elsewhere is not waiting on peers that the current user wouldn't see.
    //
    // Only peers with `api_key` configured are contacted.  Peers without a key
    // appear in the nav drawer (Stage 2) but not in the services section.
    //
    // All fetches run concurrently via `futures::future::join_all`.  Each
    // individual fetch has its own timeout (see PEER_FETCH_TIMEOUT in
    // services.rs), so the overall latency is bounded by the slowest peer, not
    // the sum of all peers.
    // See: https://docs.rs/futures/latest/futures/future/fn.join_all.html
    let peer_groups = if auth_user.as_ref().map(|u| u.is_gm()).unwrap_or(false) {
        let peers_with_keys: Vec<_> = state.peers.iter().filter(|p| p.api_key.is_some()).collect();
        futures::future::join_all(
            peers_with_keys
                .iter()
                .map(|p| crate::services::fetch_peer_services(p, &state.http_client)),
        )
        .await
    } else {
        Vec::new()
    };

    let page = Index {
        auth_user,
        services: local_services,
        peer_groups,
        ..state.index.clone()
    };
    Ok(Html(page.render()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route::Routes;

    async fn make_index(entries: impl IntoIterator<Item = OptionalEntry>) -> Index {
        Index::new(
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
    async fn index_without_notes_has_no_notes_entry() {
        let index = make_index([]).await;
        assert!(
            !index.routes.iter().any(|r| r.href == "/notes"),
            "notes entry should be absent when Notes not passed"
        );
    }

    #[tokio::test]
    async fn index_with_notes_has_notes_entry() {
        let index = make_index([OptionalEntry::Notes]).await;
        assert!(
            index.routes.iter().any(|r| r.href == "/notes"),
            "notes entry should be present when Notes passed"
        );
    }

    #[tokio::test]
    async fn index_always_has_static_entries() {
        let index = make_index([]).await;
        assert!(index.routes.iter().any(|r| r.href == "/breaker"));
        assert!(index.routes.iter().any(|r| r.href == "/qr"));
    }

    #[tokio::test]
    async fn index_entries_sorted_alphabetically() {
        let index = make_index([OptionalEntry::Notes]).await;
        let names: Vec<&str> = index.routes.iter().map(|r| r.display_name()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(
            names, sorted,
            "index entries should be in alphabetical order"
        );
    }

    #[tokio::test]
    async fn index_without_recipes_has_no_recipes_entry() {
        let index = make_index([]).await;
        assert!(
            !index.routes.iter().any(|r| r.href == "/recipes"),
            "recipes entry should be absent when Recipes not passed"
        );
    }

    #[tokio::test]
    async fn index_with_recipes_has_recipes_entry() {
        let index = make_index([OptionalEntry::Recipes]).await;
        assert!(
            index.routes.iter().any(|r| r.href == "/recipes"),
            "recipes entry should be present when Recipes passed"
        );
    }

    #[tokio::test]
    async fn index_without_mqtt_devices_has_no_devices_entry() {
        let index = make_index([OptionalEntry::Mqtt]).await;
        assert!(
            !index.routes.iter().any(|r| r.href == "/mqtt/devices"),
            "mqtt devices entry should be absent when MqttDevices not passed"
        );
    }

    #[tokio::test]
    async fn index_with_mqtt_devices_has_devices_entry() {
        let index = make_index([OptionalEntry::Mqtt, OptionalEntry::MqttDevices]).await;
        assert!(
            index.routes.iter().any(|r| r.href == "/mqtt/devices"),
            "mqtt devices entry should be present when MqttDevices passed"
        );
    }

    #[tokio::test]
    async fn index_mqtt_devices_entry_has_expected_fields() {
        let index = make_index([OptionalEntry::Mqtt, OptionalEntry::MqttDevices]).await;
        let entry = index
            .routes
            .iter()
            .find(|r| r.href == "/mqtt/devices")
            .unwrap();
        assert_eq!(entry.display_name(), "/mqtt/devices");
        assert!(!entry.description.is_empty());
    }

    #[tokio::test]
    async fn index_mqtt_devices_sorted_adjacent_to_mqtt() {
        let index = make_index([OptionalEntry::Mqtt, OptionalEntry::MqttDevices]).await;
        let names: Vec<&str> = index.routes.iter().map(|r| r.display_name()).collect();
        let mqtt_pos = names.iter().position(|n| *n == "/mqtt").unwrap();
        let devices_pos = names.iter().position(|n| *n == "/mqtt/devices").unwrap();
        assert_eq!(
            devices_pos,
            mqtt_pos + 1,
            "mqtt devices should immediately follow mqtt"
        );
    }

    #[tokio::test]
    async fn index_deduplicates_routes_matching_service_urls() {
        let routes: Routes =
            toml::from_str("[grafana]\nurl = \"grafana.example.com\"\ndescription = \"Grafana\"\n")
                .unwrap();
        let service_urls: HashSet<String> = ["https://grafana.example.com".to_string()].into();
        let index = Index::new(
            routes,
            std::iter::empty::<OptionalEntry>(),
            &service_urls,
            None,
            Arc::new([]),
        )
        .await
        .unwrap();
        assert!(
            !index
                .routes
                .iter()
                .any(|r| r.href == "https://grafana.example.com"),
            "route matching a service url should be filtered out"
        );
    }
}
