use std::sync::Arc;

use askama::Template;
use axum::{
    extract::{FromRef, State},
    response::Html,
};

use crate::{
    ServerState,
    auth::{AuthUserInfo, MaybeAuthUser},
    blog::{BlogEntry, BlogStore},
    error::Error,
};

/// A navigation link shown in the site-wide nav bar.
#[derive(Debug, Clone)]
pub struct NavLink {
    pub name: String,
    pub href: String,
    /// When true, the link is only rendered for admin users.
    pub is_admin: bool,
}

/// The public landing page.
#[derive(Debug, Clone, Template)]
#[template(path = "index.html")]
pub struct Index {
    pub version: &'static str,
    pub auth_user: Option<AuthUserInfo>,
    pub logo_url: Option<String>,
    pub nav_links: Arc<[NavLink]>,
    /// Whether `/recipes` is configured and should be linked from the landing page.
    pub has_recipes: bool,
    /// Whether `/about` is configured and should be linked from the landing page.
    pub has_about: bool,
    /// Most recent blog posts, fetched live per-request (see `index`). Empty
    /// when no blog vault is configured.
    pub recent_posts: Vec<BlogEntry>,
}

impl Index {
    pub fn new(
        logo_url: Option<String>,
        nav_links: Arc<[NavLink]>,
        has_recipes: bool,
        has_about: bool,
    ) -> Self {
        Index {
            version: crate::VERSION,
            auth_user: None,
            logo_url,
            nav_links,
            has_recipes,
            has_about,
            recent_posts: Vec::new(),
        }
    }
}

/// Number of recent posts teased on the landing page.
const RECENT_POSTS_COUNT: usize = 3;

/// Resolve just the pre-rendered index template out of `ServerState`, so
/// `index` doesn't need to depend on the rest of the app's state.
impl FromRef<ServerState> for Index {
    fn from_ref(state: &ServerState) -> Self {
        state.index.clone()
    }
}

/// `GET /` — public landing page.
pub async fn index(
    MaybeAuthUser(auth_user): MaybeAuthUser,
    State(index_template): State<Index>,
    State(blog_store): State<Option<Arc<BlogStore>>>,
) -> Result<Html<String>, Error> {
    let recent_posts = blog_store
        .as_ref()
        .map(|store| {
            store
                .entries
                .iter()
                .take(RECENT_POSTS_COUNT)
                .cloned()
                .collect()
        })
        .unwrap_or_default();

    let page = Index {
        auth_user,
        recent_posts,
        ..index_template
    };
    Ok(Html(page.render()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_with_recipes_has_recipes_flag() {
        let index = Index::new(None, Arc::new([]), true, false);
        assert!(index.has_recipes);
    }

    #[test]
    fn index_without_recipes_has_no_recipes_flag() {
        let index = Index::new(None, Arc::new([]), false, false);
        assert!(!index.has_recipes);
    }

    #[test]
    fn index_with_about_has_about_flag() {
        let index = Index::new(None, Arc::new([]), false, true);
        assert!(index.has_about);
    }

    #[test]
    fn index_without_about_has_no_about_flag() {
        let index = Index::new(None, Arc::new([]), false, false);
        assert!(!index.has_about);
    }

    #[tokio::test]
    async fn index_route_renders_without_admin_content() {
        use axum::{Router, body::Body, http::Request, routing::get};
        use tower::ServiceExt;

        let state = crate::tests::minimal_server_state().await;
        let app = Router::new().route("/", get(index)).with_state(state);
        let req = Request::builder().uri("/").body(Body::empty()).unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            !html.contains("/admin"),
            "landing page must not link /admin"
        );
        assert!(
            !html.contains("/breaker"),
            "landing page must not link /breaker"
        );
        assert!(
            !html.contains("/services"),
            "landing page must not link /services"
        );
    }
}
