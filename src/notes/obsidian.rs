/// Obsidian vault protocol implementation.
///
/// Self-contained: no axum/HTTP dependencies. Handles vault scanning, frontmatter
/// parsing, wiki-link resolution (shortest-path, with aliases), inline tag detection,
/// and secret-block redaction. Designed to be extractable as a standalone crate.
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use serde::Deserialize;
use walkdir::WalkDir;

// ─── Slug ─────────────────────────────────────────────────────────────────────

/// URL-safe slug derived from a filename stem or display name.
/// Lowercase alphanumerics and hyphens only; no leading/trailing/consecutive hyphens.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Slug(String);

impl Slug {
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

impl std::fmt::Display for Slug {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl AsRef<str> for Slug {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for Slug {
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

// ─── Frontmatter ──────────────────────────────────────────────────────────────

/// All frontmatter fields recognised across notes and recipes.
/// All fields are optional so one struct serves both consumers.
#[derive(Debug, Default, Deserialize)]
pub struct Frontmatter {
    pub title: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub aliases: Vec<String>,
    // Recipe-specific
    pub category: Option<String>,
    pub servings: Option<String>,
    pub prep_time: Option<String>,
    pub cook_time: Option<String>,
}

/// Split raw note content into `(FM, body)` where `FM` is any `DeserializeOwned + Default` type.
/// Returns `FM::default()` if the content has no `---` delimited block or YAML parse fails.
pub fn parse_frontmatter<FM>(content: &str) -> (FM, &str)
where
    FM: serde::de::DeserializeOwned + Default,
{
    let content = content.trim_start();
    if !content.starts_with("---") {
        return (FM::default(), content);
    }
    let after_open = &content[3..];
    if let Some(close_pos) = after_open.find("\n---") {
        let yaml = &after_open[..close_pos];
        let rest = &after_open[close_pos + 4..];
        let rest = rest.strip_prefix('\n').unwrap_or(rest);
        let fm = serde_yml::from_str(yaml).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "malformed frontmatter, using defaults");
            FM::default()
        });
        (fm, rest)
    } else {
        (FM::default(), content)
    }
}

// ─── VaultIndex ───────────────────────────────────────────────────────────────

/// Canonical reference to a note within the vault.
#[derive(Debug, Clone)]
pub struct NoteRef {
    /// Relative path from vault root (e.g. `Characters/Gillen.md`).
    #[allow(dead_code)]
    pub rel_path: PathBuf,
    pub slug: Slug,
    /// Number of path components — used for shortest-path tie-breaking.
    depth: usize,
}

impl NoteRef {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn new(rel_path: PathBuf, slug: Slug) -> Self {
        let depth = rel_path.components().count();
        Self { rel_path, slug, depth }
    }
}

/// Vault-wide index: maps every known name (stem + aliases, lowercased) → `NoteRef`.
///
/// Shortest-path algorithm: when two files share a stem, the one with fewer path
/// components wins (matching Obsidian's "prefer the closest file" behaviour).
#[derive(Debug, Default)]
pub struct VaultIndex {
    by_name: HashMap<String, NoteRef>,
}

impl VaultIndex {
    pub(crate) fn register(&mut self, name: &str, note_ref: NoteRef) {
        let key = name.to_lowercase();
        match self.by_name.get(&key) {
            Some(existing) if existing.depth <= note_ref.depth => { /* shorter/equal path wins */ }
            _ => {
                let _ = self.by_name.insert(key, note_ref);
            }
        }
    }

    /// Look up a note by any name (stem or alias), case-insensitive.
    pub fn get(&self, name: &str) -> Option<&NoteRef> {
        self.by_name.get(&name.to_lowercase())
    }

    /// Set of all slugs present in the index.
    pub fn all_slugs(&self) -> HashSet<&Slug> {
        self.by_name.values().map(|r| &r.slug).collect()
    }
}

/// Errors that can occur while accessing the vault.
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("vault path `{0}` does not exist or is not a directory")]
    NotDirectory(PathBuf),
    #[error("failed to read `{path}`: {source}")]
    ReadError {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Walk `vault`, register every `.md` file by stem and aliases.
///
/// `alias_map`: absolute path → list of alias strings. Build this in the
/// caller's pass-1 scan by reading frontmatter first, then call this.
/// For a single-pass approach pass an empty map and register aliases separately.
pub fn build_vault_index(
    vault: &Path,
    alias_map: &HashMap<PathBuf, Vec<String>>,
) -> Result<(VaultIndex, Vec<PathBuf>), VaultError> {
    if !vault.is_dir() {
        return Err(VaultError::NotDirectory(vault.to_path_buf()));
    }

    let mut index = VaultIndex::default();
    let mut paths = Vec::new();

    for entry in WalkDir::new(vault)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let abs = entry.into_path();
        if abs.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        let rel = abs.strip_prefix(vault).unwrap_or(&abs).to_path_buf();
        let depth = rel.components().count();
        let stem = match abs.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_owned(),
            None => continue,
        };
        let slug = Slug::from_stem(&stem);
        let note_ref = NoteRef {
            rel_path: rel.clone(),
            slug: slug.clone(),
            depth,
        };

        index.register(&stem, note_ref);

        if let Some(aliases) = alias_map.get(&abs) {
            for alias in aliases {
                let alias_ref = NoteRef {
                    rel_path: rel.clone(),
                    slug: slug.clone(),
                    depth,
                };
                index.register(alias, alias_ref);
            }
        }

        paths.push(abs);
    }

    Ok((index, paths))
}

/// A parsed note file: frontmatter + raw body text.
///
/// `FM` defaults to [`Frontmatter`] for convenience; supply your own
/// `DeserializeOwned + Default` type to parse custom frontmatter fields.
pub struct ParsedNote<FM = Frontmatter> {
    pub path: PathBuf,
    pub slug: Slug,
    pub frontmatter: FM,
    /// Raw markdown body (after frontmatter stripped).
    pub body: String,
}

/// Read and parse a single `.md` file.
///
/// `FM` defaults to [`Frontmatter`]; use `parse_note::<MyFields>(path)` or
/// `let note: ParsedNote<MyFields> = parse_note(path)?` for custom types.
pub fn parse_note<FM>(path: &Path) -> Result<ParsedNote<FM>, VaultError>
where
    FM: serde::de::DeserializeOwned + Default,
{
    let raw = std::fs::read_to_string(path).map_err(|source| VaultError::ReadError {
        path: path.to_path_buf(),
        source,
    })?;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let slug = Slug::from_stem(stem);
    let (frontmatter, body) = parse_frontmatter(&raw);
    Ok(ParsedNote {
        path: path.to_path_buf(),
        slug,
        frontmatter,
        body: body.to_owned(),
    })
}

// ─── Wiki-link resolution ─────────────────────────────────────────────────────

/// A parsed `[[...]]` wiki link.
#[derive(Debug, PartialEq)]
pub struct WikiLink {
    /// Link target (file stem or path, left of `|` and `#`).
    pub target: String,
    /// In-note heading anchor (`[[Note#Heading]]`), right of `#`.
    pub heading: Option<String>,
    /// Display text (`[[Note|Display]]`), defaults to `target` if absent.
    pub display: String,
}

/// Parse the inner content of `[[...]]` (without the brackets).
pub fn parse_wiki_link(inner: &str) -> WikiLink {
    // Split display first (rightmost `|`)
    let (target_part, display_override) = if let Some(pipe) = inner.rfind('|') {
        (&inner[..pipe], Some(&inner[pipe + 1..]))
    } else {
        (inner, None)
    };

    // Split heading from target
    let (target, heading) = if let Some(hash) = target_part.find('#') {
        (&target_part[..hash], Some(target_part[hash + 1..].to_owned()))
    } else {
        (target_part, None)
    };

    let display = display_override.unwrap_or(target).to_owned();
    WikiLink {
        target: target.to_owned(),
        heading,
        display,
    }
}

/// Resolve a `WikiLink` against a `VaultIndex`. Returns the `NoteRef` if found.
///
/// Resolution order for a given name:
/// 1. Case-insensitive exact match against the registered stem/alias.
/// 2. Slug-normalised match (`[[The Known World]]` → `the-known-world`).
/// 3. Strip any path prefix (`[[Characters/Gillen]]` → `Gillen`) then repeat 1–2.
pub fn resolve_link<'a>(link: &WikiLink, index: &'a VaultIndex) -> Option<&'a NoteRef> {
    if let Some(r) = index.get(&link.target) {
        return Some(r);
    }
    // Slug-normalised: [[The Known World]] → "the-known-world"
    if let Some(r) = index.get(Slug::from_stem(&link.target).as_str()) {
        return Some(r);
    }
    // Strip path prefix: [[Characters/Gillen]] → "Gillen"
    if let Some(stem) = Path::new(&link.target).file_stem().and_then(|s| s.to_str()) {
        if stem != link.target {
            if let Some(r) = index.get(stem) {
                return Some(r);
            }
            return index.get(Slug::from_stem(stem).as_str());
        }
    }
    None
}

/// Post-process rendered HTML: replace `[[...]]` tokens with `<a>` links or dead-link spans.
///
/// - `index`: full vault index (all `.md` files) for resolution.
/// - `live_slugs`: slugs that are actually served (have HTTP routes). Links to
///   untagged or otherwise excluded notes become dead-link spans to avoid 404s.
/// - `base_path`: URL prefix, e.g. `"/notes/"` or `"/recipes/"`.
///
/// **Caller contract**: `html` must be pulldown-cmark output so that any
/// user-supplied display text is already HTML-escaped before this function runs.
pub fn resolve_wiki_links(
    html: &str,
    index: &VaultIndex,
    live_slugs: &HashSet<Slug>,
    base_path: &str,
) -> String {
    let mut result = String::with_capacity(html.len());
    let mut remaining = html;

    while let Some(open) = remaining.find("[[") {
        result.push_str(&remaining[..open]);
        remaining = &remaining[open + 2..];

        if let Some(close) = remaining.find("]]") {
            let inner = &remaining[..close];
            remaining = &remaining[close + 2..];

            let link = parse_wiki_link(inner);
            // display is already HTML-safe (escaped by pulldown-cmark upstream)
            let display = &link.display;

            match resolve_link(&link, index) {
                Some(note_ref) if live_slugs.contains(&note_ref.slug) => {
                    let slug = &note_ref.slug;
                    let anchor = link
                        .heading
                        .as_deref()
                        .map(|h| format!("#{}", Slug::from_stem(h)))
                        .unwrap_or_default();
                    result.push_str(&format!(
                        r#"<a href="{base_path}{slug}{anchor}" class="leet-link">{display}</a>"#
                    ));
                }
                _ => {
                    result.push_str(&format!(
                        r#"<span class="notes-dead-link">{display}</span>"#
                    ));
                }
            }
        } else {
            result.push_str("[[");
        }
    }
    result.push_str(remaining);
    result
}

// ─── Inline-tag / secret-redaction helpers ────────────────────────────────────

/// `true` if `text` contains `#secret` as a standalone whitespace-delimited word.
/// Does NOT match `#secrets`, `#secretive`, etc.
pub fn has_secret_tag(text: &str) -> bool {
    text.split_whitespace().any(|word| word == "#secret")
}

/// Remove all `#secret` tokens from `text`, preserving line structure.
pub fn strip_secret_tag(text: &str) -> String {
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

/// Split markdown into `(content, is_secret)` paragraph pairs (blank-line delimited).
/// Paragraphs containing `#secret` are marked secret and the tag is stripped.
pub fn split_on_secret_paragraphs(text: &str) -> Vec<(String, bool)> {
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

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Slug ──────────────────────────────────────────────────────────────────

    #[test]
    fn slug_basic() {
        assert_eq!(Slug::from_stem("Foo Bar").as_str(), "foo-bar");
    }

    #[test]
    fn slug_consecutive_separators_collapsed() {
        assert_eq!(Slug::from_stem("foo  bar").as_str(), "foo-bar");
    }

    #[test]
    fn slug_leading_trailing_trimmed() {
        assert_eq!(Slug::from_stem("-foo-").as_str(), "foo");
    }

    // ── parse_frontmatter ─────────────────────────────────────────────────────

    #[test]
    fn fm_no_frontmatter_returns_defaults() {
        let (fm, body) = parse_frontmatter::<Frontmatter>("just body");
        assert!(fm.title.is_none());
        assert!(fm.tags.is_empty());
        assert_eq!(body, "just body");
    }

    #[test]
    fn fm_parses_title_and_tags() {
        let input = "---\ntitle: My Note\ntags: [world, session]\n---\nbody text";
        let (fm, body) = parse_frontmatter::<Frontmatter>(input);
        assert_eq!(fm.title.as_deref(), Some("My Note"));
        assert!(fm.tags.contains(&"world".to_string()));
        assert_eq!(body, "body text");
    }

    #[test]
    fn fm_parses_aliases() {
        let input =
            "---\ntitle: Gillen\naliases: [The Alchemist, Gillen the Grey]\n---\nbody";
        let (fm, _) = parse_frontmatter::<Frontmatter>(input);
        assert_eq!(fm.aliases.len(), 2);
        assert!(fm.aliases.contains(&"The Alchemist".to_string()));
    }

    #[test]
    fn fm_parses_recipe_fields() {
        let input =
            "---\ncategory: dinner\nservings: 4\nprep_time: 15 min\ncook_time: 30 min\n---\nbody";
        let (fm, _) = parse_frontmatter::<Frontmatter>(input);
        assert_eq!(fm.category.as_deref(), Some("dinner"));
        assert_eq!(fm.servings.as_deref(), Some("4"));
    }

    #[test]
    fn fm_missing_aliases_defaults_to_empty() {
        let (fm, _) = parse_frontmatter::<Frontmatter>("---\ntitle: Test\n---\nbody");
        assert!(fm.aliases.is_empty());
    }

    #[test]
    fn fm_no_close_delimiter_returns_defaults() {
        let (fm, body) = parse_frontmatter::<Frontmatter>("---\ntitle: Unclosed");
        assert!(fm.title.is_none());
        assert!(body.starts_with("---"));
    }

    // ── VaultIndex ────────────────────────────────────────────────────────────

    fn make_ref(rel: &str, depth: usize) -> NoteRef {
        let path = PathBuf::from(rel);
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(rel)
            .to_owned();
        NoteRef {
            slug: Slug::from_stem(&stem),
            rel_path: path,
            depth,
        }
    }

    #[test]
    fn vault_index_shorter_path_wins() {
        let mut idx = VaultIndex::default();
        idx.register("gillen", make_ref("Sub/gillen.md", 2));
        idx.register("gillen", make_ref("gillen.md", 1));
        assert_eq!(idx.get("gillen").unwrap().rel_path, PathBuf::from("gillen.md"));
    }

    #[test]
    fn vault_index_longer_does_not_displace_shorter() {
        let mut idx = VaultIndex::default();
        idx.register("gillen", make_ref("gillen.md", 1));
        idx.register("gillen", make_ref("Sub/gillen.md", 2));
        assert_eq!(idx.get("gillen").unwrap().rel_path, PathBuf::from("gillen.md"));
    }

    #[test]
    fn vault_index_case_insensitive_lookup() {
        let mut idx = VaultIndex::default();
        idx.register("Gillen", make_ref("Gillen.md", 1));
        assert!(idx.get("gillen").is_some());
        assert!(idx.get("GILLEN").is_some());
    }

    #[test]
    fn vault_index_alias_lookup() {
        let mut idx = VaultIndex::default();
        let r = make_ref("Gillen.md", 1);
        idx.register("Gillen", r.clone());
        idx.register("The Alchemist", NoteRef { ..r });
        assert!(idx.get("the alchemist").is_some());
        assert_eq!(
            idx.get("the alchemist").unwrap().slug,
            idx.get("gillen").unwrap().slug
        );
    }

    // ── parse_wiki_link ───────────────────────────────────────────────────────

    #[test]
    fn wl_plain() {
        let wl = parse_wiki_link("Foo Bar");
        assert_eq!(wl.target, "Foo Bar");
        assert_eq!(wl.display, "Foo Bar");
        assert!(wl.heading.is_none());
    }

    #[test]
    fn wl_pipe_display() {
        let wl = parse_wiki_link("Foo Bar|the foo");
        assert_eq!(wl.target, "Foo Bar");
        assert_eq!(wl.display, "the foo");
    }

    #[test]
    fn wl_heading_only() {
        let wl = parse_wiki_link("Foo Bar#Background");
        assert_eq!(wl.target, "Foo Bar");
        assert_eq!(wl.heading.as_deref(), Some("Background"));
        assert_eq!(wl.display, "Foo Bar");
    }

    #[test]
    fn wl_heading_and_pipe() {
        let wl = parse_wiki_link("Foo Bar#Background|see here");
        assert_eq!(wl.target, "Foo Bar");
        assert_eq!(wl.heading.as_deref(), Some("Background"));
        assert_eq!(wl.display, "see here");
    }

    #[test]
    fn wl_empty() {
        let wl = parse_wiki_link("");
        assert_eq!(wl.target, "");
        assert_eq!(wl.display, "");
    }

    // ── resolve_link ──────────────────────────────────────────────────────────

    fn simple_index(stems: &[&str]) -> VaultIndex {
        let mut idx = VaultIndex::default();
        for &s in stems {
            idx.register(s, make_ref(&format!("{s}.md"), 1));
        }
        idx
    }

    #[test]
    fn resolve_exact_stem() {
        let idx = simple_index(&["Gillen"]);
        assert!(resolve_link(&parse_wiki_link("Gillen"), &idx).is_some());
    }

    #[test]
    fn resolve_case_insensitive() {
        let idx = simple_index(&["Gillen"]);
        assert!(resolve_link(&parse_wiki_link("gillen"), &idx).is_some());
    }

    #[test]
    fn resolve_path_prefix_stripped() {
        let idx = simple_index(&["Gillen"]);
        assert!(resolve_link(&parse_wiki_link("Characters/Gillen"), &idx).is_some());
    }

    #[test]
    fn resolve_dead_link_returns_none() {
        let idx = simple_index(&[]);
        assert!(resolve_link(&parse_wiki_link("Nobody"), &idx).is_none());
    }

    // ── resolve_wiki_links ────────────────────────────────────────────────────

    fn live(slugs: &[&str]) -> HashSet<Slug> {
        slugs.iter().map(|s| Slug::from_stem(s)).collect()
    }

    #[test]
    fn rwl_known_live_link_becomes_anchor() {
        let idx = simple_index(&["Gillen"]);
        let result = resolve_wiki_links("<p>[[Gillen]]</p>", &idx, &live(&["gillen"]), "/notes/");
        assert!(result.contains(r#"href="/notes/gillen""#));
        assert!(result.contains("Gillen"));
    }

    #[test]
    fn rwl_unindexed_note_becomes_dead_link() {
        let idx = simple_index(&["Gillen"]);
        let result = resolve_wiki_links("<p>[[Gillen]]</p>", &idx, &live(&[]), "/notes/");
        assert!(result.contains("notes-dead-link"));
        assert!(!result.contains("href="));
    }

    #[test]
    fn rwl_unknown_becomes_dead_link() {
        let idx = simple_index(&[]);
        let result = resolve_wiki_links("<p>[[Nobody]]</p>", &idx, &live(&[]), "/notes/");
        assert!(result.contains("notes-dead-link"));
    }

    #[test]
    fn rwl_heading_appended_to_href() {
        let idx = simple_index(&["Gillen"]);
        let result = resolve_wiki_links(
            "<p>[[Gillen#Background]]</p>",
            &idx,
            &live(&["gillen"]),
            "/notes/",
        );
        assert!(result.contains(r#"href="/notes/gillen#background""#));
    }

    #[test]
    fn rwl_pipe_display_used() {
        let idx = simple_index(&["Gillen"]);
        let result = resolve_wiki_links(
            "<p>[[Gillen|the alchemist]]</p>",
            &idx,
            &live(&["gillen"]),
            "/notes/",
        );
        assert!(result.contains("the alchemist"));
        assert!(!result.contains(">Gillen<"));
    }

    #[test]
    fn rwl_unclosed_bracket_passes_through() {
        let idx = simple_index(&[]);
        let result = resolve_wiki_links("[[unclosed", &idx, &live(&[]), "/notes/");
        assert_eq!(result, "[[unclosed");
    }

    #[test]
    fn rwl_xss_blocked_by_pipeline() {
        let idx = simple_index(&["target"]);
        // Simulate pulldown-cmark output: display already escaped
        let html = r#"<p>[[target|&lt;script&gt;alert(1)&lt;/script&gt;]]</p>"#;
        let result = resolve_wiki_links(html, &idx, &live(&["target"]), "/notes/");
        assert!(!result.contains("<script>"));
    }

    // ── has_secret_tag ────────────────────────────────────────────────────────

    #[test]
    fn secret_tag_detected() {
        assert!(has_secret_tag("Some text #secret here"));
    }

    #[test]
    fn secret_tag_not_partial_match() {
        assert!(!has_secret_tag("no #secretive words"));
        assert!(!has_secret_tag("no #secrets here"));
    }

    #[test]
    fn secret_tag_stripped() {
        let result = strip_secret_tag("keep this #secret please");
        assert!(!result.contains("#secret"));
        assert!(result.contains("keep this"));
    }

    // ── split_on_secret_paragraphs ────────────────────────────────────────────

    #[test]
    fn split_no_secrets_single_public_part() {
        let parts = split_on_secret_paragraphs("hello\nworld\n");
        assert_eq!(parts.len(), 1);
        assert!(!parts[0].1);
    }

    #[test]
    fn split_secret_paragraph_marked() {
        let input = "public\n\nsecret #secret\n\npublic again";
        let parts = split_on_secret_paragraphs(input);
        assert_eq!(parts.len(), 3);
        assert!(!parts[0].1);
        assert!(parts[1].1);
        assert!(!parts[2].1);
    }

    #[test]
    fn split_secret_tag_stripped_from_content() {
        let parts = split_on_secret_paragraphs("hidden #secret");
        assert!(!parts[0].0.contains("#secret"));
    }
}
