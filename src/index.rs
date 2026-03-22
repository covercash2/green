use askama::Template;
use axum::{extract::State, response::Html};

use crate::{Routes, ServerState, auth::{AuthUserInfo, MaybeAuthUser}, error::Error};

#[derive(Debug, Clone)]
pub struct IndexEntry {
    pub name: String,
    pub href: String,
    pub description: String,
}

#[derive(Debug, Clone, Template)]
#[template(path = "index.html")]
pub struct Index {
    pub routes: Vec<IndexEntry>,
    pub version: &'static str,
    pub auth_user: Option<AuthUserInfo>,
}

impl Index {
    pub async fn new(routes: Routes, has_notes: bool, has_mqtt: bool, has_mqtt_devices: bool) -> Result<Self, Error> {
        let static_entries = [
            IndexEntry {
                name: "breaker box".into(),
                href: "/breaker".into(),
                description: "Electrical circuit layout".into(),
            },
            IndexEntry {
                name: "qr code".into(),
                href: "/qr".into(),
                description: "Generate a QR code".into(),
            },
            IndexEntry {
                name: "tailscale".into(),
                href: "/tailscale".into(),
                description: "Tailscale peer list".into(),
            },
        ];

        let notes_entry = has_notes.then_some(IndexEntry {
            name: "notes".into(),
            href: "/notes".into(),
            description: "D&D campaign notes".into(),
        });

        let mqtt_entry = has_mqtt.then_some(IndexEntry {
            name: "mqtt".into(),
            href: "/mqtt".into(),
            description: "Live MQTT message feed".into(),
        });

        let mqtt_devices_entry = has_mqtt_devices.then_some(IndexEntry {
            name: "mqtt devices".into(),
            href: "/mqtt/devices".into(),
            description: "MQTT device inventory".into(),
        });

        let mut routes: Vec<IndexEntry> = routes
            .into_iter()
            .map(|(name, info)| IndexEntry {
                name,
                href: format!("https://{}", info.url),
                description: info.description,
            })
            .chain(static_entries)
            .chain(notes_entry)
            .chain(mqtt_entry)
            .chain(mqtt_devices_entry)
            .collect();

        routes.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(Index {
            routes,
            version: crate::VERSION,
            auth_user: None,
        })
    }
}

pub async fn index(
    MaybeAuthUser(auth_user): MaybeAuthUser,
    State(state): State<ServerState>,
) -> Result<Html<String>, Error> {
    let page = Index {
        auth_user,
        ..state.index.clone()
    };
    Ok(Html(page.render()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route::Routes;

    #[tokio::test]
    async fn index_without_notes_has_no_notes_entry() {
        let index = Index::new(Routes::default(), false, false, false).await.unwrap();
        assert!(
            !index.routes.iter().any(|r| r.href == "/notes"),
            "notes entry should be absent when has_notes=false"
        );
    }

    #[tokio::test]
    async fn index_with_notes_has_notes_entry() {
        let index = Index::new(Routes::default(), true, false, false).await.unwrap();
        assert!(
            index.routes.iter().any(|r| r.href == "/notes"),
            "notes entry should be present when has_notes=true"
        );
    }

    #[tokio::test]
    async fn index_always_has_static_entries() {
        let index = Index::new(Routes::default(), false, false, false).await.unwrap();
        assert!(index.routes.iter().any(|r| r.href == "/breaker"));
        assert!(index.routes.iter().any(|r| r.href == "/qr"));
    }

    #[tokio::test]
    async fn index_entries_sorted_alphabetically() {
        let index = Index::new(Routes::default(), true, false, false).await.unwrap();
        let names: Vec<&str> = index.routes.iter().map(|r| r.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "index entries should be in alphabetical order");
    }

    #[tokio::test]
    async fn index_without_mqtt_devices_has_no_devices_entry() {
        let index = Index::new(Routes::default(), false, true, false).await.unwrap();
        assert!(
            !index.routes.iter().any(|r| r.href == "/mqtt/devices"),
            "mqtt devices entry should be absent when has_mqtt_devices=false"
        );
    }

    #[tokio::test]
    async fn index_with_mqtt_devices_has_devices_entry() {
        let index = Index::new(Routes::default(), false, true, true).await.unwrap();
        assert!(
            index.routes.iter().any(|r| r.href == "/mqtt/devices"),
            "mqtt devices entry should be present when has_mqtt_devices=true"
        );
    }

    #[tokio::test]
    async fn index_mqtt_devices_entry_has_expected_fields() {
        let index = Index::new(Routes::default(), false, true, true).await.unwrap();
        let entry = index.routes.iter().find(|r| r.href == "/mqtt/devices").unwrap();
        assert_eq!(entry.name, "mqtt devices");
        assert!(!entry.description.is_empty());
    }

    #[tokio::test]
    async fn index_mqtt_devices_sorted_adjacent_to_mqtt() {
        let index = Index::new(Routes::default(), false, true, true).await.unwrap();
        let names: Vec<&str> = index.routes.iter().map(|r| r.name.as_str()).collect();
        let mqtt_pos = names.iter().position(|&n| n == "mqtt").unwrap();
        let devices_pos = names.iter().position(|&n| n == "mqtt devices").unwrap();
        assert_eq!(devices_pos, mqtt_pos + 1, "mqtt devices should immediately follow mqtt");
    }
}
