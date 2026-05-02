use crate::state::{Mode, Reader};

impl Reader {
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
  }

  pub fn nav_up(&mut self) {
    if self.cursor_y > 0 {
      self.cursor_y -= 1;
    } else if self.offset > 0 {
      self.offset -= 1;
    }
  }

  pub fn nav_top(&mut self) {
    self.offset = 0;
    self.cursor_y = 0;
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
    }
  }

  pub fn jump_prev_section(&mut self) {
    let cur = self.current_line();
    let target = self.sections.iter().rfind(|s| s.0 < cur).map(|s| s.0);
    if let Some(line) = target {
      self.push_nav_mark();
      self.offset = line;
      self.cursor_y = 0;
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
  }

  pub fn jump_screen_top(&mut self) {
    self.cursor_y = 0;
  }

  pub fn jump_screen_middle(&mut self) {
    let ch = self.content_height();
    let visible = self.total_lines().saturating_sub(self.offset).min(ch);
    self.cursor_y = (visible / 2).saturating_sub(1);
  }

  pub fn jump_screen_bottom(&mut self) {
    let ch = self.content_height();
    let visible = self.total_lines().saturating_sub(self.offset).min(ch);
    self.cursor_y = visible.saturating_sub(1);
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
}
