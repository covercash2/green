use askama::Template;
use axum::{extract::State, response::Html};

use crate::{Routes, ServerState, error::Error};

#[derive(Debug, Clone, Template)]
#[template(path = "index.html")]
pub struct Index {
    pub routes: Vec<(String, String)>,
}

impl From<Routes> for Index {
    fn from(routes: Routes) -> Self {
        let routes = routes.into_iter().collect();
        Index { routes }
    }
}

pub async fn index(State(state): State<ServerState>) -> Result<Html<String>, Error> {
    Ok(Html(state.index.render()?))
}
