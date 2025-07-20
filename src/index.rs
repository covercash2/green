use askama::Template;
use axum::{extract::State, response::Html};

use crate::{error::Error, route::RouteInfo, Routes, ServerState};

#[derive(Debug, Clone, Template)]
#[template(path = "index.html")]
pub struct Index {
    pub routes: Vec<(String, RouteInfo)>,
}

impl Index {
    pub async fn new(routes: Routes) -> Result<Self, Error> {
        let routes = {
            let mut routes = routes.into_iter().collect::<Vec<_>>();
            routes.sort_by(|a, b| a.0.cmp(&b.0));
            routes
        };

        Ok(Index { routes })
    }
}

pub async fn index(State(state): State<ServerState>) -> Result<Html<String>, Error> {
    Ok(Html(state.index.render()?))
}
