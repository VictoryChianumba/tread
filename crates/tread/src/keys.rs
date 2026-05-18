//! Per-mode key handlers dispatched from `Reader::handle_event`.  One
//! function per `Mode` variant, plus the visual-selection helpers
//! (yank, commit-as-highlight) and the OSC52 clipboard write.  All
//! state mutation goes through `Reader`'s seam methods (`nav_*`,
//! `enter_*`, `return_to_normal`, …); this module is the dispatcher,
//! not a state owner.

use crate::commands::{self, ReaderAction};
use crate::state::{FindKind, Mode, Operator, Reader};
use crate::text_objects;
use crossterm::event::{KeyCode, KeyModifiers};

pub(crate) fn take_count(reader: &mut Reader) -> usize {
  if reader.count_buf.is_empty() {
    1
  } else {
    let n: usize = reader.count_buf.parse().unwrap_or(1).max(1).min(9999);
    reader.count_buf.clear();
    n
  }
}

pub(crate) fn handle_normal(reader: &mut Reader, code: KeyCode, mods: KeyModifiers) -> bool {
  // Dismiss help overlay on any key.
  if reader.help_visible {
    reader.help_visible = false;
    return false;
  }

  // Voice keys take priority — `r`, `R`, `Ctrl+P` enter or restart
  // reading mode; `Space`, `c`, `Esc` only fire while reading_mode is
  // true.  Putting this BEFORE the digit accumulator and the global
  // `Esc`-quits match means reading-mode Esc stops audio without
  // also quitting the reader.
  let key_event = crossterm::event::KeyEvent::new(code, mods);
  if crate::state::voice_control::handle_voice_keys(reader, key_event) {
    reader.count_buf.clear();
    return false;
  }

  // Digit accumulation for count prefix (1–9 to start, 0 only after first digit).
  if let KeyCode::Char(c) = code {
    if c.is_ascii_digit() && (c != '0' || !reader.count_buf.is_empty()) {
      reader.count_buf.push(c);
      return false;
    }
  }

  match code {
    KeyCode::Char('q') | KeyCode::Esc => {
      reader.count_buf.clear();
      return true;
    }
    KeyCode::Char('j') | KeyCode::Down => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_down();
      }
    }
    KeyCode::Char('k') | KeyCode::Up => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_up();
      }
    }
    KeyCode::Char('g') => {
      reader.enter_awaiting_g();
    }
    KeyCode::Char('G') => {
      if reader.count_buf.is_empty() {
        reader.nav_bottom();
      } else {
        let n = take_count(reader);
        let target = n
          .saturating_sub(1)
          .min(reader.total_lines().saturating_sub(1));
        reader.push_nav_mark();
        reader.jump_to_line(target);
      }
    }
    KeyCode::Char('d') if mods.contains(KeyModifiers::CONTROL) => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_half_page_down();
      }
    }
    KeyCode::Char('u') if mods.contains(KeyModifiers::CONTROL) => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_half_page_up();
      }
    }
    KeyCode::PageDown => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_page_down();
      }
    }
    KeyCode::PageUp => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_page_up();
      }
    }
    KeyCode::Char('}') => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.jump_next_paragraph();
      }
    }
    KeyCode::Char('{') => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.jump_prev_paragraph();
      }
    }
    KeyCode::Char('H') => {
      reader.count_buf.clear();
      reader.jump_screen_top();
    }
    KeyCode::Char('M') => {
      reader.count_buf.clear();
      reader.jump_screen_middle();
    }
    KeyCode::Char('L') => {
      reader.count_buf.clear();
      reader.jump_screen_bottom();
    }
    KeyCode::Char('z') => {
      reader.count_buf.clear();
      reader.center_cursor();
    }
    KeyCode::Char('h') | KeyCode::Left => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_left();
      }
    }
    KeyCode::Char('l') | KeyCode::Right => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_right();
      }
    }
    // Word motions — `w`/`W` forward, `b`/`B` back, `e`/`E` to word-end.
    KeyCode::Char('w') => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_word_forward(false);
      }
      reader.remember_column();
    }
    KeyCode::Char('W') => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_word_forward(true);
      }
      reader.remember_column();
    }
    KeyCode::Char('b') => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_word_back(false);
      }
      reader.remember_column();
    }
    KeyCode::Char('B') => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_word_back(true);
      }
      reader.remember_column();
    }
    KeyCode::Char('e') => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_word_end(false);
      }
      reader.remember_column();
    }
    KeyCode::Char('E') => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_word_end(true);
      }
      reader.remember_column();
    }
    // Line edges — `0` to byte 0, `^` to first non-blank, `$` to last char.
    KeyCode::Char('0') => {
      reader.count_buf.clear();
      reader.nav_line_start();
      reader.remember_column();
    }
    KeyCode::Char('^') => {
      reader.count_buf.clear();
      reader.nav_line_first_nonblank();
      reader.remember_column();
    }
    KeyCode::Char('$') => {
      reader.count_buf.clear();
      reader.nav_line_end();
      reader.remember_column();
    }
    // Find char on current line — enters AwaitingChar mode for the next keystroke.
    KeyCode::Char('f') => {
      reader.enter_awaiting_char(FindKind::F);
    }
    KeyCode::Char('F') => {
      reader.enter_awaiting_char(FindKind::ShiftF);
    }
    KeyCode::Char('t') => {
      reader.enter_awaiting_char(FindKind::T);
    }
    KeyCode::Char('T') => {
      reader.enter_awaiting_char(FindKind::ShiftT);
    }
    // Matching brace — `%` jumps between paired brackets on the current line.
    KeyCode::Char('%') => {
      reader.count_buf.clear();
      reader.nav_match_brace();
      reader.remember_column();
    }
    // Sentence motion — `)` next, `(` previous.  Cross-line.
    KeyCode::Char(')') => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_sentence_forward();
      }
      reader.remember_column();
    }
    KeyCode::Char('(') => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_sentence_back();
      }
      reader.remember_column();
    }
    // Cross-reference / citation actions.  `Enter` jumps to the link
    // target under the cursor (no-op if not on a link); `K` and
    // `Shift+Enter` show a citation popup instead of jumping.
    // (`Shift+Enter` requires the kitty keyboard protocol; `K` is the
    // universal fallback — vim's canonical "look up" gesture, distinct
    // from lowercase `k` which is cursor-up.)
    KeyCode::Enter if mods.contains(KeyModifiers::SHIFT) => {
      reader.count_buf.clear();
      popup_citation_at_cursor(reader);
    }
    KeyCode::Enter => {
      reader.count_buf.clear();
      follow_link_at_cursor(reader);
    }
    KeyCode::Char('K') => {
      reader.count_buf.clear();
      popup_citation_at_cursor(reader);
    }
    // Remove highlight under cursor — eXcise.
    KeyCode::Char('X') => {
      reader.count_buf.clear();
      if let Some(vl) = reader.visual_lines.get(reader.current_line()) {
        if vl.block_byte_end > vl.block_byte_start {
          let local = reader
            .cursor_x()
            .min(vl.block_byte_end - vl.block_byte_start - 1);
          let byte_in_block = vl.block_byte_start + local;
          reader.highlights.remove_at(vl.block_idx, byte_in_block);
        }
      }
    }
    KeyCode::Char('*') => {
      reader.count_buf.clear();
      if let Some(word) = reader.word_at_cursor() {
        reader.search_query = word;
        reader.update_search_matches();
        if !reader.search_matches.is_empty() {
          reader.push_nav_mark();
          let idx = reader.search_idx;
          reader.jump_to_match(idx);
        }
      }
    }
    KeyCode::Char(':') => {
      reader.enter_command_mode();
    }
    KeyCode::Char('/') => {
      reader.count_buf.clear();
      reader.enter_search();
    }
    KeyCode::Char('n') => {
      reader.count_buf.clear();
      reader.search_next();
    }
    KeyCode::Char('N') => {
      reader.count_buf.clear();
      reader.search_prev();
    }
    KeyCode::Char(']') => {
      // `]` is now a prefix (vim convention): `]]` jumps section,
      // `]f` steps the figure preview.  Section jump is one keystroke
      // longer than before but matches vim's section-motion idiom.
      reader.enter_awaiting_bracket(true);
    }
    KeyCode::Char('[') => {
      reader.enter_awaiting_bracket(false);
    }
    KeyCode::Char('i') => {
      // Figure-preview side pane.  Toggle is a single keystroke
      // because tread has no insert mode to collide with.
      reader.count_buf.clear();
      reader.toggle_figure_preview();
    }
    // TOC moved off `t` to free the key for vim's `t<char>` find motion.
    KeyCode::Char('\\') => {
      reader.count_buf.clear();
      reader.toggle_toc();
    }
    KeyCode::Char('o') if mods.contains(KeyModifiers::CONTROL) => {
      reader.count_buf.clear();
      reader.nav_back();
    }
    KeyCode::Char('?') => {
      reader.count_buf.clear();
      reader.toggle_help();
    }
    KeyCode::Char('m') => {
      reader.enter_awaiting_mark_name(true);
    }
    KeyCode::Char('\'') | KeyCode::Char('`') => {
      reader.enter_awaiting_mark_name(false);
    }
    KeyCode::Char('y') => {
      // Vim operator-pending: bare `y` waits for a follow-up.  `yy`
      // yanks the current line, `yi<obj>` / `ya<obj>` yanks a text
      // object.  Any other key cancels and returns to Normal.
      reader.enter_awaiting_operator(Operator::Yank);
    }
    KeyCode::Char('v') => {
      reader.enter_visual_mode(false);
    }
    KeyCode::Char('V') => {
      reader.enter_visual_mode(true);
    }
    _ => {
      reader.count_buf.clear();
    }
  }
  false
}

pub(crate) fn handle_search(reader: &mut Reader, code: KeyCode) {
  match code {
    KeyCode::Esc => reader.cancel_search(),
    KeyCode::Enter => reader.confirm_search(),
    KeyCode::Backspace => {
      reader.search_query.pop();
      reader.update_search_matches();
    }
    KeyCode::Char(c) => {
      reader.search_query.push(c);
      reader.update_search_matches();
    }
    _ => {}
  }
}

pub(crate) fn handle_awaiting_char(reader: &mut Reader, code: KeyCode, kind: FindKind) {
  // One-shot: any keystroke ends AwaitingChar.  A Char(c) performs the find;
  // anything else (Esc, arrow keys, etc.) is a quiet cancel.
  if let KeyCode::Char(c) = code {
    if let Some(idx) = reader.find_char_in_line(c, kind) {
      reader.set_cursor_x(idx);
    }
  }
  reader.return_to_normal();
}

pub(crate) fn handle_command(reader: &mut Reader, code: KeyCode) -> ReaderAction {
  match code {
    KeyCode::Esc => {
      reader.return_to_normal();
      ReaderAction::Continue
    }
    KeyCode::Enter => {
      let line = std::mem::take(&mut reader.cmd_buf);
      reader.return_to_normal();
      commands::execute(reader, &line)
    }
    KeyCode::Backspace => {
      if reader.cmd_buf.pop().is_none() {
        // Empty buffer: backspace exits command mode (matches search bar UX).
        reader.return_to_normal();
      }
      ReaderAction::Continue
    }
    KeyCode::Char(c) => {
      reader.cmd_buf.push(c);
      ReaderAction::Continue
    }
    _ => ReaderAction::Continue,
  }
}

pub(crate) fn handle_awaiting_operator(reader: &mut Reader, code: KeyCode, op: Operator) {
  // After an operator key (currently only `y`):
  //  - Doubled key (`yy`) applies to the current line.
  //  - `i` / `a` enter text-object mode.
  //  - Anything else cancels back to Normal.
  match code {
    KeyCode::Char('y') if op == Operator::Yank => {
      if let Some(vl) = reader.visual_lines.get(reader.current_line()) {
        let text = vl.text.clone();
        osc52_yank(&text);
      }
      reader.return_to_normal();
    }
    KeyCode::Char('i') => {
      reader.enter_awaiting_text_object(op, false);
    }
    KeyCode::Char('a') => {
      reader.enter_awaiting_text_object(op, true);
    }
    _ => {
      reader.return_to_normal();
    }
  }
}

pub(crate) fn handle_awaiting_text_object(
  reader: &mut Reader,
  code: KeyCode,
  op: Operator,
  around: bool,
) {
  // Look up the text object spec character and produce the yank target.
  // `b` aliases parens, `B` aliases braces (vim convention).
  let yanked: Option<String> = match code {
    KeyCode::Char('w') => text_objects::word(reader, false, around),
    KeyCode::Char('W') => text_objects::word(reader, true, around),
    KeyCode::Char('"') => text_objects::quote(reader, '"', around),
    KeyCode::Char('\'') => text_objects::quote(reader, '\'', around),
    KeyCode::Char('`') => text_objects::quote(reader, '`', around),
    KeyCode::Char('(') | KeyCode::Char(')') | KeyCode::Char('b') => {
      text_objects::pair(reader, '(', ')', around)
    }
    KeyCode::Char('[') | KeyCode::Char(']') => text_objects::pair(reader, '[', ']', around),
    KeyCode::Char('{') | KeyCode::Char('}') | KeyCode::Char('B') => {
      text_objects::pair(reader, '{', '}', around)
    }
    KeyCode::Char('p') => text_objects::paragraph(reader, around),
    KeyCode::Char('s') => text_objects::sentence(reader, around),
    _ => None,
  };
  if let Some(text) = yanked {
    match op {
      Operator::Yank => osc52_yank(&text),
    }
  }
  reader.return_to_normal();
}

/// Resolve the `LinkTarget` (if any) under the cursor by scanning the
/// current visual line's `StyledProse` spans for the one containing
/// `cursor_x`.  Returns `None` for non-styled lines, non-prose blocks,
/// or when the cursor doesn't sit on a linked span.
fn link_at_cursor(reader: &Reader) -> Option<doc_model::LinkTarget> {
  let vl = reader.visual_lines.get(reader.current_line())?;
  let spans = match &vl.kind {
    doc_model::VisualLineKind::StyledProse(s) => s,
    _ => return None,
  };
  let mut byte = 0usize;
  for span in spans {
    let next = byte + span.text.len();
    if reader.cursor_x() >= byte && reader.cursor_x() < next {
      return span.link_target.clone();
    }
    byte = next;
  }
  None
}

/// Enter dispatch: jump to whatever the cursor is sitting on.  No-op if
/// nothing is there.  Pushes onto the back-nav stack so `Ctrl+O` rewinds.
fn follow_link_at_cursor(reader: &mut Reader) {
  let Some(target) = link_at_cursor(reader) else {
    return;
  };
  let line = match &target {
    doc_model::LinkTarget::Internal(label) => reader.label_lines.get(label).copied(),
    doc_model::LinkTarget::Citation(key) => reader.bib_entry_lines.get(key).copied(),
  };
  let Some(line) = line else { return };
  // Land one line before the labeled element so it's fully visible
  // below the cursor (vim convention for jumps).  Clamp at 0.
  let target_line = line.saturating_sub(1);
  reader.push_nav_mark();
  reader.jump_to_line(target_line);
}

/// `K` / `Shift+Enter`: show the citation entry in a popup.  Only acts
/// on `LinkTarget::Citation`; `Internal` targets are silently ignored
/// (Enter is the right key for those).
fn popup_citation_at_cursor(reader: &mut Reader) {
  let Some(target) = link_at_cursor(reader) else {
    return;
  };
  let key = match target {
    doc_model::LinkTarget::Citation(k) => k,
    _ => return,
  };
  let entry = reader
    .bib_entries
    .get(&key)
    .cloned()
    .unwrap_or_else(|| "(no entry available)".to_string());
  // Wrap to ~60 chars for readability in the popup.
  let lines: Vec<String> = wrap_for_popup(&entry, 60);
  reader.popup = Some(crate::state::PopupContent {
    title: format!("[{key}]"),
    lines,
  });
}

fn wrap_for_popup(text: &str, width: usize) -> Vec<String> {
  let mut out = Vec::new();
  let mut line = String::new();
  for word in text.split_whitespace() {
    if line.is_empty() {
      line.push_str(word);
    } else if line.chars().count() + 1 + word.chars().count() <= width {
      line.push(' ');
      line.push_str(word);
    } else {
      out.push(std::mem::take(&mut line));
      line.push_str(word);
    }
  }
  if !line.is_empty() {
    out.push(line);
  }
  if out.is_empty() {
    out.push(String::new());
  }
  out
}

pub(crate) fn handle_awaiting_g(reader: &mut Reader, code: KeyCode) {
  // `gg` → top of doc; `ge` / `gE` → backward word-end.  Any other key cancels.
  match code {
    KeyCode::Char('g') => {
      reader.nav_top();
    }
    KeyCode::Char('e') => {
      reader.nav_word_end_back(false);
      reader.remember_column();
    }
    KeyCode::Char('E') => {
      reader.nav_word_end_back(true);
      reader.remember_column();
    }
    _ => {}
  }
  reader.return_to_normal();
}

pub(crate) fn handle_awaiting_bracket(reader: &mut Reader, code: KeyCode, forward: bool) {
  // `]]` / `[[` jump section (legacy `]` / `[` semantics moved here
  // so the second keystroke disambiguates from `]f` / `[f`).
  // `]f` / `[f` step the figure-preview cursor.  Anything else
  // cancels with no movement — matches the `Mode::AwaitingG` shape.
  match code {
    KeyCode::Char(']') if forward => {
      reader.jump_next_section();
    }
    KeyCode::Char('[') if !forward => {
      reader.jump_prev_section();
    }
    KeyCode::Char('f') => {
      reader.step_figure(if forward { 1 } else { -1 });
    }
    _ => {}
  }
  reader.return_to_normal();
}

pub(crate) fn handle_awaiting_mark_name(reader: &mut Reader, code: KeyCode, for_set: bool) {
  // One-shot: a letter `Char(c)` either sets or jumps; anything else cancels.
  if let KeyCode::Char(c) = code {
    if c.is_ascii_alphabetic() {
      if for_set {
        reader.set_mark(c);
      } else {
        reader.jump_to_mark(c);
      }
    }
  }
  reader.return_to_normal();
}

pub(crate) fn handle_visual(reader: &mut Reader, code: KeyCode) {
  match code {
    KeyCode::Esc | KeyCode::Char('v') | KeyCode::Char('V') => {
      reader.return_to_normal();
    }
    KeyCode::Char('j') | KeyCode::Down => reader.nav_down(),
    KeyCode::Char('k') | KeyCode::Up => reader.nav_up(),
    KeyCode::Char('h') | KeyCode::Left => reader.nav_left(),
    KeyCode::Char('l') | KeyCode::Right => reader.nav_right(),
    KeyCode::Char('y') => {
      let text = yank_selection(reader);
      osc52_yank(&text);
      reader.return_to_normal();
    }
    KeyCode::Char('H') => {
      commit_selection_as_highlights(reader);
      reader.return_to_normal();
    }
    _ => {}
  }
}

/// Convert the current visual selection into one or more `Highlight`
/// entries (one per *block* the selection touches), and add them to
/// `reader.highlights`.  No-op for empty char-visual selections.  Save
/// to disk happens on clean exit alongside bookmarks.
fn commit_selection_as_highlights(reader: &mut Reader) {
  use crate::highlights::Highlight;

  let cur = reader.current_line();
  let anchor = reader.visual_anchor;
  let (lo, hi) = (cur.min(anchor), cur.max(anchor));
  let is_line_mode = matches!(reader.mode(), Mode::Visual { line_mode: true });
  let ax = reader.visual_anchor_x;
  let cx = reader.cursor_x();

  // Empty char-visual selection: anchor and cursor identical → no-op.
  if !is_line_mode && lo == hi && ax == cx {
    return;
  }

  // Normalize first/last column endpoints based on which end is the anchor.
  let (start_x, end_x_incl) = if anchor <= cur { (ax, cx) } else { (cx, ax) };

  // Walk VLs lo..=hi, building per-VL byte ranges within their parent
  // block, and merging consecutive same-block runs into one highlight.
  let mut current: Option<(usize, usize, usize)> = None; // (block_idx, byte_start, byte_end)
  for i in lo..=hi {
    let Some(vl) = reader.visual_lines.get(i) else {
      continue;
    };
    // Skip non-text blocks (Matrix, Rule, Blank) — they have zero byte range.
    if vl.block_byte_end == vl.block_byte_start {
      continue;
    }

    let local_start = if !is_line_mode && i == lo {
      start_x.min(vl.text.len())
    } else {
      0
    };
    let local_end_excl = if !is_line_mode && i == hi {
      next_char_boundary(&vl.text, end_x_incl)
    } else {
      vl.text.len()
    };
    let local_start = snap_back_to_boundary(&vl.text, local_start);
    let local_end_excl = local_end_excl.min(vl.text.len());

    let byte_start = vl.block_byte_start + local_start;
    let byte_end = vl.block_byte_start + local_end_excl;
    if byte_end <= byte_start {
      continue;
    }

    match &mut current {
      Some((blk, _, end)) if *blk == vl.block_idx => {
        *end = byte_end;
      }
      _ => {
        if let Some((blk, s, e)) = current.take() {
          reader.highlights.add(Highlight {
            block_idx: blk,
            byte_start: s,
            byte_end: e,
          });
        }
        current = Some((vl.block_idx, byte_start, byte_end));
      }
    }
  }
  if let Some((blk, s, e)) = current {
    reader.highlights.add(Highlight {
      block_idx: blk,
      byte_start: s,
      byte_end: e,
    });
  }
}

/// Snap `byte_idx` down to the nearest UTF-8 char boundary at or before it.
fn snap_back_to_boundary(text: &str, byte_idx: usize) -> usize {
  let mut i = byte_idx.min(text.len());
  while i > 0 && !text.is_char_boundary(i) {
    i -= 1;
  }
  i
}

/// Return the byte position immediately *after* the codepoint that starts
/// at (or contains) `byte_idx`.  Used to compute exclusive end positions
/// in selections.
fn next_char_boundary(text: &str, byte_idx: usize) -> usize {
  let start = snap_back_to_boundary(text, byte_idx);
  let mut i = start + 1;
  while i < text.len() && !text.is_char_boundary(i) {
    i += 1;
  }
  i.min(text.len())
}

fn yank_selection(reader: &Reader) -> String {
  let cur = reader.current_line();
  let anchor = reader.visual_anchor;
  let (lo, hi) = (cur.min(anchor), cur.max(anchor));
  let is_line_mode = matches!(reader.mode(), Mode::Visual { line_mode: true });

  let lines: Vec<&str> = (lo..=hi)
    .filter_map(|i| reader.visual_lines.get(i))
    .map(|vl| vl.text.as_str())
    .collect();

  if is_line_mode || (lo == hi && reader.cursor_x() == reader.visual_anchor_x) {
    lines.join("\n")
  } else {
    let first = lines.first().copied().unwrap_or("");
    let last = lines.last().copied().unwrap_or("");
    let ax = reader.visual_anchor_x;
    let cx = reader.cursor_x();
    if lo == hi {
      let (s, e) = (ax.min(cx), ax.max(cx) + 1);
      first.get(s..e.min(first.len())).unwrap_or("").to_string()
    } else {
      let (first_start, last_end) = if anchor <= cur {
        (ax, cx + 1)
      } else {
        (cx, ax + 1)
      };
      let mut parts = vec![first.get(first_start..).unwrap_or("").to_string()];
      if lines.len() > 2 {
        parts.extend(lines[1..lines.len() - 1].iter().map(|s| s.to_string()));
      }
      parts.push(
        last
          .get(..last_end.min(last.len()))
          .unwrap_or("")
          .to_string(),
      );
      parts.join("\n")
    }
  }
}

pub(crate) fn osc52_yank(text: &str) {
  use std::io::Write;
  let encoded = base64_encode(text.as_bytes());
  let _ = std::io::stdout().write_all(format!("\x1b]52;c;{encoded}\x07").as_bytes());
  let _ = std::io::stdout().flush();
}

fn base64_encode(data: &[u8]) -> String {
  const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
  for chunk in data.chunks(3) {
    let b0 = chunk[0] as usize;
    let b1 = chunk.get(1).copied().unwrap_or(0) as usize;
    let b2 = chunk.get(2).copied().unwrap_or(0) as usize;
    out.push(T[b0 >> 2] as char);
    out.push(T[((b0 & 3) << 4) | (b1 >> 4)] as char);
    out.push(if chunk.len() > 1 {
      T[((b1 & 15) << 2) | (b2 >> 6)] as char
    } else {
      '='
    });
    out.push(if chunk.len() > 2 {
      T[b2 & 63] as char
    } else {
      '='
    });
  }
  out
}
