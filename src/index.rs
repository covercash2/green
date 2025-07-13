use std::sync::Arc;

use askama::Template;
use axum::{extract::State, response::Html};

use crate::{error::Error, ServerState};

#[derive(Debug, Clone, Template)]
#[template(path = "index.html")]
pub struct Index {
    pub routes: Arc<[String]>,
}

impl<T> From<T> for Index
where
    T: AsRef<[String]>,
{
    fn from(routes: T) -> Self {
        Index {
            routes: Arc::from(routes.as_ref().to_owned()),
        }
    }
}

pub async fn index(
    State(state): State<ServerState>,
) -> Result<Html<String>, Error> {
    Ok(Html(state.index.render()?))
}

