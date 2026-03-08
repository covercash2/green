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
    pub async fn new(routes: Routes) -> Result<Self, Error> {
        let mut routes: Vec<IndexEntry> = routes
            .into_iter()
            .map(|(name, info)| IndexEntry {
                name,
                href: format!("https://{}", info.url),
                description: info.description,
            })
            .chain([
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
            ])
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
