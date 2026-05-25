//! The block-to-visual-lines orchestrator.
//!
//! `build_visual_lines` is the single public entry point.  It dispatches
//! on `Block` variants: prose/header/blank cases are flat; the
//! whitespace-sensitive cases (`StyledLine`, `ListItem`, `Quote`) call
//! into `wrap`; `Figure` calls into `figure::emit_figure_lines`; and
//! consecutive `Rule` / `Matrix` blocks are gathered into a table group
//! and rendered together by `table::emit_table_group_lines`.

use crate::{
    Block, VisualLine, VisualLineKind, visual_width,
    figure::{emit_figure_lines, figure_row_budget},
    table::emit_table_group_lines,
    wrap::{wrap_list_item, wrap_spans},
};

/// Expand a block list into the flat visual line table.
///
/// Called once at document load and again on terminal resize.
///
/// `terminal_width` is the full content width; tables, figures, display
/// math and rules use it so they can "break out" to the whole area.
/// `prose_width` (≤ `terminal_width`) is the narrower reading measure
/// that body prose, lists and quotes wrap to — the caller centres that
/// column at render time.  Pass `prose_width == terminal_width` to
/// disable the measure (everything full width).  `terminal_height` caps
/// figure image rows so one figure + caption never overflows the view.
pub fn build_visual_lines(
    blocks: &[Block],
    terminal_width: usize,
    prose_width: usize,
    terminal_height: usize,
) -> Vec<VisualLine> {
    let figure_budget = figure_row_budget(terminal_height);
    let mut out = Vec::new();
    let mut i = 0;

    while i < blocks.len() {
        // Table groups: consecutive Rule and Matrix blocks are rendered together
        // so they can share column widths and proper box-drawing borders.
        if matches!(&blocks[i], Block::Rule | Block::Matrix { .. }) {
            let group_start = i;
            while i < blocks.len() && matches!(&blocks[i], Block::Rule | Block::Matrix { .. }) {
                i += 1;
            }
            emit_table_group_lines(&mut out, &blocks[group_start..i], group_start, terminal_width);
            continue;
        }

        let block_idx = i;
        match &blocks[i] {
            Block::Line(s) => {
                let len = s.len();
                out.push(VisualLine {
                    block_idx,
                    line_in_block: 0,
                    text: s.clone(),
                    kind: VisualLineKind::Prose,
                    block_byte_start: 0,
                    block_byte_end: len,
                });
            }

            Block::Blank => {
                out.push(VisualLine {
                    block_idx,
                    line_in_block: 0,
                    text: String::new(),
                    kind: VisualLineKind::Blank,
                    block_byte_start: 0,
                    block_byte_end: 0,
                });
            }

            Block::Header { level, text, number } => {
                // Breathing room above every header so a new section
                // reads as a break while scrolling.  Skip when the
                // previous emitted line is already blank (avoid doubling
                // on the parser's own spacing) or when the header is the
                // very first line.
                let needs_gap = out
                    .last()
                    .is_some_and(|vl| !matches!(vl.kind, VisualLineKind::Blank));
                if needs_gap {
                    out.push(VisualLine {
                        block_idx,
                        line_in_block: 0,
                        text: String::new(),
                        kind: VisualLineKind::Blank,
                        block_byte_start: 0,
                        block_byte_end: 0,
                    });
                }
                let len = text.len();
                out.push(VisualLine {
                    block_idx,
                    line_in_block: 0,
                    text: text.clone(),
                    kind: VisualLineKind::Header {
                        level: *level,
                        number: number.clone(),
                    },
                    block_byte_start: 0,
                    block_byte_end: len,
                });
            }

            Block::DisplayMath { lines, num } => {
                let block_width = lines.iter().map(|l| visual_width(l)).max().unwrap_or(0);
                // Expand each logical row into one or more rendered sub-lines:
                // rows that fit are centred; over-wide rows wrap at the relation
                // (see `wrap_math_line`) and break out to the full width.
                let mut rendered: Vec<String> = Vec::with_capacity(lines.len());
                for line in lines {
                    if visual_width(line) <= terminal_width {
                        rendered.push(center_line(line, block_width, terminal_width));
                    } else {
                        rendered.extend(wrap_math_line(line, terminal_width));
                    }
                }
                // The equation number sits at the right edge of the final
                // rendered sub-line.
                if let Some(eq_num) = num
                    && let Some(last) = rendered.last_mut()
                {
                    let tag = format!("({})", eq_num);
                    let used = visual_width(last);
                    let avail = terminal_width.saturating_sub(tag.len());
                    if used < avail {
                        last.push_str(&" ".repeat(avail - used));
                    }
                    last.push_str(&tag);
                }
                let total = rendered.len();
                for (li, text) in rendered.into_iter().enumerate() {
                    out.push(VisualLine {
                        block_idx,
                        line_in_block: li,
                        text,
                        kind: VisualLineKind::MathLine {
                            block_width,
                            is_first: li == 0,
                            is_last: li + 1 == total,
                        },
                        block_byte_start: 0,
                        block_byte_end: 0,
                    });
                }
            }

            Block::StyledLine(spans) => {
                // "Block text" for highlight purposes is the concatenation of the
                // wrapped lines joined by single spaces — this is the canonical
                // post-normalization form (wrap_spans collapses whitespace).
                let wrapped = wrap_spans(spans, prose_width);
                let mut byte_cursor = 0usize;
                let n = wrapped.len();
                for (li, (line_spans, plain)) in wrapped.into_iter().enumerate() {
                    let start = byte_cursor;
                    let end = start + plain.len();
                    out.push(VisualLine {
                        block_idx,
                        line_in_block: li,
                        text: plain,
                        kind: VisualLineKind::StyledProse(line_spans),
                        block_byte_start: start,
                        block_byte_end: end,
                    });
                    byte_cursor = end;
                    if li + 1 < n {
                        byte_cursor += 1;
                    } // separator space between wrapped lines
                }
            }

            Block::ListItem {
                depth,
                marker,
                content,
            } => {
                let wrapped = wrap_list_item(*depth, marker, content, prose_width);
                let mut byte_cursor = 0usize;
                let n = wrapped.len();
                for (li, (_line_spans, plain, is_continuation)) in wrapped.into_iter().enumerate() {
                    let start = byte_cursor;
                    let end = start + plain.len();
                    out.push(VisualLine {
                        block_idx,
                        line_in_block: li,
                        text: plain,
                        kind: VisualLineKind::ListItem {
                            depth: *depth,
                            marker_len: marker.len() as u8,
                            is_continuation,
                        },
                        block_byte_start: start,
                        block_byte_end: end,
                    });
                    byte_cursor = end;
                    if li + 1 < n {
                        byte_cursor += 1;
                    }
                }
            }

            Block::CodeBlock { lines, .. } => {
                // Block text is the lines joined by '\n' — preserves raw layout so
                // highlights on code track exact characters.
                let n = lines.len();
                let mut byte_cursor = 0usize;
                for (i, line) in lines.iter().enumerate() {
                    let start = byte_cursor;
                    let end = start + line.len();
                    out.push(VisualLine {
                        block_idx,
                        line_in_block: i,
                        text: line.clone(),
                        kind: VisualLineKind::Code {
                            is_first: i == 0,
                            is_last: i == n - 1,
                        },
                        block_byte_start: start,
                        block_byte_end: end,
                    });
                    byte_cursor = end + 1; // '\n' separator between lines
                }
            }

            Block::Quote(spans) => {
                let quote_width = prose_width.saturating_sub(4).max(1);
                let wrapped = wrap_spans(spans, quote_width);
                let mut byte_cursor = 0usize;
                let n = wrapped.len();
                for (li, (_line_spans, plain)) in wrapped.into_iter().enumerate() {
                    let start = byte_cursor;
                    let end = start + plain.len();
                    out.push(VisualLine {
                        block_idx,
                        line_in_block: li,
                        text: plain,
                        kind: VisualLineKind::Quote {
                            is_continuation: li > 0,
                        },
                        block_byte_start: start,
                        block_byte_end: end,
                    });
                    byte_cursor = end;
                    if li + 1 < n {
                        byte_cursor += 1;
                    }
                }
            }

            // Rule and Matrix are handled above via the table-group path.
            Block::Rule | Block::Matrix { .. } => {}
            // Anchors are invisible — they tag the next visible block for
            // label-to-line resolution by the reader.
            Block::Anchor(_) => {}
            Block::Figure { rows, alt, .. } => {
                emit_figure_lines(&mut out, block_idx, rows, alt, terminal_width, figure_budget);
            }
        }

        i += 1;
    }

    normalize_blank_rhythm(out)
}

/// Normalize vertical rhythm: collapse runs of blank lines to a single
/// blank and trim leading/trailing blanks, so inter-block spacing is
/// exactly one line regardless of how many `Block::Blank`s a given parser
/// emitted (the two parsers don't agree, and the header path already had
/// to dedupe against them — see `Block::Header` above).  This pass only
/// *removes* redundant blanks; it never inserts, so the header's
/// layout-inserted leading gap is preserved.  A blank flanked by content
/// on both sides (e.g. a multi-panel figure's inter-panel separator) is
/// never consecutive with another blank, so it survives untouched.
fn normalize_blank_rhythm(lines: Vec<VisualLine>) -> Vec<VisualLine> {
    let mut out: Vec<VisualLine> = Vec::with_capacity(lines.len());
    for vl in lines {
        if matches!(vl.kind, VisualLineKind::Blank) {
            // Drop when nothing precedes (leading) or the previous emitted
            // line is already blank (collapse the run to one).
            let redundant = out
                .last()
                .is_none_or(|prev| matches!(prev.kind, VisualLineKind::Blank));
            if redundant {
                continue;
            }
        }
        out.push(vl);
    }
    if out
        .last()
        .is_some_and(|prev| matches!(prev.kind, VisualLineKind::Blank))
    {
        out.pop(); // trailing blank
    }
    out
}

/// Center `line` (of visual width `block_width`) within `terminal_width`.
fn center_line(line: &str, block_width: usize, terminal_width: usize) -> String {
    if terminal_width <= block_width {
        return line.to_string();
    }
    let pad = (terminal_width - block_width) / 2;
    format!("{}{}", " ".repeat(pad), line)
}

/// Fallback continuation indent for a wrapped equation with no usable
/// relation to align on.
const MATH_HANG: usize = 4;
/// Minimum width a continuation line must keep; if aligning at the
/// relation would leave less, fall back to the hanging indent.
const MATH_MIN_CONT: usize = 8;

/// Relation operators a wrapped equation aligns its continuations under
/// (continuation lines start just past the first one).  Rendered forms
/// after `strip_latex`, space-delimited as their own tokens.
const MATH_RELATIONS: &[&str] = &[
    "=", "≠", "≤", "≥", "<", ">", "≈", "≡", "≜", "→", "⟶", "⇒", "⟹", "∝", "∼", "↦",
];
/// Binary operators that, if they'd be left dangling at a line end, are
/// carried down to start the continuation line instead.
const MATH_BINARY_OPS: &[&str] = &[
    "+", "−", "-", "±", "∓", "×", "⋅", "·", "∪", "∩", "⊕", "⊗", "∨", "∧",
];

/// Continuation indent for wrapping `tokens`: the column just past the
/// first relation operator (so the right-hand side lines up), or
/// [`MATH_HANG`] when there's no relation or it sits too far right to
/// leave a usable continuation width.
fn relation_indent(tokens: &[&str], width: usize) -> usize {
    let cap = width.saturating_sub(MATH_MIN_CONT);
    if let Some(idx) = tokens.iter().position(|t| MATH_RELATIONS.contains(t)) {
        // visual width of "lhs … rel" plus the space before the RHS.
        let prefix: usize = tokens[..=idx]
            .iter()
            .map(|t| visual_width(t))
            .sum::<usize>()
            + idx // single spaces joining tokens[0..=idx]
            + 1; // the space after the relation
        if prefix <= cap {
            return prefix;
        }
    }
    MATH_HANG.min(cap)
}

/// Wrap an over-wide display-math line at spaces, aligning continuation
/// lines under the right-hand side of the first relation (see
/// [`relation_indent`]).  A binary operator that would be left dangling
/// at a line end is carried down so the continuation leads with it.  A
/// single token wider than the budget is emitted as-is (we never break
/// inside a symbol cluster).
fn wrap_math_line(line: &str, width: usize) -> Vec<String> {
    let tokens: Vec<&str> = line.split(' ').filter(|t| !t.is_empty()).collect();
    if tokens.is_empty() {
        return vec![String::new()];
    }
    let indent = relation_indent(&tokens, width);
    let cont_width = width.saturating_sub(indent).max(1);

    let mut lines: Vec<Vec<&str>> = vec![Vec::new()];
    let mut cur_w = 0usize;
    for &tok in &tokens {
        let tw = visual_width(tok);
        let avail = if lines.len() == 1 { width } else { cont_width };
        let cur = lines.last_mut().unwrap();
        if cur.is_empty() {
            cur.push(tok);
            cur_w = tw;
        } else if cur_w + 1 + tw <= avail {
            cur.push(tok);
            cur_w += 1 + tw;
        } else {
            // Carry a dangling binary operator down to the new line so
            // the continuation reads as "+ …" rather than orphaning it.
            let carry = cur
                .last()
                .is_some_and(|t| MATH_BINARY_OPS.contains(t))
                .then(|| cur.pop().unwrap());
            let mut next = Vec::new();
            if let Some(op) = carry {
                next.push(op);
            }
            next.push(tok);
            cur_w = next.iter().map(|t| visual_width(t)).sum::<usize>()
                + next.len().saturating_sub(1);
            lines.push(next);
        }
    }

    lines
        .into_iter()
        .enumerate()
        .map(|(i, toks)| {
            let body = toks.join(" ");
            if i == 0 {
                body
            } else {
                format!("{}{}", " ".repeat(indent), body)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Headers get a leading blank for breathing room (unless one is
    /// already there) and carry their level + number into the visual
    /// line; the `text` stays the clean title.
    #[test]
    fn header_emits_leading_gap_and_carries_number() {
        let blocks = vec![
            Block::Line("body".into()),
            Block::Header {
                level: 1,
                text: "Background".into(),
                number: Some("2".into()),
            },
        ];
        let out = build_visual_lines(&blocks, 40, 40, 24);
        // body, inserted blank, header
        assert_eq!(out.len(), 3);
        assert!(matches!(out[1].kind, VisualLineKind::Blank));
        assert_eq!(out[2].text, "Background");
        assert!(matches!(
            out[2].kind,
            VisualLineKind::Header { level: 1, number: Some(ref n) } if n == "2"
        ));
    }

    /// No doubled gap when a blank already precedes the header, and an
    /// unnumbered header carries `number: None`.  (Content leads so the
    /// blank isn't trimmed as a leading blank — that case is covered by
    /// `normalize_collapses_runs_and_trims_edges`.)
    #[test]
    fn header_dedupes_existing_blank_and_allows_no_number() {
        let blocks = vec![
            Block::Line("body".into()),
            Block::Blank,
            Block::Header {
                level: 1,
                text: "References".into(),
                number: None,
            },
        ];
        let out = build_visual_lines(&blocks, 40, 40, 24);
        // body, blank, header — exactly one blank, no extra inserted.
        assert_eq!(out.len(), 3);
        assert!(matches!(out[1].kind, VisualLineKind::Blank));
        assert!(matches!(
            out[2].kind,
            VisualLineKind::Header { number: None, .. }
        ));
    }

    /// Rhythm normalization: a leading blank is dropped, runs of blanks
    /// collapse to one, and the trailing blank is trimmed — so spacing
    /// between blocks is exactly one line whatever the parser emitted.
    #[test]
    fn normalize_collapses_runs_and_trims_edges() {
        let blocks = vec![
            Block::Blank, // leading — dropped
            Block::Line("one".into()),
            Block::Blank,
            Block::Blank, // run of 2 — collapses to 1
            Block::Line("two".into()),
            Block::Blank, // trailing — trimmed
        ];
        let out = build_visual_lines(&blocks, 40, 40, 24);
        let kinds: Vec<_> = out
            .iter()
            .map(|vl| matches!(vl.kind, VisualLineKind::Blank))
            .collect();
        // one, blank, two — no leading/trailing blank, single gap.
        assert_eq!(out.len(), 3);
        assert_eq!(kinds, vec![false, true, false]);
        assert_eq!(out[0].text, "one");
        assert_eq!(out[2].text, "two");
    }

    /// DisplayMath blocks emit one VL per line; every line is centered;
    /// the last line carries the (right-aligned) equation tag when
    /// `num` is Some.  Pins both the centering and the tag-placement.
    #[test]
    fn display_math_emits_centered_lines_with_eq_num() {
        let block = Block::DisplayMath {
            lines: vec!["a + b".into(), "= c".into()],
            num: Some(42),
        };
        let out = build_visual_lines(std::slice::from_ref(&block), 40, 40, 24);
        assert_eq!(out.len(), 2, "two math lines → two VLs");

        // First line: centered, no tag.
        let first = &out[0];
        assert!(matches!(
            first.kind,
            VisualLineKind::MathLine {
                is_first: true,
                is_last: false,
                ..
            }
        ));
        assert!(
            first.text.starts_with("  "),
            "first line should be padded, got {:?}",
            first.text
        );
        assert!(!first.text.contains("(42)"));

        // Last line: centered, ends with the equation tag at the right edge.
        let last = &out[1];
        assert!(matches!(
            last.kind,
            VisualLineKind::MathLine {
                is_first: false,
                is_last: true,
                ..
            }
        ));
        assert!(
            last.text.ends_with("(42)"),
            "last line must end with tag, got {:?}",
            last.text
        );
        // The pre-tag content should still be centered at block_width.
        // block_width = max("a + b".chars().count, "= c".chars().count) = 5.
        // pad = (40 - 5) / 2 = 17 leading spaces.
        let pre_tag = last.text.strip_suffix("(42)").unwrap();
        assert!(
            pre_tag.starts_with(&" ".repeat(17)),
            "centered prefix wrong, got {:?}",
            pre_tag
        );
    }

    /// An over-wide equation wraps at spaces; continuation lines align
    /// under the right-hand side of the relation (here `=`, at column 7),
    /// lead with the carried-down operator, and the equation number lands
    /// on the final sub-line.
    #[test]
    fn display_math_wraps_overlong_line_at_relation() {
        let block = Block::DisplayMath {
            lines: vec!["f(x) = a + b + c + d + e + g".into()],
            num: Some(3),
        };
        // Width 20 forces a wrap; "f(x) = " is 7 cells wide.
        let out = build_visual_lines(std::slice::from_ref(&block), 20, 20, 24);
        assert!(out.len() >= 2, "over-wide line should wrap, got {out:?}");

        // First sub-line is flush-left (no centering — it doesn't fit).
        assert!(!out[0].text.starts_with(' '));
        assert!(out[0].text.starts_with("f(x) = "));

        // Continuation aligns under the RHS (7 spaces) and leads with the
        // binary operator carried down from the previous line.
        let cont = &out[1];
        assert!(
            cont.text.starts_with(&" ".repeat(7)),
            "continuation should align at the relation, got {:?}",
            cont.text
        );
        assert!(
            cont.text.trim_start().starts_with('+'),
            "continuation should lead with the operator, got {:?}",
            cont.text
        );

        // All sub-lines are MathLine; first/last flags bracket the block.
        assert!(matches!(
            out[0].kind,
            VisualLineKind::MathLine { is_first: true, .. }
        ));
        let last = out.last().unwrap();
        assert!(matches!(
            last.kind,
            VisualLineKind::MathLine { is_last: true, .. }
        ));
        assert!(last.text.ends_with("(3)"), "eq number on last sub-line, got {:?}", last.text);
    }
}
