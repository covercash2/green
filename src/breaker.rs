use askama::Template;
use axum::{
    extract::{Path, State},
    response::Html,
};

use std::sync::Arc;

use crate::{auth::{AuthUserInfo, GmUser}, breaker_detail::{BreakerDetailStore, BreakerSlot}, error::Error, index::NavLink, ServerState};

/// Pre-computed breaker panel HTML content (the circuit layout).
/// Stored in ServerState and used to construct `BreakerPage` per request.
#[derive(Debug, Clone)]
pub struct BreakerContent(pub String);

impl BreakerContent {
    pub fn new(store: &dyn BreakerDetailStore) -> Self {
        BreakerContent(render_from_store(store))
    }
}

#[derive(Debug, Clone, Template)]
#[template(path = "breaker.html")]
pub struct BreakerPage {
    pub content: String,
    pub version: &'static str,
    pub auth_user: Option<AuthUserInfo>,
    pub nav_links: Arc<[NavLink]>,
}

fn breaker_class(label: &str) -> &'static str {
    match label.trim() {
        "" => "breaker-unlabeled",
        "X" | "x" => "breaker-unused",
        "???" => "breaker-unknown",
        _ => "breaker-known",
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn render_slot(
    store: &dyn BreakerDetailStore,
    key: &str,
    row_num: u32,
    side_class: &str,
) -> String {
    let primary_key = store.coupled_primary_of(key);
    let fetch_key = primary_key.unwrap_or(key);

    let label = if let Some(pk) = primary_key {
        store.get(pk).and_then(|s| s.label.as_deref()).unwrap_or("")
    } else {
        store.get(key).and_then(|s| s.label.as_deref()).unwrap_or("")
    };

    // Both coupled slots target the secondary's (lower) detail panel.
    let detail_row = if primary_key.is_some() {
        // This slot is the secondary — use its own row.
        row_num
    } else if let Some(sk) = store.coupled_secondary_of(key) {
        // This slot is the primary — target the secondary's row.
        sk.split_once('-')
            .and_then(|(n, _)| n.parse::<u32>().ok())
            .unwrap_or(row_num)
    } else {
        row_num
    };

    let mut extra_class = String::new();
    if primary_key.is_some() {
        extra_class.push_str(" breaker-coupled-secondary");
    } else if store.is_coupled_primary(key) {
        extra_class.push_str(" breaker-coupled-primary");
    }

    let text = if label.is_empty() {
        "—".to_string()
    } else {
        html_escape(label)
    };
    let base_class = breaker_class(label);

    format!(
        r##"<div class="breaker-slot {side_class} {base_class}{extra_class} breaker-clickable" onclick="var k='{fetch_key}',d=document.getElementById('breaker-detail-{detail_row}');if(d.dataset.activeKey===k){{d.classList.remove('breaker-opening');d.classList.add('breaker-closing');setTimeout(function(){{d.innerHTML='';d.dataset.activeKey='';d.classList.remove('breaker-closing');}},150);}}else{{d.dataset.activeKey=k;fetch('/api/breaker/'+k).then(function(r){{return r.text()}}).then(function(h){{d.classList.remove('breaker-closing');d.innerHTML=h;d.classList.remove('breaker-opening');d.offsetHeight;d.classList.add('breaker-opening');}});}}"><span class="breaker-label">{text}</span></div>"##
    )
}

fn render_from_store(store: &dyn BreakerDetailStore) -> String {
    let mut output = String::new();
    output.push_str(r#"<div class="breaker-panel"><div class="breaker-slots">"#);

    for row_num in 1..=store.row_count() {
        let left_key = format!("{row_num}-left");
        let right_key = format!("{row_num}-right");

        output.push_str(&render_slot(store, &left_key, row_num, "breaker-slot-left"));
        output.push_str(&format!(
            r#"<div class="breaker-row-num">{row_num}</div>"#
        ));
        output.push_str(&render_slot(store, &right_key, row_num, "breaker-slot-right"));
        output.push_str(&format!(
            r#"<div id="breaker-detail-{row_num}" class="breaker-row-detail"></div>"#
        ));
    }

    output.push_str("</div></div>");

    let todos = store.todos();
    if !todos.is_empty() {
        output.push_str(r#"<div class="breaker-todos"><h3>needs labeling</h3><ul>"#);
        for todo in todos {
            output.push_str(&format!(r#"<li>{}</li>"#, html_escape(todo)));
        }
        output.push_str("</ul></div>");
    }

    output
}

pub async fn breaker_route(
    user: GmUser,
    State(state): State<ServerState>,
) -> Result<Html<String>, Error> {
    let auth_user = Some(AuthUserInfo {
        username: user.0.username.clone(),
        role: user.0.role.clone(),
    });
    let page = BreakerPage {
        content: state.breaker_content.0.clone(),
        version: crate::VERSION,
        auth_user,
        nav_links: state.nav_links.clone(),
    };
    Ok(Html(page.render()?))
}

#[derive(Template)]
#[template(path = "partials/breaker_detail.html")]
pub struct BreakerDetailTemplate<'a> {
    pub detail: Option<&'a BreakerSlot>,
}

pub async fn breaker_detail_route(
    _user: GmUser,
    Path(key): Path<String>,
    State(state): State<ServerState>,
) -> Result<Html<String>, Error> {
    let lookup_key = state
        .breaker_detail_store
        .coupled_primary_of(&key)
        .unwrap_or(&key)
        .to_owned();
    let detail = state.breaker_detail_store.get(&lookup_key).filter(|s| {
        s.amperage.is_some() || s.devices.is_some() || s.notes.is_some()
    });
    Ok(Html(BreakerDetailTemplate { detail }.render()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::breaker_detail::{BreakerData, BreakerSlot, BreakerStore};
    use std::collections::HashMap;

    fn make_store(slots: HashMap<String, BreakerSlot>, todos: Vec<String>) -> BreakerStore {
        BreakerStore::from_data(BreakerData {
            todos,
            slots,
            couples: vec![],
        })
        .unwrap()
    }

    fn slot(label: &str) -> BreakerSlot {
        BreakerSlot {
            label: Some(label.into()),
            amperage: None,
            devices: None,
            notes: None,
        }
    }

    // ── breaker_class ──────────────────────────────────────────────────────────

    #[test]
    fn class_empty_string() {
        assert_eq!(breaker_class(""), "breaker-unlabeled");
    }

    #[test]
    fn class_whitespace_only() {
        assert_eq!(breaker_class("   "), "breaker-unlabeled");
    }

    #[test]
    fn class_x_uppercase() {
        assert_eq!(breaker_class("X"), "breaker-unused");
    }

    #[test]
    fn class_x_lowercase() {
        assert_eq!(breaker_class("x"), "breaker-unused");
    }

    #[test]
    fn class_unknown_marker() {
        assert_eq!(breaker_class("???"), "breaker-unknown");
    }

    #[test]
    fn class_known_label() {
        assert_eq!(breaker_class("garage"), "breaker-known");
        assert_eq!(breaker_class("kitchen lights"), "breaker-known");
    }

    // ── html_escape ────────────────────────────────────────────────────────────

    #[test]
    fn escape_ampersand() {
        assert_eq!(html_escape("a&b"), "a&amp;b");
    }

    #[test]
    fn escape_angle_brackets() {
        assert_eq!(html_escape("<div>"), "&lt;div&gt;");
    }

    #[test]
    fn escape_double_quote() {
        assert_eq!(html_escape(r#"say "hi""#), "say &quot;hi&quot;");
    }

    #[test]
    fn escape_no_special_chars() {
        assert_eq!(html_escape("plain text"), "plain text");
    }

    #[test]
    fn escape_all_specials() {
        assert_eq!(html_escape(r#"<a href="x&y">"#), "&lt;a href=&quot;x&amp;y&quot;&gt;");
    }

    // ── render_from_store ──────────────────────────────────────────────────────

    #[test]
    fn render_empty_store_has_panel_wrapper() {
        let store = make_store(HashMap::new(), vec![]);
        let html = render_from_store(&store);
        assert!(html.contains(r#"class="breaker-panel""#));
        assert!(html.contains(r#"class="breaker-slots""#));
        assert!(!html.contains("breaker-todos"));
    }

    #[test]
    fn render_one_row_contains_label() {
        let mut slots = HashMap::new();
        let _ = slots.insert("1-left".into(), slot("kitchen"));
        let store = make_store(slots, vec![]);
        let html = render_from_store(&store);
        assert!(html.contains("kitchen"));
        assert!(html.contains("breaker-row-num"));
    }

    #[test]
    fn render_todos_section_when_present() {
        let store = make_store(HashMap::new(), vec!["unlabeled breaker 7".into()]);
        let html = render_from_store(&store);
        assert!(html.contains("breaker-todos"));
        assert!(html.contains("unlabeled breaker 7"));
    }

    #[test]
    fn render_label_html_escaped() {
        let mut slots = HashMap::new();
        let _ = slots.insert("1-left".into(), slot("A&C"));
        let store = make_store(slots, vec![]);
        let html = render_from_store(&store);
        assert!(html.contains("A&amp;C"));
        assert!(!html.contains("A&C\""));
    }

    #[test]
    fn render_empty_label_shows_em_dash() {
        let mut slots = HashMap::new();
        let _ = slots.insert(
            "1-left".into(),
            BreakerSlot {
                label: None,
                amperage: None,
                devices: None,
                notes: None,
            },
        );
        let store = make_store(slots, vec![]);
        let html = render_from_store(&store);
        assert!(html.contains('—'));
    }

    // ── BreakerContent ─────────────────────────────────────────────────────────

    #[test]
    fn breaker_content_new_wraps_panel() {
        let store = make_store(HashMap::new(), vec![]);
        let content = BreakerContent::new(&store);
        assert!(content.0.contains("breaker-panel"));
    }
}
