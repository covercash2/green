//! About-me page — a single Markdown file rendered once at startup.
//!
//! Mirrors `breaker::BreakerContent`'s single-file pattern: no
//! vault-scanning machinery is needed for one static page.

use std::sync::Arc;

use askama::Template;
use axum::{
    extract::{FromRef, FromRequestParts, State},
    http::request::Parts,
    response::Html,
};

use crate::{
    ServerState, VERSION,
    auth::{AuthUserInfo, MaybeAuthUser},
    error::Error,
    index::NavLink,
    notes::render_markdown,
};

/// Pre-rendered about-me page content, loaded once at startup from a single
/// configured Markdown file.
#[derive(Debug, Clone)]
pub struct AboutContent(pub String);

impl AboutContent {
    pub fn new(markdown: &str) -> Self {
        AboutContent(render_markdown(markdown).into_inner())
    }
}

/// Resolve just the about-me content out of `ServerState`, so `about_route`
/// doesn't need to depend on the rest of the app's state.
impl FromRef<ServerState> for Option<Arc<AboutContent>> {
    fn from_ref(state: &ServerState) -> Self {
        state.about_content.clone()
    }
}

/// Resolves to the configured about-me content, or rejects with
/// [`Error::AboutNotConfigured`] — so `about_route` doesn't have to repeat
/// the "is it configured?" check itself.
pub struct About(pub Arc<AboutContent>);

impl<S> FromRequestParts<S> for About
where
    S: Send + Sync,
    Option<Arc<AboutContent>>: FromRef<S>,
{
    type Rejection = Error;

    async fn from_request_parts(_parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Option::<Arc<AboutContent>>::from_ref(state)
            .map(About)
            .ok_or(Error::AboutNotConfigured)
    }
}

#[derive(Template)]
#[template(path = "about.html")]
pub struct AboutPage {
    pub version: &'static str,
    /// Pre-rendered HTML — safe for `|safe` in the template.
    pub content: String,
    pub auth_user: Option<AuthUserInfo>,
    pub nav_links: Arc<[NavLink]>,
}

/// GET /about — about-me page.
pub async fn about_route(
    MaybeAuthUser(auth_user): MaybeAuthUser,
    About(content): About,
    State(nav_links): State<Arc<[NavLink]>>,
) -> Result<Html<String>, Error> {
    let page = AboutPage {
        version: VERSION,
        content: content.0.clone(),
        auth_user,
        nav_links,
    };
    Ok(Html(page.render()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::get,
    };
    use tower::ServiceExt;

    #[test]
    fn about_content_renders_markdown() {
        let content = AboutContent::new("# Hi\n\nI'm someone.");
        assert!(content.0.contains("<h1>"));
        assert!(content.0.contains("I'm someone."));
    }

    async fn make_state_without_about() -> ServerState {
        crate::tests::minimal_server_state().await
    }

    async fn make_state_with_about() -> ServerState {
        let mut state = crate::tests::minimal_server_state().await;
        state.about_content = Some(Arc::new(AboutContent::new("# Hi\n\nabout content")));
        state
    }

    #[tokio::test]
    async fn about_returns_404_without_content() {
        let state = make_state_without_about().await;
        let app = axum::Router::new()
            .route("/about", get(about_route))
            .with_state(state);
        let req = Request::builder()
            .uri("/about")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn about_returns_200_with_content() {
        let state = make_state_with_about().await;
        let app = axum::Router::new()
            .route("/about", get(about_route))
            .with_state(state);
        let req = Request::builder()
            .uri("/about")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("about content"));
    }
}
