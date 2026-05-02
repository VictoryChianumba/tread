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
    Mode::Normal | Mode::Visual { .. } => {
      let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(right);
      (v[0], v[1], None)
    }
    Mode::Search => {
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
      let is_bookmarked = reader.bookmarks.binary_search(&vl_idx).is_ok();
      let is_selected = visual_range.map_or(false, |(lo, hi)| vl_idx >= lo && vl_idx <= hi);
      let cursor_col = if is_cursor { Some(reader.cursor_x) } else { None };
      render_visual_line(vl, is_cursor, is_bookmarked, is_selected, cursor_col, &q, &reader.search_matches, vl_idx, t)
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
      if let Some(col) = cursor_col {
        apply_char_cursor(text, col, bg, t)
      } else {
        Line::styled(text.clone(), base_style.fg(fg).add_modifier(modifier))
      }
    }

    VisualLineKind::MatrixLine { is_header, .. } => {
      let style = if *is_header {
        base_style.fg(t.text).add_modifier(Modifier::BOLD)
      } else {
        base_style.fg(t.text)
      };
      Line::styled(text.clone(), style)
    }

    VisualLineKind::StyledProse(spans) => {
      if !query.is_empty() && matches.contains(&vl_idx) {
        highlight_spans(spans, query, bg, t)
      } else {
        let ratatui_spans: Vec<Span> = spans.iter().map(|s| {
          let mut style = base_style;
          if s.bold        { style = style.add_modifier(Modifier::BOLD); }
          if s.italic      { style = style.add_modifier(Modifier::ITALIC); }
          if s.underline   { style = style.add_modifier(Modifier::UNDERLINED); }
          if s.strikethrough { style = style.add_modifier(Modifier::CROSSED_OUT); }
          if s.monospace   { style = style.fg(t.mono); }
          if let Some((r, g, b)) = s.color { style = style.fg(Color::Rgb(r, g, b)); }
          if let Some(url) = &s.url {
            // OSC 8 clickable link: terminals that don't support it show plain text.
            let linked = format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", url, s.text);
            Span::styled(linked, style)
          } else {
            Span::styled(s.text.clone(), style)
          }
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
      } else {
        Line::styled(text.clone(), base_style)
      }
    }

    VisualLineKind::Code { is_first, is_last } => {
      let prefix = if *is_first { "╔ " } else if *is_last { "╚ " } else { "║ " };
      Line::styled(
        format!("{}{}", prefix, text),
        Style::default().bg(t.bg_code).fg(t.text),
      )
    }

    VisualLineKind::Rule => {
      Line::styled(text.clone(), Style::default().fg(t.rule))
    }

    VisualLineKind::Quote { .. } => {
      Line::styled(
        format!("    {}", text),
        base_style
          .fg(t.text_dim)
          .add_modifier(Modifier::ITALIC),
      )
    }
  }
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
  };
  let count_str = if !reader.count_buf.is_empty() {
    format!("  {}_", reader.count_buf)
  } else {
    String::new()
  };
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

fn draw_help_overlay(frame: &mut Frame, area: Rect, t: &Theme) {
  const W: u16 = 64;
  const H: u16 = 22;
  let x = area.x + area.width.saturating_sub(W) / 2;
  let y = area.y + area.height.saturating_sub(H) / 2;
  let popup = Rect { x, y, width: W.min(area.width), height: H.min(area.height) };

  let help_bg = t.bg_popup;
  let key_fg = t.accent;
  let dim_fg = t.text_dim;
  let sec_fg = t.header;

  let rows: &[(&str, &str, &str, &str)] = &[
    ("j / k",         "scroll down / up",       "] / [",    "next / prev section"),
    ("PageDn / Up",   "full page scroll",        "Ctrl+d/u", "half page"),
    ("} / {",         "next / prev paragraph",   "H / M / L","screen top/mid/bottom"),
    ("g / G",         "top / bottom",            "z",        "center cursor"),
    ("h / l",         "cursor ← / →",            "5j  10G",  "count prefix"),
    ("*",             "search word under cursor", "Ctrl+O",   "go back"),
    ("/  n  N",       "search / next / prev",     "m  '  `",  "bookmark: set/fwd/back"),
    ("y",             "yank line (OSC 52)",        "t",        "toggle TOC"),
    ("q / Esc",       "quit",                     "?",        "this help"),
  ];

  let visual_rows: &[(&str, &str)] = &[
    ("v / V",         "enter char / line visual mode"),
    ("j / k  h / l",  "extend selection"),
    ("y",             "yank selection to clipboard"),
    ("Esc / v / V",   "cancel visual mode"),
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
