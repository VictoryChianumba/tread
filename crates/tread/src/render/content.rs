//! Reader content rows.
//!
//! Owns `draw_content` and the visual-line dispatch (`render_visual_line`)
//! plus every cursor/highlight/search overlay used by the per-row
//! render.  The cursor cell, persistent highlights, active voice word,
//! and search-match highlights all converge here — splitting them
//! across modules would mean every overlay change has to walk multiple
//! files in lockstep.
//!
//! Pure helpers (`is_box_drawing`, `snap_to_char_boundary`,
//! `clamp_to_char_boundary`) stay private to this module; they exist
//! only to keep the overlay code safe across multi-byte char
//! boundaries (C1 fix — see test
//! `overlay_highlights_renders_full_text_when_range_straddles_multibyte_char`).

use doc_model::VisualLineKind;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
};
use ui_theme::Theme;

use crate::state::{Mode, Reader};

pub(super) fn draw_content(frame: &mut Frame, reader: &Reader, area: Rect, t: &Theme) {
    let ch = area.height as usize;
    let total = reader.total_lines();
    let q = reader.search_query.to_lowercase();

    let visual_range: Option<(usize, usize)> = match reader.mode() {
        Mode::Visual { .. } => {
            let cur = reader.current_line();
            let anchor = reader.visual_anchor;
            Some((cur.min(anchor), cur.max(anchor)))
        }
        _ => None,
    };

    // Voice playback effects, computed once per draw and reused per row.
    let voice_word = crate::state::voice_control::active_voice_word(reader);
    let voice_active = crate::state::voice_control::voice_rendering_active(reader);

    // Focus mode: dim everything outside the cursor's paragraph.  The
    // range is computed once per draw (like the voice paragraph range).
    let focus_range = reader.focus_mode().then(|| reader.focus_para_range());

    // Left pad that centres the narrow prose column within the full
    // content width.  Zero when the reading measure is off or wider than
    // the area.  Only text-column lines get it; tables / figures / math /
    // rules render at full width so they "break out" of the column.
    let prose_pad = (area.width as usize).saturating_sub(reader.prose_width()) / 2;

    let lines: Vec<Line> = (0..ch)
        .map(|row| {
            let vl_idx = reader.offset() + row;
            if vl_idx >= total {
                return Line::raw("");
            }
            // Defensive `.get()` (C6): `vl_idx < total` is equivalent to
            // `vl_idx < visual_lines.len()` today because `total_lines`
            // returns the length directly, but a future refactor that
            // changes that semantics (e.g. excluding image rows from
            // the count) would turn this loop into a silent panic.
            // The `.get()` form returns a blank line on out-of-bounds,
            // which is the visually correct degraded behaviour.
            let Some(vl) = reader.visual_lines().get(vl_idx) else {
                return Line::raw("");
            };
            let is_cursor = row == reader.cursor_y();
            let is_bookmarked = reader.is_line_bookmarked(vl_idx);
            let is_selected = visual_range.is_some_and(|(lo, hi)| vl_idx >= lo && vl_idx <= hi);
            let cursor_col = if is_cursor {
                Some(reader.cursor_x())
            } else {
                None
            };
            // Compute persistent-highlight byte ranges that overlap this VL,
            // translated into vl-local byte coordinates for the renderer.
            let mut highlight_ranges: Vec<(usize, usize)> =
                if vl.block_byte_end > vl.block_byte_start {
                    reader
                        .highlights
                        .overlapping(vl.block_idx, vl.block_byte_start..vl.block_byte_end)
                        .map(|h| {
                            (
                                h.byte_start.max(vl.block_byte_start) - vl.block_byte_start,
                                h.byte_end.min(vl.block_byte_end) - vl.block_byte_start,
                            )
                        })
                        .collect()
                } else {
                    Vec::new()
                };
            // Active-word highlight rides on the same overlay path as
            // persistent character highlights; reuse rather than introducing
            // a parallel system.  Visually distinct because search/highlight
            // never occur during active playback.
            if let Some((word_vl, ws, we)) = voice_word
                && word_vl == vl_idx {
                    highlight_ranges.push((ws, we));
                }
            let mut line = render_visual_line(
                vl,
                is_cursor,
                is_bookmarked,
                is_selected,
                cursor_col,
                &q,
                reader.search_matches(),
                vl_idx,
                &highlight_ranges,
                t,
            );
            // Centre the prose column: prepend the left pad to text-column
            // lines.  Prepending a span just shifts the already-rendered
            // spans (cursor cell, highlights) right, so byte coordinates
            // stay correct.  The pad carries the line's bg so a selected /
            // bookmarked line tints its left margin too.
            if prose_pad > 0 && is_text_column(&vl.kind) {
                let bg = if is_selected {
                    t.bg_selection
                } else if is_bookmarked {
                    t.bookmark_bg
                } else {
                    Color::Reset
                };
                let mut spans = Vec::with_capacity(line.spans.len() + 1);
                spans.push(Span::styled(" ".repeat(prose_pad), Style::default().bg(bg)));
                spans.extend(line.spans);
                line = Line::from(spans);
            }
            // Dim non-paragraph lines during voice playback so the active
            // paragraph reads as the focused region.
            if voice_active && crate::state::voice_control::voice_line_dimmed(reader, vl_idx) {
                line = line.style(Style::default().fg(t.text_dim));
            }
            // Focus mode applies the same dim around the *cursor's*
            // paragraph (independent of voice; both can be active).
            if let Some((lo, hi)) = focus_range
                && (vl_idx < lo || vl_idx > hi)
            {
                line = line.style(Style::default().fg(t.text_dim));
            }
            line
        })
        .collect();

    let paragraph = Paragraph::new(lines).block(Block::default());
    frame.render_widget(paragraph, area);
}

/// Whether a line belongs to the centred reading column (prose-like) and
/// should receive the centering left pad.  Wide block content — tables,
/// figures, display math, rules — is excluded so it spans the full width.
fn is_text_column(kind: &doc_model::VisualLineKind) -> bool {
    matches!(
        kind,
        doc_model::VisualLineKind::Prose
            | doc_model::VisualLineKind::StyledProse(_)
            | doc_model::VisualLineKind::ListItem { .. }
            | doc_model::VisualLineKind::Quote { .. }
            | doc_model::VisualLineKind::Header { .. }
            | doc_model::VisualLineKind::Code { .. }
    )
}

fn render_visual_line<'a>(
    vl: &'a doc_model::VisualLine,
    _is_cursor: bool,
    is_bookmarked: bool,
    is_selected: bool,
    cursor_col: Option<usize>,
    query: &str,
    matches: &[usize],
    vl_idx: usize,
    highlight_ranges: &[(usize, usize)],
    t: &Theme,
) -> Line<'a> {
    let text = &vl.text;
    let bg = if is_selected {
        t.bg_selection
    } else if is_bookmarked {
        t.bookmark_bg
    } else {
        Color::Reset
    };

    let base_style = Style::default().bg(bg);

    match &vl.kind {
        VisualLineKind::Blank => {
            if cursor_col.is_some() {
                Line::from(vec![Span::styled(
                    " ",
                    Style::default().bg(t.cursor_bg).fg(t.cursor_fg),
                )])
            } else {
                Line::styled("", base_style)
            }
        }

        VisualLineKind::Prose => {
            if let Some(col) = cursor_col {
                apply_char_cursor(text, col, bg, t)
            } else if !query.is_empty() && matches.contains(&vl_idx) {
                highlight_query(text, query, bg, t)
            } else if !highlight_ranges.is_empty() {
                overlay_highlights(text, base_style, highlight_ranges, t.bg_highlight)
            } else {
                Line::styled(text.clone(), base_style)
            }
        }

        VisualLineKind::MathLine { .. } => Line::styled(text.clone(), base_style.fg(t.math)),

        VisualLineKind::Header { level, number } => {
            let (fg, modifier) = match level {
                1 => (t.accent, Modifier::BOLD),
                2 => (t.header, Modifier::BOLD),
                _ => (t.header, Modifier::empty()),
            };
            let hdr_style = base_style.fg(fg).add_modifier(modifier);
            // Number prefix on numbered sections (e.g. "2  ", "3.1  ").
            // It is NOT part of the addressable title, so cursor / search /
            // highlight keep operating on `text` (the clean title).
            let prefix = match number {
                Some(n) => format!("{n}  "),
                None => String::new(),
            };
            let title_line = if let Some(col) = cursor_col {
                apply_char_cursor(text, col, bg, t)
            } else if !highlight_ranges.is_empty() {
                overlay_highlights(text, hdr_style, highlight_ranges, t.bg_highlight)
            } else {
                Line::styled(text.clone(), hdr_style)
            };
            if prefix.is_empty() {
                title_line
            } else {
                let mut spans = vec![Span::styled(prefix, hdr_style)];
                spans.extend(title_line.spans);
                Line::from(spans)
            }
        }

        VisualLineKind::MatrixLine { is_header, .. } => {
            // Cell content inherits the terminal's default foreground (matches
            // prose).  Setting `.fg(t.text)` explicitly was making cells dimmer
            // than surrounding prose because `t.text` is slightly off-white
            // while terminal defaults are typically pure white.  Box-drawing
            // chars (│ ┬ ┼ ┴ ├ ┤ ┌ ┐ └ ┘ ─) keep `t.rule` so vertical rules
            // visually match horizontal ones without overwhelming the cells.
            let cell_style = if *is_header {
                base_style.add_modifier(Modifier::BOLD)
            } else {
                base_style
            };
            let rule_style = base_style.fg(t.rule);
            // When the cursor is on this row, walk char-by-char and emit each
            // byte as its own span at the cursor position so the cursor is
            // visible inside table cells.  Less efficient than the run-merging
            // path below, but simpler than splicing one cursor cell into a
            // pre-built span list.
            if let Some(col) = cursor_col {
                let safe = snap_to_char_boundary(text, col);
                let mut spans: Vec<Span> = Vec::new();
                let mut byte_idx = 0usize;
                for ch in text.chars() {
                    let ch_len = ch.len_utf8();
                    let style = if byte_idx == safe {
                        Style::default().bg(t.cursor_bg).fg(t.cursor_fg)
                    } else if is_box_drawing(ch) {
                        rule_style
                    } else {
                        cell_style
                    };
                    spans.push(Span::styled(ch.to_string(), style));
                    byte_idx += ch_len;
                }
                if spans.is_empty() {
                    spans.push(Span::styled(
                        " ",
                        Style::default().bg(t.cursor_bg).fg(t.cursor_fg),
                    ));
                }
                return Line::from(spans);
            }
            let mut spans: Vec<Span> = Vec::new();
            let mut buf = String::new();
            let mut buf_is_rule = false;
            for ch in text.chars() {
                let ch_is_rule = is_box_drawing(ch);
                if !buf.is_empty() && ch_is_rule != buf_is_rule {
                    let s = if buf_is_rule { rule_style } else { cell_style };
                    spans.push(Span::styled(std::mem::take(&mut buf), s));
                }
                buf_is_rule = ch_is_rule;
                buf.push(ch);
            }
            if !buf.is_empty() {
                let s = if buf_is_rule { rule_style } else { cell_style };
                spans.push(Span::styled(buf, s));
            }
            Line::from(spans)
        }

        VisualLineKind::StyledProse(spans) => {
            if let Some(col) = cursor_col {
                apply_styled_cursor(spans, base_style, col, t)
            } else if !query.is_empty() && matches.contains(&vl_idx) {
                highlight_spans(spans, query, bg, t)
            } else if !highlight_ranges.is_empty() {
                overlay_highlights_styled(spans, base_style, highlight_ranges, t.bg_highlight, t)
            } else {
                let ratatui_spans: Vec<Span> = spans
                    .iter()
                    .map(|s| {
                        let mut style = base_style;
                        if s.bold {
                            style = style.add_modifier(Modifier::BOLD);
                        }
                        if s.italic {
                            style = style.add_modifier(Modifier::ITALIC);
                        }
                        if s.underline {
                            style = style.add_modifier(Modifier::UNDERLINED);
                        }
                        if s.strikethrough {
                            style = style.add_modifier(Modifier::CROSSED_OUT);
                        }
                        if s.monospace {
                            style = style.fg(t.mono).bg(t.bg_code);
                        }
                        if let Some((r, g, b)) = s.color {
                            style = style.fg(Color::Rgb(r, g, b));
                        }
                        if s.url.is_some() {
                            // Mark external URLs with underline. Embedding raw OSC 8 sequences in ratatui
                            // Spans corrupts cell-width accounting; ratatui counts escape bytes as columns.
                            style = style.add_modifier(Modifier::UNDERLINED);
                        }
                        if s.link_target.is_some() {
                            style = style.fg(t.link_fg).add_modifier(Modifier::UNDERLINED);
                        }
                        Span::styled(s.text.clone(), style)
                    })
                    .collect();
                Line::from(ratatui_spans)
            }
        }

        VisualLineKind::ListItem { .. } => {
            // text already contains indent + marker prefix from build_visual_lines.
            if let Some(col) = cursor_col {
                apply_char_cursor(text, col, bg, t)
            } else if !query.is_empty() && matches.contains(&vl_idx) {
                highlight_query(text, query, bg, t)
            } else if !highlight_ranges.is_empty() {
                overlay_highlights(text, base_style, highlight_ranges, t.bg_highlight)
            } else {
                Line::styled(text.clone(), base_style)
            }
        }

        VisualLineKind::Code { is_first, is_last } => {
            let prefix = if *is_first {
                "╔ "
            } else if *is_last {
                "╚ "
            } else {
                "║ "
            };
            let code_style = Style::default().bg(t.bg_code).fg(t.text);
            let combined = format!("{}{}", prefix, text);
            let prefix_len = prefix.len();
            if let Some(col) = cursor_col {
                // Cursor lives in the original text; shift past the prefix.
                apply_inline_cursor(&combined, code_style, prefix_len + col, t)
            } else if !highlight_ranges.is_empty() {
                let shifted: Vec<(usize, usize)> = highlight_ranges
                    .iter()
                    .map(|&(s, e)| (s + prefix_len, e + prefix_len))
                    .collect();
                overlay_highlights(&combined, code_style, &shifted, t.bg_highlight)
            } else {
                Line::styled(combined, code_style)
            }
        }

        VisualLineKind::Rule => Line::styled(text.clone(), Style::default().fg(t.rule)),

        VisualLineKind::Image { .. } | VisualLineKind::ImageRow { .. } => {
            // The actual image pixels arrive via the post-draw Kitty `a=T`
            // escape; ratatui paints a blank row underneath so cells exist
            // for the image to land on.
            Line::styled(text.clone(), base_style)
        }

        VisualLineKind::Quote { .. } => {
            // A left rule bar reads as a blockquote far better than a bare
            // indent.  `BAR` is the bar glyph + a space; its byte length is
            // the prefix offset for the cursor/highlight paths, which render
            // the whole line in one style.
            const BAR: &str = "▌ ";
            let prefix_len = BAR.len();
            let quote_style = base_style.fg(t.text_dim).add_modifier(Modifier::ITALIC);
            let bar_style = base_style.fg(t.border_active);
            if let Some(col) = cursor_col {
                apply_inline_cursor(&format!("{BAR}{text}"), quote_style, prefix_len + col, t)
            } else if !highlight_ranges.is_empty() {
                let shifted: Vec<(usize, usize)> = highlight_ranges
                    .iter()
                    .map(|&(s, e)| (s + prefix_len, e + prefix_len))
                    .collect();
                overlay_highlights(&format!("{BAR}{text}"), quote_style, &shifted, t.bg_highlight)
            } else {
                // Bar and text as separate spans so the rule colour is
                // distinct from the (dimmed, italic) quote body.
                Line::from(vec![
                    Span::styled(BAR, bar_style),
                    Span::styled(text.clone(), quote_style),
                ])
            }
        }
    }
}

/// Apply a highlight background to byte ranges within a single-style line.
/// Splits `text` at the range boundaries, emits Spans with the highlight bg
/// applied to bytes inside any range and the base style elsewhere.  All
/// indices in `ranges` must be valid char boundaries within `text`.
fn overlay_highlights(
    text: &str,
    base_style: Style,
    ranges: &[(usize, usize)],
    hl_bg: Color,
) -> Line<'static> {
    let bytes = text.len();
    if bytes == 0 || ranges.is_empty() {
        return Line::styled(text.to_string(), base_style);
    }

    // Build sorted list of cut points: 0, every range start/end, total length.
    // Each range endpoint is snapped down to a valid UTF-8 char boundary
    // so a highlight that straddles a multi-byte char still emits — and
    // emits the full underlying text — even if the highlight bounds
    // themselves are off by a byte or two.
    let mut cuts: Vec<usize> = Vec::with_capacity(ranges.len() * 2 + 2);
    cuts.push(0);
    cuts.push(bytes);
    for &(s, e) in ranges {
        cuts.push(clamp_to_char_boundary(text, s));
        cuts.push(clamp_to_char_boundary(text, e));
    }
    cuts.sort();
    cuts.dedup();

    let mut out: Vec<Span<'static>> = Vec::with_capacity(cuts.len());
    for w in cuts.windows(2) {
        let (s, e) = (w[0], w[1]);
        if s >= e {
            continue;
        }
        let segment = text[s..e].to_string();
        let in_range = ranges.iter().any(|&(rs, re)| s >= rs && e <= re);
        let style = if in_range {
            base_style.bg(hl_bg)
        } else {
            base_style
        };
        out.push(Span::styled(segment, style));
    }
    Line::from(out)
}

/// Apply a highlight background to byte ranges within a multi-style
/// (StyledProse) line.  Walks the inline spans, computing each span's
/// cumulative byte offset within the vl text, and within each span splits
/// at any overlapping highlight range boundaries.  Inline styling
/// (bold/italic/url-underline/etc.) is preserved on every emitted span.
fn overlay_highlights_styled(
    spans: &[doc_model::InlineSpan],
    base_style: Style,
    ranges: &[(usize, usize)],
    hl_bg: Color,
    t: &Theme,
) -> Line<'static> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut byte_cursor = 0usize;

    for ispan in spans {
        let span_start = byte_cursor;
        let span_end = byte_cursor + ispan.text.len();
        byte_cursor = span_end;

        let mut style = base_style;
        if ispan.bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if ispan.italic {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if ispan.underline {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        if ispan.strikethrough {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        if ispan.monospace {
            style = style.fg(t.mono).bg(t.bg_code);
        }
        if let Some((r, g, b)) = ispan.color {
            style = style.fg(Color::Rgb(r, g, b));
        }
        if ispan.url.is_some() {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        if ispan.link_target.is_some() {
            // Refs / citations get `t.link_fg` + underline so they're visibly
            // clickable in body prose.  Combined with the prefix-word
            // back-extension in pandoc_parse, phrases like "Table 3" /
            // "Section 6.2" colour uniformly across the whole label.
            style = style.fg(t.link_fg).add_modifier(Modifier::UNDERLINED);
        }

        // Cut points within this span (in absolute vl-byte coords).
        // Each range endpoint is snapped down to a valid char boundary
        // in this span's local coordinates — see C1 in the persistent-
        // highlight overlay path.  Same rationale: a highlight that
        // straddles a multi-byte char still emits the underlying text.
        let mut cuts: Vec<usize> = vec![span_start, span_end];
        for &(rs, re) in ranges {
            if rs < span_end && re > span_start {
                let local_s = rs.max(span_start) - span_start;
                let local_e = re.min(span_end) - span_start;
                cuts.push(span_start + clamp_to_char_boundary(&ispan.text, local_s));
                cuts.push(span_start + clamp_to_char_boundary(&ispan.text, local_e));
            }
        }
        cuts.sort();
        cuts.dedup();

        for w in cuts.windows(2) {
            let (s, e) = (w[0], w[1]);
            if s >= e {
                continue;
            }
            let local_s = s - span_start;
            let local_e = e - span_start;
            let segment = ispan.text[local_s..local_e].to_string();
            let in_range = ranges.iter().any(|&(rs, re)| s >= rs && e <= re);
            let seg_style = if in_range { style.bg(hl_bg) } else { style };
            out.push(Span::styled(segment, seg_style));
        }
    }
    Line::from(out)
}

/// Render a line with a single character highlighted at `byte_col` (the cursor position).
/// Used to show cursor_x within the current line in Normal mode.
fn apply_char_cursor(text: &str, byte_col: usize, bg: Color, t: &Theme) -> Line<'static> {
    if text.is_empty() {
        return Line::from(vec![Span::styled(
            " ",
            Style::default().bg(t.cursor_bg).fg(t.cursor_fg),
        )]);
    }
    // Snap to nearest valid char boundary at or before byte_col.
    let safe = (0..=byte_col.min(text.len()))
        .rev()
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(0);
    let before = &text[..safe];
    let mut rest_chars = text[safe..].chars();
    let cur: String = rest_chars
        .next()
        .map(|c| c.to_string())
        .unwrap_or_else(|| " ".to_string());
    let after: String = rest_chars.collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    if !before.is_empty() {
        spans.push(Span::styled(before.to_string(), Style::default().bg(bg)));
    }
    spans.push(Span::styled(
        cur,
        Style::default().bg(t.cursor_bg).fg(t.cursor_fg),
    ));
    if !after.is_empty() {
        spans.push(Span::styled(after, Style::default().bg(bg)));
    }
    Line::from(spans)
}

// Unicode "Box Drawing" block (U+2500..U+257F). Used by MatrixLine span
// splitting so vertical separators render with `t.rule` instead of `t.text`.
fn is_box_drawing(ch: char) -> bool {
    matches!(ch, '\u{2500}'..='\u{257F}')
}

/// Snap `byte_col` down to the nearest UTF-8 char boundary at or before it,
/// clamped to the last char start in `text`.
fn snap_to_char_boundary(text: &str, byte_col: usize) -> usize {
    if text.is_empty() {
        return 0;
    }
    let max = text.len() - 1;
    let mut i = byte_col.min(max);
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Clamp `idx` to a valid UTF-8 char boundary at or below it, capped
/// at `text.len()` (the end-of-text boundary, always valid).  Used
/// before slicing highlight / search ranges so a range that straddles
/// a multi-byte character snaps to a safe boundary instead of being
/// silently dropped (or worse, panicking).  Pre-C1 fix, the overlay
/// helpers `continue`d on misaligned boundaries — visually the
/// highlighted segment plus its trailing context just vanished.
fn clamp_to_char_boundary(text: &str, idx: usize) -> usize {
    let mut i = idx.min(text.len());
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Render a single-style line with the cursor cell painted at `byte_col`.
/// Like `apply_char_cursor` but accepts a full `base_style` so callers
/// (Code, Quote) can preserve their fg/bg/modifier alongside the cursor.
fn apply_inline_cursor(text: &str, base_style: Style, byte_col: usize, t: &Theme) -> Line<'static> {
    if text.is_empty() {
        return Line::from(vec![Span::styled(
            " ",
            Style::default().bg(t.cursor_bg).fg(t.cursor_fg),
        )]);
    }
    let safe = snap_to_char_boundary(text, byte_col);
    let before = &text[..safe];
    let mut rest = text[safe..].chars();
    let cur: String = rest
        .next()
        .map(|c| c.to_string())
        .unwrap_or_else(|| " ".to_string());
    let after: String = rest.collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    if !before.is_empty() {
        spans.push(Span::styled(before.to_string(), base_style));
    }
    spans.push(Span::styled(
        cur,
        Style::default().bg(t.cursor_bg).fg(t.cursor_fg),
    ));
    if !after.is_empty() {
        spans.push(Span::styled(after, base_style));
    }
    Line::from(spans)
}

/// Render a `StyledProse` line with the cursor cell painted at `byte_col`.
/// Walks the inline spans, locates the one containing the cursor, splits
/// it at the codepoint boundary, and overrides the cursor cell's style.
/// Existing inline styling (bold/italic/url-underline/colour/monospace)
/// is preserved on every other span so the visible cursor doesn't strip
/// surrounding emphasis.
fn apply_styled_cursor(
    spans: &[doc_model::InlineSpan],
    base_style: Style,
    byte_col: usize,
    t: &Theme,
) -> Line<'static> {
    let total: usize = spans.iter().map(|s| s.text.len()).sum();
    if total == 0 {
        return Line::from(vec![Span::styled(
            " ",
            Style::default().bg(t.cursor_bg).fg(t.cursor_fg),
        )]);
    }
    let safe_col = byte_col.min(total - 1);

    let mut out: Vec<Span<'static>> = Vec::new();
    let mut byte_cursor = 0usize;
    let mut cursor_painted = false;

    for ispan in spans {
        let span_start = byte_cursor;
        let span_end = byte_cursor + ispan.text.len();
        byte_cursor = span_end;

        let mut style = base_style;
        if ispan.bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if ispan.italic {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if ispan.underline {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        if ispan.strikethrough {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        if ispan.monospace {
            style = style.fg(t.mono).bg(t.bg_code);
        }
        if let Some((r, g, b)) = ispan.color {
            style = style.fg(Color::Rgb(r, g, b));
        }
        if ispan.url.is_some() {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        if ispan.link_target.is_some() {
            // Refs / citations get `t.link_fg` + underline so they're visibly
            // clickable in body prose.  Combined with the prefix-word
            // back-extension in pandoc_parse, phrases like "Table 3" /
            // "Section 6.2" colour uniformly across the whole label.
            style = style.fg(t.link_fg).add_modifier(Modifier::UNDERLINED);
        }

        if !cursor_painted && safe_col >= span_start && safe_col < span_end {
            let local = snap_to_char_boundary(&ispan.text, safe_col - span_start);
            let mut next = local + 1;
            while next < ispan.text.len() && !ispan.text.is_char_boundary(next) {
                next += 1;
            }
            if local > 0 {
                out.push(Span::styled(ispan.text[..local].to_string(), style));
            }
            let cur_text = ispan.text.get(local..next).unwrap_or(" ").to_string();
            out.push(Span::styled(
                cur_text,
                Style::default().bg(t.cursor_bg).fg(t.cursor_fg),
            ));
            if next < ispan.text.len() {
                out.push(Span::styled(ispan.text[next..].to_string(), style));
            }
            cursor_painted = true;
        } else {
            out.push(Span::styled(ispan.text.clone(), style));
        }
    }

    // If text was empty or cursor landed exactly past the end, paint a
    // synthetic cell so the cursor is still visible.
    if !cursor_painted {
        out.push(Span::styled(
            " ".to_string(),
            Style::default().bg(t.cursor_bg).fg(t.cursor_fg),
        ));
    }
    Line::from(out)
}

fn highlight_query(text: &str, query: &str, bg: Color, t: &Theme) -> Line<'static> {
    let lower = text.to_lowercase();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut pos = 0;
    let ql = query.len();

    while let Some(start) = lower[pos..].find(query) {
        let abs = pos + start;
        // `lower` can have a different byte structure from `text` when
        // case folding changes encoded width (e.g. İ → i̇).  Snap both
        // endpoints back to valid `text` boundaries so a near-miss
        // highlights an approximate match rather than panicking on a
        // mid-char slice.
        let snap_abs = clamp_to_char_boundary(text, abs);
        let snap_end = clamp_to_char_boundary(text, abs + ql);
        if snap_end <= snap_abs {
            // Match collapsed to nothing after snapping — skip it and
            // advance pos so the loop terminates.
            pos = abs + ql.max(1);
            continue;
        }
        if snap_abs > pos {
            spans.push(Span::styled(
                text[pos..snap_abs].to_string(),
                Style::default().bg(bg),
            ));
        }
        spans.push(Span::styled(
            text[snap_abs..snap_end].to_string(),
            Style::default().bg(t.search_match_bg).fg(t.search_match_fg),
        ));
        pos = snap_end;
    }
    if pos < text.len() {
        spans.push(Span::styled(
            text[pos..].to_string(),
            Style::default().bg(bg),
        ));
    }

    Line::from(spans)
}

/// Render a StyledProse line with search term highlighting.
/// Each span is rendered with its own style; the matching substring is
/// overridden with a yellow-bg highlight wherever it appears.
fn highlight_spans(
    spans: &[doc_model::InlineSpan],
    query: &str,
    bg: Color,
    t: &Theme,
) -> Line<'static> {
    let mut ratatui_spans: Vec<Span<'static>> = Vec::new();

    for s in spans {
        let mut style = Style::default().bg(bg);
        if s.bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if s.italic {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if s.underline {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        if s.strikethrough {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        if s.monospace {
            style = style.fg(t.mono).bg(t.bg_code);
        }
        if let Some((r, g, b)) = s.color {
            style = style.fg(Color::Rgb(r, g, b));
        }

        let lower = s.text.to_lowercase();
        let ql = query.len();
        let mut pos = 0;

        while let Some(start) = lower[pos..].find(query) {
            let abs = pos + start;
            // Same char-boundary defense as `highlight_query` — see C1.
            let snap_abs = clamp_to_char_boundary(&s.text, abs);
            let snap_end = clamp_to_char_boundary(&s.text, abs + ql);
            if snap_end <= snap_abs {
                pos = abs + ql.max(1);
                continue;
            }
            if snap_abs > pos {
                ratatui_spans.push(Span::styled(s.text[pos..snap_abs].to_string(), style));
            }
            ratatui_spans.push(Span::styled(
                s.text[snap_abs..snap_end].to_string(),
                Style::default().bg(t.search_match_bg).fg(t.search_match_fg),
            ));
            pos = snap_end;
        }
        if pos < s.text.len() {
            ratatui_spans.push(Span::styled(s.text[pos..].to_string(), style));
        }
    }

    Line::from(ratatui_spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// C1 regression: highlight ranges that straddle a multi-byte
    /// char used to silently drop the affected segment.  Now they
    /// snap to a safe boundary so the underlying text always emits.
    #[test]
    fn overlay_highlights_renders_full_text_when_range_straddles_multibyte_char() {
        // "café" → 'c' 'a' 'f' [é=2 bytes].  Total 5 bytes.
        // A highlight range of (3, 4) starts before 'é' (ok) and ends
        // INSIDE its UTF-8 sequence (byte 4 is not a char boundary).
        let text = "café";
        let line = overlay_highlights(text, Style::default(), &[(3, 4)], Color::Yellow);
        assert_eq!(
            rendered_text(&line),
            "café",
            "every char of the input must still render after a misaligned highlight",
        );
    }

    #[test]
    fn overlay_highlights_does_not_panic_on_misaligned_range() {
        // Multiple multi-byte chars, range endpoints mid-character.
        let text = "αβγδε"; // each Greek letter is 2 bytes
        let line = overlay_highlights(text, Style::default(), &[(1, 5)], Color::Yellow);
        assert_eq!(rendered_text(&line), "αβγδε");
    }

    #[test]
    fn highlight_query_handles_multibyte_text() {
        // Query is ASCII; text has a multi-byte char before the match.
        let line = highlight_query(
            "café — foo bar",
            "foo",
            Color::Reset,
            &crate::config::resolve_theme(),
        );
        assert_eq!(rendered_text(&line), "café — foo bar");
    }

    fn vl(text: &str, kind: VisualLineKind) -> doc_model::VisualLine {
        doc_model::VisualLine {
            block_idx: 0,
            line_in_block: 0,
            text: text.to_string(),
            kind,
            block_byte_start: 0,
            block_byte_end: text.len(),
        }
    }

    /// Inline code (a monospace span) gets the code background as a
    /// "pill" so it stops blending into surrounding prose.
    #[test]
    fn inline_code_span_gets_background_pill() {
        let t = crate::config::resolve_theme();
        let v = vl(
            "code",
            VisualLineKind::StyledProse(vec![doc_model::InlineSpan {
                text: "code".to_string(),
                monospace: true,
                ..Default::default()
            }]),
        );
        let line = render_visual_line(&v, false, false, false, None, "", &[], 0, &[], &t);
        assert_eq!(line.spans[0].style.bg, Some(t.bg_code));
        assert_eq!(line.spans[0].style.fg, Some(t.mono));
    }

    /// Blockquotes lead with a coloured left rule bar instead of a bare
    /// indent; the bar is its own span so its colour is distinct.
    #[test]
    fn blockquote_leads_with_coloured_bar() {
        let t = crate::config::resolve_theme();
        let v = vl("quoted", VisualLineKind::Quote { is_continuation: false });
        let line = render_visual_line(&v, false, false, false, None, "", &[], 0, &[], &t);
        assert_eq!(line.spans[0].content.as_ref(), "▌ ");
        assert_eq!(line.spans[0].style.fg, Some(t.border_active));
        assert_eq!(rendered_text(&line), "▌ quoted");
    }
}
