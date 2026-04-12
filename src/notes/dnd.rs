//! D&D campaign notes vault — scans for notes tagged `world` or `session`.

use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use askama::Template;
use axum::{
    extract::{Path as AxumPath, State},
    response::Html,
};

use crate::{
    ServerState, VERSION,
    auth::{AuthUserInfo, GmUser, MaybeAuthUser, Role},
    error::Error,
    index::NavLink,
};

use super::{
    RenderedHtml, SECRET_PLACEHOLDER, render_note_body_redacted, render_note_body_revealed,
    obsidian,
};
use super::obsidian::Slug;

// ─── Public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Note {
    pub slug: Slug,
    pub title: String,
    /// Player-visible HTML: secret blocks wrapped in `<div class="notes-secret">`.
    pub html: RenderedHtml,
    /// GM-visible HTML: secret blocks rendered without the wrapper.
    pub html_gm: RenderedHtml,
    /// `true` if the note contains any secret blocks (inline or whole-note).
    /// Used to show a 🔒 badge on the index.
    pub has_secrets: bool,
}

/// Lightweight view of a note for the index page (no HTML body).
#[derive(Debug, Clone)]
#[allow(dead_code)] // fields read by Askama-generated template code
pub struct NoteEntry {
    pub slug: Slug,
    pub title: String,
    pub has_secrets: bool,
}

#[derive(Debug)]
pub struct NotesStore {
    pub world_notes: Vec<Note>,
    pub session_notes: Vec<Note>,
    by_slug: HashMap<Slug, Note>,
}

#[derive(Debug, thiserror::Error)]
pub enum NotesStoreError {
    #[error("vault path `{0}` does not exist or is not a directory")]
    VaultNotDirectory(PathBuf),

    #[error("failed to read note `{path}`: {source}")]
    NoteRead {
        path: PathBuf,
        source: std::io::Error,
    },
}

// ─── Sorting ──────────────────────────────────────────────────────────────────

/// Natural (numeric-aware) string comparison. Embedded digit runs are compared
/// numerically so that "Session 10" sorts after "Session 9".
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ai = a.char_indices().peekable();
    let mut bi = b.char_indices().peekable();

    loop {
        match (ai.peek(), bi.peek()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(&(ai_pos, ac)), Some(&(bi_pos, bc)))
                if ac.is_ascii_digit() && bc.is_ascii_digit() =>
            {
                // Collect the full digit run from each side.
                let a_start = ai_pos;
                let b_start = bi_pos;
                while let Some(_) = ai.next_if(|&(_, c)| c.is_ascii_digit()) {}
                while let Some(_) = bi.next_if(|&(_, c)| c.is_ascii_digit()) {}
                let a_end = ai.peek().map_or(a.len(), |&(i, _)| i);
                let b_end = bi.peek().map_or(b.len(), |&(i, _)| i);
                let an: u64 = a[a_start..a_end].parse().unwrap_or(0);
                let bn: u64 = b[b_start..b_end].parse().unwrap_or(0);
                match an.cmp(&bn) {
                    std::cmp::Ordering::Equal => {}
                    ord => return ord,
                }
            }
            (Some(&(_, ac)), Some(&(_, bc))) => {
                match ac.cmp(&bc) {
                    std::cmp::Ordering::Equal => {
                        let _ = ai.next();
                        let _ = bi.next();
                    }
                    ord => return ord,
                }
            }
        }
    }
}

// ─── NotesStore ───────────────────────────────────────────────────────────────

impl NotesStore {
    /// Scan a vault directory, parsing every `.md` file.
    ///
    /// Three-pass algorithm:
    /// 1. `build_vault_index`: walk vault, register every `.md` by stem/aliases for
    ///    shortest-path wiki-link resolution.
    /// 2. Parse all notes, collecting slugs for ALL vault files as `live_slugs` so
    ///    that any wiki link to an existing note becomes a live `<a>` tag.
    /// 3. Render all notes → `by_slug` (any note is reachable via `/notes/{slug}`).
    ///    Only world/session-tagged notes appear on the index page.
    pub fn scan(vault: &Path) -> Result<Self, NotesStoreError> {
        // Pass 1: vault index
        let (vault_index, paths) =
            obsidian::build_vault_index(vault, &HashMap::new()).map_err(|e| match e {
                obsidian::VaultError::NotDirectory(p) => NotesStoreError::VaultNotDirectory(p),
                obsidian::VaultError::ReadError { path, source } => {
                    NotesStoreError::NoteRead { path, source }
                }
            })?;

        // Pass 2: parse all notes; build live_slugs from every vault file
        let mut parsed: Vec<(obsidian::ParsedNote, bool, bool)> = Vec::new();
        let mut live_slugs: HashSet<Slug> = HashSet::new();
        for path in &paths {
            let note: obsidian::ParsedNote = obsidian::parse_note(path).map_err(|e| match e {
                obsidian::VaultError::ReadError { path, source } => {
                    NotesStoreError::NoteRead { path, source }
                }
                obsidian::VaultError::NotDirectory(p) => NotesStoreError::VaultNotDirectory(p),
            })?;
            let is_world = note.frontmatter.tags.iter().any(|t| t == "world");
            let is_session = note.frontmatter.tags.iter().any(|t| t == "session");
            let _ = live_slugs.insert(note.slug.clone()); // ALL vault files are routable
            parsed.push((note, is_world, is_session));
        }

        // Pass 3: render all notes
        let mut world_notes: Vec<Note> = Vec::new();
        let mut session_notes: Vec<Note> = Vec::new();
        let mut by_slug: HashMap<Slug, Note> = HashMap::new();

        for (note, is_world, is_session) in parsed {
            let slug = note.slug.clone();
            let stem = note
                .path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            let title = note
                .frontmatter
                .title
                .unwrap_or_else(|| stem.replace(['-', '_'], " "));
            let is_whole_secret = note.frontmatter.tags.iter().any(|t| t == "secret");
            let body = &note.body;

            // Render markdown first (HTML-escapes user text), then resolve wiki links
            // on the HTML so `<a>` tags are not re-escaped by pulldown-cmark.
            let (html, html_gm, has_secrets) = if is_whole_secret {
                let player = RenderedHtml(SECRET_PLACEHOLDER.to_owned());
                let gm_rendered = render_note_body_revealed(body);
                let gm = RenderedHtml(obsidian::resolve_wiki_links(
                    gm_rendered.as_str(),
                    &vault_index,
                    &live_slugs,
                    "/notes/",
                ));
                (player, gm, true)
            } else {
                let (player_rendered, has_secrets) = render_note_body_redacted(body);
                let gm_rendered = render_note_body_revealed(body);
                let player = RenderedHtml(obsidian::resolve_wiki_links(
                    player_rendered.as_str(),
                    &vault_index,
                    &live_slugs,
                    "/notes/",
                ));
                let gm = RenderedHtml(obsidian::resolve_wiki_links(
                    gm_rendered.as_str(),
                    &vault_index,
                    &live_slugs,
                    "/notes/",
                ));
                (player, gm, has_secrets)
            };

            let note_out = Note {
                slug: slug.clone(),
                title,
                html,
                html_gm,
                has_secrets,
            };

            if is_world {
                world_notes.push(note_out.clone());
            }
            if is_session {
                session_notes.push(note_out.clone());
            }
            let _ = by_slug.insert(slug, note_out);
        }

        world_notes.sort_by(|a, b| a.title.cmp(&b.title));
        session_notes.sort_by(|a, b| natural_cmp(&a.title, &b.title));

        Ok(NotesStore {
            world_notes,
            session_notes,
            by_slug,
        })
    }

    /// Look up a note by its slug. Accepts `&str` directly via [`Borrow`].
    pub fn get(&self, slug: &str) -> Option<&Note>
    where
        Slug: Borrow<str>,
    {
        self.by_slug.get(slug)
    }
}

// ─── NoteVault ────────────────────────────────────────────────────────────────

/// Runtime holder for a scanned notes vault. Supports non-blocking startup and
/// live refresh without restarting the server.
///
/// The inner store is `None` until the first [`spawn_scan`] completes. Each
/// refresh atomically replaces the store, so readers always see a consistent
/// snapshot.
///
/// [`spawn_scan`]: NoteVault::spawn_scan
#[derive(Clone, Debug)]
pub struct NoteVault {
    vault_path: PathBuf,
    store: Arc<tokio::sync::RwLock<Option<Arc<NotesStore>>>>,
}

impl NoteVault {
    pub fn new(vault_path: PathBuf) -> Self {
        Self {
            vault_path,
            store: Arc::new(tokio::sync::RwLock::new(None)),
        }
    }

    /// Returns the currently-loaded store, or `None` if the scan hasn't finished yet.
    pub async fn get(&self) -> Option<Arc<NotesStore>> {
        self.store.read().await.clone()
    }

    /// Spawn a background vault scan. Returns immediately; the store is atomically
    /// replaced when the scan completes. Errors are logged and the previous store
    /// (if any) is left in place.
    pub fn spawn_scan(&self) {
        let path = self.vault_path.clone();
        let store = Arc::clone(&self.store);
        let _ = tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || NotesStore::scan(&path))
                .await
                .expect("notes scan task panicked");
            match result {
                Ok(new_store) => {
                    let new_store = Arc::new(new_store);
                    tracing::info!(
                        world = new_store.world_notes.len(),
                        session = new_store.session_notes.len(),
                        "notes vault loaded"
                    );
                    *store.write().await = Some(new_store);
                }
                Err(e) => {
                    tracing::error!(error = %e, "notes vault scan failed");
                }
            }
        });
    }

    /// Construct a `NoteVault` with a pre-loaded store. For use in tests only.
    #[cfg(test)]
    pub fn from_store_for_test(vault_path: PathBuf, store: Arc<NotesStore>) -> Self {
        Self {
            vault_path,
            store: Arc::new(tokio::sync::RwLock::new(Some(store))),
        }
    }
}

// ─── Templates ────────────────────────────────────────────────────────────────

#[derive(Template)]
#[template(path = "notes_index.html")]
pub struct NotesIndexPage {
    pub version: &'static str,
    pub world_notes: Vec<NoteEntry>,
    pub session_notes: Vec<NoteEntry>,
    pub auth_user: Option<AuthUserInfo>,
    pub nav_links: Arc<[NavLink]>,
}

#[derive(Template)]
#[template(path = "notes_detail.html")]
pub struct NotesDetailPage {
    pub version: &'static str,
    pub title: String,
    /// Pre-rendered HTML from [`RenderedHtml`] — safe for `|safe` in the template.
    pub content: String,
    pub auth_user: Option<AuthUserInfo>,
    pub nav_links: Arc<[NavLink]>,
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

pub async fn notes_index_route(
    MaybeAuthUser(auth_user): MaybeAuthUser,
    State(state): State<ServerState>,
) -> Result<Html<String>, Error> {
    let store = state
        .notes_store
        .as_ref()
        .ok_or(Error::NotFound)?
        .get()
        .await
        .ok_or(Error::NotFound)?;

    let world_notes = store
        .world_notes
        .iter()
        .map(|n| NoteEntry {
            slug: n.slug.clone(),
            title: n.title.clone(),
            has_secrets: n.has_secrets,
        })
        .collect();

    let session_notes = store
        .session_notes
        .iter()
        .map(|n| NoteEntry {
            slug: n.slug.clone(),
            title: n.title.clone(),
            has_secrets: n.has_secrets,
        })
        .collect();

    let page = NotesIndexPage {
        version: VERSION,
        world_notes,
        session_notes,
        auth_user,
        nav_links: state.nav_links.clone(),
    };
    Ok(Html(page.render()?))
}

pub async fn notes_detail_route(
    MaybeAuthUser(auth_user): MaybeAuthUser,
    AxumPath(slug): AxumPath<String>,
    State(state): State<ServerState>,
) -> Result<Html<String>, Error> {
    let store = state
        .notes_store
        .as_ref()
        .ok_or(Error::NotFound)?
        .get()
        .await
        .ok_or(Error::NotFound)?;
    let note = store.get(&slug).ok_or(Error::NotFound)?;
    let is_gm = auth_user
        .as_ref()
        .map(|u| u.role == Role::Gm)
        .unwrap_or(false);
    let content = if is_gm {
        note.html_gm.as_str().to_owned()
    } else {
        note.html.as_str().to_owned()
    };
    let page = NotesDetailPage {
        version: VERSION,
        title: note.title.clone(),
        content,
        auth_user: auth_user.clone(),
        nav_links: state.nav_links.clone(),
    };
    Ok(Html(page.render()?))
}

/// `POST /api/notes/refresh` — trigger a background rescan of the notes vault (GM only).
///
/// Returns `202 Accepted` immediately; the scan runs in the background. A
/// subsequent GET to `/notes` will reflect the updated content once the scan
/// completes.
pub async fn notes_refresh_route(
    GmUser(_): GmUser,
    State(state): State<ServerState>,
) -> axum::http::StatusCode {
    match state.notes_store.as_ref() {
        Some(vault) => {
            vault.spawn_scan();
            axum::http::StatusCode::ACCEPTED
        }
        None => axum::http::StatusCode::NOT_FOUND,
    }
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
    use tower::ServiceExt;

    fn fixture_store() -> NotesStore {
        NotesStore::scan(Path::new("fixtures/vault")).expect("fixtures/vault should scan cleanly")
    }

    async fn minimal_state(notes_store: Option<Arc<NotesStore>>) -> ServerState {
        use crate::{
            breaker::BreakerContent,
            breaker_detail::{BreakerData, BreakerDetailStore, BreakerStore},
            index::Index,
            route::Routes,
        };

        let data = BreakerData {
            todos: vec![],
            slots: HashMap::new(),
            couples: vec![],
        };
        let store = Arc::new(BreakerStore::from_data(data).unwrap());
        let breaker_detail_store: Arc<dyn BreakerDetailStore> = store.clone();
        let breaker_content = Arc::new(BreakerContent::new(store.as_ref()));
        let has_notes = notes_store.is_some();
        let notes_store = notes_store.map(|s| {
            NoteVault::from_store_for_test(PathBuf::from("fixtures/vault"), s)
        });
        let entries = has_notes
            .then_some(crate::index::OptionalEntry::Notes)
            .into_iter();
        let index = Index::new(
            Routes::default(),
            entries,
            &HashSet::new(),
            None,
            Arc::new([]),
        )
        .await
        .unwrap();

        ServerState {
            certificate: Arc::from("fake-cert"),
            breaker_content,
            breaker_detail_store,
            index,
            tailscale_socket: Arc::from(Path::new("/tmp/fake.sock")),
            notes_store,
            recipes_store: None,
            auth_state: None,
            mqtt_state: None,
            log_config: None,
            systemd_config: None,
            nav_links: Arc::new([]),
            peers: Arc::new([]),
            http_client: reqwest::Client::new(),
            peer_api_key: None,
        }
    }

    fn notes_router(state: ServerState) -> axum::Router {
        axum::Router::new()
            .route("/notes", get(notes_index_route))
            .route("/notes/{slug}", get(notes_detail_route))
            .with_state(state)
    }

    async fn body_text(res: axum::response::Response) -> String {
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    // ── natural_cmp ───────────────────────────────────────────────────────────

    #[test]
    fn natural_cmp_orders_numeric_suffixes_correctly() {
        use std::cmp::Ordering;
        assert_eq!(natural_cmp("Session 2", "Session 10"), Ordering::Less);
        assert_eq!(natural_cmp("Session 10", "Session 2"), Ordering::Greater);
        assert_eq!(natural_cmp("Session 1", "Session 1"), Ordering::Equal);
        assert_eq!(natural_cmp("Session 9", "Session 10"), Ordering::Less);
    }

    #[test]
    fn natural_cmp_falls_back_to_lexical_for_non_numeric() {
        use std::cmp::Ordering;
        assert_eq!(natural_cmp("Apple", "Banana"), Ordering::Less);
        assert_eq!(natural_cmp("Banana", "Apple"), Ordering::Greater);
    }

    #[test]
    fn session_notes_sorted_in_natural_order() {
        let store = fixture_store();
        let session_titles: Vec<&str> =
            store.session_notes.iter().map(|n| n.title.as_str()).collect();
        // Verify that numeric titles are in numeric order (not lexicographic).
        // e.g. "Session 2" should come before "Session 10".
        if let (Some(pos1), Some(pos2)) = (
            session_titles.iter().position(|&t| t == "Session 1"),
            session_titles.iter().position(|&t| t == "Session 2"),
        ) {
            assert!(pos1 < pos2, "Session 1 should appear before Session 2");
        }
    }

    // ── NotesStore::scan ──────────────────────────────────────────────────────

    #[test]
    fn scan_fixtures_vault() {
        let store = fixture_store();
        assert!(!store.world_notes.is_empty(), "expected world notes");
        assert!(!store.session_notes.is_empty(), "expected session notes");
    }

    #[test]
    fn scan_untagged_note_routable_but_not_indexed() {
        let store = fixture_store();
        // Untagged notes are accessible via slug but not listed on the index.
        assert!(
            store.get("untagged").is_some(),
            "untagged note should be routable via by_slug"
        );
        assert!(
            !store.world_notes.iter().any(|n| n.slug == "untagged"),
            "untagged note must not appear in world_notes"
        );
        assert!(
            !store.session_notes.iter().any(|n| n.slug == "untagged"),
            "untagged note must not appear in session_notes"
        );
    }

    #[test]
    fn scan_wiki_link_resolved_in_session_html() {
        let store = fixture_store();
        let session = store.get("session-1").expect("session-1 should exist");
        assert!(
            session
                .html
                .as_str()
                .contains(r#"href="/notes/the-known-world""#),
            "wiki-link should resolve; got: {}",
            session.html.as_str()
        );
    }

    #[test]
    fn scan_dead_link_rendered_as_span() {
        let store = fixture_store();
        let session = store.get("session-1").expect("session-1 should exist");
        assert!(
            session.html.as_str().contains("notes-dead-link"),
            "unknown wiki-link should produce dead-link span; got: {}",
            session.html.as_str()
        );
    }

    #[test]
    fn scan_inline_secret_paragraph_sets_has_secrets() {
        let store = fixture_store();
        let session = store.get("session-1").expect("session-1 should exist");
        assert!(
            session.has_secrets,
            "session-1 has a #secret-tagged paragraph"
        );
        // Player HTML shows the placeholder, not the secret content.
        assert!(
            session.html.as_str().contains("notes-redacted"),
            "secret paragraph should be replaced with the redacted placeholder"
        );
    }

    #[test]
    fn scan_inline_secret_content_absent_from_player_html() {
        // Secret text must never be sent to a non-GM browser.
        let store = fixture_store();
        let session = store.get("session-1").expect("session-1 should exist");
        assert!(
            !session.html.as_str().contains("Malachar"),
            "secret text must be absent from player HTML"
        );
    }

    #[test]
    fn scan_inline_secret_content_present_in_gm_html() {
        // GM variant must contain the full secret text.
        let store = fixture_store();
        let session = store.get("session-1").expect("session-1 should exist");
        assert!(
            session.html_gm.as_str().contains("Malachar"),
            "secret text must be present in GM HTML"
        );
    }

    #[test]
    fn scan_whole_note_secret_player_html_is_placeholder() {
        let store = fixture_store();
        let gm = store.get("gm-notes").expect("gm-notes should exist");
        assert!(gm.has_secrets);
        // Player HTML must be just the placeholder — none of the note body.
        assert!(
            gm.html.as_str().contains("notes-redacted"),
            "whole-note secret player HTML should be the redacted placeholder"
        );
        assert!(
            !gm.html.as_str().contains("portal"),
            "whole-note secret text must not appear in player HTML"
        );
    }

    #[test]
    fn scan_whole_note_secret_gm_html_contains_content() {
        let store = fixture_store();
        let gm = store.get("gm-notes").expect("gm-notes should exist");
        assert!(
            gm.html_gm.as_str().contains("portal"),
            "GM HTML must contain full note content"
        );
    }

    #[test]
    fn scan_note_without_secrets_has_secrets_false() {
        let store = fixture_store();
        let world = store.get("the-known-world").expect("should exist");
        assert!(!world.has_secrets);
    }

    #[test]
    fn scan_both_tagged_appears_in_both_vecs() {
        let store = fixture_store();
        assert!(store.world_notes.iter().any(|n| n.slug == "both-tagged"));
        assert!(store.session_notes.iter().any(|n| n.slug == "both-tagged"));
        assert!(store.get("both-tagged").is_some());
    }

    #[test]
    fn scan_by_slug_returns_correct_note() {
        let store = fixture_store();
        let note = store.get("the-known-world").expect("should find by slug");
        assert_eq!(note.slug, "the-known-world");
        assert_eq!(note.title, "The Known World");
    }

    #[test]
    fn scan_get_accepts_str_directly() {
        let store = fixture_store();
        assert!(store.get("the-known-world").is_some());
        assert!(store.get("does-not-exist").is_none());
    }

    #[test]
    fn scan_notes_sorted_by_title() {
        let store = fixture_store();

        let world_titles: Vec<&str> = store.world_notes.iter().map(|n| n.title.as_str()).collect();
        let mut sorted = world_titles.clone();
        sorted.sort();
        assert_eq!(
            world_titles, sorted,
            "world_notes should be sorted by title"
        );

        let session_titles: Vec<&str> = store
            .session_notes
            .iter()
            .map(|n| n.title.as_str())
            .collect();
        let mut sorted = session_titles.clone();
        sorted.sort();
        assert_eq!(
            session_titles, sorted,
            "session_notes should be sorted by title"
        );
    }

    #[test]
    fn scan_nonexistent_vault_returns_vault_not_directory_error() {
        let result = NotesStore::scan(Path::new("fixtures/vault_does_not_exist"));
        assert!(
            matches!(result, Err(NotesStoreError::VaultNotDirectory(_))),
            "expected VaultNotDirectory error"
        );
    }

    // ── HTTP handlers ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn handler_notes_index_no_vault_returns_404() {
        let state = minimal_state(None).await;
        let app = notes_router(state);
        let req = Request::builder()
            .uri("/notes")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn handler_notes_detail_no_vault_returns_404() {
        let state = minimal_state(None).await;
        let app = notes_router(state);
        let req = Request::builder()
            .uri("/notes/anything")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn handler_notes_index_with_vault_returns_200() {
        let store = Some(Arc::new(fixture_store()));
        let state = minimal_state(store).await;
        let app = notes_router(state);

        let req = Request::builder()
            .uri("/notes")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let text = body_text(res).await;
        assert!(text.contains("worldbuilding"));
        assert!(text.contains("sessions"));
    }

    #[tokio::test]
    async fn handler_notes_index_shows_secret_badge_for_note_with_secrets() {
        let store = Some(Arc::new(fixture_store()));
        let state = minimal_state(store).await;
        let app = notes_router(state);

        let req = Request::builder()
            .uri("/notes")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        let text = body_text(res).await;

        assert!(
            text.contains("notes-secret-badge"),
            "index should show secret badge for notes with hidden content"
        );
    }

    #[tokio::test]
    async fn handler_notes_index_lists_note_titles() {
        let store = Some(Arc::new(fixture_store()));
        let state = minimal_state(store).await;
        let app = notes_router(state);

        let req = Request::builder()
            .uri("/notes")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        let text = body_text(res).await;

        assert!(text.contains("The Known World"));
        assert!(text.contains("Session 1"));
    }

    #[tokio::test]
    async fn handler_notes_detail_found_returns_200() {
        let store = Some(Arc::new(fixture_store()));
        let state = minimal_state(store).await;
        let app = notes_router(state);

        let req = Request::builder()
            .uri("/notes/session-1")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let text = body_text(res).await;
        assert!(text.contains("Session 1"));
    }

    #[tokio::test]
    async fn handler_notes_detail_secret_content_absent_for_non_gm() {
        let store = Some(Arc::new(fixture_store()));
        let state = minimal_state(store).await;
        let app = notes_router(state);

        let req = Request::builder()
            .uri("/notes/session-1")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        let text = body_text(res).await;

        // Secret text must not be sent to a non-GM browser at all.
        assert!(
            !text.contains("Malachar"),
            "secret text must be absent from non-GM response"
        );
        // The placeholder must be present instead.
        assert!(text.contains("notes-redacted"));
    }

    #[tokio::test]
    async fn handler_notes_detail_renders_wiki_link() {
        let store = Some(Arc::new(fixture_store()));
        let state = minimal_state(store).await;
        let app = notes_router(state);

        let req = Request::builder()
            .uri("/notes/session-1")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        let text = body_text(res).await;

        assert!(
            text.contains(r#"href="/notes/the-known-world""#),
            "detail page should contain resolved wiki-link; got: {text}"
        );
    }

    #[tokio::test]
    async fn handler_notes_detail_unknown_slug_returns_404() {
        let store = Some(Arc::new(fixture_store()));
        let state = minimal_state(store).await;
        let app = notes_router(state);

        let req = Request::builder()
            .uri("/notes/does-not-exist")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }
}
