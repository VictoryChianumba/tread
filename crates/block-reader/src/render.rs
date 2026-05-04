use doc_model::VisualLineKind;
use ratatui::{
  Frame,
  layout::{Constraint, Direction, Layout, Rect},
  style::{Color, Modifier, Style},
  text::{Line, Span},
  widgets::{Block, Borders, Clear, Paragraph},
};
use ui_theme::Theme;

use crate::state::{Mode, Reader, TOC_WIDTH};

pub fn draw(frame: &mut Frame, reader: &Reader, t: &Theme) {
  let area = frame.area();
  let (header_area, toc_area, content_area, status_area, search_area) =
    split_layout(area, reader);

  if let Some(ha) = header_area {
    draw_header(frame, reader, ha, t);
  }
  if let Some(ta) = toc_area {
    draw_toc(frame, reader, ta, t);
  }
  draw_content(frame, reader, content_area, t);
  draw_status(frame, reader, status_area, t);
  if reader.mode == Mode::Search {
    draw_search_bar(frame, reader, search_area.unwrap(), t);
  }
  if reader.mode == Mode::Command {
    draw_command_bar(frame, reader, search_area.unwrap(), t);
  }
  if let Some(popup) = &reader.popup {
    draw_text_popup(frame, area, t, popup);
  }
  if reader.help_visible {
    draw_help_overlay(frame, area, t);
  }
}

fn split_layout(
  area: Rect,
  reader: &Reader,
) -> (Option<Rect>, Option<Rect>, Rect, Rect, Option<Rect>) {
  // Optional 1-row header at the very top.
  let (header_area, below_header) = if reader.meta.is_some() {
    let v = Layout::default()
      .direction(Direction::Vertical)
      .constraints([Constraint::Length(1), Constraint::Min(1)])
      .split(area);
    (Some(v[0]), v[1])
  } else {
    (None, area)
  };

  // Optional TOC panel on the left.
  let (toc_area, right) = if reader.toc_visible {
    let h = Layout::default()
      .direction(Direction::Horizontal)
      .constraints([Constraint::Length(TOC_WIDTH as u16), Constraint::Min(1)])
      .split(below_header);
    (Some(h[0]), h[1])
  } else {
    (None, below_header)
  };

  let (content_area, status_area, search_area) = match reader.mode {
    Mode::Normal | Mode::Visual { .. } | Mode::AwaitingChar { .. } | Mode::AwaitingMarkName { .. } | Mode::AwaitingG | Mode::AwaitingOperator { .. } | Mode::AwaitingTextObject { .. } => {
      let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(right);
      (v[0], v[1], None)
    }
    Mode::Search | Mode::Command => {
      // Command mode reuses the search-bar slot — same shape, different prefix.
      let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1), Constraint::Length(1)])
        .split(right);
      (v[0], v[1], Some(v[2]))
    }
  };

  (header_area, toc_area, content_area, status_area, search_area)
}

fn draw_header(frame: &mut Frame, reader: &Reader, area: Rect, t: &Theme) {
  let Some(meta) = &reader.meta else { return };
  let w = area.width as usize;
  let title = &meta.title;
  let sep = if meta.authors.is_empty() { "" } else { "  " };
  let raw = format!(" {}{}{}", title, sep, meta.authors);
  let truncated = toc_trunc(&raw, w);
  let header = Paragraph::new(truncated)
    .style(Style::default().bg(t.bg_panel).fg(t.accent));
  frame.render_widget(header, area);
}

fn draw_content(frame: &mut Frame, reader: &Reader, area: Rect, t: &Theme) {
  let ch = area.height as usize;
  let total = reader.total_lines();
  let q = reader.search_query.to_lowercase();

  let visual_range: Option<(usize, usize)> = match &reader.mode {
    Mode::Visual { .. } => {
      let cur = reader.current_line();
      let anchor = reader.visual_anchor;
      Some((cur.min(anchor), cur.max(anchor)))
    }
    _ => None,
  };

  let lines: Vec<Line> = (0..ch)
    .map(|row| {
      let vl_idx = reader.offset + row;
      if vl_idx >= total {
        return Line::raw("");
      }
      let vl = &reader.visual_lines[vl_idx];
      let is_cursor = row == reader.cursor_y;
      let is_bookmarked = reader.bookmarks.values().any(|&l| l == vl_idx);
      let is_selected = visual_range.map_or(false, |(lo, hi)| vl_idx >= lo && vl_idx <= hi);
      let cursor_col = if is_cursor { Some(reader.cursor_x) } else { None };
      // Compute persistent-highlight byte ranges that overlap this VL,
      // translated into vl-local byte coordinates for the renderer.
      let highlight_ranges: Vec<(usize, usize)> = if vl.block_byte_end > vl.block_byte_start {
        reader.highlights
          .overlapping(vl.block_idx, vl.block_byte_start..vl.block_byte_end)
          .map(|h| (
            h.byte_start.max(vl.block_byte_start) - vl.block_byte_start,
            h.byte_end.min(vl.block_byte_end) - vl.block_byte_start,
          ))
          .collect()
      } else {
        Vec::new()
      };
      render_visual_line(vl, is_cursor, is_bookmarked, is_selected, cursor_col, &q, &reader.search_matches, vl_idx, &highlight_ranges, t)
    })
    .collect();

  let paragraph = Paragraph::new(lines).block(Block::default());
  frame.render_widget(paragraph, area);
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

    VisualLineKind::MathLine { .. } => {
      Line::styled(text.clone(), base_style.fg(t.math))
    }

    VisualLineKind::Header(level) => {
      let (fg, modifier) = match level {
        1 => (t.accent, Modifier::BOLD),
        2 => (t.header, Modifier::BOLD),
        _ => (t.header, Modifier::empty()),
      };
      let hdr_style = base_style.fg(fg).add_modifier(modifier);
      if let Some(col) = cursor_col {
        apply_char_cursor(text, col, bg, t)
      } else if !highlight_ranges.is_empty() {
        overlay_highlights(text, hdr_style, highlight_ranges, t.bg_highlight)
      } else {
        Line::styled(text.clone(), hdr_style)
      }
    }

    VisualLineKind::MatrixLine { is_header, .. } => {
      // Cell content uses the body text colour (bold on headers); the
      // box-drawing chars (│ ┬ ┼ ┴ ├ ┤ ┌ ┐ └ ┘ ─) are repainted with the
      // dimmer `t.rule` colour so vertical rules visually match horizontal
      // ones. Without this split, ratatui paints the whole line with the
      // cell style and the verticals look brighter (and bold on headers).
      let cell_style = if *is_header {
        base_style.fg(t.text).add_modifier(Modifier::BOLD)
      } else {
        base_style.fg(t.text)
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
          spans.push(Span::styled(" ", Style::default().bg(t.cursor_bg).fg(t.cursor_fg)));
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
        let ratatui_spans: Vec<Span> = spans.iter().map(|s| {
          let mut style = base_style;
          if s.bold        { style = style.add_modifier(Modifier::BOLD); }
          if s.italic      { style = style.add_modifier(Modifier::ITALIC); }
          if s.underline   { style = style.add_modifier(Modifier::UNDERLINED); }
          if s.strikethrough { style = style.add_modifier(Modifier::CROSSED_OUT); }
          if s.monospace   { style = style.fg(t.mono); }
          if let Some((r, g, b)) = s.color { style = style.fg(Color::Rgb(r, g, b)); }
          if s.url.is_some() {
            // Mark external URLs with underline. Embedding raw OSC 8 sequences in ratatui
            // Spans corrupts cell-width accounting; ratatui counts escape bytes as columns.
            style = style.add_modifier(Modifier::UNDERLINED);
          }
          if s.link_target.is_some() {
            // Underline-only — see note in `apply_styled_cursor`.
            style = style.add_modifier(Modifier::UNDERLINED);
          }
          Span::styled(s.text.clone(), style)
        }).collect();
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
      let prefix = if *is_first { "╔ " } else if *is_last { "╚ " } else { "║ " };
      let code_style = Style::default().bg(t.bg_code).fg(t.text);
      let combined = format!("{}{}", prefix, text);
      let prefix_len = prefix.len();
      if let Some(col) = cursor_col {
        // Cursor lives in the original text; shift past the prefix.
        apply_inline_cursor(&combined, code_style, prefix_len + col, t)
      } else if !highlight_ranges.is_empty() {
        let shifted: Vec<(usize, usize)> = highlight_ranges.iter()
          .map(|&(s, e)| (s + prefix_len, e + prefix_len))
          .collect();
        overlay_highlights(&combined, code_style, &shifted, t.bg_highlight)
      } else {
        Line::styled(combined, code_style)
      }
    }

    VisualLineKind::Rule => {
      Line::styled(text.clone(), Style::default().fg(t.rule))
    }

    VisualLineKind::Quote { .. } => {
      let quote_style = base_style
        .fg(t.text_dim)
        .add_modifier(Modifier::ITALIC);
      let combined = format!("    {}", text);
      let prefix_len = 4; // "    "
      if let Some(col) = cursor_col {
        apply_inline_cursor(&combined, quote_style, prefix_len + col, t)
      } else if !highlight_ranges.is_empty() {
        let shifted: Vec<(usize, usize)> = highlight_ranges.iter()
          .map(|&(s, e)| (s + prefix_len, e + prefix_len))
          .collect();
        overlay_highlights(&combined, quote_style, &shifted, t.bg_highlight)
      } else {
        Line::styled(combined, quote_style)
      }
    }
  }
}

/// Apply a highlight background to byte ranges within a single-style line.
/// Splits `text` at the range boundaries, emits Spans with the highlight bg
/// applied to bytes inside any range and the base style elsewhere.  All
/// indices in `ranges` must be valid char boundaries within `text`.
fn overlay_highlights(text: &str, base_style: Style, ranges: &[(usize, usize)], hl_bg: Color) -> Line<'static> {
  let bytes = text.len();
  if bytes == 0 || ranges.is_empty() {
    return Line::styled(text.to_string(), base_style);
  }

  // Build sorted list of cut points: 0, every range start/end, total length.
  let mut cuts: Vec<usize> = Vec::with_capacity(ranges.len() * 2 + 2);
  cuts.push(0);
  cuts.push(bytes);
  for &(s, e) in ranges {
    cuts.push(s.min(bytes));
    cuts.push(e.min(bytes));
  }
  cuts.sort();
  cuts.dedup();

  let mut out: Vec<Span<'static>> = Vec::with_capacity(cuts.len());
  for w in cuts.windows(2) {
    let (s, e) = (w[0], w[1]);
    if s >= e || !text.is_char_boundary(s) || !text.is_char_boundary(e) { continue; }
    let segment = text[s..e].to_string();
    let in_range = ranges.iter().any(|&(rs, re)| s >= rs && e <= re);
    let style = if in_range { base_style.bg(hl_bg) } else { base_style };
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
    if ispan.bold        { style = style.add_modifier(Modifier::BOLD); }
    if ispan.italic      { style = style.add_modifier(Modifier::ITALIC); }
    if ispan.underline   { style = style.add_modifier(Modifier::UNDERLINED); }
    if ispan.strikethrough { style = style.add_modifier(Modifier::CROSSED_OUT); }
    if ispan.monospace   { style = style.fg(t.mono); }
    if let Some((r, g, b)) = ispan.color { style = style.fg(Color::Rgb(r, g, b)); }
    if ispan.url.is_some() { style = style.add_modifier(Modifier::UNDERLINED); }
    if ispan.link_target.is_some() {
      // Underline-only — same visual treatment as external URLs.  We
      // tried tinting with a dedicated `link_fg` colour but it read as
      // distracting; underline alone is enough to mark interactive
      // text without bright shifts in body prose.
      style = style.add_modifier(Modifier::UNDERLINED);
    }

    // Cut points within this span (in absolute vl-byte coords).
    let mut cuts: Vec<usize> = vec![span_start, span_end];
    for &(rs, re) in ranges {
      if rs < span_end && re > span_start {
        cuts.push(rs.max(span_start));
        cuts.push(re.min(span_end));
      }
    }
    cuts.sort();
    cuts.dedup();

    for w in cuts.windows(2) {
      let (s, e) = (w[0], w[1]);
      if s >= e { continue; }
      let local_s = s - span_start;
      let local_e = e - span_start;
      if !ispan.text.is_char_boundary(local_s) || !ispan.text.is_char_boundary(local_e) {
        continue;
      }
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
  let cur: String = rest_chars.next().map(|c| c.to_string()).unwrap_or_else(|| " ".to_string());
  let after: String = rest_chars.collect();
  let mut spans: Vec<Span<'static>> = Vec::new();
  if !before.is_empty() {
    spans.push(Span::styled(before.to_string(), Style::default().bg(bg)));
  }
  spans.push(Span::styled(cur, Style::default().bg(t.cursor_bg).fg(t.cursor_fg)));
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
  if text.is_empty() { return 0; }
  let max = text.len() - 1;
  let mut i = byte_col.min(max);
  while i > 0 && !text.is_char_boundary(i) { i -= 1; }
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
  let cur: String = rest.next().map(|c| c.to_string()).unwrap_or_else(|| " ".to_string());
  let after: String = rest.collect();
  let mut spans: Vec<Span<'static>> = Vec::new();
  if !before.is_empty() {
    spans.push(Span::styled(before.to_string(), base_style));
  }
  spans.push(Span::styled(cur, Style::default().bg(t.cursor_bg).fg(t.cursor_fg)));
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
    if ispan.bold        { style = style.add_modifier(Modifier::BOLD); }
    if ispan.italic      { style = style.add_modifier(Modifier::ITALIC); }
    if ispan.underline   { style = style.add_modifier(Modifier::UNDERLINED); }
    if ispan.strikethrough { style = style.add_modifier(Modifier::CROSSED_OUT); }
    if ispan.monospace   { style = style.fg(t.mono); }
    if let Some((r, g, b)) = ispan.color { style = style.fg(Color::Rgb(r, g, b)); }
    if ispan.url.is_some() { style = style.add_modifier(Modifier::UNDERLINED); }
    if ispan.link_target.is_some() {
      // Underline-only — same visual treatment as external URLs.  We
      // tried tinting with a dedicated `link_fg` colour but it read as
      // distracting; underline alone is enough to mark interactive
      // text without bright shifts in body prose.
      style = style.add_modifier(Modifier::UNDERLINED);
    }

    if !cursor_painted && safe_col >= span_start && safe_col < span_end {
      let local = snap_to_char_boundary(&ispan.text, safe_col - span_start);
      let mut next = local + 1;
      while next < ispan.text.len() && !ispan.text.is_char_boundary(next) { next += 1; }
      if local > 0 {
        out.push(Span::styled(ispan.text[..local].to_string(), style));
      }
      let cur_text = ispan.text.get(local..next).unwrap_or(" ").to_string();
      out.push(Span::styled(cur_text, Style::default().bg(t.cursor_bg).fg(t.cursor_fg)));
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
    if abs > pos {
      spans.push(Span::styled(text[pos..abs].to_string(), Style::default().bg(bg)));
    }
    spans.push(Span::styled(
      text[abs..abs + ql].to_string(),
      Style::default().bg(t.search_match_bg).fg(t.search_match_fg),
    ));
    pos = abs + ql;
  }
  if pos < text.len() {
    spans.push(Span::styled(text[pos..].to_string(), Style::default().bg(bg)));
  }

  Line::from(spans)
}

/// Render a StyledProse line with search term highlighting.
/// Each span is rendered with its own style; the matching substring is
/// overridden with a yellow-bg highlight wherever it appears.
fn highlight_spans(spans: &[doc_model::InlineSpan], query: &str, bg: Color, t: &Theme) -> Line<'static> {
  let mut ratatui_spans: Vec<Span<'static>> = Vec::new();

  for s in spans {
    let mut style = Style::default().bg(bg);
    if s.bold        { style = style.add_modifier(Modifier::BOLD); }
    if s.italic      { style = style.add_modifier(Modifier::ITALIC); }
    if s.underline   { style = style.add_modifier(Modifier::UNDERLINED); }
    if s.strikethrough { style = style.add_modifier(Modifier::CROSSED_OUT); }
    if s.monospace   { style = style.fg(t.mono); }
    if let Some((r, g, b)) = s.color { style = style.fg(Color::Rgb(r, g, b)); }

    let lower = s.text.to_lowercase();
    let ql = query.len();
    let mut pos = 0;

    while let Some(start) = lower[pos..].find(query) {
      let abs = pos + start;
      if abs > pos {
        ratatui_spans.push(Span::styled(s.text[pos..abs].to_string(), style));
      }
      ratatui_spans.push(Span::styled(
        s.text[abs..abs + ql].to_string(),
        Style::default().bg(t.search_match_bg).fg(t.search_match_fg),
      ));
      pos = abs + ql;
    }
    if pos < s.text.len() {
      ratatui_spans.push(Span::styled(s.text[pos..].to_string(), style));
    }
  }

  Line::from(ratatui_spans)
}

fn draw_toc(frame: &mut Frame, reader: &Reader, area: Rect, t: &Theme) {
  let panel_h = area.height as usize;
  // 1 char right border + 1 char leading space = 2 chars overhead
  let inner_w = area.width.saturating_sub(2) as usize;
  let cur_sec = reader.current_section_idx();

  // Scroll to keep current section vertically centered in the panel.
  let toc_scroll = cur_sec
    .map(|idx| idx.saturating_sub(panel_h / 2))
    .unwrap_or(0);

  let total = reader.sections.len();

  let lines: Vec<Line> = (0..panel_h)
    .map(|row| {
      let sec_idx = toc_scroll + row;
      if sec_idx >= total {
        return Line::raw("");
      }
      let (_, level, text) = &reader.sections[sec_idx];
      let indent = match level {
        1 => 0usize,
        2 => 2usize,
        _ => 4usize,
      };
      let avail = inner_w.saturating_sub(indent);
      let label = format!(" {}{}", " ".repeat(indent), toc_trunc(text, avail));
      let is_current = cur_sec.map_or(false, |c| c == sec_idx);
      if is_current {
        Line::styled(label, Style::default().fg(t.accent).add_modifier(Modifier::BOLD))
      } else {
        Line::styled(label, Style::default().fg(t.toc_dim))
      }
    })
    .collect();

  let widget = Paragraph::new(lines).block(
    Block::default()
      .borders(Borders::RIGHT)
      .border_style(Style::default().fg(t.text_dim)),
  );
  frame.render_widget(widget, area);
}

fn toc_trunc(s: &str, max: usize) -> String {
  if max == 0 {
    return String::new();
  }
  let count = s.chars().count();
  if count <= max {
    s.to_string()
  } else if max > 1 {
    let end = s.char_indices().nth(max - 1).map(|(i, _)| i).unwrap_or(s.len());
    format!("{}…", &s[..end])
  } else {
    s.chars().take(max).collect()
  }
}

fn draw_status(frame: &mut Frame, reader: &Reader, area: Rect, t: &Theme) {
  let cur = reader.current_line() + 1;
  let tot = reader.total_lines();
  let pct = if tot == 0 { 0 } else { cur * 100 / tot };
  let match_info = if !reader.search_matches.is_empty() {
    format!("  [{}/{}]", reader.search_idx + 1, reader.search_matches.len())
  } else {
    String::new()
  };
  let mode_str = match &reader.mode {
    Mode::Normal | Mode::Search => String::new(),
    Mode::Visual { line_mode } => {
      if *line_mode { "  VISUAL LINE".to_string() } else { "  VISUAL".to_string() }
    }
    Mode::AwaitingChar { kind } => {
      // Show the operator we're awaiting a target for, e.g. "  f_".
      let prefix = match kind {
        crate::state::FindKind::F      => "f",
        crate::state::FindKind::ShiftF => "F",
        crate::state::FindKind::T      => "t",
        crate::state::FindKind::ShiftT => "T",
      };
      format!("  {prefix}_")
    }
    Mode::AwaitingMarkName { for_set } => {
      if *for_set { "  m_".to_string() } else { "  '_".to_string() }
    }
    Mode::AwaitingG => "  g_".to_string(),
    Mode::Command => String::new(), // command bar shows its own prompt
    Mode::AwaitingOperator { op } => match op {
      crate::state::Operator::Yank => "  y_".to_string(),
    },
    Mode::AwaitingTextObject { op, around } => {
      let prefix = match op { crate::state::Operator::Yank => "y" };
      let mid = if *around { "a" } else { "i" };
      format!("  {prefix}{mid}_")
    }
  };
  let count_str = if !reader.count_buf.is_empty() {
    format!("  {}_", reader.count_buf)
  } else {
    String::new()
  };
  // Errors from the most recent `:` command override the rest of the status
  // line so they're hard to miss.  Cleared on the next keystroke.
  if let Some(err) = &reader.cmd_error {
    let status = Paragraph::new(format!(" {err}"))
      .style(Style::default().bg(t.bg_input).fg(t.error));
    frame.render_widget(status, area);
    return;
  }
  let text = format!(" {cur}/{tot}  {pct}%{match_info}{mode_str}{count_str}");
  let status = Paragraph::new(text)
    .style(Style::default().bg(t.bg_input).fg(t.text_dim));
  frame.render_widget(status, area);
}

fn draw_search_bar(frame: &mut Frame, reader: &Reader, area: Rect, t: &Theme) {
  let text = format!("/{}", reader.search_query);
  let bar = Paragraph::new(text)
    .style(Style::default().bg(t.bg_input).fg(t.cursor_bg));
  frame.render_widget(bar, area);
}

fn draw_command_bar(frame: &mut Frame, reader: &Reader, area: Rect, t: &Theme) {
  let text = format!(":{}_", reader.cmd_buf);
  let bar = Paragraph::new(text)
    .style(Style::default().bg(t.bg_input).fg(t.cursor_bg));
  frame.render_widget(bar, area);
}

/// Render a generic text popup (used by `:marks`, `:highlights`, `:about`,
/// `:placement`).  Sized to the longest line + padding, centered in `area`.
/// Dismissed by any keystroke (handled in the event loop).
fn draw_text_popup(frame: &mut Frame, area: Rect, t: &Theme, popup: &crate::state::PopupContent) {
  use ratatui::text::Line;
  let max_line = popup.lines.iter().map(|l| l.chars().count()).max().unwrap_or(0)
    .max(popup.title.chars().count() + 4);
  let w = (max_line as u16 + 4).min(area.width.saturating_sub(4)).max(20);
  let h = (popup.lines.len() as u16 + 4).min(area.height.saturating_sub(4)).max(6);
  let x = area.x + area.width.saturating_sub(w) / 2;
  let y = area.y + area.height.saturating_sub(h) / 2;
  let popup_area = Rect { x, y, width: w, height: h };

  let mut lines: Vec<Line> = Vec::new();
  lines.push(Line::styled(
    format!("  {}", popup.title),
    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
  ));
  lines.push(Line::raw(""));
  for l in &popup.lines {
    lines.push(Line::styled(l.clone(), Style::default().fg(t.text)));
  }
  lines.push(Line::raw(""));
  lines.push(Line::styled(
    "  Press any key to dismiss",
    Style::default().fg(t.text_dim),
  ));

  frame.render_widget(Clear, popup_area);
  let para = Paragraph::new(lines)
    .style(Style::default().bg(t.bg_popup))
    .block(
      Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(t.text_dim)),
    );
  frame.render_widget(para, popup_area);
}

fn draw_help_overlay(frame: &mut Frame, area: Rect, t: &Theme) {
  const W: u16 = 64;
  const H: u16 = 44;
  let x = area.x + area.width.saturating_sub(W) / 2;
  let y = area.y + area.height.saturating_sub(H) / 2;
  let popup = Rect { x, y, width: W.min(area.width), height: H.min(area.height) };

  let help_bg = t.bg_popup;
  let key_fg = t.accent;
  let dim_fg = t.text_dim;
  let sec_fg = t.header;

  let rows: &[(&str, &str, &str, &str)] = &[
    ("j / k",         "scroll down / up",        "] / [",    "next / prev section"),
    ("PageDn / Up",   "full page scroll",        "Ctrl+d/u", "half page"),
    ("} / {",         "next / prev paragraph",   "( / )",    "prev / next sentence"),
    ("gg / G",        "top / bottom",            "H / M / L","screen top/mid/bottom"),
    ("h / l",         "cursor ← / → (wraps)",    "z",        "center cursor"),
    ("w / W",         "word forward (small/BIG)","b / B",    "word back (small/BIG)"),
    ("e / E",         "word end (small/BIG)",    "ge / gE",  "back to word end"),
    ("0  ^  $",       "line start / first / end","5j 10G",   "count prefix"),
    ("f / F",         "find char fwd / back",    "t / T",    "till char fwd / back"),
    ("%",             "matching brace",          "*",        "search word under cursor"),
    ("/  n  N",       "search / next / prev",    "m{a} '{a}","set/jump named mark"),
    ("yy",            "yank current line (OSC 52)","\\",      "toggle TOC"),
    ("yi/ya<obj>",    "yank inner / around obj",  "X",        "remove highlight"),
    ("Enter",         "follow link / citation",   "K",        "popup citation entry"),
    (":",             "command mode (see below)", "Ctrl+O",   "go back"),
    ("?",             "this help",                "q / Esc",  "quit"),
  ];

  let visual_rows: &[(&str, &str)] = &[
    ("v / V",         "enter char / line visual mode"),
    ("j / k  h / l",  "extend selection"),
    ("y",             "yank selection to clipboard"),
    ("H",             "highlight selection (persists)"),
    ("Esc / v / V",   "cancel visual mode"),
  ];

  let cmd_rows: &[(&str, &str)] = &[
    (":<N>",            "go to line N"),
    (":goto <N|text>",  "jump to section"),
    (":abstract",       "jump to abstract"),
    (":references",     "jump to references"),
    (":set theme=<id>", "change theme (or 'trench' to sync)"),
    (":marks",          "list named marks"),
    (":highlights",     "list highlights"),
    (":about",          "paper metadata"),
    (":url  :cite",     "copy URL / BibTeX to clipboard"),
    (":open",           "open URL in browser"),
    (":q  :help",       "quit / help"),
  ];

  let mut lines: Vec<Line> = vec![
    Line::styled(
      "  Keybindings",
      Style::default().fg(key_fg).add_modifier(Modifier::BOLD),
    ),
    Line::raw(""),
  ];
  for (k1, d1, k2, d2) in rows {
    let left  = format!("  {:<14} {:<24}", k1, d1);
    let right = format!("{:<10} {}", k2, d2);
    lines.push(Line::from(vec![
      Span::styled(left,  Style::default().fg(key_fg)),
      Span::styled(right, Style::default().fg(dim_fg)),
    ]));
  }
  lines.push(Line::raw(""));
  lines.push(Line::styled(
    "  Visual mode",
    Style::default().fg(sec_fg).add_modifier(Modifier::BOLD),
  ));
  for (k, d) in visual_rows {
    lines.push(Line::from(vec![
      Span::styled(format!("  {:<16} ", k), Style::default().fg(key_fg)),
      Span::styled(d.to_string(), Style::default().fg(dim_fg)),
    ]));
  }
  lines.push(Line::raw(""));
  lines.push(Line::styled(
    "  Command mode  (`:` to enter)",
    Style::default().fg(sec_fg).add_modifier(Modifier::BOLD),
  ));
  for (k, d) in cmd_rows {
    lines.push(Line::from(vec![
      Span::styled(format!("  {:<18} ", k), Style::default().fg(key_fg)),
      Span::styled(d.to_string(), Style::default().fg(dim_fg)),
    ]));
  }
  lines.push(Line::raw(""));
  lines.push(Line::styled(
    "  Press any key to dismiss",
    Style::default().fg(dim_fg),
  ));

  frame.render_widget(Clear, popup);
  let widget = Paragraph::new(lines)
    .style(Style::default().bg(help_bg))
    .block(
      Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(t.text_dim))
        .style(Style::default().bg(help_bg)),
    );
  frame.render_widget(widget, popup);
}
