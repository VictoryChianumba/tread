//! HTML → tread `Block` conversion.
//!
//! Walks the DOM produced by `scraper` (which uses `html5ever`
//! underneath — the standard Rust HTML parser) and emits the same
//! Block tree the arxiv / markdown / pdf / epub paths produce.
//! Used by `PaperData::from_html` to support web articles, RSS
//! `<content:encoded>` bodies, GitHub READMEs rendered as HTML,
//! and any other host content already in HTML form.
//!
//! Block-level mapping:
//!   <h1>..<h6>         → Block::Header { level: 1..3 clamped }
//!   <p>                → Block::StyledLine(spans)
//!   <blockquote>       → Block::Quote(spans)
//!   <ul> / <ol> > <li> → Block::ListItem { depth, marker, content }
//!   <pre><code>        → Block::CodeBlock { lang, lines }
//!   <hr>               → Block::Rule
//!   <img>              → italic [Image: alt] placeholder span
//!                          (tread doesn't fetch image bytes — host
//!                          responsibility for now)
//!   <br>               → soft-break inside the current span run
//!
//! Inline mapping:
//!   <strong>, <b>      → bold
//!   <em>, <i>          → italic
//!   <code>             → monospace
//!   <s>, <del>, <strike> → strikethrough
//!   <a href>           → link span (carries `url`)
//!
//! Stripped (no visible output):
//!   <script>, <style>, <noscript>, <head>, <meta>, <link>,
//!   <iframe>, <object>, <embed>, <svg>, <math>
//!
//! Tables: `scraper` exposes the structure but tread's `Matrix`
//! block expects LaTeX-shaped cells.  v2 backlog — for now tables
//! flatten to a sequence of paragraphs (each row becomes one).

use std::collections::HashSet;

use doc_model::{Block, InlineSpan};
use scraper::{ElementRef, Html, Node};

/// Parse an HTML document and return tread blocks in document order.
/// Empty input yields no blocks (not an error).  Malformed HTML is
/// repaired by html5ever per the standard browser-style algorithm,
/// so this never errors — at worst it produces empty output.
pub fn html_to_blocks(html: &str) -> Vec<Block> {
    let doc = Html::parse_document(html);
    let mut walker = Walker::default();
    // Find <body>, fall back to root if absent.
    let body = doc
        .root_element()
        .descendants()
        .find_map(|n| {
            ElementRef::wrap(n).and_then(|el| {
                if el.value().name() == "body" {
                    Some(el)
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| doc.root_element());
    walker.walk_children(body);
    walker.flush_paragraph();
    walker.blocks
}

#[derive(Default)]
struct Walker {
    blocks: Vec<Block>,
    pending: Vec<InlineSpan>,
    style: Style,
    link_url: Option<String>,
    /// Stack of list ordinal counters.  None = unordered (use bullet),
    /// Some(n) = ordered and n is the next item number.
    list_stack: Vec<Option<u64>>,
    /// True while we're inside <pre>; relevant for `<code>` so it
    /// emits a CodeBlock instead of inline monospace.
    in_pre: bool,
    /// Skip-recursion stack for elements like <script> whose text
    /// content should be dropped.
    skip_depth: u32,
    /// True while we're inside <blockquote>; <p>/<div>/etc. defer
    /// their flush so the surrounding blockquote sees the spans.
    in_quote: bool,
    /// Active table being built.  `None` outside any <table>; `Some`
    /// from <table> open to </table> close.  Cell content is
    /// collected as plain text (Block::Matrix's shape) — inline
    /// styles inside cells are deliberately dropped, matching how
    /// the arxiv path renders.  Nested tables are rare and currently
    /// flatten to outer-cell text.
    table: Option<TableState>,
}

#[derive(Default)]
struct TableState {
    rows: Vec<Vec<(String, usize)>>,
    current_row: Vec<(String, usize)>,
    /// Text accumulator for the current <td>/<th>.
    current_cell: String,
}

#[derive(Default, Clone, Copy)]
struct Style {
    bold: bool,
    italic: bool,
    monospace: bool,
    strikethrough: bool,
}

fn skipped_tags() -> &'static HashSet<&'static str> {
    use std::sync::OnceLock;
    static TAGS: OnceLock<HashSet<&'static str>> = OnceLock::new();
    TAGS.get_or_init(|| {
        [
            "script", "style", "noscript", "head", "meta", "link", "iframe", "object", "embed",
            "svg", "math", "template", "title", // page title is metadata, not body content
        ]
        .iter()
        .copied()
        .collect()
    })
}

impl Walker {
    fn walk_children(&mut self, parent: ElementRef) {
        for child in parent.children() {
            self.walk_node(child);
        }
    }

    fn walk_node(&mut self, node: ego_tree::NodeRef<Node>) {
        if self.skip_depth > 0 {
            // Inside a skipped subtree — don't emit anything but still
            // descend so the depth counter increments correctly when we
            // hit nested skip tags (rare but defensible).
            if let Some(el) = ElementRef::wrap(node) {
                if skipped_tags().contains(el.value().name()) {
                    self.skip_depth += 1;
                }
                for child in el.children() {
                    self.walk_node(child);
                }
                if skipped_tags().contains(el.value().name()) {
                    self.skip_depth -= 1;
                }
            }
            return;
        }

        match node.value() {
            Node::Text(text) => {
                let raw = text.to_string();
                let collapsed = if self.in_pre {
                    raw
                } else {
                    collapse_whitespace(&raw)
                };
                if !collapsed.is_empty() {
                    self.push_text(collapsed);
                }
            }
            Node::Element(_) => {
                let Some(el) = ElementRef::wrap(node) else {
                    return;
                };
                let name = el.value().name().to_ascii_lowercase();
                self.handle_element(el, &name);
            }
            _ => {}
        }
    }

    fn handle_element(&mut self, el: ElementRef, name: &str) {
        if skipped_tags().contains(name) {
            self.skip_depth += 1;
            self.walk_children(el);
            self.skip_depth -= 1;
            return;
        }

        match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush_paragraph();
                let level = match name {
                    "h1" => 1,
                    "h2" => 2,
                    _ => 3,
                };
                // Collect heading text (no styled spans — Block::Header is plain text).
                let mut buf = String::new();
                collect_text(el, &mut buf, /* in_pre */ false);
                let text = buf.trim().to_string();
                if !text.is_empty() {
                    self.blocks.push(Block::Header { level, text });
                    self.blocks.push(Block::Blank);
                }
            }
            "p" | "div" | "section" | "article" | "main" | "header" | "footer" | "aside"
            | "nav" | "figure" => {
                // <p> is the canonical block; the others are containers
                // we recurse into but also flush before/after so their
                // contents form a coherent paragraph break.  This handles
                // sites that use <div> for paragraph layout.  Inside a
                // <blockquote> the flush is deferred so the surrounding
                // quote captures the spans.
                if !self.in_quote {
                    self.flush_paragraph();
                }
                self.walk_children(el);
                if !self.in_quote {
                    self.flush_paragraph();
                }
            }
            "blockquote" => {
                self.flush_paragraph();
                let saved = std::mem::take(&mut self.pending);
                let prev_in_quote = self.in_quote;
                self.in_quote = true;
                self.walk_children(el);
                self.in_quote = prev_in_quote;
                let quote_spans = std::mem::take(&mut self.pending);
                self.pending = saved;
                if !quote_spans.is_empty() {
                    self.blocks.push(Block::Quote(quote_spans));
                    self.blocks.push(Block::Blank);
                }
            }
            "ul" => {
                self.flush_paragraph();
                self.list_stack.push(None);
                self.walk_children(el);
                self.list_stack.pop();
                if self.list_stack.is_empty() {
                    self.blocks.push(Block::Blank);
                }
            }
            "ol" => {
                self.flush_paragraph();
                // `start` attribute could override but we keep it simple.
                self.list_stack.push(Some(1));
                self.walk_children(el);
                self.list_stack.pop();
                if self.list_stack.is_empty() {
                    self.blocks.push(Block::Blank);
                }
            }
            "li" => {
                let depth = self.list_stack.len().saturating_sub(1) as u8;
                let marker = match self.list_stack.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        m
                    }
                    _ => "• ".to_string(),
                };
                // Save the surrounding paragraph state (rare but possible
                // for nested lists), collect this item's spans, restore.
                let saved = std::mem::take(&mut self.pending);
                self.walk_children(el);
                let item_spans = std::mem::take(&mut self.pending);
                self.pending = saved;
                if !item_spans.is_empty() {
                    self.blocks.push(Block::ListItem {
                        depth,
                        marker,
                        content: item_spans,
                    });
                }
            }
            "pre" => {
                self.flush_paragraph();
                // Detect language from a child <code class="language-rust">
                // (Markdown-converted HTML and most syntax-highlighted
                // pages use this convention).
                let lang = el
                    .children()
                    .filter_map(ElementRef::wrap)
                    .find(|c| c.value().name() == "code")
                    .and_then(|c| {
                        c.value().attr("class").and_then(|cls| {
                            cls.split_whitespace()
                                .find_map(|c| c.strip_prefix("language-").map(str::to_string))
                        })
                    });
                let mut buf = String::new();
                collect_text(el, &mut buf, /* in_pre */ true);
                // Trim a single trailing newline (common from <pre> source);
                // keep leading whitespace which may be intentional indent.
                if buf.ends_with('\n') {
                    buf.pop();
                }
                let lines: Vec<String> = buf.split('\n').map(|s| s.to_string()).collect();
                self.blocks.push(Block::CodeBlock { lang, lines });
                self.blocks.push(Block::Blank);
            }
            "code" => {
                // Inline-only here — block-level <pre><code> handled above.
                self.style.monospace = true;
                self.walk_children(el);
                self.style.monospace = false;
            }
            "hr" => {
                self.flush_paragraph();
                self.blocks.push(Block::Rule);
            }
            "br" => {
                if let Some(last) = self.pending.last_mut() {
                    if !last.text.ends_with(' ') {
                        last.text.push(' ');
                    }
                }
            }
            "strong" | "b" => {
                let prev = self.style.bold;
                self.style.bold = true;
                self.walk_children(el);
                self.style.bold = prev;
            }
            "em" | "i" => {
                let prev = self.style.italic;
                self.style.italic = true;
                self.walk_children(el);
                self.style.italic = prev;
            }
            "s" | "del" | "strike" => {
                let prev = self.style.strikethrough;
                self.style.strikethrough = true;
                self.walk_children(el);
                self.style.strikethrough = prev;
            }
            "a" => {
                let prev = self.link_url.clone();
                self.link_url = el.value().attr("href").map(str::to_string);
                self.walk_children(el);
                self.link_url = prev;
            }
            "img" => {
                let alt = el.value().attr("alt").unwrap_or("").to_string();
                let label = if alt.is_empty() {
                    "[Image]".to_string()
                } else {
                    format!("[Image: {alt}]")
                };
                self.pending.push(InlineSpan {
                    text: label,
                    italic: true,
                    ..InlineSpan::default()
                });
            }
            "table" => {
                self.flush_paragraph();
                // Nested tables: skip the inner one entirely — Matrix doesn't
                // nest, and rendering the cells of the outer table doesn't
                // need the inner structure.
                if self.table.is_some() {
                    return;
                }
                self.table = Some(TableState::default());
                self.walk_children(el);
                if let Some(state) = self.table.take() {
                    if !state.rows.is_empty() {
                        self.blocks.push(Block::Matrix {
                            rows: state.rows,
                            vertical_rules: Vec::new(), // HTML has no per-column rules
                        });
                        self.blocks.push(Block::Blank);
                    }
                }
            }
            "thead" | "tbody" | "tfoot" | "colgroup" => {
                self.walk_children(el);
            }
            "tr" => {
                if let Some(state) = self.table.as_mut() {
                    state.current_row.clear();
                }
                self.walk_children(el);
                if let Some(state) = self.table.as_mut() {
                    if !state.current_row.is_empty() {
                        let row = std::mem::take(&mut state.current_row);
                        state.rows.push(row);
                    }
                }
            }
            "td" | "th" => {
                if self.table.is_some() {
                    let colspan = el
                        .value()
                        .attr("colspan")
                        .and_then(|s| s.parse::<usize>().ok())
                        .unwrap_or(1)
                        .max(1);
                    // Collect cell text via the existing helper, which already
                    // handles <br> as newline and skips script/style.
                    let mut buf = String::new();
                    collect_text(el, &mut buf, /* in_pre */ false);
                    let text = buf.trim().to_string();
                    if let Some(state) = self.table.as_mut() {
                        state.current_row.push((text, colspan));
                        state.current_cell.clear();
                    }
                } else {
                    // Stray <td>/<th> outside a <table> — render as a regular
                    // paragraph so we don't drop content silently.  The bold
                    // applies for <th>; <td> stays unstyled.
                    let prev = self.style.bold;
                    if name == "th" {
                        self.style.bold = true;
                    }
                    self.walk_children(el);
                    self.style.bold = prev;
                }
            }
            // Default: recurse with current state — handles spans, divs,
            // and any tag we don't have a specific case for.
            _ => self.walk_children(el),
        }
    }

    fn push_text(&mut self, text: String) {
        self.pending.push(InlineSpan {
            text,
            bold: self.style.bold,
            italic: self.style.italic,
            monospace: self.style.monospace,
            strikethrough: self.style.strikethrough,
            url: self.link_url.clone(),
            ..InlineSpan::default()
        });
    }

    fn flush_paragraph(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        // Drop a trailing " | " separator left by the last cell of a row.
        if let Some(last) = self.pending.last_mut() {
            if last.text == " | " {
                self.pending.pop();
            }
        }
        if self.pending.is_empty() {
            return;
        }
        let spans = std::mem::take(&mut self.pending);
        self.blocks.push(Block::StyledLine(spans));
        self.blocks.push(Block::Blank);
    }
}

/// Recursively collect text content of an element, used for headings
/// and code blocks where we don't care about inline span structure.
fn collect_text(el: ElementRef, out: &mut String, in_pre: bool) {
    for child in el.children() {
        match child.value() {
            Node::Text(t) => {
                if in_pre {
                    out.push_str(t);
                } else {
                    out.push_str(&collapse_whitespace(t));
                }
            }
            Node::Element(_) => {
                if let Some(c) = ElementRef::wrap(child) {
                    if skipped_tags().contains(c.value().name()) {
                        continue;
                    }
                    if c.value().name() == "br" {
                        out.push('\n');
                        continue;
                    }
                    collect_text(c, out, in_pre);
                }
            }
            _ => {}
        }
    }
}

/// Collapse runs of whitespace to a single space and trim per
/// HTML convention.  Within `<pre>` blocks the caller skips this
/// path so newlines and multiple spaces survive.
fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_ws = true; // collapse leading
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !last_was_ws {
                out.push(' ');
                last_was_ws = true;
            }
        } else {
            out.push(ch);
            last_was_ws = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(html: &str) -> Vec<Block> {
        html_to_blocks(html)
    }

    #[test]
    fn empty_input_emits_no_blocks() {
        assert!(parse("").is_empty());
    }

    #[test]
    fn paragraph_emits_styled_line() {
        let blocks = parse("<p>Hello world.</p>");
        assert!(blocks.iter().any(|b| matches!(b, Block::StyledLine(_))));
    }

    #[test]
    fn heading_levels_clamp_to_three() {
        let blocks = parse("<h1>A</h1><h2>B</h2><h5>C</h5>");
        let levels: Vec<u8> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Header { level, .. } => Some(*level),
                _ => None,
            })
            .collect();
        assert_eq!(levels, vec![1, 2, 3]);
    }

    #[test]
    fn bold_italic_code_become_inline_styles() {
        let blocks = parse(
            "<p>This is <strong>bold</strong> and <em>italic</em> and <code>code</code>.</p>",
        );
        let spans: Vec<&InlineSpan> = blocks
            .iter()
            .find_map(|b| match b {
                Block::StyledLine(s) => Some(s.iter().collect()),
                _ => None,
            })
            .unwrap();
        assert!(spans.iter().any(|s| s.bold && s.text.contains("bold")));
        assert!(spans.iter().any(|s| s.italic && s.text.contains("italic")));
        assert!(spans.iter().any(|s| s.monospace && s.text.contains("code")));
    }

    #[test]
    fn link_carries_href_on_span() {
        let blocks = parse(r#"<p><a href="https://example.com">click</a></p>"#);
        let spans: Vec<&InlineSpan> = blocks
            .iter()
            .find_map(|b| match b {
                Block::StyledLine(s) => Some(s.iter().collect()),
                _ => None,
            })
            .unwrap();
        assert!(
            spans
                .iter()
                .any(|s| s.text == "click" && s.url.as_deref() == Some("https://example.com"))
        );
    }

    #[test]
    fn unordered_list_emits_bullet_marker() {
        let blocks = parse("<ul><li>one</li><li>two</li></ul>");
        let items: Vec<&Block> = blocks
            .iter()
            .filter(|b| matches!(b, Block::ListItem { .. }))
            .collect();
        assert_eq!(items.len(), 2);
        if let Block::ListItem { marker, depth, .. } = &items[0] {
            assert_eq!(marker, "• ");
            assert_eq!(*depth, 0);
        }
    }

    #[test]
    fn ordered_list_increments_marker() {
        let blocks = parse("<ol><li>one</li><li>two</li><li>three</li></ol>");
        let markers: Vec<&str> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::ListItem { marker, .. } => Some(marker.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(markers, vec!["1. ", "2. ", "3. "]);
    }

    #[test]
    fn pre_code_keeps_lang_and_lines() {
        let blocks = parse(
            r#"<pre><code class="language-rust">fn main() {}
let x = 1;</code></pre>"#,
        );
        let cb = blocks.iter().find_map(|b| {
            if let Block::CodeBlock { lang, lines } = b {
                Some((lang.clone(), lines.clone()))
            } else {
                None
            }
        });
        let (lang, lines) = cb.expect("expected CodeBlock");
        assert_eq!(lang.as_deref(), Some("rust"));
        assert_eq!(lines, vec!["fn main() {}", "let x = 1;"]);
    }

    #[test]
    fn blockquote_becomes_quote_block() {
        let blocks = parse("<blockquote><p>wisdom</p></blockquote>");
        assert!(blocks.iter().any(|b| matches!(b, Block::Quote(_))));
    }

    #[test]
    fn hr_emits_rule_block() {
        let blocks = parse("<hr>");
        assert!(blocks.iter().any(|b| matches!(b, Block::Rule)));
    }

    #[test]
    fn script_and_style_are_dropped() {
        let blocks = parse("<p>Visible.</p><script>alert(1)</script><style>p{color:red}</style>");
        let combined: String = blocks
            .iter()
            .filter_map(|b| match b {
                Block::StyledLine(spans) => {
                    Some(spans.iter().map(|s| s.text.as_str()).collect::<String>())
                }
                _ => None,
            })
            .collect();
        assert!(combined.contains("Visible"));
        assert!(!combined.contains("alert"));
        assert!(!combined.contains("color"));
    }

    #[test]
    fn whitespace_collapses_outside_pre() {
        let blocks = parse("<p>a   b\n\nc</p>");
        let text: String = blocks
            .iter()
            .filter_map(|b| match b {
                Block::StyledLine(spans) => {
                    Some(spans.iter().map(|s| s.text.as_str()).collect::<String>())
                }
                _ => None,
            })
            .collect();
        assert!(!text.contains("   "), "got: {text}");
    }

    #[test]
    fn table_emits_matrix_block_with_rows() {
        let blocks = parse(
            "<table><tr><th>Name</th><th>Score</th></tr>\
       <tr><td>Alice</td><td>10</td></tr>\
       <tr><td>Bob</td><td>20</td></tr></table>",
        );
        let m = blocks.iter().find_map(|b| match b {
            Block::Matrix { rows, .. } => Some(rows.clone()),
            _ => None,
        });
        let rows = m.expect("expected Matrix block");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0][0].0, "Name");
        assert_eq!(rows[0][1].0, "Score");
        assert_eq!(rows[1][0].0, "Alice");
        assert_eq!(rows[2][1].0, "20");
    }

    #[test]
    fn table_colspan_attribute_carried_through() {
        let blocks = parse(r#"<table><tr><td colspan="2">spanned</td><td>tail</td></tr></table>"#);
        let rows = blocks
            .iter()
            .find_map(|b| match b {
                Block::Matrix { rows, .. } => Some(rows.clone()),
                _ => None,
            })
            .expect("expected Matrix");
        assert_eq!(rows[0][0], ("spanned".to_string(), 2));
        assert_eq!(rows[0][1], ("tail".to_string(), 1));
    }

    #[test]
    fn empty_table_skipped() {
        // <table></table> with no rows shouldn't emit a Matrix.
        let blocks = parse("<p>before</p><table></table><p>after</p>");
        assert!(!blocks.iter().any(|b| matches!(b, Block::Matrix { .. })));
    }

    #[test]
    fn img_degrades_to_alt_placeholder() {
        let blocks = parse(r#"<p>Before <img alt="cat" src="x.png"> after</p>"#);
        let text: String = blocks
            .iter()
            .filter_map(|b| match b {
                Block::StyledLine(spans) => {
                    Some(spans.iter().map(|s| s.text.as_str()).collect::<String>())
                }
                _ => None,
            })
            .collect();
        assert!(text.contains("[Image: cat]"), "got: {text}");
    }
}
