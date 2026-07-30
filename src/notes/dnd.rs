//! D&D campaign notes vault — scans for notes tagged `world` or `session`.

use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use askama::Template;
use axum::{
    extract::{FromRef, FromRequestParts, Path as AxumPath, Query, State},
    http::request::Parts,
    response::Html,
};
use serde::Deserialize;
use tokio::task::JoinHandle;

use crate::{
    ServerState, VERSION,
    auth::{AdminUser, AuthUserInfo, MaybeAuthUser, Role},
    error::Error,
    index::NavLink,
};

use super::obsidian::Slug;
use super::{
    RenderedHtml, SECRET_PLACEHOLDER, obsidian, render_note_body_redacted,
    render_note_body_revealed,
};

#[derive(Debug, Clone)]
pub struct Note {
    pub slug: Slug,
    pub title: String,
    /// Player-visible HTML: secret blocks wrapped in `<div class="notes-secret">`.
    pub html: RenderedHtml,
    /// Admin-visible HTML: secret blocks rendered without the wrapper.
    pub html_admin: RenderedHtml,
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
                while ai.next_if(|&(_, c)| c.is_ascii_digit()).is_some() {}
                while bi.next_if(|&(_, c)| c.is_ascii_digit()).is_some() {}
                let a_end = ai.peek().map_or(a.len(), |&(i, _)| i);
                let b_end = bi.peek().map_or(b.len(), |&(i, _)| i);
                let an: u64 = a[a_start..a_end].parse().unwrap_or(0);
                let bn: u64 = b[b_start..b_end].parse().unwrap_or(0);
                match an.cmp(&bn) {
                    std::cmp::Ordering::Equal => {}
                    ord => return ord,
                }
            }
            (Some(&(_, ac)), Some(&(_, bc))) => match ac.cmp(&bc) {
                std::cmp::Ordering::Equal => {
                    let _ = ai.next();
                    let _ = bi.next();
                }
                ord => return ord,
            },
        }
    }
}

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
            let (html, html_admin, has_secrets) = if is_whole_secret {
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
                html_admin,
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

/// Shared, atomically-swappable snapshot of a scanned vault. Readers always
/// see either `None` (no successful scan yet) or the result of the most
/// recently *completed* scan — a failed or superseded scan never clobbers a
/// good snapshot.
#[derive(Clone, Debug, Default)]
struct StoreCell(Arc<tokio::sync::RwLock<Option<Arc<NotesStore>>>>);

impl StoreCell {
    #[cfg_attr(not(test), allow(dead_code))]
    fn preloaded(store: Arc<NotesStore>) -> Self {
        Self(Arc::new(tokio::sync::RwLock::new(Some(store))))
    }

    async fn get(&self) -> Option<Arc<NotesStore>> {
        self.0.read().await.clone()
    }

    async fn replace(&self, store: Arc<NotesStore>) {
        *self.0.write().await = Some(store);
    }
}

/// State for the single in-flight scan task (if any), plus whether a
/// follow-up scan has been requested while it runs.
///
/// `generation` is a fencing token. Each spawned task captures the
/// generation it was started under, and `on_complete` only retires the slot
/// for a matching generation. Without this, an old task's completion could
/// run after [`NoteVault::force_rescan`] already replaced it, and clobber
/// the newer task's `handle` back to `None` — making the vault look idle
/// while a scan is still actually running, so a subsequent request would
/// spawn a second, concurrent scan.
///
/// These transitions are plain, synchronous, lock-free methods so they can
/// be unit-tested directly against synthetic state, independent of real
/// scanning or task scheduling.
#[derive(Debug, Default)]
struct ScanSlot {
    generation: u64,
    handle: Option<JoinHandle<()>>,
    pending: bool,
}

impl ScanSlot {
    /// A scan was requested. `Some(generation)` means nothing was running —
    /// spawn a new task under that generation. `None` means a scan is
    /// already running and this request coalesced into its follow-up.
    fn on_request(&mut self) -> Option<u64> {
        if self.handle.is_some() {
            self.pending = true;
            None
        } else {
            self.generation += 1;
            Some(self.generation)
        }
    }

    /// A fresh scan was forced. Always spawns, dropping any queued
    /// follow-up. Returns the previous handle (if any) for the caller to
    /// `.abort()`, and the generation to spawn the replacement under.
    fn on_force(&mut self) -> (Option<JoinHandle<()>>, u64) {
        let previous = self.handle.take();
        self.pending = false;
        self.generation += 1;
        (previous, self.generation)
    }

    /// Generation `completed` finished. `Some(generation)` means the caller
    /// should spawn a queued follow-up under that generation. `None` means
    /// either this generation was superseded (do nothing — the newer
    /// generation owns the slot) or it's current with no follow-up (the
    /// slot is cleared to idle here).
    fn on_complete(&mut self, completed: u64) -> Option<u64> {
        if self.generation != completed {
            return None;
        }
        if self.pending {
            self.pending = false;
            self.generation += 1;
            Some(self.generation)
        } else {
            self.handle = None;
            None
        }
    }
}

/// Shared handle to a vault's [`ScanSlot`].
#[derive(Clone, Debug, Default)]
struct ScanCell(Arc<std::sync::Mutex<ScanSlot>>);

impl ScanCell {
    fn lock(&self) -> std::sync::MutexGuard<'_, ScanSlot> {
        self.0.lock().expect("scan state poisoned")
    }
}

/// Runtime holder for a scanned notes vault. Supports non-blocking startup and
/// live refresh without restarting the server.
///
/// The inner store is `None` until the first scan completes. Each scan
/// atomically replaces the store, so readers always see a consistent
/// snapshot. At most one scan runs at a time — see [`Self::request_scan`]
/// and [`Self::force_rescan`].
#[derive(Clone, Debug)]
pub struct NoteVault {
    vault_path: PathBuf,
    store: StoreCell,
    scan: ScanCell,
}

impl NoteVault {
    pub fn new(vault_path: PathBuf) -> Self {
        Self {
            vault_path,
            store: StoreCell::default(),
            scan: ScanCell::default(),
        }
    }

    /// Returns the currently-loaded store, or `None` if the scan hasn't finished yet.
    pub async fn get(&self) -> Option<Arc<NotesStore>> {
        self.store.get().await
    }

    /// Request a background vault scan. Returns immediately. If a scan is
    /// already running, this queues exactly one follow-up rather than
    /// starting a second, concurrent scan.
    pub fn request_scan(&self) {
        let mut slot = self.scan.lock();
        if let Some(generation) = slot.on_request() {
            slot.handle = Some(self.spawn_scan(generation));
        }
    }

    /// Force a fresh background scan right now, aborting any scan already in
    /// flight (and any queued follow-up, which this restart supersedes).
    pub fn force_rescan(&self) {
        let mut slot = self.scan.lock();
        let (previous, generation) = slot.on_force();
        if let Some(previous) = previous {
            previous.abort();
        }
        slot.handle = Some(self.spawn_scan(generation));
    }

    /// Retire `generation`'s slot: spawn a queued follow-up, or go idle.
    /// Called from within the spawned scan task itself once it finishes.
    fn on_scan_complete(&self, generation: u64) {
        let mut slot = self.scan.lock();
        if let Some(next_generation) = slot.on_complete(generation) {
            slot.handle = Some(self.spawn_scan(next_generation));
        }
    }

    /// Spawn the scan task for `generation`. Callers are responsible for
    /// recording the returned handle in the slot themselves, atomically
    /// with whatever [`ScanSlot`] transition produced `generation` — this
    /// function only spawns, it never touches `self.scan` directly (its own
    /// completion callback runs later, in the spawned task).
    fn spawn_scan(&self, generation: u64) -> JoinHandle<()> {
        let path = self.vault_path.clone();
        let store = self.store.clone();
        let vault = self.clone();

        tokio::spawn(async move {
            match tokio::task::spawn_blocking(move || NotesStore::scan(&path)).await {
                Ok(Ok(new_store)) => {
                    let new_store = Arc::new(new_store);
                    tracing::info!(
                        world = new_store.world_notes.len(),
                        session = new_store.session_notes.len(),
                        "notes vault loaded"
                    );
                    store.replace(new_store).await;
                }
                Ok(Err(e)) => {
                    tracing::error!(error = %e, "notes vault scan failed");
                }
                Err(join_err) => {
                    tracing::error!(error = %join_err, "notes scan task panicked");
                }
            }
            vault.on_scan_complete(generation);
        })
    }

    /// Construct a `NoteVault` with a pre-loaded store. For use in tests only.
    #[cfg(test)]
    pub fn from_store_for_test(vault_path: PathBuf, store: Arc<NotesStore>) -> Self {
        Self {
            vault_path,
            store: StoreCell::preloaded(store),
            scan: ScanCell::default(),
        }
    }

    /// Test-only: wait for the scan slot to settle (no scan running,
    /// including any queued follow-up), so assertions can run afterward.
    /// Not safe to call concurrently with another `request_scan`/
    /// `force_rescan` on the same vault — it briefly takes `handle` out of
    /// the slot itself so it can own and await it, which would look like
    /// "idle" to another caller racing against it.
    #[cfg(test)]
    async fn wait_scan_idle(&self) {
        loop {
            let handle = self.scan.lock().handle.take();
            match handle {
                Some(handle) => {
                    let _ = handle.await;
                }
                None => break,
            }
        }
    }
}

/// Resolve just the notes vault out of `ServerState`, so notes routes don't
/// need to depend on the rest of the app's state.
impl FromRef<ServerState> for Option<NoteVault> {
    fn from_ref(state: &ServerState) -> Self {
        state.notes_store.clone()
    }
}

/// Resolves to the configured notes vault, or rejects with
/// [`Error::NotesNotConfigured`] — so notes routes don't each have to repeat
/// the "is it configured?" check themselves. Note that this only checks
/// whether a vault is configured at all; whether its background scan has
/// finished is a separate, per-request check via `NoteVault::get`.
pub struct Notes(pub NoteVault);

impl<S> FromRequestParts<S> for Notes
where
    S: Send + Sync,
    Option<NoteVault>: FromRef<S>,
{
    type Rejection = Error;

    async fn from_request_parts(_parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Option::<NoteVault>::from_ref(state)
            .map(Notes)
            .ok_or(Error::NotesNotConfigured)
    }
}

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

pub async fn notes_index_route(
    MaybeAuthUser(auth_user): MaybeAuthUser,
    Notes(vault): Notes,
    State(nav_links): State<Arc<[NavLink]>>,
) -> Result<Html<String>, Error> {
    let store = vault.get().await.ok_or(Error::NotesVaultLoading)?;

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
        nav_links,
    };
    Ok(Html(page.render()?))
}

pub async fn notes_detail_route(
    MaybeAuthUser(auth_user): MaybeAuthUser,
    AxumPath(slug): AxumPath<String>,
    Notes(vault): Notes,
    State(nav_links): State<Arc<[NavLink]>>,
) -> Result<Html<String>, Error> {
    let store = vault.get().await.ok_or(Error::NotesVaultLoading)?;
    let note = store.get(&slug).ok_or(Error::NotFound)?;
    let is_admin = auth_user
        .as_ref()
        .map(|u| u.role == Role::Admin)
        .unwrap_or(false);
    let content = if is_admin {
        note.html_admin.as_str().to_owned()
    } else {
        note.html.as_str().to_owned()
    };
    let page = NotesDetailPage {
        version: VERSION,
        title: note.title.clone(),
        content,
        auth_user: auth_user.clone(),
        nav_links,
    };
    Ok(Html(page.render()?))
}

#[derive(Debug, Deserialize)]
pub struct NotesRefreshParams {
    #[serde(default)]
    force: bool,
}

/// `POST /api/notes/refresh` — trigger a background rescan of the notes vault (admin only).
///
/// Returns `202 Accepted` immediately; the scan runs in the background. A
/// subsequent GET to `/notes` will reflect the updated content once the scan
/// completes.
///
/// By default, a refresh while a scan is already running queues exactly one
/// follow-up rather than starting a second, concurrent scan. Pass
/// `?force=true` to instead abort the in-flight scan and restart immediately.
pub async fn notes_refresh_route(
    AdminUser(_): AdminUser,
    Notes(vault): Notes,
    Query(params): Query<NotesRefreshParams>,
) -> axum::http::StatusCode {
    if params.force {
        vault.force_rescan();
    } else {
        vault.request_scan();
    }
    axum::http::StatusCode::ACCEPTED
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

    fn fixture_store() -> NotesStore {
        NotesStore::scan(Path::new("fixtures/vault")).expect("fixtures/vault should scan cleanly")
    }

    async fn minimal_state(notes_store: Option<Arc<NotesStore>>) -> ServerState {
        use crate::{
            admin::AdminDashboard,
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
        let notes_store =
            notes_store.map(|s| NoteVault::from_store_for_test(PathBuf::from("fixtures/vault"), s));
        let index = Index::new(None, Arc::new([]), false, false);
        let admin_dashboard = Arc::new(
            AdminDashboard::new(
                Routes::default(),
                std::iter::empty::<crate::admin::OptionalEntry>(),
                &HashSet::new(),
                None,
                Arc::new([]),
            )
            .await
            .unwrap(),
        );

        ServerState {
            ultron: crate::ultron::Ultron::new(reqwest::Client::new(), "test".into()).into(),
            certificate: Arc::from("fake-cert"),
            breaker_content,
            breaker_detail_store,
            index,
            admin_dashboard,
            tailscale_socket: Arc::from(Path::new("/tmp/fake.sock")),
            notes_store,
            recipes_store: None,
            blog_store: None,
            about_content: None,
            auth_state: None,
            mqtt_state: None,
            log_config: None,
            systemd_config: None,
            nav_links: Arc::new([]),
            peers: Arc::new([]),
            http_client: reqwest::Client::new(),
            peer_api_key: None,
            webhook_secret: None,
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
        let session_titles: Vec<&str> = store
            .session_notes
            .iter()
            .map(|n| n.title.as_str())
            .collect();
        // Verify that numeric titles are in numeric order (not lexicographic).
        // e.g. "Session 2" should come before "Session 10".
        if let (Some(pos1), Some(pos2)) = (
            session_titles.iter().position(|&t| t == "Session 1"),
            session_titles.iter().position(|&t| t == "Session 2"),
        ) {
            assert!(pos1 < pos2, "Session 1 should appear before Session 2");
        }
    }

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
    fn scan_inline_secret_content_present_in_admin_html() {
        // GM variant must contain the full secret text.
        let store = fixture_store();
        let session = store.get("session-1").expect("session-1 should exist");
        assert!(
            session.html_admin.as_str().contains("Malachar"),
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
    fn scan_whole_note_secret_admin_html_contains_content() {
        let store = fixture_store();
        let gm = store.get("gm-notes").expect("gm-notes should exist");
        assert!(
            gm.html_admin.as_str().contains("portal"),
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

    /// Stands in for "a scan is running" in `ScanSlot` tests below, which
    /// only inspect struct fields and never actually await this — its body
    /// never needs to run.
    fn dummy_handle() -> JoinHandle<()> {
        tokio::spawn(std::future::pending())
    }

    #[tokio::test]
    async fn scan_slot_on_request_when_idle_spawns_generation_one() {
        let mut slot = ScanSlot::default();
        assert_eq!(slot.on_request(), Some(1));
        assert_eq!(slot.generation, 1);
        assert!(!slot.pending);
    }

    #[tokio::test]
    async fn scan_slot_on_request_when_busy_queues_follow_up() {
        let mut slot = ScanSlot {
            handle: Some(dummy_handle()),
            ..Default::default()
        };
        assert_eq!(slot.on_request(), None);
        assert!(slot.pending, "request while busy should be queued");
        assert_eq!(slot.generation, 0, "generation must not bump for a queue");
    }

    #[tokio::test]
    async fn scan_slot_on_request_while_already_pending_stays_coalesced() {
        let mut slot = ScanSlot {
            generation: 1,
            handle: Some(dummy_handle()),
            pending: true,
        };
        assert_eq!(slot.on_request(), None);
        assert!(slot.pending);
        assert_eq!(slot.generation, 1, "extra requests must not each queue");
    }

    #[tokio::test]
    async fn scan_slot_on_force_when_idle_still_bumps_generation() {
        let mut slot = ScanSlot::default();
        let (previous, generation) = slot.on_force();
        assert!(previous.is_none());
        assert_eq!(generation, 1);
    }

    #[tokio::test]
    async fn scan_slot_on_force_when_busy_returns_previous_handle_and_drops_pending() {
        let mut slot = ScanSlot {
            generation: 1,
            handle: Some(dummy_handle()),
            pending: true,
        };
        let (previous, generation) = slot.on_force();
        assert!(
            previous.is_some(),
            "caller must abort the superseded handle"
        );
        assert_eq!(generation, 2);
        assert!(!slot.pending, "force supersedes any queued follow-up");
    }

    #[tokio::test]
    async fn scan_slot_on_complete_matching_generation_with_no_pending_goes_idle() {
        let mut slot = ScanSlot {
            generation: 3,
            handle: Some(dummy_handle()),
            pending: false,
        };
        assert_eq!(slot.on_complete(3), None);
        assert!(slot.handle.is_none());
    }

    #[tokio::test]
    async fn scan_slot_on_complete_matching_generation_with_pending_spawns_follow_up() {
        let mut slot = ScanSlot {
            generation: 3,
            handle: Some(dummy_handle()),
            pending: true,
        };
        assert_eq!(slot.on_complete(3), Some(4));
        assert_eq!(slot.generation, 4);
        assert!(!slot.pending);
    }

    #[tokio::test]
    async fn scan_slot_on_complete_stale_generation_is_ignored() {
        // Simulates the race the fencing token exists for: an old task
        // finishes after `force` already replaced it with a newer
        // generation. The stale completion must not touch the slot that
        // now belongs to the newer (still-running) task.
        let newer_handle = dummy_handle();
        let mut slot = ScanSlot {
            generation: 5,
            handle: Some(newer_handle),
            pending: false,
        };
        assert_eq!(slot.on_complete(4), None, "stale generation is ignored");
        assert_eq!(slot.generation, 5, "current generation is untouched");
        assert!(
            slot.handle.is_some(),
            "the newer task's handle must not be clobbered"
        );
    }

    #[tokio::test]
    async fn note_vault_request_scan_populates_store() {
        let vault = NoteVault::new(PathBuf::from("fixtures/vault"));
        assert!(vault.get().await.is_none());

        vault.request_scan();
        vault.wait_scan_idle().await;

        let store = vault.get().await.expect("scan should have populated store");
        assert!(!store.world_notes.is_empty());
    }

    #[tokio::test]
    async fn note_vault_force_rescan_populates_store() {
        let vault = NoteVault::new(PathBuf::from("fixtures/vault"));

        vault.force_rescan();
        vault.wait_scan_idle().await;

        let store = vault.get().await.expect("scan should have populated store");
        assert!(!store.world_notes.is_empty());
    }

    #[tokio::test]
    async fn note_vault_request_scan_while_busy_coalesces_into_one_follow_up() {
        let vault = NoteVault::new(PathBuf::from("fixtures/vault"));

        // Occupy the slot without a real task, so the second request
        // deterministically observes "busy" instead of racing a fast scan.
        let generation = vault.scan.lock().on_request().expect("should be idle");
        assert_eq!(generation, 1);
        vault.scan.lock().handle = Some(dummy_handle());

        vault.request_scan();
        vault.request_scan();
        {
            let slot = vault.scan.lock();
            assert!(slot.pending, "requests while busy should coalesce");
            assert_eq!(
                slot.generation, 1,
                "coalesced requests must not bump generation"
            );
        }

        // Release the placeholder task and let the real follow-up scan run.
        let placeholder = vault.scan.lock().handle.take().unwrap();
        placeholder.abort();
        vault.on_scan_complete(1);
        vault.wait_scan_idle().await;

        let store = vault.get().await.expect("follow-up scan should have run");
        assert!(!store.world_notes.is_empty());
    }

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
    async fn handler_notes_detail_secret_content_absent_for_non_admin() {
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
