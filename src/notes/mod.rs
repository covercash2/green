pub mod dnd;
pub mod obsidian;
pub mod recipes;

pub use dnd::NoteVault;
pub use obsidian::Slug;

// ─── RenderedHtml ─────────────────────────────────────────────────────────────

/// HTML that has been produced by our rendering pipeline and is safe to
/// inject into templates with `|safe`.
///
/// The only way to obtain a `RenderedHtml` is via [`render_note_body_redacted`],
/// [`render_note_body_revealed`], or the lower-level [`render_markdown`]. All
/// three go through pulldown-cmark with raw-HTML sanitisation, so no raw user
/// input can reach the template. This makes `|safe` in Askama templates
/// self-documenting and auditable.
#[derive(Debug, Clone)]
pub struct RenderedHtml(pub(super) String);

impl RenderedHtml {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }

    /// Construct a `RenderedHtml` directly from a static placeholder string.
    ///
    /// This is only for use by sibling modules (e.g. `recipes`) that produce
    /// the same server-side secret placeholder without going through the full
    /// render pipeline.  The caller is responsible for ensuring the string is
    /// safe HTML.
    pub(crate) fn from_placeholder(html: &'static str) -> Self {
        Self(html.to_owned())
    }

    /// Construct a `RenderedHtml` from an owned `String` of already-safe HTML.
    ///
    /// Intended for sibling modules that post-process the output of the render
    /// pipeline (e.g. wiki-link resolution on rendered HTML).
    pub(crate) fn from_html(html: String) -> Self {
        Self(html)
    }
}

// ─── Rendering helpers ────────────────────────────────────────────────────────

/// Escape the five HTML-special characters so that untrusted text is safe to
/// embed in an HTML attribute value or element content.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            c => out.push(c),
        }
    }
    out
}

/// Render Markdown to trusted HTML. The returned [`RenderedHtml`] is the
/// only public surface of this transformation — callers cannot construct
/// one independently.
///
/// Raw HTML in Markdown (`Event::Html` / `Event::InlineHtml`) is converted to
/// `Event::Text` before rendering so that pulldown-cmark HTML-escapes it. This
/// prevents a note author from injecting arbitrary HTML/JS through literal
/// `<script>` blocks or inline tags, preserving the `RenderedHtml` safety
/// guarantee.
pub(crate) fn render_markdown(md: &str) -> RenderedHtml {
    use pulldown_cmark::{Event, Options, Parser, html};

    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let parser = Parser::new_ext(md, opts).map(|event| match event {
        // Treat raw HTML as plain text so pulldown-cmark escapes it on output.
        Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
        other => other,
    });
    let mut output = String::new();
    html::push_html(&mut output, parser);
    RenderedHtml(output)
}

/// Server-side placeholder emitted in place of every secret paragraph for non-GM viewers.
/// The secret text is never included in the response.
pub(crate) const SECRET_PLACEHOLDER: &str = "<p class=\"notes-redacted\">🔒 redacted</p>\n";

/// Render the note body for a non-GM viewer.
///
/// Secret paragraphs (inline `#secret` tag) are replaced with
/// [`SECRET_PLACEHOLDER`] — the secret text is **never sent to the browser**.
/// Public paragraphs are rendered as normal Markdown HTML.
///
/// Returns `(html, has_secrets)`.
pub(crate) fn render_note_body_redacted(text: &str) -> (RenderedHtml, bool) {
    let parts = obsidian::split_on_secret_paragraphs(text);
    let has_secrets = parts.iter().any(|(_, is_secret)| *is_secret);
    let mut output = String::new();
    for (content, is_secret) in &parts {
        if *is_secret {
            output.push_str(SECRET_PLACEHOLDER);
        } else {
            output.push_str(render_markdown(content).as_str());
        }
    }
    (RenderedHtml(output), has_secrets)
}

/// Render the full note body for a GM viewer: all paragraphs, including secret
/// ones, rendered as plain Markdown HTML with no redaction or wrapper.
pub(crate) fn render_note_body_revealed(text: &str) -> RenderedHtml {
    let parts = obsidian::split_on_secret_paragraphs(text);
    let mut output = String::new();
    for (content, _is_secret) in &parts {
        let rendered = render_markdown(content);
        output.push_str(rendered.as_str());
    }
    RenderedHtml(output)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use super::obsidian::{Frontmatter, parse_frontmatter};
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;

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
        let (fm, rest) = parse_frontmatter::<Frontmatter>(content);
        assert_eq!(fm.title.as_deref(), Some("My Note"));
        assert!(fm.tags.contains(&"world".to_string()));
        assert!(rest.starts_with("# Body"));
    }

    #[test]
    fn parse_fm_no_frontmatter() {
        let content = "# Just a heading\nSome text.";
        let (fm, rest) = parse_frontmatter::<Frontmatter>(content);
        assert!(fm.title.is_none());
        assert!(fm.tags.is_empty());
        assert_eq!(rest, content);
    }

    #[test]
    fn parse_fm_malformed_yaml() {
        let content = "---\ntitle: [unclosed\n---\nbody text";
        let (fm, rest) = parse_frontmatter::<Frontmatter>(content);
        assert!(fm.title.is_none());
        assert!(rest.contains("body text"));
    }

    #[test]
    fn parse_fm_empty_frontmatter() {
        let content = "---\n---\nbody here";
        let (fm, rest) = parse_frontmatter::<Frontmatter>(content);
        assert!(fm.title.is_none());
        assert!(fm.tags.is_empty());
        assert_eq!(rest, "body here");
    }

    #[test]
    fn parse_fm_leading_whitespace_stripped() {
        let content = "  ---\ntitle: Hi\n---\nbody";
        let (fm, rest) = parse_frontmatter::<Frontmatter>(content);
        assert_eq!(fm.title.as_deref(), Some("Hi"));
        assert_eq!(rest, "body");
    }

    #[test]
    fn parse_fm_secret_tag_in_tags_vec() {
        let content = "---\ntags: [world, secret]\n---\nbody";
        let (fm, _rest) = parse_frontmatter::<Frontmatter>(content);
        assert!(fm.tags.iter().any(|t| t == "secret"));
    }

    #[test]
    fn parse_fm_no_secret_tag_when_absent() {
        let content = "---\ntags: [world]\n---\nbody";
        let (fm, _rest) = parse_frontmatter::<Frontmatter>(content);
        assert!(!fm.tags.iter().any(|t| t == "secret"));
    }

    // ── render pipeline: wiki-link XSS and entity escaping ────────────────────
    // Pure wiki-link resolution tests live in obsidian.rs. These tests cover the
    // render_markdown → resolve_wiki_links pipeline (pulldown-cmark escaping first).

    fn test_vault_index(stems: &[&str]) -> (obsidian::VaultIndex, HashSet<Slug>) {
        let mut idx = obsidian::VaultIndex::default();
        let mut live = HashSet::new();
        for &s in stems {
            let slug = Slug::from_stem(s);
            let note_ref = obsidian::NoteRef::new(
                PathBuf::from(format!("{s}.md")),
                slug.clone(),
            );
            idx.register(s, note_ref);
            let _ = live.insert(slug);
        }
        (idx, live)
    }

    #[test]
    fn xss_in_wiki_link_display_blocked_by_pipeline() {
        let (idx, live) = test_vault_index(&["target"]);
        let rendered = render_markdown(r#"[[target|<script>alert(1)</script>]]"#);
        let result = obsidian::resolve_wiki_links(rendered.as_str(), &idx, &live, "/notes/");
        assert!(!result.contains("<script>"), "raw <script> must not appear");
        assert!(result.contains("&lt;script&gt;"));
    }

    #[test]
    fn xss_in_dead_link_display_blocked_by_pipeline() {
        let (idx, live) = test_vault_index(&[]);
        let rendered = render_markdown(r#"[[Unknown|<img src=x onerror=alert(1)>]]"#);
        let result = obsidian::resolve_wiki_links(rendered.as_str(), &idx, &live, "/notes/");
        assert!(!result.contains("<img"), "raw <img> must not appear");
        assert!(result.contains("&lt;img"));
    }

    #[test]
    fn resolve_html_entities_in_display_text() {
        let (idx, live) = test_vault_index(&["target"]);
        let rendered = render_markdown(r#"[[target|A & B "quoted"]]"#);
        let result = obsidian::resolve_wiki_links(rendered.as_str(), &idx, &live, "/notes/");
        // pulldown-cmark escapes & → &amp; but leaves " unescaped in element content.
        assert!(result.contains("A &amp; B"));
        assert!(!result.contains("A & B"));
    }

    #[test]
    fn escape_html_encodes_all_special_chars() {
        assert_eq!(escape_html(r#"<>&"'"#), "&lt;&gt;&amp;&quot;&#x27;");
    }

    // ── split_on_secret_paragraphs ────────────────────────────────────────────

    #[test]
    fn split_no_secrets_returns_single_public_part() {
        let parts = obsidian::split_on_secret_paragraphs("hello\nworld\n");
        assert_eq!(parts.len(), 1);
        assert!(!parts[0].1, "should be public");
        assert!(parts[0].0.contains("hello"));
    }

    #[test]
    fn split_empty_input_returns_empty() {
        assert!(obsidian::split_on_secret_paragraphs("").is_empty());
    }

    #[test]
    fn split_one_secret_paragraph() {
        let text = "before\n\nhidden content #secret\n\nafter\n";
        let parts = obsidian::split_on_secret_paragraphs(text);
        assert_eq!(parts.len(), 3);
        assert!(!parts[0].1 && parts[0].0.contains("before"));
        assert!(parts[1].1 && parts[1].0.contains("hidden content"));
        assert!(!parts[2].1 && parts[2].0.contains("after"));
    }

    #[test]
    fn split_secret_tag_stripped_from_output() {
        let parts = obsidian::split_on_secret_paragraphs("GM only info #secret");
        assert_eq!(parts.len(), 1);
        assert!(parts[0].1, "should be secret");
        assert!(!parts[0].0.contains("#secret"), "tag should be stripped");
        assert!(parts[0].0.contains("GM only info"));
    }

    #[test]
    fn split_multiple_secret_paragraphs() {
        let text = "pub1\n\nsec1 #secret\n\npub2\n\nsec2 #secret\n\npub3\n";
        let parts = obsidian::split_on_secret_paragraphs(text);
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
        let parts = obsidian::split_on_secret_paragraphs(text);
        assert!(parts.iter().any(|(c, s)| *s && c.contains("hidden")));
        assert!(parts.iter().any(|(c, s)| !s && c.contains("public")));
    }

    #[test]
    fn split_inline_tag_does_not_match_partial_word() {
        // `#secrets` and `#secretive` must NOT be treated as the `#secret` tag.
        let parts = obsidian::split_on_secret_paragraphs("these are #secrets and #secretive things");
        assert_eq!(parts.len(), 1);
        assert!(
            !parts[0].1,
            "partial-word tags must not mark paragraph as secret"
        );
    }

    #[test]
    fn split_only_secret_paragraph() {
        let text = "all secret #secret\n";
        let parts = obsidian::split_on_secret_paragraphs(text);
        assert_eq!(parts.len(), 1);
        assert!(parts[0].1);
    }

    // ── render_markdown (HTML sanitisation) ──────────────────────────────────

    #[test]
    fn render_markdown_escapes_raw_html_block() {
        let html = render_markdown("<script>alert(1)</script>\n");
        assert!(
            !html.as_str().contains("<script>"),
            "raw <script> must not pass through"
        );
        assert!(html.as_str().contains("&lt;script&gt;"));
    }

    #[test]
    fn render_markdown_escapes_inline_html() {
        let html = render_markdown("Hello <b>world</b> text");
        assert!(
            !html.as_str().contains("<b>"),
            "inline HTML must not pass through"
        );
        assert!(html.as_str().contains("&lt;b&gt;"));
    }

    #[test]
    fn render_markdown_escapes_script_injection_via_inline_html() {
        let html = render_markdown("Click <img src=x onerror=alert(1)>");
        assert!(
            !html.as_str().contains("<img"),
            "raw <img> must not pass through"
        );
        assert!(html.as_str().contains("&lt;img"));
    }

    #[test]
    fn render_markdown_normal_formatting_unaffected() {
        let html = render_markdown("**bold** and _italic_");
        assert!(html.as_str().contains("<strong>bold</strong>"));
        assert!(html.as_str().contains("<em>italic</em>"));
    }

    // ── render_note_body ──────────────────────────────────────────────────────

    #[test]
    fn render_body_redacts_secret_paragraph() {
        let text = "before\n\nhidden content #secret\n\nafter\n";
        let (html, has_secrets) = render_note_body_redacted(text);
        let s = html.as_str();
        assert!(has_secrets);
        // Secret text must not appear in the player HTML at all.
        assert!(
            !s.contains("hidden content"),
            "secret text must not be sent to browser"
        );
        // Placeholder is present instead.
        assert!(s.contains("notes-redacted"), "placeholder must be present");
        // Public content is still rendered.
        assert!(s.contains("before") && s.contains("after"));
    }

    #[test]
    fn render_body_public_content_not_redacted() {
        let text = "public content\n\nhidden #secret\n";
        let (html, _) = render_note_body_redacted(text);
        let s = html.as_str();
        assert!(
            s.contains("public content"),
            "public paragraphs must be present"
        );
        assert!(
            !s.contains("hidden"),
            "secret paragraph text must be absent"
        );
    }

    #[test]
    fn render_body_no_secrets_has_secrets_false() {
        let (_, has_secrets) = render_note_body_redacted("just public content\n");
        assert!(!has_secrets);
    }

    #[test]
    fn render_body_revealed_includes_secret_text() {
        let text = "**bold secret** #secret\n";
        let html = render_note_body_revealed(text);
        // GM view: secret text is rendered (no redaction).
        assert!(
            html.as_str().contains("bold secret"),
            "GM view must include secret text"
        );
    }
}
