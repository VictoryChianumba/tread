//! Word-wrapping for styled-prose and list-item blocks.
//!
//! Output is a vec of `(line_spans, plain_text [, is_continuation])` per
//! visual line, where the plain text is whitespace-normalized
//! (split-on-whitespace + single-space joins).  Byte offsets into the
//! plain text are what the rest of the doc-model uses for
//! `VisualLine::block_byte_start` / `..._end`.

use crate::{InlineSpan, LinkTarget};

/// Word-wrap a sequence of styled spans to `width` columns.
/// Returns a vec of (line_spans, plain_text) pairs — one entry per visual line.
/// Adjacent words with identical style are coalesced into a single span.
pub(crate) fn wrap_spans(spans: &[InlineSpan], width: usize) -> Vec<(Vec<InlineSpan>, String)> {
    struct Word {
        text: String,
        bold: bool,
        italic: bool,
        underline: bool,
        strikethrough: bool,
        monospace: bool,
        color: Option<(u8, u8, u8)>,
        url: Option<String>,
        link_target: Option<LinkTarget>,
    }

    let mut words: Vec<Word> = Vec::new();
    for span in spans {
        for word in span.text.split_whitespace() {
            if !word.is_empty() {
                words.push(Word {
                    text: word.to_string(),
                    bold: span.bold,
                    italic: span.italic,
                    underline: span.underline,
                    strikethrough: span.strikethrough,
                    monospace: span.monospace,
                    color: span.color,
                    url: span.url.clone(),
                    link_target: span.link_target.clone(),
                });
            }
        }
    }

    if words.is_empty() {
        return vec![(vec![], String::new())];
    }

    let effective_width = width.max(1);
    let mut result: Vec<(Vec<InlineSpan>, String)> = Vec::new();
    let mut line_spans: Vec<InlineSpan> = Vec::new();
    let mut line_plain = String::new();
    let mut line_width = 0usize;

    for word in &words {
        let wlen = word.text.chars().count();
        let needed = if line_width == 0 {
            wlen
        } else {
            line_width + 1 + wlen
        };

        if line_width > 0 && needed > effective_width {
            result.push((
                std::mem::take(&mut line_spans),
                std::mem::take(&mut line_plain),
            ));
            line_width = 0;
        }

        let prefix = if line_width > 0 { " " } else { "" };
        let token = format!("{}{}", prefix, word.text);
        line_plain.push_str(&token);
        line_width += token.chars().count();

        // Coalesce with previous span when style is identical.
        let coalesce = line_spans.last().is_some_and(|last| {
            last.bold == word.bold
                && last.italic == word.italic
                && last.underline == word.underline
                && last.strikethrough == word.strikethrough
                && last.monospace == word.monospace
                && last.color == word.color
                && last.url == word.url
                && last.link_target == word.link_target
        });

        if coalesce {
            line_spans.last_mut().unwrap().text.push_str(&token);
        } else {
            line_spans.push(InlineSpan {
                text: token,
                bold: word.bold,
                italic: word.italic,
                underline: word.underline,
                strikethrough: word.strikethrough,
                monospace: word.monospace,
                color: word.color,
                url: word.url.clone(),
                link_target: word.link_target.clone(),
            });
        }
    }

    if !line_plain.is_empty() {
        result.push((line_spans, line_plain));
    }

    result
}

/// Wrap a list item's content to `width`, prepending the indent+marker prefix.
/// Returns (line_spans, plain_text, is_continuation) per visual line.
pub(crate) fn wrap_list_item(
    depth: u8,
    marker: &str,
    content: &[InlineSpan],
    width: usize,
) -> Vec<(Vec<InlineSpan>, String, bool)> {
    let indent_len = depth as usize * 2;
    let prefix_len = indent_len + marker.len();
    let content_width = width.saturating_sub(prefix_len).max(1);

    let wrapped = wrap_spans(content, content_width);

    wrapped
        .into_iter()
        .enumerate()
        .map(|(i, (spans, plain))| {
            let is_continuation = i > 0;
            let prefix = if is_continuation {
                format!(
                    "{}{}",
                    "  ".repeat(depth as usize),
                    " ".repeat(marker.len())
                )
            } else {
                format!("{}{}", "  ".repeat(depth as usize), marker)
            };
            let plain_with_prefix = format!("{}{}", prefix, plain);
            let mut all_spans = vec![InlineSpan::plain(prefix)];
            all_spans.extend(spans);
            (all_spans, plain_with_prefix, is_continuation)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Adjacent words with identical styling must produce a single span,
    /// not one span per word.  A regression silently inflates the spans
    /// vec on every paragraph, slowing render.
    #[test]
    fn wrap_spans_coalesces_same_style_runs() {
        let spans = vec![InlineSpan::bold("alpha beta gamma")];
        let out = wrap_spans(&spans, 80);
        assert_eq!(out.len(), 1, "fits on one line");
        let (line_spans, plain) = &out[0];
        assert_eq!(plain, "alpha beta gamma");
        assert_eq!(
            line_spans.len(),
            1,
            "three same-style words should coalesce to one span, got {:?}",
            line_spans
        );
        assert!(line_spans[0].bold);
        assert_eq!(line_spans[0].text, "alpha beta gamma");
    }

    /// Continuation lines of a wrapped list item must indent by
    /// `depth*2 + marker.len()` spaces — same width as the marker on
    /// the first line — so wrapped text aligns under the first content
    /// character.
    #[test]
    fn wrap_list_item_continuation_indent_matches_marker_width() {
        // depth=1 → 2 indent + "• " marker (2 chars) = 4-char prefix.
        // Width 14 forces "one two three four five" onto multiple lines.
        let content = vec![InlineSpan::plain("one two three four five six seven")];
        let wrapped = wrap_list_item(1, "• ", &content, 14);
        assert!(wrapped.len() >= 2, "expected wrap, got {:?}", wrapped);

        let (_, first_plain, first_is_cont) = &wrapped[0];
        assert!(!first_is_cont);
        assert!(
            first_plain.starts_with("  • "),
            "first line prefix: {:?}",
            first_plain
        );

        let (_, second_plain, second_is_cont) = &wrapped[1];
        assert!(second_is_cont);
        assert!(
            second_plain.starts_with("    "),
            "continuation must indent to marker width (4 spaces), got: {:?}",
            second_plain
        );
        // The first non-space char of the continuation should sit at column
        // 4 — same column as the first non-marker char on line 1.
        let first_content_col = first_plain.find(|c: char| c != ' ' && c != '•').unwrap();
        let cont_content_col = second_plain.find(|c: char| c != ' ').unwrap();
        assert_eq!(first_content_col, cont_content_col);
    }
}
