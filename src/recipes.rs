//! Recipe vault page — scans an Obsidian-style vault for notes tagged `recipe`.

use std::{
    borrow::Borrow,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use askama::Template;
use axum::{
    extract::{Path as AxumPath, State},
    response::Html,
};
use serde::Deserialize;

use crate::{
    ServerState, VERSION,
    auth::{AuthUserInfo, MaybeAuthUser, Role},
    error::Error,
    index::NavLink,
    notes::{
        RenderedHtml, Slug,
        render_markdown, render_note_body_redacted, render_note_body_revealed,
        resolve_wiki_links,
        SECRET_PLACEHOLDER,
    },
};

// ─── Frontmatter ──────────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
struct RecipeFrontMatter {
    pub title: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub servings: Option<String>,
    #[serde(default)]
    pub prep_time: Option<String>,
    #[serde(default)]
    pub cook_time: Option<String>,
}

fn parse_recipe_frontmatter(content: &str) -> (RecipeFrontMatter, &str) {
    let content = content.trim_start();
    if !content.starts_with("---") {
        return (RecipeFrontMatter::default(), content);
    }
    let after_open = &content[3..];
    if let Some(close_pos) = after_open.find("\n---") {
        let yaml = &after_open[..close_pos];
        let rest = &after_open[close_pos + 4..]; // skip "\n---"
        let rest = rest.strip_prefix('\n').unwrap_or(rest);
        let fm = serde_yml::from_str(yaml).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "malformed recipe frontmatter, using defaults");
            RecipeFrontMatter::default()
        });
        (fm, rest)
    } else {
        (RecipeFrontMatter::default(), content)
    }
}

// ─── Public types ─────────────────────────────────────────────────────────────

/// A fully-parsed and rendered recipe note.
#[derive(Debug, Clone)]
pub struct Recipe {
    pub slug: Slug,
    pub title: String,
    pub category: String,
    pub servings: Option<String>,
    pub prep_time: Option<String>,
    pub cook_time: Option<String>,
    /// Player-visible HTML: secret blocks replaced with placeholder.
    pub html: RenderedHtml,
    /// GM-visible HTML: all blocks rendered without redaction.
    pub html_gm: RenderedHtml,
    /// `true` if any secret blocks were found.
    pub has_secrets: bool,
}

/// Lightweight view of a recipe for the index page (no HTML body).
#[derive(Debug, Clone)]
#[allow(dead_code)] // fields read by Askama-generated template code
pub struct RecipeEntry {
    pub slug: Slug,
    pub title: String,
    pub has_secrets: bool,
}

/// A group of recipes sharing the same category.
#[derive(Debug, Clone)]
pub struct CategoryGroup {
    pub name: String,
    pub recipes: Vec<RecipeEntry>,
}

#[derive(Debug)]
pub struct RecipeStore {
    /// Recipes pre-grouped by category (sorted: category name, then recipe title).
    pub groups: Vec<CategoryGroup>,
    by_slug: HashMap<Slug, Recipe>,
}

#[derive(Debug, thiserror::Error)]
pub enum RecipeStoreError {
    #[error("recipe vault path `{0}` does not exist or is not a directory")]
    VaultNotDirectory(PathBuf),

    #[error("failed to read recipe `{path}`: {source}")]
    RecipeRead {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl RecipeStore {
    /// Scan a vault directory for markdown files tagged `recipe`.
    ///
    /// Two-pass algorithm:
    /// 1. Collect all `.md` stems → `HashSet<Slug>` for wiki-link resolution.
    /// 2. Read each file, parse frontmatter, filter by `recipe` tag, render with
    ///    secret-block processing, group by category.
    pub fn scan(vault: &Path) -> Result<Self, RecipeStoreError> {
        use std::collections::HashSet;
        use walkdir::WalkDir;

        if !vault.is_dir() {
            return Err(RecipeStoreError::VaultNotDirectory(vault.to_path_buf()));
        }

        // Pass 1: build slug set for wiki-link resolution
        let mut md_paths: Vec<PathBuf> = Vec::new();
        let mut slug_set: HashSet<Slug> = HashSet::new();

        for entry in WalkDir::new(vault)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.into_path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    let _ = slug_set.insert(Slug::from_stem(stem));
                }
                md_paths.push(path);
            }
        }

        // Pass 2: parse, filter by recipe tag, render
        let mut by_category: HashMap<String, Vec<Recipe>> = HashMap::new();
        let mut by_slug: HashMap<Slug, Recipe> = HashMap::new();

        for path in &md_paths {
            let raw =
                std::fs::read_to_string(path).map_err(|source| RecipeStoreError::RecipeRead {
                    path: path.clone(),
                    source,
                })?;

            let (fm, body) = parse_recipe_frontmatter(&raw);

            if !fm.tags.iter().any(|t| t == "recipe") {
                continue;
            }

            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            let slug = Slug::from_stem(stem);
            let title = fm.title.unwrap_or_else(|| {
                stem.chars()
                    .map(|c| if c == '-' || c == '_' { ' ' } else { c })
                    .collect()
            });
            let category = fm.category.unwrap_or_else(|| "uncategorized".to_string());

            let resolved = resolve_wiki_links(body, &slug_set, "/recipes/");

            // `tags: [secret]` marks the entire note as redacted.
            let is_whole_secret = fm.tags.iter().any(|t| t == "secret");
            let (html, html_gm, has_secrets) = if is_whole_secret {
                let player = RenderedHtml::from_placeholder(SECRET_PLACEHOLDER);
                let gm = render_markdown(&resolved);
                (player, gm, true)
            } else {
                let (player_html, has_secrets) = render_note_body_redacted(&resolved);
                let gm_html = render_note_body_revealed(&resolved);
                (player_html, gm_html, has_secrets)
            };

            let recipe = Recipe {
                slug: slug.clone(),
                title,
                category: category.clone(),
                servings: fm.servings,
                prep_time: fm.prep_time,
                cook_time: fm.cook_time,
                html,
                html_gm,
                has_secrets,
            };

            by_category
                .entry(category)
                .or_default()
                .push(recipe.clone());
            let _ = by_slug.insert(slug, recipe);
        }

        // Sort recipes within each category, then sort categories by name.
        let mut groups: Vec<CategoryGroup> = by_category
            .into_iter()
            .map(|(name, mut recipes)| {
                recipes.sort_by(|a, b| a.title.cmp(&b.title));
                let entries = recipes
                    .iter()
                    .map(|r| RecipeEntry {
                        slug: r.slug.clone(),
                        title: r.title.clone(),
                        has_secrets: r.has_secrets,
                    })
                    .collect();
                CategoryGroup { name, recipes: entries }
            })
            .collect();
        groups.sort_by(|a, b| a.name.cmp(&b.name));

        tracing::info!(count = by_slug.len(), "recipe vault loaded");

        Ok(RecipeStore { groups, by_slug })
    }

    /// Look up a recipe by its slug. Accepts `&str` directly via [`Borrow`].
    pub fn get(&self, slug: &str) -> Option<&Recipe>
    where
        Slug: Borrow<str>,
    {
        self.by_slug.get(slug)
    }
}

// ─── Templates ────────────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "recipes_index.html")]
pub struct RecipesIndexPage {
    pub version: &'static str,
    pub groups: Vec<CategoryGroup>,
    pub auth_user: Option<AuthUserInfo>,
    pub nav_links: Arc<[NavLink]>,
}

#[derive(Template)]
#[template(path = "recipes_detail.html")]
pub struct RecipesDetailPage {
    pub version: &'static str,
    pub title: String,
    pub category: String,
    pub servings: Option<String>,
    pub prep_time: Option<String>,
    pub cook_time: Option<String>,
    /// Pre-rendered HTML — safe for `|safe` in the template.
    pub content: String,
    pub auth_user: Option<AuthUserInfo>,
    pub nav_links: Arc<[NavLink]>,
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

/// GET /recipes — recipe index, grouped by category.
pub async fn recipes_index_route(
    MaybeAuthUser(auth_user): MaybeAuthUser,
    State(state): State<ServerState>,
) -> Result<Html<String>, Error> {
    let store: &Arc<RecipeStore> = state.recipes_store.as_ref().ok_or(Error::NotFound)?;

    let page = RecipesIndexPage {
        version: VERSION,
        groups: store.groups.clone(),
        auth_user,
        nav_links: state.nav_links.clone(),
    };
    Ok(Html(page.render()?))
}

/// GET /recipes/{slug} — individual recipe detail page.
pub async fn recipes_detail_route(
    MaybeAuthUser(auth_user): MaybeAuthUser,
    AxumPath(slug): AxumPath<String>,
    State(state): State<ServerState>,
) -> Result<Html<String>, Error> {
    let store: &Arc<RecipeStore> = state.recipes_store.as_ref().ok_or(Error::NotFound)?;
    let recipe = store.get(&slug).ok_or(Error::NotFound)?;
    let is_gm = auth_user.as_ref().map(|u| u.role == Role::Gm).unwrap_or(false);
    let content = if is_gm {
        recipe.html_gm.as_str().to_owned()
    } else {
        recipe.html.as_str().to_owned()
    };
    let page = RecipesDetailPage {
        version: VERSION,
        title: recipe.title.clone(),
        category: recipe.category.clone(),
        servings: recipe.servings.clone(),
        prep_time: recipe.prep_time.clone(),
        cook_time: recipe.cook_time.clone(),
        content,
        auth_user,
        nav_links: state.nav_links.clone(),
    };
    Ok(Html(page.render()?))
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::get,
    };
    use std::path::PathBuf;
    use tower::ServiceExt;

    fn fixture_vault() -> PathBuf {
        PathBuf::from("fixtures/vault")
    }

    // ── RecipeStore::scan ────────────────────────────────────────────────────

    #[test]
    fn recipe_store_scan_loads_recipe_notes() {
        let store = RecipeStore::scan(&fixture_vault()).expect("scan should succeed");
        // At least one recipe should be loaded (Chocolate Cake, Pasta Carbonara)
        assert!(!store.by_slug.is_empty(), "should have at least one recipe");
        let has_cake = store.by_slug.values().any(|r| r.title == "Chocolate Cake");
        assert!(has_cake, "Chocolate Cake fixture should be present");
    }

    #[test]
    fn recipe_store_excludes_non_recipe_notes() {
        let store = RecipeStore::scan(&fixture_vault()).expect("scan should succeed");
        // Notes tagged world/session (not recipe) should not appear as recipes
        let has_world_note = store
            .by_slug
            .values()
            .any(|r| r.title == "The Known World");
        assert!(!has_world_note, "world-tagged notes must not appear in recipe store");
    }

    #[test]
    fn recipe_entry_has_category() {
        let store = RecipeStore::scan(&fixture_vault()).expect("scan should succeed");
        let cake = store
            .by_slug
            .values()
            .find(|r| r.title == "Chocolate Cake")
            .expect("Chocolate Cake should be present");
        assert_eq!(cake.category, "dessert");
    }

    #[test]
    fn recipe_entry_has_servings_and_times() {
        let store = RecipeStore::scan(&fixture_vault()).expect("scan should succeed");
        let cake = store
            .by_slug
            .values()
            .find(|r| r.title == "Chocolate Cake")
            .expect("Chocolate Cake should be present");
        assert_eq!(cake.servings.as_deref(), Some("8"));
        assert_eq!(cake.prep_time.as_deref(), Some("20 min"));
        assert_eq!(cake.cook_time.as_deref(), Some("35 min"));
    }

    #[test]
    fn recipe_entry_defaults_category_to_uncategorized() {
        // Parse a note without a category field
        let raw = "---\ntitle: Bare Recipe\ntags: [recipe]\n---\nSome text.\n";
        let (fm, _body) = parse_recipe_frontmatter(raw);
        let category = fm.category.unwrap_or_else(|| "uncategorized".to_string());
        assert_eq!(category, "uncategorized");
    }

    #[test]
    fn recipe_store_groups_by_category() {
        let store = RecipeStore::scan(&fixture_vault()).expect("scan should succeed");
        let dessert_group = store.groups.iter().find(|g| g.name == "dessert");
        assert!(dessert_group.is_some(), "dessert category group should exist");
        let group = dessert_group.unwrap();
        assert!(
            group.recipes.iter().any(|r| r.title == "Chocolate Cake"),
            "Chocolate Cake should be in dessert group"
        );
    }

    #[test]
    fn recipe_store_groups_sorted_by_name() {
        let store = RecipeStore::scan(&fixture_vault()).expect("scan should succeed");
        let names: Vec<&str> = store.groups.iter().map(|g| g.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "category groups should be sorted alphabetically");
    }

    // ── parse_recipe_frontmatter ──────────────────────────────────────────────

    #[test]
    fn parse_recipe_fm_all_fields() {
        let raw = "---\ntitle: Test Recipe\ntags: [recipe]\ncategory: breakfast\nservings: \"2\"\nprep_time: \"5 min\"\ncook_time: \"10 min\"\n---\nBody.\n";
        let (fm, rest) = parse_recipe_frontmatter(raw);
        assert_eq!(fm.title.as_deref(), Some("Test Recipe"));
        assert!(fm.tags.contains(&"recipe".to_string()));
        assert_eq!(fm.category.as_deref(), Some("breakfast"));
        assert_eq!(fm.servings.as_deref(), Some("2"));
        assert_eq!(fm.prep_time.as_deref(), Some("5 min"));
        assert_eq!(fm.cook_time.as_deref(), Some("10 min"));
        assert_eq!(rest, "Body.\n");
    }

    #[test]
    fn parse_recipe_fm_missing_optional_fields() {
        let raw = "---\ntags: [recipe]\n---\nBody.\n";
        let (fm, _rest) = parse_recipe_frontmatter(raw);
        assert!(fm.title.is_none());
        assert!(fm.category.is_none());
        assert!(fm.servings.is_none());
        assert!(fm.prep_time.is_none());
        assert!(fm.cook_time.is_none());
    }

    // ── Handler integration tests ─────────────────────────────────────────────

    async fn make_state_without_recipes() -> ServerState {
        crate::tests::minimal_server_state().await
    }

    async fn make_state_with_recipes() -> ServerState {
        let mut state = crate::tests::minimal_server_state().await;
        let store = RecipeStore::scan(&fixture_vault()).expect("scan should succeed");
        state.recipes_store = Some(Arc::new(store));
        state
    }

    #[tokio::test]
    async fn recipes_index_returns_404_without_vault() {
        let state = make_state_without_recipes().await;
        let app = axum::Router::new()
            .route("/recipes", get(recipes_index_route))
            .with_state(state);
        let req = Request::builder()
            .uri("/recipes")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn recipes_index_returns_200_with_vault() {
        let state = make_state_with_recipes().await;
        let app = axum::Router::new()
            .route("/recipes", get(recipes_index_route))
            .with_state(state);
        let req = Request::builder()
            .uri("/recipes")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn recipes_index_contains_category_name() {
        let state = make_state_with_recipes().await;
        let app = axum::Router::new()
            .route("/recipes", get(recipes_index_route))
            .with_state(state);
        let req = Request::builder()
            .uri("/recipes")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("dessert"), "response should include category name");
    }

    #[tokio::test]
    async fn recipes_detail_returns_404_without_vault() {
        let state = make_state_without_recipes().await;
        let app = axum::Router::new()
            .route("/recipes/{slug}", get(recipes_detail_route))
            .with_state(state);
        let req = Request::builder()
            .uri("/recipes/chocolate-cake")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn recipes_detail_returns_404_for_missing_slug() {
        let state = make_state_with_recipes().await;
        let app = axum::Router::new()
            .route("/recipes/{slug}", get(recipes_detail_route))
            .with_state(state);
        let req = Request::builder()
            .uri("/recipes/nonexistent-recipe")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn recipes_detail_returns_200_for_existing_recipe() {
        let state = make_state_with_recipes().await;
        let app = axum::Router::new()
            .route("/recipes/{slug}", get(recipes_detail_route))
            .with_state(state);
        let req = Request::builder()
            .uri("/recipes/chocolate-cake")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
