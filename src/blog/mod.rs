//! Blog module — scans an Obsidian-style vault for notes tagged `blog`.

use std::{
    borrow::Borrow,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use askama::Template;
use axum::{
    extract::{FromRef, FromRequestParts, Path as AxumPath, State},
    http::request::Parts,
    response::Html,
};
use serde::Deserialize;
use time::{Date, format_description::FormatItem, macros::format_description};

use super::notes::{
    RenderedHtml,
    obsidian::{self, Slug, deserialize_tags},
    render_markdown,
};
use crate::{
    ServerState, VERSION,
    auth::{AuthUserInfo, MaybeAuthUser},
    error::Error,
    index::NavLink,
};

const DATE_FORMAT: &[FormatItem<'static>] = format_description!("[year]-[month]-[day]");

/// A post's publish date, parsed from frontmatter `date: YYYY-MM-DD`.
///
/// A newtype rather than a raw `String` so malformed dates are rejected
/// explicitly at scan time ([`BlogStoreError::InvalidDate`]) instead of
/// silently sorting wrong, and so callers get `Ord`/`Display` for free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PostDate(Date);

impl PostDate {
    fn parse(s: &str) -> Result<Self, time::error::Parse> {
        Date::parse(s, DATE_FORMAT).map(PostDate)
    }
}

impl std::fmt::Display for PostDate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let formatted = self.0.format(DATE_FORMAT).map_err(|_| std::fmt::Error)?;
        write!(f, "{formatted}")
    }
}

/// Blog-specific frontmatter fields, parsed independently of the shared
/// [`obsidian::Frontmatter`] used by notes/recipes.
#[derive(Debug, Default, Deserialize)]
struct BlogFrontmatter {
    title: Option<String>,
    #[serde(default, deserialize_with = "deserialize_tags")]
    tags: Vec<String>,
    date: Option<String>,
    /// Creation timestamp (e.g. `2026-01-15T09:32`) that a vault-wide plugin
    /// stamps onto every note automatically. Used as the post date when
    /// `date` isn't set explicitly — see [`resolve_date_str`].
    created: Option<String>,
    /// Last-modified timestamp, stamped by the same plugin on every edit.
    /// Shown on the post page alongside the publish date.
    updated: Option<String>,
    summary: Option<String>,
    /// Only `Some(false)` publishes a post. Absent — or explicit `true` —
    /// means draft. The vault's tagging plugin applies `tags`/`created` to
    /// every note automatically, so a note tagged `blog` could otherwise go
    /// live the moment it's created; requiring an explicit opt-out keeps
    /// publishing a deliberate action.
    #[serde(default)]
    draft: Option<bool>,
}

/// A post is published only once its author explicitly sets `draft: false`.
/// Absent, or explicit `true`, means draft — see [`BlogFrontmatter::draft`].
fn is_published(fm: &BlogFrontmatter) -> bool {
    fm.tags.iter().any(|t| t == "blog") && fm.draft == Some(false)
}

/// Drop the time-of-day portion of a plugin-stamped timestamp
/// (`2026-01-15T09:32` -> `2026-01-15`), leaving a bare date string.
fn date_only(timestamp: &str) -> &str {
    timestamp.split('T').next().unwrap_or(timestamp)
}

/// Resolve the frontmatter string to parse as the post's date: `date` if set
/// explicitly, otherwise the date portion of the plugin-supplied `created`
/// timestamp.
fn resolve_date_str(fm: &BlogFrontmatter) -> Option<String> {
    fm.date
        .clone()
        .or_else(|| fm.created.as_deref().map(date_only).map(str::to_owned))
}

/// A fully-parsed and rendered blog post. Posts are always public — no
/// secret-block redaction like notes/recipes.
#[derive(Debug, Clone)]
pub struct BlogPost {
    pub slug: Slug,
    pub title: String,
    pub date: PostDate,
    /// Last-modified date, shown on the post page. `None` if the note has no
    /// `updated` frontmatter field.
    pub updated: Option<PostDate>,
    pub summary: Option<String>,
    pub html: RenderedHtml,
}

/// Lightweight view of a post for the index listing and landing-page teasers.
#[derive(Debug, Clone)]
pub struct BlogEntry {
    pub slug: Slug,
    pub title: String,
    pub date: PostDate,
    pub summary: Option<String>,
}

#[derive(Debug)]
pub struct BlogStore {
    /// Posts sorted newest-first.
    pub entries: Vec<BlogEntry>,
    by_slug: HashMap<Slug, BlogPost>,
}

#[derive(Debug, thiserror::Error)]
pub enum BlogStoreError {
    #[error("blog vault path `{0}` does not exist or is not a directory")]
    VaultNotDirectory(PathBuf),

    #[error("failed to read post `{path}`: {source}")]
    PostRead {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("post `{path}` is tagged `blog` but has no `date` in its frontmatter")]
    MissingDate { path: PathBuf },

    #[error("invalid `date` in post `{path}`: {source}")]
    InvalidDate {
        path: PathBuf,
        source: time::error::Parse,
    },
}

impl BlogStore {
    /// Scan a vault directory for markdown files tagged `blog`.
    ///
    /// Two-pass algorithm mirroring [`crate::notes::recipes::RecipeStore::scan`]:
    /// 1. Collect all `.md` stems tagged `blog` (and not `draft`) for wiki-link
    ///    resolution.
    /// 2. Read each file, parse frontmatter, filter by `blog` tag, skip drafts,
    ///    render markdown (no redaction — posts are fully public).
    pub fn scan(vault: &Path) -> Result<Self, BlogStoreError> {
        use std::collections::HashSet;

        // Pass 1: build vault index for wiki-link resolution
        let (vault_index, md_paths) =
            obsidian::build_vault_index(vault, &HashMap::new()).map_err(|e| match e {
                obsidian::VaultError::NotDirectory(p) => BlogStoreError::VaultNotDirectory(p),
                obsidian::VaultError::ReadError { path, source } => {
                    BlogStoreError::PostRead { path, source }
                }
            })?;
        let live_slugs: HashSet<Slug> = md_paths
            .iter()
            .filter_map(|path| {
                let note: obsidian::ParsedNote<BlogFrontmatter> =
                    obsidian::parse_note(path).ok()?;
                is_published(&note.frontmatter).then_some(note.slug)
            })
            .collect();

        // Pass 2: parse, filter by blog tag, skip drafts, render
        let mut by_slug: HashMap<Slug, BlogPost> = HashMap::new();

        for path in &md_paths {
            let note: obsidian::ParsedNote<BlogFrontmatter> =
                obsidian::parse_note(path).map_err(|e| match e {
                    obsidian::VaultError::ReadError { path, source } => {
                        BlogStoreError::PostRead { path, source }
                    }
                    obsidian::VaultError::NotDirectory(p) => BlogStoreError::VaultNotDirectory(p),
                })?;

            if !is_published(&note.frontmatter) {
                continue;
            }

            let fm = note.frontmatter;
            let slug = note.slug;
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            let date_str = resolve_date_str(&fm)
                .ok_or_else(|| BlogStoreError::MissingDate { path: path.clone() })?;
            let title = fm.title.unwrap_or_else(|| stem.replace(['-', '_'], " "));
            let date =
                PostDate::parse(&date_str).map_err(|source| BlogStoreError::InvalidDate {
                    path: path.clone(),
                    source,
                })?;
            let updated = fm
                .updated
                .as_deref()
                .map(date_only)
                .map(|s| {
                    PostDate::parse(s).map_err(|source| BlogStoreError::InvalidDate {
                        path: path.clone(),
                        source,
                    })
                })
                .transpose()?;

            let rendered = render_markdown(&note.body);
            let html = RenderedHtml::from_html(obsidian::resolve_wiki_links(
                rendered.as_str(),
                &vault_index,
                &live_slugs,
                "/blog/",
            ));

            let post = BlogPost {
                slug: slug.clone(),
                title,
                date,
                updated,
                summary: fm.summary,
                html,
            };
            let _ = by_slug.insert(slug, post);
        }

        let mut entries: Vec<BlogEntry> = by_slug
            .values()
            .map(|p| BlogEntry {
                slug: p.slug.clone(),
                title: p.title.clone(),
                date: p.date,
                summary: p.summary.clone(),
            })
            .collect();
        entries.sort_by(|a, b| b.date.cmp(&a.date));

        tracing::info!(count = by_slug.len(), "blog vault loaded");

        Ok(BlogStore { entries, by_slug })
    }

    /// Look up a post by its slug. Accepts `&str` directly via [`Borrow`].
    pub fn get(&self, slug: &str) -> Option<&BlogPost>
    where
        Slug: Borrow<str>,
    {
        self.by_slug.get(slug)
    }
}

/// Resolve just the scanned blog vault out of `ServerState`, so blog routes
/// don't need to depend on the rest of the app's state.
impl FromRef<ServerState> for Option<Arc<BlogStore>> {
    fn from_ref(state: &ServerState) -> Self {
        state.blog_store.clone()
    }
}

/// Resolves to the configured blog vault, or rejects with
/// [`Error::BlogNotConfigured`] — so blog routes don't each have to repeat
/// the "is it configured?" check themselves.
pub struct Blog(pub Arc<BlogStore>);

impl<S> FromRequestParts<S> for Blog
where
    S: Send + Sync,
    Option<Arc<BlogStore>>: FromRef<S>,
{
    type Rejection = Error;

    async fn from_request_parts(_parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Option::<Arc<BlogStore>>::from_ref(state)
            .map(Blog)
            .ok_or(Error::BlogNotConfigured)
    }
}

#[derive(Template)]
#[template(path = "blog_index.html")]
pub struct BlogIndexPage {
    pub version: &'static str,
    pub entries: Vec<BlogEntry>,
    pub auth_user: Option<AuthUserInfo>,
    pub nav_links: Arc<[NavLink]>,
}

#[derive(Template)]
#[template(path = "blog_detail.html")]
pub struct BlogDetailPage {
    pub version: &'static str,
    pub title: String,
    pub date: PostDate,
    pub updated: Option<PostDate>,
    pub summary: Option<String>,
    /// Pre-rendered HTML — safe for `|safe` in the template.
    pub content: String,
    pub auth_user: Option<AuthUserInfo>,
    pub nav_links: Arc<[NavLink]>,
}

/// GET /blog — post index, newest first.
pub async fn blog_index_route(
    MaybeAuthUser(auth_user): MaybeAuthUser,
    Blog(store): Blog,
    State(nav_links): State<Arc<[NavLink]>>,
) -> Result<Html<String>, Error> {
    let page = BlogIndexPage {
        version: VERSION,
        entries: store.entries.clone(),
        auth_user,
        nav_links,
    };
    Ok(Html(page.render()?))
}

/// GET /blog/{slug} — individual post.
pub async fn blog_detail_route(
    MaybeAuthUser(auth_user): MaybeAuthUser,
    AxumPath(slug): AxumPath<String>,
    Blog(store): Blog,
    State(nav_links): State<Arc<[NavLink]>>,
) -> Result<Html<String>, Error> {
    let post = store.get(&slug).ok_or(Error::NotFound)?;
    let page = BlogDetailPage {
        version: VERSION,
        title: post.title.clone(),
        date: post.date,
        updated: post.updated,
        summary: post.summary.clone(),
        content: post.html.as_str().to_owned(),
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

    fn fixture_vault() -> PathBuf {
        PathBuf::from("fixtures/vault")
    }

    #[test]
    fn blog_store_scan_loads_blog_posts() {
        let store = BlogStore::scan(&fixture_vault()).expect("scan should succeed");
        assert!(!store.entries.is_empty(), "should have at least one post");
    }

    #[test]
    fn blog_store_excludes_non_blog_notes() {
        let store = BlogStore::scan(&fixture_vault()).expect("scan should succeed");
        assert!(
            !store.entries.iter().any(|e| e.title == "The Known World"),
            "world-tagged notes must not appear in blog store"
        );
    }

    #[test]
    fn blog_store_excludes_drafts() {
        let store = BlogStore::scan(&fixture_vault()).expect("scan should succeed");
        assert!(
            !store.entries.iter().any(|e| e.title == "Unfinished Draft"),
            "draft posts must not appear in the store"
        );
    }

    #[test]
    fn blog_store_excludes_notes_without_a_draft_field() {
        let store = BlogStore::scan(&fixture_vault()).expect("scan should succeed");
        assert!(
            !store.entries.iter().any(|e| e.title == "No Draft Field"),
            "a blog-tagged note with no draft field must stay unpublished"
        );
    }

    #[test]
    fn blog_store_falls_back_to_created_for_date() {
        let store = BlogStore::scan(&fixture_vault()).expect("scan should succeed");
        let entry = store
            .entries
            .iter()
            .find(|e| e.title == "Created Fallback Post")
            .expect("post with no explicit date, falling back to created, should be published");
        assert_eq!(entry.date.to_string(), "2026-01-10");
    }

    #[test]
    fn blog_store_parses_updated_field() {
        let store = BlogStore::scan(&fixture_vault()).expect("scan should succeed");
        let post = store
            .get("created-fallback-post")
            .expect("post should be in the store");
        assert_eq!(
            post.updated.map(|d| d.to_string()),
            Some("2026-01-12".to_string())
        );
    }

    #[test]
    fn blog_store_updated_is_none_without_frontmatter_field() {
        let store = BlogStore::scan(&fixture_vault()).expect("scan should succeed");
        let post = store
            .get("first-post")
            .expect("post should be in the store");
        assert_eq!(post.updated, None);
    }

    #[test]
    fn blog_store_sorted_newest_first() {
        let store = BlogStore::scan(&fixture_vault()).expect("scan should succeed");
        let dates: Vec<PostDate> = store.entries.iter().map(|e| e.date).collect();
        let mut sorted = dates.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(dates, sorted, "entries should be sorted newest-first");
    }

    #[test]
    fn post_date_parses_iso_format() {
        let date = PostDate::parse("2026-01-15").expect("should parse");
        assert_eq!(date.to_string(), "2026-01-15");
    }

    #[test]
    fn post_date_rejects_malformed_input() {
        assert!(PostDate::parse("not-a-date").is_err());
        assert!(PostDate::parse("2026/01/15").is_err());
    }

    #[test]
    fn parse_blog_fm_all_fields() {
        let raw = "---\ntitle: Test Post\ntags: [blog]\ndate: \"2026-01-15\"\nsummary: A test post\n---\nBody.\n";
        let (fm, rest) = obsidian::parse_frontmatter::<BlogFrontmatter>(raw);
        assert_eq!(fm.title.as_deref(), Some("Test Post"));
        assert!(fm.tags.contains(&"blog".to_string()));
        assert_eq!(fm.date.as_deref(), Some("2026-01-15"));
        assert_eq!(fm.summary.as_deref(), Some("A test post"));
        assert_eq!(fm.draft, None);
        assert_eq!(rest, "Body.\n");
    }

    #[test]
    fn parse_blog_fm_draft_flag() {
        let raw = "---\ntags: [blog]\ndraft: true\n---\nBody.\n";
        let (fm, _rest) = obsidian::parse_frontmatter::<BlogFrontmatter>(raw);
        assert_eq!(fm.draft, Some(true));
    }

    #[test]
    fn parse_blog_fm_draft_false_publishes() {
        let raw = "---\ntags: [blog]\ndraft: false\n---\nBody.\n";
        let (fm, _rest) = obsidian::parse_frontmatter::<BlogFrontmatter>(raw);
        assert_eq!(fm.draft, Some(false));
        assert!(is_published(&fm));
    }

    #[test]
    fn is_published_requires_explicit_draft_false() {
        let tagged = BlogFrontmatter {
            tags: vec!["blog".to_string()],
            ..Default::default()
        };
        assert!(
            !is_published(&tagged),
            "a blog-tagged note with no draft field must stay unpublished"
        );

        let explicit_draft = BlogFrontmatter {
            tags: vec!["blog".to_string()],
            draft: Some(true),
            ..Default::default()
        };
        assert!(!is_published(&explicit_draft));

        let published = BlogFrontmatter {
            tags: vec!["blog".to_string()],
            draft: Some(false),
            ..Default::default()
        };
        assert!(is_published(&published));
    }

    #[test]
    fn resolve_date_str_prefers_explicit_date() {
        let fm = BlogFrontmatter {
            date: Some("2026-02-02".to_string()),
            created: Some("2020-01-01T00:00".to_string()),
            ..Default::default()
        };
        assert_eq!(resolve_date_str(&fm), Some("2026-02-02".to_string()));
    }

    #[test]
    fn resolve_date_str_falls_back_to_created() {
        let fm = BlogFrontmatter {
            created: Some("2026-01-15T09:32".to_string()),
            ..Default::default()
        };
        assert_eq!(resolve_date_str(&fm), Some("2026-01-15".to_string()));
    }

    #[test]
    fn resolve_date_str_none_when_both_absent() {
        let fm = BlogFrontmatter::default();
        assert_eq!(resolve_date_str(&fm), None);
    }

    async fn make_state_without_blog() -> ServerState {
        crate::tests::minimal_server_state().await
    }

    async fn make_state_with_blog() -> ServerState {
        let mut state = crate::tests::minimal_server_state().await;
        let store = BlogStore::scan(&fixture_vault()).expect("scan should succeed");
        state.blog_store = Some(Arc::new(store));
        state
    }

    #[tokio::test]
    async fn blog_index_returns_404_without_vault() {
        let state = make_state_without_blog().await;
        let app = axum::Router::new()
            .route("/blog", get(blog_index_route))
            .with_state(state);
        let req = Request::builder().uri("/blog").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn blog_index_returns_200_with_vault() {
        let state = make_state_with_blog().await;
        let app = axum::Router::new()
            .route("/blog", get(blog_index_route))
            .with_state(state);
        let req = Request::builder().uri("/blog").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn blog_detail_returns_404_without_vault() {
        let state = make_state_without_blog().await;
        let app = axum::Router::new()
            .route("/blog/{slug}", get(blog_detail_route))
            .with_state(state);
        let req = Request::builder()
            .uri("/blog/anything")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn blog_detail_returns_404_for_missing_slug() {
        let state = make_state_with_blog().await;
        let app = axum::Router::new()
            .route("/blog/{slug}", get(blog_detail_route))
            .with_state(state);
        let req = Request::builder()
            .uri("/blog/nonexistent-post")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn blog_detail_returns_200_for_existing_post() {
        let state = make_state_with_blog().await;
        let store = state.blog_store.clone().unwrap();
        let slug = store.entries[0].slug.clone();
        let app = axum::Router::new()
            .route("/blog/{slug}", get(blog_detail_route))
            .with_state(state);
        let req = Request::builder()
            .uri(format!("/blog/{slug}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
