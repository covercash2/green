use askama::Template;
use axum::{extract::State, response::Html};
use pulldown_cmark::{html, Event, Options, Parser, Tag, TagEnd};

use crate::{error::Error, ServerState};

#[derive(Debug, Clone, Template)]
#[template(path = "breaker.html")]
pub struct BreakerPage {
    pub content: String,
    pub version: &'static str,
}

impl BreakerPage {
    pub fn new(markdown: &str) -> Self {
        let content = render_content(markdown);
        BreakerPage {
            content,
            version: crate::VERSION,
        }
    }
}

fn breaker_class(text: &str) -> &'static str {
    match text.trim() {
        "" => "breaker-empty",
        "X" | "x" => "breaker-off",
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

fn render_content(markdown: &str) -> String {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let parser = Parser::new_ext(markdown, options);

    let mut output = String::new();
    let mut in_table = false;
    let mut in_table_head = false;
    let mut in_row = false;
    let mut in_cell = false;
    let mut cells: Vec<String> = Vec::new();
    let mut current_cell = String::new();
    let mut row_num: u32 = 0;

    for event in parser {
        match event {
            Event::Start(Tag::Table(_)) => {
                in_table = true;
                output.push_str(r#"<div class="breaker-panel"><div class="breaker-slots">"#);
            }
            Event::End(TagEnd::Table) => {
                in_table = false;
                row_num = 0;
                output.push_str("</div></div>");
            }
            Event::Start(Tag::TableHead) => {
                in_table_head = true;
            }
            Event::End(TagEnd::TableHead) => {
                in_table_head = false;
            }
            Event::Start(Tag::TableRow) if !in_table_head => {
                in_row = true;
                cells.clear();
                current_cell.clear();
            }
            Event::End(TagEnd::TableRow) if in_row => {
                in_row = false;
                row_num += 1;

                // columns: | # | left | right |
                // cells[0] = row number (ignored, we use row_num)
                // cells[1] = left circuit
                // cells[2] = right circuit
                let left = cells.get(1).map(|s| s.trim()).unwrap_or("");
                let right = cells.get(2).map(|s| s.trim()).unwrap_or("");

                let left_class = breaker_class(left);
                let right_class = breaker_class(right);
                let left_text = if left.is_empty() {
                    "—".to_string()
                } else {
                    html_escape(left)
                };
                let right_text = if right.is_empty() {
                    "—".to_string()
                } else {
                    html_escape(right)
                };

                output.push_str(&format!(
                    r#"<div class="breaker-slot breaker-slot-left {left_class}"><span class="breaker-label">{left_text}</span></div>"#
                ));
                output.push_str(&format!(
                    r#"<div class="breaker-row-num">{row_num}</div>"#
                ));
                output.push_str(&format!(
                    r#"<div class="breaker-slot breaker-slot-right {right_class}"><span class="breaker-label">{right_text}</span></div>"#
                ));
            }
            Event::Start(Tag::TableCell) if in_row => {
                in_cell = true;
                current_cell.clear();
            }
            Event::End(TagEnd::TableCell) if in_cell => {
                in_cell = false;
                cells.push(current_cell.trim().to_string());
            }
            Event::Text(text) if in_cell => {
                current_cell.push_str(&text);
            }
            // skip all other events while inside a table (header cells, etc.)
            _ if in_table => {}
            // render non-table content normally
            event => {
                html::push_html(&mut output, std::iter::once(event));
            }
        }
    }

    output
}

pub async fn breaker_route(State(state): State<ServerState>) -> Result<Html<String>, Error> {
    Ok(Html(state.breaker_page.render()?))
}
