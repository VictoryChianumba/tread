use crate::state::{FindKind, Mode, Reader};

// ── Char-class helpers (used by word motions) ─────────────────────────────────

/// Vim small-word chars: ASCII alphanumeric plus underscore.  Hyphens are
/// treated as punctuation (matches vim's `iskeyword` default for English text).
fn is_word_char(b: u8) -> bool {
  b.is_ascii_alphanumeric() || b == b'_'
}

/// Vim WORD chars: anything that isn't whitespace.  Used by `W`/`B`/`E`.
fn is_big_word_char(b: u8) -> bool {
  !b.is_ascii_whitespace()
}

/// Classify a byte for small-word boundary detection.  Returns three buckets so
/// "word→punctuation" is recognised as a boundary (which a simple
/// "is_word_char vs not" two-bucket function would miss when transitioning
/// directly from a letter to a punctuation char with no whitespace between).
///
/// In big-word mode (`W`/`B`/`E`) only whitespace separates runs, so every
/// non-whitespace byte gets the same kind.
fn char_kind(b: u8, big: bool) -> u8 {
  if b.is_ascii_whitespace() { 0 }
  else if big { 1 }
  else if is_word_char(b) { 1 }
  else { 2 }
}

/// Snap `byte_idx` down to the nearest UTF-8 char boundary at or before it,
/// clamped within `text.len()`.  Mirrors the snap in `render::apply_char_cursor`
/// so cursor moves never land mid-codepoint.
fn snap_to_char_boundary(text: &str, byte_idx: usize) -> usize {
  let max = text.len().saturating_sub(1);
  let mut i = byte_idx.min(max);
  while i > 0 && !text.is_char_boundary(i) {
    i -= 1;
  }
  i
}

impl Reader {
  /// After a vertical-line change, reset `cursor_x` to `desired_column`
  /// clamped to the new line's effective length.  Vim's `curswant` model:
  /// returns to the original column on long lines after passing through
  /// short ones.
  pub fn clamp_cursor_after_line_change(&mut self) {
    let len = self.visual_lines.get(self.current_line())
      .map(|vl| vl.text.len())
      .unwrap_or(0);
    let max = len.saturating_sub(1);
    self.cursor_x = self.desired_column.min(max);
  }

  /// Sync `desired_column` to the current `cursor_x`.  Called from every
  /// horizontal motion so future vertical motions return to here.
  pub fn remember_column(&mut self) {
    self.desired_column = self.cursor_x;
  }

  pub fn nav_down(&mut self) {
    let ch = self.content_height();
    let total = self.total_lines();
    if self.offset + self.cursor_y + 1 >= total {
      return;
    }
    if self.cursor_y + 1 < ch {
      self.cursor_y += 1;
    } else {
      self.offset += 1;
    }
    self.clamp_cursor_after_line_change();
  }

  pub fn nav_up(&mut self) {
    if self.cursor_y > 0 {
      self.cursor_y -= 1;
    } else if self.offset > 0 {
      self.offset -= 1;
    }
    self.clamp_cursor_after_line_change();
  }

  pub fn nav_top(&mut self) {
    self.offset = 0;
    self.cursor_y = 0;
    self.clamp_cursor_after_line_change();
  }

  pub fn nav_bottom(&mut self) {
    let total = self.total_lines();
    let ch = self.content_height();
    if total > ch {
      self.offset = total - ch;
      self.cursor_y = ch - 1;
    } else {
      self.offset = 0;
      self.cursor_y = total.saturating_sub(1);
    }
    self.clamp_cursor_after_line_change();
  }

  pub fn nav_half_page_down(&mut self) {
    let step = self.content_height() / 2;
    for _ in 0..step {
      self.nav_down();
    }
  }

  pub fn nav_half_page_up(&mut self) {
    let step = self.content_height() / 2;
    for _ in 0..step {
      self.nav_up();
    }
  }

  pub fn search_next(&mut self) {
    if self.search_matches.is_empty() {
      return;
    }
    self.search_idx = (self.search_idx + 1) % self.search_matches.len();
    let idx = self.search_idx;
    self.jump_to_match(idx);
  }

  pub fn search_prev(&mut self) {
    if self.search_matches.is_empty() {
      return;
    }
    self.search_idx = if self.search_idx == 0 {
      self.search_matches.len() - 1
    } else {
      self.search_idx - 1
    };
    let idx = self.search_idx;
    self.jump_to_match(idx);
  }

  pub fn enter_search(&mut self) {
    self.mode = Mode::Search;
    self.search_query.clear();
    self.search_matches.clear();
  }

  pub fn confirm_search(&mut self) {
    self.mode = Mode::Normal;
    if !self.search_matches.is_empty() {
      self.push_nav_mark();
      let idx = self.search_idx;
      self.jump_to_match(idx);
    }
  }

  pub fn cancel_search(&mut self) {
    self.mode = Mode::Normal;
    self.search_query.clear();
    self.search_matches.clear();
  }

  pub fn jump_next_section(&mut self) {
    let cur = self.current_line();
    let target = self.sections.iter().find(|s| s.0 > cur).map(|s| s.0);
    if let Some(line) = target {
      self.push_nav_mark();
      self.offset = line;
      self.cursor_y = 0;
      self.clamp_cursor_after_line_change();
    }
  }

  pub fn jump_prev_section(&mut self) {
    let cur = self.current_line();
    let target = self.sections.iter().rfind(|s| s.0 < cur).map(|s| s.0);
    if let Some(line) = target {
      self.push_nav_mark();
      self.offset = line;
      self.cursor_y = 0;
      self.clamp_cursor_after_line_change();
    }
  }

  pub fn nav_page_down(&mut self) {
    let step = self.content_height();
    for _ in 0..step {
      self.nav_down();
    }
  }

  pub fn nav_page_up(&mut self) {
    let step = self.content_height();
    for _ in 0..step {
      self.nav_up();
    }
  }

  pub fn jump_next_paragraph(&mut self) {
    let cur = self.current_line();
    let total = self.total_lines();
    let mut i = cur;
    while i < total && !self.visual_lines[i].text.trim().is_empty() {
      i += 1;
    }
    while i < total && self.visual_lines[i].text.trim().is_empty() {
      i += 1;
    }
    if i < total {
      self.push_nav_mark();
      self.offset = i;
      self.cursor_y = 0;
      self.clamp_cursor_after_line_change();
    }
  }

  pub fn jump_prev_paragraph(&mut self) {
    let cur = self.current_line();
    if cur == 0 {
      return;
    }
    let mut i = cur.saturating_sub(1);
    while i > 0 && self.visual_lines[i].text.trim().is_empty() {
      i -= 1;
    }
    while i > 0 && !self.visual_lines[i - 1].text.trim().is_empty() {
      i -= 1;
    }
    self.push_nav_mark();
    self.offset = i;
    self.cursor_y = 0;
    self.clamp_cursor_after_line_change();
  }

  pub fn jump_screen_top(&mut self) {
    self.cursor_y = 0;
    self.clamp_cursor_after_line_change();
  }

  pub fn jump_screen_middle(&mut self) {
    let ch = self.content_height();
    let visible = self.total_lines().saturating_sub(self.offset).min(ch);
    self.cursor_y = (visible / 2).saturating_sub(1);
    self.clamp_cursor_after_line_change();
  }

  pub fn jump_screen_bottom(&mut self) {
    let ch = self.content_height();
    let visible = self.total_lines().saturating_sub(self.offset).min(ch);
    self.cursor_y = visible.saturating_sub(1);
    self.clamp_cursor_after_line_change();
  }

  pub fn center_cursor(&mut self) {
    let ch = self.content_height();
    let abs = self.current_line();
    self.offset = abs.saturating_sub(ch / 2);
    self.cursor_y = abs - self.offset;
  }

  pub fn word_at_cursor(&self) -> Option<String> {
    let text = &self.visual_lines.get(self.current_line())?.text;
    let bytes = text.as_bytes();
    if bytes.is_empty() {
      return None;
    }
    let x = self.cursor_x.min(bytes.len() - 1);
    let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'-' || c == b'_';
    if !is_word(bytes[x]) {
      let start = (x..bytes.len()).find(|&i| is_word(bytes[i]))?;
      let end = (start..bytes.len()).find(|&i| !is_word(bytes[i])).unwrap_or(bytes.len());
      return Some(text[start..end].to_string());
    }
    let start = (0..=x).rfind(|&i| !is_word(bytes[i])).map(|i| i + 1).unwrap_or(0);
    let end = (x..bytes.len()).find(|&i| !is_word(bytes[i])).unwrap_or(bytes.len());
    Some(text[start..end].to_string())
  }

  // ── Intra-line motions (cheap-wins parity with cli-text-reader) ─────────────

  /// Borrow the current line's text for motion scanning.  Returns `None` when
  /// the cursor sits past the end of the document or on a missing line.
  fn current_line_text(&self) -> Option<&str> {
    self.visual_lines.get(self.current_line()).map(|vl| vl.text.as_str())
  }

  /// `h` / Left.  Moves the cursor one char left.  When at column 0 of a
  /// line, wraps to the end of the previous line.  No-op at start of doc.
  pub fn nav_left(&mut self) {
    let cur = self.current_line();
    let line_len = self.visual_lines.get(cur).map(|vl| vl.text.len()).unwrap_or(0);
    let clamped = self.cursor_x.min(line_len.saturating_sub(1));
    if clamped == 0 {
      if cur > 0 {
        self.nav_up();
        let prev_len = self.visual_lines.get(self.current_line()).map(|vl| vl.text.len()).unwrap_or(0);
        self.cursor_x = prev_len.saturating_sub(1);
      } else {
        self.cursor_x = 0;
      }
    } else {
      self.cursor_x = clamped - 1;
    }
    self.remember_column();
  }

  /// `l` / Right.  Moves the cursor one char right.  When at the last
  /// char of a line, wraps to column 0 of the next line.  No-op at end of doc.
  pub fn nav_right(&mut self) {
    let cur = self.current_line();
    let line_len = self.visual_lines.get(cur).map(|vl| vl.text.len()).unwrap_or(0);
    let max_col = line_len.saturating_sub(1);
    if line_len == 0 || self.cursor_x >= max_col {
      let total = self.total_lines();
      if cur + 1 < total {
        self.nav_down();
        self.cursor_x = 0;
      }
    } else {
      self.cursor_x += 1;
    }
    self.remember_column();
  }

  /// `w` / `W` — forward to the start of the next word.  Wraps to the
  /// first non-blank char of the following line when at end-of-line; lands
  /// on column 0 of empty lines (vim treats them as words).
  pub fn nav_word_forward(&mut self, big: bool) {
    let total = self.total_lines();
    let cur = self.current_line();
    if let Some(text) = self.visual_lines.get(cur).map(|vl| vl.text.as_str()) {
      let bytes = text.as_bytes();
      if !bytes.is_empty() && self.cursor_x < bytes.len() {
        let mut x = self.cursor_x;
        let here = char_kind(bytes[x], big);
        while x + 1 < bytes.len() && char_kind(bytes[x + 1], big) == here {
          x += 1;
        }
        while x + 1 < bytes.len() && bytes[x + 1].is_ascii_whitespace() {
          x += 1;
        }
        if x + 1 < bytes.len() {
          self.cursor_x = snap_to_char_boundary(text, x + 1);
          self.remember_column();
          return;
        }
      }
    }
    // Wrap forward: advance one VL, land on first non-blank or column 0
    // for an empty line.  Empty lines count as words and stop here.
    if cur + 1 < total {
      self.nav_down();
      let new_cur = self.current_line();
      if let Some(text) = self.visual_lines.get(new_cur).map(|vl| vl.text.as_str()) {
        match text.bytes().position(|b| !b.is_ascii_whitespace()) {
          Some(p) => self.cursor_x = snap_to_char_boundary(text, p),
          None => self.cursor_x = 0,
        }
      }
      self.remember_column();
    }
  }

  /// `b` / `B` — backward to the start of the previous word.  Wraps to
  /// the start of the last word on the previous line at column 0.
  pub fn nav_word_back(&mut self, big: bool) {
    let cur = self.current_line();
    if let Some(text) = self.visual_lines.get(cur).map(|vl| vl.text.as_str()) {
      let bytes = text.as_bytes();
      if !bytes.is_empty() && self.cursor_x > 0 {
        let mut x = self.cursor_x.min(bytes.len() - 1);
        x -= 1;
        while x > 0 && bytes[x].is_ascii_whitespace() { x -= 1; }
        if !bytes[x].is_ascii_whitespace() {
          let here = char_kind(bytes[x], big);
          while x > 0 && char_kind(bytes[x - 1], big) == here { x -= 1; }
          self.cursor_x = snap_to_char_boundary(text, x);
          return;
        }
      }
    }
    // Wrap backward.
    if cur > 0 {
      self.nav_up();
      let new_cur = self.current_line();
      if let Some(text) = self.visual_lines.get(new_cur).map(|vl| vl.text.as_str()) {
        let bytes = text.as_bytes();
        if bytes.is_empty() { self.cursor_x = 0; return; }
        // Find the start of the last non-whitespace run.
        let mut x = bytes.len() - 1;
        while x > 0 && bytes[x].is_ascii_whitespace() { x -= 1; }
        if !bytes[x].is_ascii_whitespace() {
          let here = char_kind(bytes[x], big);
          while x > 0 && char_kind(bytes[x - 1], big) == here { x -= 1; }
          self.cursor_x = snap_to_char_boundary(text, x);
        } else {
          self.cursor_x = 0;
        }
      }
    }
  }

  /// `e` / `E` — forward to the end of the current/next word.  Wraps to
  /// the end of the first word on the next line.
  pub fn nav_word_end(&mut self, big: bool) {
    let total = self.total_lines();
    let cur = self.current_line();
    if let Some(text) = self.visual_lines.get(cur).map(|vl| vl.text.as_str()) {
      let bytes = text.as_bytes();
      if !bytes.is_empty() && self.cursor_x < bytes.len() {
        let mut x = self.cursor_x;
        let here = char_kind(bytes[x], big);
        let at_end = x + 1 >= bytes.len() || char_kind(bytes[x + 1], big) != here;
        if at_end {
          // Try the next word on this line.
          let mut y = x + 1;
          while y < bytes.len() && bytes[y].is_ascii_whitespace() { y += 1; }
          if y < bytes.len() {
            let kind = char_kind(bytes[y], big);
            while y + 1 < bytes.len() && char_kind(bytes[y + 1], big) == kind { y += 1; }
            self.cursor_x = snap_to_char_boundary(text, y);
            return;
          }
          // No next word on this line — fall through to wrap.
        } else {
          while x + 1 < bytes.len() && char_kind(bytes[x + 1], big) == here { x += 1; }
          self.cursor_x = snap_to_char_boundary(text, x);
          return;
        }
      }
    }
    // Wrap forward.
    if cur + 1 < total {
      self.nav_down();
      let new_cur = self.current_line();
      if let Some(text) = self.visual_lines.get(new_cur).map(|vl| vl.text.as_str()) {
        let bytes = text.as_bytes();
        if bytes.is_empty() { self.cursor_x = 0; return; }
        if let Some(start) = bytes.iter().position(|b| !b.is_ascii_whitespace()) {
          let kind = char_kind(bytes[start], big);
          let mut y = start;
          while y + 1 < bytes.len() && char_kind(bytes[y + 1], big) == kind { y += 1; }
          self.cursor_x = snap_to_char_boundary(text, y);
        } else {
          self.cursor_x = 0;
        }
      }
    }
  }

  /// `ge` / `gE` — backward to the end of the previous word.  Wraps to
  /// the end of the last word on the previous visual line at column 0.
  pub fn nav_word_end_back(&mut self, big: bool) {
    let cur = self.current_line();
    if let Some(text) = self.visual_lines.get(cur).map(|vl| vl.text.as_str()) {
      let bytes = text.as_bytes();
      if !bytes.is_empty() && self.cursor_x > 0 {
        let mut x = self.cursor_x.min(bytes.len() - 1);
        x -= 1;
        // Skip whitespace going backward.
        while x > 0 && bytes[x].is_ascii_whitespace() { x -= 1; }
        if !bytes[x].is_ascii_whitespace() {
          // We're now at the end of some word — that's our target.
          self.cursor_x = snap_to_char_boundary(text, x);
          return;
        }
      }
    }
    // Wrap backward.
    if cur > 0 {
      self.nav_up();
      let new_cur = self.current_line();
      if let Some(text) = self.visual_lines.get(new_cur).map(|vl| vl.text.as_str()) {
        let bytes = text.as_bytes();
        if bytes.is_empty() { self.cursor_x = 0; return; }
        // Find the last non-whitespace byte (end of last word).
        let mut x = bytes.len() - 1;
        while x > 0 && bytes[x].is_ascii_whitespace() { x -= 1; }
        if !bytes[x].is_ascii_whitespace() {
          // For big-word, the kind check is irrelevant; for small-word
          // make sure we land on the last byte of the same kind run.
          let _ = big; // single-end byte; both modes land on the same byte
          self.cursor_x = snap_to_char_boundary(text, x);
        } else {
          self.cursor_x = 0;
        }
      }
    }
  }

  /// Move the cursor to (`line`, `col`), scrolling minimally to keep
  /// `line` visible.  If `line` is already on screen, `cursor_y` is just
  /// updated; otherwise the viewport repositions.
  fn goto_position(&mut self, line: usize, col: usize) {
    let total = self.total_lines();
    if total == 0 { return; }
    let line = line.min(total - 1);
    let ch = self.content_height();
    if line >= self.offset && line < self.offset + ch {
      self.cursor_y = line - self.offset;
    } else if line < self.offset {
      self.offset = line;
      self.cursor_y = 0;
    } else {
      // Below the visible window — scroll so target is at the bottom.
      let max_offset = total.saturating_sub(ch);
      self.offset = (line + 1).saturating_sub(ch).min(max_offset);
      self.cursor_y = line - self.offset;
    }
    self.cursor_x = col;
  }

  /// `)` — forward to the start of the next sentence.  A sentence ends
  /// at `.` / `!` / `?`, optionally followed by closer chars (`)` `]`
  /// `"` `'`), then whitespace or end-of-line.  The next non-whitespace
  /// byte is the sentence start.  Cross-line.
  pub fn nav_sentence_forward(&mut self) {
    let total = self.total_lines();
    let mut line = self.current_line();
    let mut col = self.cursor_x.saturating_add(1);
    let mut saw_terminator = false;
    let mut saw_ws_after = false;

    while line < total {
      let Some(text) = self.visual_lines.get(line).map(|vl| vl.text.as_str()) else { break };
      let bytes = text.as_bytes();
      while col < bytes.len() {
        let b = bytes[col];
        if saw_terminator && saw_ws_after && !b.is_ascii_whitespace() {
          self.goto_position(line, col);
          return;
        }
        if matches!(b, b'.' | b'!' | b'?') {
          saw_terminator = true;
          saw_ws_after = false;
        } else if saw_terminator && matches!(b, b')' | b']' | b'"' | b'\'') {
          // Allow closing punctuation between terminator and whitespace.
        } else if saw_terminator && b.is_ascii_whitespace() {
          saw_ws_after = true;
        } else {
          saw_terminator = false;
          saw_ws_after = false;
        }
        col += 1;
      }
      // End-of-line counts as whitespace after a terminator.
      if saw_terminator { saw_ws_after = true; }
      line += 1;
      col = 0;
    }
    // No sentence found — fall through (no-op).
  }

  /// `(` — backward to the start of the current or previous sentence.
  /// Walks back from the cursor looking for a sentence terminator
  /// followed by whitespace; the first non-whitespace byte after that
  /// is the sentence start.  At document start, lands at (0, 0).
  pub fn nav_sentence_back(&mut self) {
    let cur_line = self.current_line();
    let cur_col = self.cursor_x;

    // Build a list of "sentence start" candidates by scanning forward
    // from the document start up to the current position, then return
    // the last one strictly before cursor.
    let total = self.total_lines();
    let mut last_start: Option<(usize, usize)> = None;
    let mut saw_terminator = false;
    let mut saw_ws_after = true; // start-of-doc counts as past-whitespace
    'outer: for line in 0..total {
      let Some(text) = self.visual_lines.get(line).map(|vl| vl.text.as_str()) else { continue };
      let bytes = text.as_bytes();
      for col in 0..bytes.len() {
        if line == cur_line && col >= cur_col { break 'outer; }
        let b = bytes[col];
        if saw_ws_after && !b.is_ascii_whitespace() {
          last_start = Some((line, col));
          saw_terminator = false;
          saw_ws_after = false;
        }
        if matches!(b, b'.' | b'!' | b'?') {
          saw_terminator = true;
        } else if saw_terminator && matches!(b, b')' | b']' | b'"' | b'\'') {
          // closer — keep terminator state
        } else if saw_terminator && b.is_ascii_whitespace() {
          saw_ws_after = true;
        } else if !b.is_ascii_whitespace() {
          saw_terminator = false;
        }
      }
      if saw_terminator { saw_ws_after = true; }
    }
    if let Some((line, col)) = last_start {
      self.goto_position(line, col);
    } else {
      self.goto_position(0, 0);
    }
  }

  /// `0` — jump to byte 0 of the current line.
  pub fn nav_line_start(&mut self) {
    self.cursor_x = 0;
  }

  /// `^` — first non-whitespace byte of the current line.
  pub fn nav_line_first_nonblank(&mut self) {
    if let Some(text) = self.current_line_text() {
      self.cursor_x = text.bytes().position(|b| !b.is_ascii_whitespace()).unwrap_or(0);
    }
  }

  /// `$` — last char of the current line.
  pub fn nav_line_end(&mut self) {
    if let Some(text) = self.current_line_text() {
      let last = text.len().saturating_sub(1);
      self.cursor_x = snap_to_char_boundary(text, last);
    }
  }

  /// `f` / `F` / `t` / `T` — find a char on the current line.  Returns the
  /// target byte position on success.  ASCII-only in v1 (multibyte targets
  /// would need a different scan).
  pub fn find_char_in_line(&self, ch: char, kind: FindKind) -> Option<usize> {
    if !ch.is_ascii() { return None; }
    let text = self.current_line_text()?;
    let bytes = text.as_bytes();
    let target = ch as u8;
    let x = self.cursor_x;
    match kind {
      FindKind::F      => (x + 1..bytes.len()).find(|&i| bytes[i] == target),
      FindKind::T      => (x + 1..bytes.len()).find(|&i| bytes[i] == target).map(|i| i.saturating_sub(1)),
      FindKind::ShiftF => (0..x).rev().find(|&i| bytes[i] == target),
      FindKind::ShiftT => (0..x).rev().find(|&i| bytes[i] == target).map(|i| i + 1),
    }
  }

  /// `%` — jump between matching brackets `()` `[]` `{}` on the current line.
  /// No-op when the cursor is not on a bracket char or no match exists.
  pub fn nav_match_brace(&mut self) {
    let Some(text) = self.current_line_text() else { return };
    let bytes = text.as_bytes();
    if bytes.is_empty() { return; }
    let x = self.cursor_x.min(bytes.len() - 1);
    let (open, close, fwd) = match bytes[x] {
      b'(' => (b'(', b')', true),
      b'[' => (b'[', b']', true),
      b'{' => (b'{', b'}', true),
      b')' => (b'(', b')', false),
      b']' => (b'[', b']', false),
      b'}' => (b'{', b'}', false),
      _ => return,
    };
    let mut depth: i32 = 1;
    if fwd {
      for i in (x + 1)..bytes.len() {
        if bytes[i] == open  { depth += 1; }
        if bytes[i] == close { depth -= 1; if depth == 0 { self.cursor_x = i; return; } }
      }
    } else {
      for i in (0..x).rev() {
        if bytes[i] == close { depth += 1; }
        if bytes[i] == open  { depth -= 1; if depth == 0 { self.cursor_x = i; return; } }
      }
    }
  }
}

#[cfg(test)]
mod motion_tests {
  use super::*;

  // Minimal test harness — char_kind, is_word_char, etc. are file-private but
  // testable here.  Cursor-positional motions on a real Reader would need a
  // visual-line vector; we test the pure helpers and `find_char_in_line` /
  // brace logic via the public byte-level APIs that don't need a full state.

  #[test]
  fn word_char_predicates() {
    assert!(is_word_char(b'a'));
    assert!(is_word_char(b'Z'));
    assert!(is_word_char(b'0'));
    assert!(is_word_char(b'_'));
    assert!(!is_word_char(b'-'));
    assert!(!is_word_char(b' '));
    assert!(!is_word_char(b'.'));
  }

  #[test]
  fn big_word_separator_is_whitespace_only() {
    assert!(is_big_word_char(b'a'));
    assert!(is_big_word_char(b'.'));
    assert!(is_big_word_char(b'-'));
    assert!(!is_big_word_char(b' '));
    assert!(!is_big_word_char(b'\t'));
  }

  #[test]
  fn char_kind_distinguishes_word_and_punct() {
    // Small-word mode separates words from punctuation.
    assert_eq!(char_kind(b'a', false), 1);
    assert_eq!(char_kind(b'.', false), 2);
    assert_eq!(char_kind(b' ', false), 0);
    // Big-word mode collapses word + punctuation together.
    assert_eq!(char_kind(b'a', true), 1);
    assert_eq!(char_kind(b'.', true), 1);
    assert_eq!(char_kind(b' ', true), 0);
  }

  #[test]
  fn snap_keeps_ascii_unchanged() {
    let s = "hello world";
    assert_eq!(snap_to_char_boundary(s, 5), 5);
    assert_eq!(snap_to_char_boundary(s, 0), 0);
    assert_eq!(snap_to_char_boundary(s, 1000), s.len() - 1);
  }

  #[test]
  fn snap_walks_back_into_multibyte_char() {
    // "café " — 'é' is two bytes (0xC3 0xA9) at positions 3..5.
    let s = "café";
    // Position 4 is mid-codepoint; should snap back to 3 (start of 'é').
    assert_eq!(snap_to_char_boundary(s, 4), 3);
  }
}
