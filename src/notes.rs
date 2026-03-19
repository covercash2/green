use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use askama::Template;
use axum::{
    extract::{Path as AxumPath, State},
    response::Html,
};
use serde::Deserialize;

use crate::{ServerState, VERSION, auth::{AuthUserInfo, MaybeAuthUser, Role}, error::Error};

// ─── Slug ─────────────────────────────────────────────────────────────────────

/// A URL-safe slug derived from a note filename stem.
///
/// Slugs are lowercase, contain only alphanumerics and hyphens, with no
/// leading/trailing/consecutive hyphens. They are the sole way to address
/// a note at `/notes/{slug}` and serve as map keys in [`NotesStore`].
///
/// The only constructor is [`Slug::from_stem`], which enforces these invariants.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Slug(String);

impl Slug {
    /// Derive a slug from a filename stem (or any display name).
    ///
    /// Rules: lowercase, non-alphanumeric characters → `-`, consecutive
    /// hyphens collapsed to one, leading/trailing hyphens trimmed.
    pub fn from_stem(s: &str) -> Self {
        let lower = s.to_lowercase();
        let mut slug = String::with_capacity(lower.len());
        let mut last_was_hyphen = false;
        for ch in lower.chars() {
            if ch.is_alphanumeric() {
                slug.push(ch);
                last_was_hyphen = false;
            } else if !last_was_hyphen {
                slug.push('-');
                last_was_hyphen = true;
            }
        }
        Slug(slug.trim_matches('-').to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl AsRef<str> for Slug {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Allows `HashMap<Slug, V>::get("raw-str")` without constructing a `Slug`.
impl Borrow<str> for Slug {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for Slug {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for Slug {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

// ─── RenderedHtml ─────────────────────────────────────────────────────────────

/// HTML that has been produced by our rendering pipeline and is safe to
/// inject into templates with `|safe`.
///
/// The only way to obtain a `RenderedHtml` is via [`render_note_body`] (or
/// the lower-level [`render_markdown`]), which means any instance has gone
/// through pulldown-cmark and is not raw user input. This makes `|safe` in
/// Askama templates self-documenting and auditable.
#[derive(Debug, Clone)]
pub struct RenderedHtml(String);

impl RenderedHtml {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

// ─── Frontmatter ─────────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
struct FrontMatter {
    pub title: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

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

// ─── Private helpers ──────────────────────────────────────────────────────────

/// Escape HTML special characters to prevent injection.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn parse_frontmatter(content: &str) -> (FrontMatter, &str) {
    let content = content.trim_start();
    if !content.starts_with("---") {
        return (FrontMatter::default(), content);
    }
    let after_open = &content[3..];
    if let Some(close_pos) = after_open.find("\n---") {
        let yaml = &after_open[..close_pos];
        let rest = &after_open[close_pos + 4..]; // skip "\n---"
        let rest = rest.strip_prefix('\n').unwrap_or(rest);
        let fm = serde_yml::from_str(yaml).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "malformed frontmatter, using defaults");
            FrontMatter::default()
        });
        (fm, rest)
    } else {
        (FrontMatter::default(), content)
    }
}

fn resolve_wiki_links(text: &str, slug_set: &HashSet<Slug>) -> String {
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;

    while let Some(open) = remaining.find("[[") {
        result.push_str(&remaining[..open]);
        remaining = &remaining[open + 2..];

        if let Some(close) = remaining.find("]]") {
            let inner = &remaining[..close];
            remaining = &remaining[close + 2..];

            // Support pipe syntax: [[Target|Display text]]
            let (target, display) = if let Some(pipe) = inner.find('|') {
                (&inner[..pipe], &inner[pipe + 1..])
            } else {
                (inner, inner)
            };

            let slug = Slug::from_stem(target);
            if slug_set.contains(&slug) {
                let slug_esc = html_escape(slug.as_str());
                let display_esc = html_escape(display);
                result.push_str(&format!(
                    r#"<a href="/notes/{slug_esc}" class="leet-link">{display_esc}</a>"#
                ));
            } else {
                let display_esc = html_escape(display);
                result.push_str(&format!(
                    r#"<span class="notes-dead-link">{display_esc}</span>"#
                ));
            }
        } else {
            // Unclosed `[[` — emit as-is
            result.push_str("[[");
        }
    }
    result.push_str(remaining);
    result
}

/// Returns `true` if the paragraph contains `#secret` as a standalone word
/// (i.e. as an Obsidian-style inline tag, not as part of a longer token like
/// `#secretive` or `#secrets`).
fn has_secret_tag(text: &str) -> bool {
    text.split_whitespace().any(|word| word == "#secret")
}

/// Remove all `#secret` tokens from the text, preserving line structure.
fn strip_secret_tag(text: &str) -> String {
    text.lines()
        .map(|line| {
            line.split_whitespace()
                .filter(|&w| w != "#secret")
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Splits markdown text into `(content, is_secret)` paragraph-level pairs.
///
/// A paragraph is a blank-line-delimited block of text. If a paragraph contains
/// `#secret` as a standalone word (Obsidian-style inline tag), the whole
/// paragraph is marked secret and the tag is stripped from the output.
///
/// For whole-note secrets, see the `tags: [secret]` frontmatter approach in the
/// scan loop — this function handles inline paragraph-level redaction only.
fn split_on_secret_paragraphs(text: &str) -> Vec<(String, bool)> {
    let mut parts: Vec<(String, bool)> = Vec::new();
    let mut current = String::new();

    for line in text.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                let is_secret = has_secret_tag(&current);
                let content = if is_secret {
                    strip_secret_tag(&current)
                } else {
                    current.clone()
                };
                parts.push((content, is_secret));
                current.clear();
            }
        } else {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line);
        }
    }

    if !current.is_empty() {
        let is_secret = has_secret_tag(&current);
        let content = if is_secret {
            strip_secret_tag(&current)
        } else {
            current
        };
        parts.push((content, is_secret));
    }

    parts
}

/// Render Markdown to trusted HTML. The returned [`RenderedHtml`] is the
/// only public surface of this transformation — callers cannot construct
/// one independently.
///
/// Raw HTML blocks and inline HTML from the Markdown source are stripped to
/// prevent injection of arbitrary markup.
///
/// `[[wiki links]]` are resolved against `slug_set`:
/// - Known targets become `<a href="/notes/{slug}" class="leet-link">`.
/// - Unknown targets become `<span class="notes-dead-link">`.
fn render_markdown(md: &str, slug_set: &HashSet<Slug>) -> RenderedHtml {
    use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd, html};

    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_WIKILINKS;

    // Collect and transform events:
    // 1. Strip raw HTML (prevents author-injected markup).
    // 2. Resolve [[wiki links]] via WikiLink events instead of pre-processing strings.
    let raw_events: Vec<Event> = Parser::new_ext(md, opts)
        .filter(|e| !matches!(e, Event::Html(_) | Event::InlineHtml(_)))
        .collect();

    let mut transformed: Vec<Event> = Vec::with_capacity(raw_events.len());
    let mut in_wiki_link = false;
    let mut wiki_link_known = false;

    for event in raw_events {
        match event {
            Event::Start(Tag::Link {
                link_type: LinkType::WikiLink { .. },
                ref dest_url,
                ..
            }) => {
                let slug = Slug::from_stem(dest_url);
                let slug_esc = html_escape(slug.as_str());
                in_wiki_link = true;
                if slug_set.contains(&slug) {
                    wiki_link_known = true;
                    transformed.push(Event::Html(
                        format!(r#"<a href="/notes/{slug_esc}" class="leet-link">"#).into(),
                    ));
                } else {
                    wiki_link_known = false;
                    transformed.push(Event::Html(
                        r#"<span class="notes-dead-link">"#.into(),
                    ));
                }
            }
            Event::End(TagEnd::Link) if in_wiki_link => {
                in_wiki_link = false;
                if wiki_link_known {
                    transformed.push(Event::Html("</a>".into()));
                } else {
                    transformed.push(Event::Html("</span>".into()));
                }
            }
            other => transformed.push(other),
        }
    }

    let mut output = String::new();
    html::push_html(&mut output, transformed.into_iter());
    RenderedHtml(output)
}

/// Render the full body of a note, processing `#secret` paragraph tags.
///
/// Returns `(html, has_secrets)`. Secret paragraphs are omitted from the
/// player-visible HTML entirely. The `has_secrets` flag is set so the index
/// can show a 🔒 badge, but the secret content is never sent to non-GM clients.
fn render_note_body(text: &str, slug_set: &HashSet<Slug>) -> (RenderedHtml, bool) {
    let parts = split_on_secret_paragraphs(text);
    let has_secrets = parts.iter().any(|(_, is_secret)| *is_secret);

    let mut output = String::new();
    for (content, is_secret) in &parts {
        if *is_secret {
            // Omit secret content from the player-visible response entirely.
            continue;
        }
        let rendered = render_markdown(content, slug_set);
        output.push_str(rendered.as_str());
    }

    (RenderedHtml(output), has_secrets)
}

/// Like [`render_note_body`] but renders secret paragraphs without the
/// `<div class="notes-secret">` wrapper. Intended for GM-authenticated views.
fn render_note_body_revealed(text: &str, slug_set: &HashSet<Slug>) -> RenderedHtml {
    let parts = split_on_secret_paragraphs(text);
    let mut output = String::new();
    for (content, _is_secret) in &parts {
        let rendered = render_markdown(content, slug_set);
        output.push_str(rendered.as_str());
    }
    RenderedHtml(output)
}

// ─── NotesStore ───────────────────────────────────────────────────────────────

impl NotesStore {
    /// Scan a vault directory, parsing every `.md` file.
    ///
    /// Two-pass algorithm:
    /// 1. Collect all `.md` stems → `HashSet<Slug>` for wiki-link resolution.
    /// 2. Read each file, parse frontmatter, filter by `world`/`session` tags,
    ///    resolve wiki-links, render with secret-block processing, index by slug.
    pub fn scan(vault: &Path) -> Result<Self, NotesStoreError> {
        use walkdir::WalkDir;

        if !vault.is_dir() {
            return Err(NotesStoreError::VaultNotDirectory(vault.to_path_buf()));
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

        // Pass 2: parse, filter, render
        let mut world_notes: Vec<Note> = Vec::new();
        let mut session_notes: Vec<Note> = Vec::new();
        let mut by_slug: HashMap<Slug, Note> = HashMap::new();

        for path in &md_paths {
            let raw =
                std::fs::read_to_string(path).map_err(|source| NotesStoreError::NoteRead {
                    path: path.clone(),
                    source,
                })?;

            let (fm, body) = parse_frontmatter(&raw);

            let is_world = fm.tags.iter().any(|t| t == "world");
            let is_session = fm.tags.iter().any(|t| t == "session");

            if !is_world && !is_session {
                continue;
            }

            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            let slug = Slug::from_stem(stem);
            let title = fm
                .title
                .unwrap_or_else(|| stem.replace('-', " ").replace('_', " "));

            // `tags: [secret]` (Obsidian-style) marks the entire note as redacted.
            let is_whole_secret = fm.tags.iter().any(|t| t == "secret");
            let (html, html_gm, has_secrets) = if is_whole_secret {
                let rendered = render_markdown(body, &slug_set);
                // Player HTML: entire body is omitted, nothing is sent to non-GM clients.
                let player_html = RenderedHtml(String::new());
                // GM sees the note in full.
                (player_html, rendered, true)
            } else {
                let (player_html, has_secrets) = render_note_body(body, &slug_set);
                let gm_html = render_note_body_revealed(body, &slug_set);
                (player_html, gm_html, has_secrets)
            };

            let note = Note {
                slug: slug.clone(),
                title,
                html,
                html_gm,
                has_secrets,
            };

            if is_world {
                world_notes.push(note.clone());
            }
            if is_session {
                session_notes.push(note.clone());
            }
            let _ = by_slug.insert(slug, note);
        }

        world_notes.sort_by(|a, b| a.title.cmp(&b.title));
        session_notes.sort_by(|a, b| a.title.cmp(&b.title));

        Ok(NotesStore {
            world_notes,
            session_notes,
            by_slug,
        })
    }

    /// Look up a note by its slug. Accepts `&str` directly via [`Borrow`].
    pub fn get(&self, slug: &str) -> Option<&Note> {
        self.by_slug.get(slug)
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
}

#[derive(Template)]
#[template(path = "notes_detail.html")]
pub struct NotesDetailPage {
    pub version: &'static str,
    pub title: String,
    /// Pre-rendered HTML from [`RenderedHtml`] — safe for `|safe` in the template.
    pub content: String,
    pub auth_user: Option<AuthUserInfo>,
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

pub async fn notes_index_route(
    MaybeAuthUser(auth_user): MaybeAuthUser,
    State(state): State<ServerState>,
) -> Result<Html<String>, Error> {
    let store: &Arc<NotesStore> = state.notes_store.as_ref().ok_or(Error::NotFound)?;

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
    };
    Ok(Html(page.render()?))
}

pub async fn notes_detail_route(
    MaybeAuthUser(auth_user): MaybeAuthUser,
    AxumPath(slug): AxumPath<String>,
    State(state): State<ServerState>,
) -> Result<Html<String>, Error> {
    let store: &Arc<NotesStore> = state.notes_store.as_ref().ok_or(Error::NotFound)?;
    let note = store.get(&slug).ok_or(Error::NotFound)?;
    let is_gm = auth_user.as_ref().map(|u| u.role == Role::Gm).unwrap_or(false);
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
    };
    Ok(Html(page.render()?))
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::{Request, StatusCode}, routing::get};
    use tower::ServiceExt;

    // ── Slug::from_stem ───────────────────────────────────────────────────────

    #[test]
    fn slug_basic_spaces() {
        assert_eq!(Slug::from_stem("The Known World"), "the-known-world");
    }

    #[test]
    fn slug_apostrophe() {
        assert_eq!(Slug::from_stem("Dragon's Lair"), "dragon-s-lair");
    }

    #[test]
    fn slug_consecutive_punctuation() {
        assert_eq!(Slug::from_stem("Hello -- World"), "hello-world");
    }

    #[test]
    fn slug_leading_trailing() {
        assert_eq!(Slug::from_stem("  hello world  "), "hello-world");
    }

    #[test]
    fn slug_numeric() {
        assert_eq!(Slug::from_stem("Session 3"), "session-3");
    }

    #[test]
    fn slug_empty_string() {
        assert_eq!(Slug::from_stem(""), "");
    }

    #[test]
    fn slug_display_matches_as_str() {
        let slug = Slug::from_stem("The Known World");
        assert_eq!(slug.to_string(), slug.as_str());
    }

    #[test]
    fn slug_borrow_enables_str_lookup() {
        let mut map: HashMap<Slug, &str> = HashMap::new();
        let _ = map.insert(Slug::from_stem("hello world"), "value");
        assert_eq!(map.get("hello-world"), Some(&"value"));
    }

    // ── parse_frontmatter ─────────────────────────────────────────────────────

    #[test]
    fn parse_fm_with_tags() {
        let content = "---\ntitle: My Note\ntags: [world, lore]\n---\n# Body";
        let (fm, rest) = parse_frontmatter(content);
        assert_eq!(fm.title.as_deref(), Some("My Note"));
        assert!(fm.tags.contains(&"world".to_string()));
        assert!(rest.starts_with("# Body"));
    }

    #[test]
    fn parse_fm_no_frontmatter() {
        let content = "# Just a heading\nSome text.";
        let (fm, rest) = parse_frontmatter(content);
        assert!(fm.title.is_none());
        assert!(fm.tags.is_empty());
        assert_eq!(rest, content);
    }

    #[test]
    fn parse_fm_malformed_yaml() {
        let content = "---\ntitle: [unclosed\n---\nbody text";
        let (fm, rest) = parse_frontmatter(content);
        assert!(fm.title.is_none());
        assert!(rest.contains("body text"));
    }

    #[test]
    fn parse_fm_empty_frontmatter() {
        let content = "---\n---\nbody here";
        let (fm, rest) = parse_frontmatter(content);
        assert!(fm.title.is_none());
        assert!(fm.tags.is_empty());
        assert_eq!(rest, "body here");
    }

    #[test]
    fn parse_fm_leading_whitespace_stripped() {
        let content = "  ---\ntitle: Hi\n---\nbody";
        let (fm, rest) = parse_frontmatter(content);
        assert_eq!(fm.title.as_deref(), Some("Hi"));
        assert_eq!(rest, "body");
    }

    #[test]
    fn parse_fm_secret_tag_in_tags_vec() {
        let content = "---\ntags: [world, secret]\n---\nbody";
        let (fm, _rest) = parse_frontmatter(content);
        assert!(fm.tags.iter().any(|t| t == "secret"));
    }

    #[test]
    fn parse_fm_no_secret_tag_when_absent() {
        let content = "---\ntags: [world]\n---\nbody";
        let (fm, _rest) = parse_frontmatter(content);
        assert!(!fm.tags.iter().any(|t| t == "secret"));
    }

    // ── resolve_wiki_links ────────────────────────────────────────────────────

    fn slug_set(items: &[&str]) -> HashSet<Slug> {
        items.iter().map(|s| Slug::from_stem(s)).collect()
    }

    #[test]
    fn resolve_known_link() {
        let known = slug_set(&["The Known World"]);
        let result = resolve_wiki_links("See [[The Known World]] for details.", &known);
        assert!(result.contains(r#"href="/notes/the-known-world""#));
        assert!(result.contains("The Known World"));
    }

    #[test]
    fn resolve_pipe_display_syntax() {
        let known = slug_set(&["The Known World"]);
        let result = resolve_wiki_links("[[The Known World|the world]]", &known);
        assert!(result.contains(r#"href="/notes/the-known-world""#));
        assert!(result.contains("the world"));
        assert!(!result.contains("The Known World"));
    }

    #[test]
    fn resolve_dead_link() {
        let known = slug_set(&[]);
        let result = resolve_wiki_links("[[Nonexistent Place]]", &known);
        assert!(result.contains("notes-dead-link"));
        assert!(result.contains("Nonexistent Place"));
    }

    #[test]
    fn resolve_multiple_links() {
        let known = slug_set(&["Place A", "Place B"]);
        let result = resolve_wiki_links("Visit [[Place A]] and [[Place B]].", &known);
        assert!(result.contains(r#"href="/notes/place-a""#));
        assert!(result.contains(r#"href="/notes/place-b""#));
    }

    #[test]
    fn resolve_unclosed_bracket_passes_through() {
        let known = slug_set(&[]);
        let result = resolve_wiki_links("[[unclosed", &known);
        assert_eq!(result, "[[unclosed");
    }

    #[test]
    fn resolve_no_links_unchanged() {
        let known = slug_set(&[]);
        let input = "Just plain text with no links.";
        assert_eq!(resolve_wiki_links(input, &known), input);
    }

    // ── split_on_secret_paragraphs ────────────────────────────────────────────

    #[test]
    fn split_no_secrets_returns_single_public_part() {
        let parts = split_on_secret_paragraphs("hello\nworld\n");
        assert_eq!(parts.len(), 1);
        assert!(!parts[0].1, "should be public");
        assert!(parts[0].0.contains("hello"));
    }

    #[test]
    fn split_empty_input_returns_empty() {
        assert!(split_on_secret_paragraphs("").is_empty());
    }

    #[test]
    fn split_one_secret_paragraph() {
        let text = "before\n\nhidden content #secret\n\nafter\n";
        let parts = split_on_secret_paragraphs(text);
        assert_eq!(parts.len(), 3);
        assert!(!parts[0].1 && parts[0].0.contains("before"));
        assert!(parts[1].1 && parts[1].0.contains("hidden content"));
        assert!(!parts[2].1 && parts[2].0.contains("after"));
    }

    #[test]
    fn split_secret_tag_stripped_from_output() {
        let parts = split_on_secret_paragraphs("GM only info #secret");
        assert_eq!(parts.len(), 1);
        assert!(parts[0].1, "should be secret");
        assert!(!parts[0].0.contains("#secret"), "tag should be stripped");
        assert!(parts[0].0.contains("GM only info"));
    }

    #[test]
    fn split_multiple_secret_paragraphs() {
        let text = "pub1\n\nsec1 #secret\n\npub2\n\nsec2 #secret\n\npub3\n";
        let parts = split_on_secret_paragraphs(text);
        let secret_parts: Vec<_> = parts.iter().filter(|(_, s)| *s).collect();
        let public_parts: Vec<_> = parts.iter().filter(|(_, s)| !*s).collect();
        assert_eq!(secret_parts.len(), 2);
        assert_eq!(public_parts.len(), 3);
        assert!(secret_parts[0].0.contains("sec1"));
        assert!(secret_parts[1].0.contains("sec2"));
    }

    #[test]
    fn split_secret_paragraph_at_start_of_text() {
        let text = "hidden #secret\n\npublic\n";
        let parts = split_on_secret_paragraphs(text);
        assert!(parts.iter().any(|(c, s)| *s && c.contains("hidden")));
        assert!(parts.iter().any(|(c, s)| !s && c.contains("public")));
    }

    #[test]
    fn split_inline_tag_does_not_match_partial_word() {
        // `#secrets` and `#secretive` must NOT be treated as the `#secret` tag.
        let parts = split_on_secret_paragraphs("these are #secrets and #secretive things");
        assert_eq!(parts.len(), 1);
        assert!(!parts[0].1, "partial-word tags must not mark paragraph as secret");
    }

    #[test]
    fn split_only_secret_paragraph() {
        let text = "all secret #secret\n";
        let parts = split_on_secret_paragraphs(text);
        assert_eq!(parts.len(), 1);
        assert!(parts[0].1);
    }

    // ── render_note_body ──────────────────────────────────────────────────────

    #[test]
    fn render_body_wraps_secret_paragraph_in_div() {
        let text = "before\n\nhidden content #secret\n\nafter\n";
        let (html, has_secrets) = render_note_body(text, &HashSet::new());
        assert!(has_secrets);
        // Secret content must NOT appear in the player-visible HTML.
        assert!(!html.as_str().contains("hidden content"), "secret content should be omitted from player HTML");
        // Public content should still be present.
        assert!(html.as_str().contains("before"));
        assert!(html.as_str().contains("after"));
    }

    #[test]
    fn render_body_public_content_not_wrapped() {
        let text = "public content\n\nhidden #secret\n";
        let (html, _) = render_note_body(text, &HashSet::new());
        let s = html.as_str();
        // Public content must be present.
        assert!(s.contains("public content"), "public content should be present");
        // Secret content must be absent.
        assert!(!s.contains("hidden"), "secret content should be omitted from player HTML");
    }

    #[test]
    fn render_body_no_secrets_has_secrets_false() {
        let (_, has_secrets) = render_note_body("just public content\n", &HashSet::new());
        assert!(!has_secrets);
    }

    #[test]
    fn render_body_markdown_inside_secret_is_omitted() {
        let text = "**bold secret** #secret\n";
        let (html, has_secrets) = render_note_body(text, &HashSet::new());
        assert!(has_secrets, "paragraph tagged #secret should set has_secrets");
        assert!(!html.as_str().contains("<strong>"), "secret content must be omitted from player HTML");
    }

    // ── NotesStore::scan ──────────────────────────────────────────────────────

    fn fixture_store() -> NotesStore {
        NotesStore::scan(Path::new("fixtures/vault"))
            .expect("fixtures/vault should scan cleanly")
    }

    #[test]
    fn scan_fixtures_vault() {
        let store = fixture_store();
        assert!(!store.world_notes.is_empty(), "expected world notes");
        assert!(!store.session_notes.is_empty(), "expected session notes");
    }

    #[test]
    fn scan_untagged_note_excluded() {
        let store = fixture_store();
        assert!(store.get("untagged").is_none(), "untagged note should be excluded");
    }

    #[test]
    fn scan_wiki_link_resolved_in_session_html() {
        let store = fixture_store();
        let session = store.get("session-1").expect("session-1 should exist");
        assert!(
            session.html.as_str().contains(r#"href="/notes/the-known-world""#),
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
        assert!(session.has_secrets, "session-1 has a #secret-tagged paragraph");
        // Secret content must not appear in the player-visible HTML.
        assert!(
            !session.html.as_str().contains("Malachar"),
            "secret content must be absent from player HTML; got: {}",
            session.html.as_str()
        );
    }

    #[test]
    fn scan_inline_secret_content_absent_in_player_html() {
        // Secret content must NOT be sent to non-GM users.
        let store = fixture_store();
        let session = store.get("session-1").expect("session-1 should exist");
        assert!(
            !session.html.as_str().contains("Malachar"),
            "secret content must be omitted from player HTML"
        );
        // GM HTML must contain the secret content.
        assert!(
            session.html_gm.as_str().contains("Malachar"),
            "secret content must be present in GM HTML"
        );
    }

    #[test]
    fn scan_secret_tag_in_frontmatter_omits_body_for_players() {
        let store = fixture_store();
        let gm = store.get("gm-notes").expect("gm-notes should exist");
        assert!(gm.has_secrets, "tags: [secret] note should have has_secrets");
        // Player HTML must be empty (whole note is secret).
        assert!(
            gm.html.as_str().is_empty(),
            "player HTML must be empty for whole-note secrets; got: {}",
            gm.html.as_str()
        );
        // GM HTML must contain the actual content.
        assert!(
            gm.html_gm.as_str().contains("Malachar"),
            "GM HTML must contain secret content"
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

        let world_titles: Vec<&str> =
            store.world_notes.iter().map(|n| n.title.as_str()).collect();
        let mut sorted = world_titles.clone();
        sorted.sort();
        assert_eq!(world_titles, sorted, "world_notes should be sorted by title");

        let session_titles: Vec<&str> =
            store.session_notes.iter().map(|n| n.title.as_str()).collect();
        let mut sorted = session_titles.clone();
        sorted.sort();
        assert_eq!(session_titles, sorted, "session_notes should be sorted by title");
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
        let index = Index::new(Routes::default(), has_notes, false).await.unwrap();

        ServerState {
            certificate: Arc::from("fake-cert"),
            breaker_content,
            breaker_detail_store,
            index,
            tailscale_socket: Arc::from(Path::new("/tmp/fake.sock")),
            notes_store,
            auth_state: None,
            mqtt_state: None,
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

    #[tokio::test]
    async fn handler_notes_index_no_vault_returns_404() {
        let state = minimal_state(None).await;
        let app = notes_router(state);
        let req = Request::builder().uri("/notes").body(Body::empty()).unwrap();
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

        let req = Request::builder().uri("/notes").body(Body::empty()).unwrap();
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

        let req = Request::builder().uri("/notes").body(Body::empty()).unwrap();
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

        let req = Request::builder().uri("/notes").body(Body::empty()).unwrap();
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
    async fn handler_notes_detail_secret_content_omitted_for_players() {
        let store = Some(Arc::new(fixture_store()));
        let state = minimal_state(store).await;
        let app = notes_router(state);

        let req = Request::builder()
            .uri("/notes/session-1")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        let text = body_text(res).await;

        // Secret content must NOT be present in the player-visible response.
        assert!(!text.contains("Malachar"), "secret content must be omitted from player response");
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
