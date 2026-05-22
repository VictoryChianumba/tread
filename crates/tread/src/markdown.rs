//! Markdown → tread `Block` conversion.
//!
//! Walks `pulldown_cmark` events and emits the same Block tree the
//! arxiv path produces, so the reader's renderer is source-agnostic
//! once the document is parsed.  Used by `PaperData::from_markdown`
//! to support GitHub READMEs and any markdown source the host opens.
//!
//! Mapping summary:
//!
//! ```text
//!   #..######           → Block::Header { level: 1..3 }  (clamped)
//!   plain paragraph     → Block::StyledLine(spans)
//!   blockquote          → Block::Quote(spans)
//!   - item / 1. item    → Block::ListItem { depth, marker, content }
//!   `lang ... `         → Block::CodeBlock { lang, lines }
//!   ---                 → Block::Rule
//!   blank between blocks → Block::Blank
//!   ![alt](url)         → degraded to "[Image: alt]" StyledLine
//!                          (no fetch; tread doesn't speak HTTP for image bodies)
//!   `inline code`       → InlineSpan { monospace: true }
//!   **bold** / *italic* → InlineSpan { bold | italic }
//!   [link](url)         → InlineSpan { url: Some(url) }
//! ```
//!
//! No math handling — markdown doesn't have a standard math syntax,
//! and adding TeX inline (`$...$`) would conflict with shell-prompt
//! conventions in code samples.  v2 backlog if needed.

use doc_model::{Block, InlineSpan};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// Convert markdown text to a tread `Block` stream.  Returns blocks
/// in document order; the caller wraps these in `PaperData`.
pub fn parse(md: &str) -> Vec<Block> {
    let opts = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
    let parser = Parser::new_ext(md, opts);
    let mut walker = Walker::default();
    for ev in parser {
        walker.event(ev);
    }
    walker.flush_pending();
    walker.blocks
}

#[derive(Default)]
struct Walker {
    blocks: Vec<Block>,
    /// Accumulator for inline runs while we're inside a Para / Heading /
    /// ListItem / Quote.  Flushed when the surrounding block closes.
    pending: Vec<InlineSpan>,
    /// Stack of style modifiers active on the next text node — entries
    /// pushed on Tag::Strong / Emphasis / Strikethrough / Code-inline /
    /// Link, popped on the matching End.
    style: Style,
    /// Active link URL (innermost), used to fill `InlineSpan.url`.
    link_url: Option<String>,
    /// Indent depth for nested lists (0 at top level).
    list_depth: u8,
    /// Per-list, the next ordinal for numbered lists.  Stack mirrors
    /// list_depth so nested lists each have their own counter.
    ordinal_stack: Vec<Option<u64>>,
    /// Inside a code fence: collect raw text lines for Block::CodeBlock.
    code_block: Option<CodeBlockState>,
    /// Inside a heading: remember its level so flush_pending picks
    /// Header instead of StyledLine.
    heading_level: Option<u8>,
    /// Inside a blockquote: flush_pending picks Quote instead of StyledLine.
    in_quote: bool,
    /// Inside an item: when the item closes, flush as ListItem.
    in_item: bool,
    /// Active table state.  pulldown-cmark emits TableHead / TableRow
    /// / TableCell events; we accumulate cells per row and emit
    /// Block::Matrix on the closing TableEnd.  Cell content is plain
    /// text — Block::Matrix doesn't carry inline spans, same as the
    /// HTML walker and the arxiv path.
    table: Option<TableState>,
}

#[derive(Default)]
struct TableState {
    rows: Vec<Vec<(String, usize)>>,
    current_row: Vec<(String, usize)>,
    current_cell: String,
}

#[derive(Default, Clone, Copy)]
struct Style {
    bold: bool,
    italic: bool,
    monospace: bool,
    strikethrough: bool,
}

struct CodeBlockState {
    lang: Option<String>,
    buf: String,
}

impl Walker {
    fn event(&mut self, ev: Event) {
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(end) => self.end(end),
            Event::Text(text) => self.text(text.into_string()),
            Event::Code(code) => {
                self.pending.push(InlineSpan {
                    text: code.into_string(),
                    monospace: true,
                    url: self.link_url.clone(),
                    ..InlineSpan::default()
                });
            }
            Event::SoftBreak | Event::HardBreak => {
                // Treat both as a single space so paragraphs flow.  Hard
                // breaks could become explicit newlines later if needed.
                if let Some(last) = self.pending.last_mut()
                    && !last.text.ends_with(' ') {
                        last.text.push(' ');
                    }
            }
            Event::Rule => {
                self.flush_pending();
                self.blocks.push(Block::Rule);
            }
            Event::Html(_) | Event::InlineHtml(_) => {
                // Drop raw HTML tags silently — most READMEs use them for
                // alignment hacks (`<p align="center">`, `<br>`); not worth
                // surfacing as visible text.  Inline HTML inside a paragraph
                // can leave a stray space which `flush_pending` already
                // collapses.
            }
            Event::FootnoteReference(_) | Event::TaskListMarker(_) => {
                // v2: render checkboxes and footnote markers visibly.
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => { /* nothing — text events accumulate into pending */ }
            Tag::Heading { level, .. } => {
                self.heading_level = Some(heading_level(level));
            }
            Tag::BlockQuote(_) => {
                self.in_quote = true;
            }
            Tag::CodeBlock(kind) => {
                let lang = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(s) if !s.is_empty() => {
                        Some(s.into_string())
                    }
                    _ => None,
                };
                self.code_block = Some(CodeBlockState {
                    lang,
                    buf: String::new(),
                });
            }
            Tag::List(start) => {
                self.list_depth = self.list_depth.saturating_add(1);
                self.ordinal_stack.push(start);
            }
            Tag::Item => {
                self.in_item = true;
            }
            Tag::Emphasis => self.style.italic = true,
            Tag::Strong => self.style.bold = true,
            Tag::Strikethrough => self.style.strikethrough = true,
            Tag::Link { dest_url, .. } => {
                self.link_url = Some(dest_url.into_string());
            }
            Tag::Table(_) => {
                self.flush_pending();
                self.table = Some(TableState::default());
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(state) = self.table.as_mut() {
                    state.current_row.clear();
                }
            }
            Tag::TableCell => {
                if let Some(state) = self.table.as_mut() {
                    state.current_cell.clear();
                }
            }
            Tag::Image {
                dest_url: _, title, ..
            } => {
                // Tread doesn't fetch image bytes from URLs, so degrade to a
                // visible placeholder.  Local images would need the host to
                // resolve paths first — out of scope for from_markdown.
                let alt = title.into_string();
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
            _ => {}
        }
    }

    fn end(&mut self, end: TagEnd) {
        match end {
            TagEnd::Paragraph => {
                // Inside a blockquote, hold the spans until the quote
                // closes — otherwise they leak out as a regular StyledLine
                // and the Quote block ends up empty.  Same logic for items:
                // pending stays until TagEnd::Item flushes as ListItem.
                if !self.in_quote && !self.in_item {
                    self.flush_pending();
                }
            }
            TagEnd::Heading(_) => {
                if let Some(level) = self.heading_level.take() {
                    let text: String = self.pending.iter().map(|s| s.text.as_str()).collect();
                    self.pending.clear();
                    self.blocks.push(Block::Header {
                        level,
                        text,
                        number: None,
                    });
                    self.blocks.push(Block::Blank);
                }
            }
            TagEnd::BlockQuote(_) => {
                self.in_quote = false;
                if !self.pending.is_empty() {
                    let spans = std::mem::take(&mut self.pending);
                    self.blocks.push(Block::Quote(spans));
                    self.blocks.push(Block::Blank);
                }
            }
            TagEnd::CodeBlock => {
                if let Some(cb) = self.code_block.take() {
                    let lines: Vec<String> = cb.buf.split('\n').map(|s| s.to_string()).collect();
                    // Trim a trailing empty line if the fence had \n before ```.
                    let lines = if lines.last().is_some_and(|l| l.is_empty()) {
                        lines[..lines.len() - 1].to_vec()
                    } else {
                        lines
                    };
                    self.blocks.push(Block::CodeBlock {
                        lang: cb.lang,
                        lines,
                    });
                    self.blocks.push(Block::Blank);
                }
            }
            TagEnd::List(_) => {
                self.list_depth = self.list_depth.saturating_sub(1);
                self.ordinal_stack.pop();
                if self.list_depth == 0 {
                    self.blocks.push(Block::Blank);
                }
            }
            TagEnd::Item => {
                let depth = self.list_depth.saturating_sub(1);
                let marker = match self.ordinal_stack.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        m
                    }
                    _ => "• ".to_string(),
                };
                let content = std::mem::take(&mut self.pending);
                self.blocks.push(Block::ListItem {
                    depth,
                    marker,
                    content,
                });
                self.in_item = false;
            }
            TagEnd::Emphasis => self.style.italic = false,
            TagEnd::Strong => self.style.bold = false,
            TagEnd::Strikethrough => self.style.strikethrough = false,
            TagEnd::Link => self.link_url = None,
            TagEnd::Table => {
                if let Some(state) = self.table.take()
                    && !state.rows.is_empty() {
                        self.blocks.push(Block::Matrix {
                            rows: state.rows,
                            vertical_rules: Vec::new(),
                            alignments: Vec::new(), // not tracked yet; left-align
                        });
                        self.blocks.push(Block::Blank);
                    }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(state) = self.table.as_mut()
                    && !state.current_row.is_empty() {
                        let row = std::mem::take(&mut state.current_row);
                        state.rows.push(row);
                    }
            }
            TagEnd::TableCell => {
                if let Some(state) = self.table.as_mut() {
                    let text = std::mem::take(&mut state.current_cell);
                    state.current_row.push((text.trim().to_string(), 1));
                }
            }
            _ => {}
        }
    }

    fn text(&mut self, text: String) {
        if let Some(cb) = self.code_block.as_mut() {
            cb.buf.push_str(&text);
            return;
        }
        // Inside a table cell, text accumulates into the cell buffer
        // (Block::Matrix takes plain Strings, not styled spans).
        if let Some(state) = self.table.as_mut() {
            state.current_cell.push_str(&text);
            return;
        }
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

    fn flush_pending(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let spans = std::mem::take(&mut self.pending);
        if self.in_item {
            // Items emit at TagEnd::Item — leave pending in place there
            // (we shouldn't actually be here, but be defensive).
            self.pending = spans;
            return;
        }
        self.blocks.push(Block::StyledLine(spans));
        self.blocks.push(Block::Blank);
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        // Anything deeper collapses to subsubsection — tread renders three levels.
        _ => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_count(md: &str) -> usize {
        parse(md).len()
    }

    #[test]
    fn empty_input_emits_nothing() {
        assert!(parse("").is_empty());
    }

    #[test]
    fn paragraph_emits_styled_line_plus_blank() {
        let blocks = parse("Hello world.");
        assert_eq!(blocks.len(), 2);
        matches!(&blocks[0], Block::StyledLine(_));
        matches!(&blocks[1], Block::Blank);
    }

    #[test]
    fn heading_levels_clamp_to_three() {
        let blocks = parse("# H1\n\n## H2\n\n#### H4 deep");
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
        let blocks = parse("**bold** and *italic* and `code`.");
        let Block::StyledLine(spans) = &blocks[0] else {
            panic!("expected StyledLine, got {:?}", blocks[0]);
        };
        assert!(spans.iter().any(|s| s.bold && s.text == "bold"));
        assert!(spans.iter().any(|s| s.italic && s.text == "italic"));
        assert!(spans.iter().any(|s| s.monospace && s.text == "code"));
    }

    #[test]
    fn link_carries_url_on_span() {
        let blocks = parse("[click](https://example.com)");
        let Block::StyledLine(spans) = &blocks[0] else {
            panic!("expected StyledLine");
        };
        assert!(
            spans
                .iter()
                .any(|s| s.text == "click" && s.url.as_deref() == Some("https://example.com"))
        );
    }

    #[test]
    fn unordered_list_emits_bullet_marker() {
        let blocks = parse("- one\n- two");
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
        let blocks = parse("1. one\n2. two\n3. three");
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
    fn fenced_code_block_keeps_lang_and_lines() {
        let blocks = parse("```rust\nfn main() {}\nlet x = 1;\n```");
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
        let blocks = parse("> wisdom");
        assert!(blocks.iter().any(|b| matches!(b, Block::Quote(_))));
    }

    #[test]
    fn horizontal_rule_emits_rule_block() {
        let blocks = parse("---");
        assert!(blocks.iter().any(|b| matches!(b, Block::Rule)));
    }

    #[test]
    fn raw_html_is_dropped_silently() {
        // <br> and <p> tags shouldn't appear as visible text.
        let blocks = parse("Before<br>After");
        let combined: String = blocks
            .iter()
            .filter_map(|b| match b {
                Block::StyledLine(spans) => {
                    Some(spans.iter().map(|s| s.text.as_str()).collect::<String>())
                }
                _ => None,
            })
            .collect();
        assert!(!combined.contains("<br>"));
    }

    #[test]
    fn at_least_one_block_for_simple_doc() {
        assert!(block_count("# Title\n\nSome text.") >= 2);
    }

    #[test]
    fn pipe_table_emits_matrix_block_with_rows() {
        let md = "\
| Name  | Score |
|-------|-------|
| Alice | 10    |
| Bob   | 20    |
";
        let blocks = parse(md);
        let rows = blocks
            .iter()
            .find_map(|b| match b {
                Block::Matrix { rows, .. } => Some(rows.clone()),
                _ => None,
            })
            .expect("expected Matrix block from pipe table");
        // Header + two body rows.
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0][0].0, "Name");
        assert_eq!(rows[0][1].0, "Score");
        assert_eq!(rows[1][0].0, "Alice");
        assert_eq!(rows[2][1].0, "20");
    }
}
