use askama::Template;
use axum::{extract::State, response::Html};
use pulldown_cmark::{html, Options, Parser};

use crate::{error::Error, ServerState};

#[derive(Debug, Clone, Template)]
#[template(path = "breaker.html")]
pub struct BreakerPage {
    pub content: String,
    pub version: &'static str,
}

impl BreakerPage {
    pub fn new(markdown: &str) -> Self {
        let parser = Parser::new_ext(markdown, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS);
        let mut content = String::new();
        html::push_html(&mut content, parser);
        BreakerPage { content, version: crate::VERSION }
    }
}

pub async fn breaker_route(State(state): State<ServerState>) -> Result<Html<String>, Error> {
    Ok(Html(state.breaker_page.render()?))
}
