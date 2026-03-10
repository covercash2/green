use askama::Template;
use axum::{extract::State, response::Html};

use crate::{Routes, ServerState, error::Error};

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
}

impl Index {
    pub async fn new(routes: Routes, has_notes: bool) -> Result<Self, Error> {
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
        ];

        let notes_entry = has_notes.then_some(IndexEntry {
            name: "notes".into(),
            href: "/notes".into(),
            description: "D&D campaign notes".into(),
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
            .collect();

        routes.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(Index {
            routes,
            version: crate::VERSION,
        })
    }
}

pub async fn index(State(state): State<ServerState>) -> Result<Html<String>, Error> {
    Ok(Html(state.index.render()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route::Routes;

    #[tokio::test]
    async fn index_without_notes_has_no_notes_entry() {
        let index = Index::new(Routes::default(), false).await.unwrap();
        assert!(
            !index.routes.iter().any(|r| r.href == "/notes"),
            "notes entry should be absent when has_notes=false"
        );
    }

    #[tokio::test]
    async fn index_with_notes_has_notes_entry() {
        let index = Index::new(Routes::default(), true).await.unwrap();
        assert!(
            index.routes.iter().any(|r| r.href == "/notes"),
            "notes entry should be present when has_notes=true"
        );
    }

    #[tokio::test]
    async fn index_always_has_static_entries() {
        let index = Index::new(Routes::default(), false).await.unwrap();
        assert!(index.routes.iter().any(|r| r.href == "/breaker"));
        assert!(index.routes.iter().any(|r| r.href == "/qr"));
    }

    #[tokio::test]
    async fn index_entries_sorted_alphabetically() {
        let index = Index::new(Routes::default(), true).await.unwrap();
        let names: Vec<&str> = index.routes.iter().map(|r| r.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "index entries should be in alphabetical order");
    }
}
