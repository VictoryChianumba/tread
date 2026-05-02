use doc_model::{Block, VisualLine, VisualLineKind, build_visual_lines};

pub const TOC_WIDTH: usize = 28;

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
  Normal,
  Search,
  Visual { line_mode: bool },
}

/// Paper-level metadata shown in the header bar.
#[derive(Debug, Clone)]
pub struct PaperMeta {
  pub title: String,
  pub authors: String,
}

pub struct Reader {
  pub blocks: Vec<Block>,
  pub visual_lines: Vec<VisualLine>,
  pub sections: Vec<(usize, u8, String)>, // (line_idx, level, title)
  pub toc_visible: bool,
  pub help_visible: bool,
  pub offset: usize,
  pub cursor_y: usize,
  pub width: usize,
  pub height: usize,
  pub search_query: String,
  pub search_matches: Vec<usize>,
  pub search_idx: usize,
  pub mode: Mode,
  /// Back-navigation stack: (offset, cursor_y) entries pushed before jumps.
  pub nav_history: Vec<(usize, usize)>,
  /// Optional paper metadata shown in the header bar.
  pub meta: Option<PaperMeta>,
  /// Sorted list of bookmarked absolute line indices.
  pub bookmarks: Vec<usize>,
  /// Logical column cursor — used by `*` and visual char mode; reset to 0 on line change.
  pub cursor_x: usize,
  /// Absolute line index where visual selection started.
  pub visual_anchor: usize,
  /// Column index where visual selection started.
  pub visual_anchor_x: usize,
  /// Accumulated digit prefix for count motions (e.g. "5" before `j`).
  pub count_buf: String,
}

impl Reader {
  pub fn new(blocks: Vec<Block>, width: usize, height: usize) -> Self {
    let cw = content_width_for(width, false);
    let visual_lines = build_visual_lines(&blocks, cw);
    let sections = build_sections(&visual_lines);
    Self {
      blocks,
      visual_lines,
      sections,
      toc_visible: false,
      help_visible: false,
      offset: 0,
      cursor_y: 0,
      width,
      height,
      search_query: String::new(),
      search_matches: Vec::new(),
      search_idx: 0,
      mode: Mode::Normal,
      nav_history: Vec::new(),
      meta: None,
      bookmarks: Vec::new(),
      cursor_x: 0,
      visual_anchor: 0,
      visual_anchor_x: 0,
      count_buf: String::new(),
    }
  }

  /// Effective text column width after subtracting the TOC panel (if visible).
  pub fn content_width(&self) -> usize {
    content_width_for(self.width, self.toc_visible)
  }

  pub fn resize(&mut self, width: usize, height: usize) {
    self.width = width;
    self.height = height;
    let cw = self.content_width();
    self.visual_lines = build_visual_lines(&self.blocks, cw);
    self.sections = build_sections(&self.visual_lines);
    self.clamp_position();
  }

  pub fn toggle_toc(&mut self) {
    self.toc_visible = !self.toc_visible;
    let cw = self.content_width();
    self.visual_lines = build_visual_lines(&self.blocks, cw);
    self.sections = build_sections(&self.visual_lines);
    self.clamp_position();
  }

  /// Clamp offset and cursor_y to stay within current document bounds.
  pub fn clamp_position(&mut self) {
    let total = self.visual_lines.len();
    let ch = self.content_height();
    if total == 0 {
      self.offset = 0;
      self.cursor_y = 0;
      return;
    }
    let max_offset = total.saturating_sub(ch).max(0);
    self.offset = self.offset.min(max_offset);
    let max_cursor = ch.saturating_sub(1).min(total.saturating_sub(1 + self.offset));
    self.cursor_y = self.cursor_y.min(max_cursor);
  }

  /// Push current position onto the back-navigation stack before a jump.
  pub fn push_nav_mark(&mut self) {
    let pos = (self.offset, self.cursor_y);
    if self.nav_history.last() != Some(&pos) {
      self.nav_history.push(pos);
      // Cap history at 50 entries to avoid unbounded growth.
      if self.nav_history.len() > 50 {
        self.nav_history.remove(0);
      }
    }
  }

  /// Return to the previous position in the back-navigation stack.
  pub fn nav_back(&mut self) {
    if let Some((offset, cursor_y)) = self.nav_history.pop() {
      self.offset = offset;
      self.cursor_y = cursor_y;
    }
  }

  pub fn toggle_help(&mut self) {
    self.help_visible = !self.help_visible;
  }

  pub fn toggle_bookmark(&mut self) {
    let line = self.offset + self.cursor_y;
    match self.bookmarks.binary_search(&line) {
      Ok(pos) => { self.bookmarks.remove(pos); }
      Err(pos) => { self.bookmarks.insert(pos, line); }
    }
  }

  pub fn next_bookmark(&mut self) {
    let cur = self.offset + self.cursor_y;
    if let Some(&target) = self.bookmarks.iter().find(|&&b| b > cur) {
      self.push_nav_mark();
      self.offset = target;
      self.cursor_y = 0;
    }
  }

  pub fn prev_bookmark(&mut self) {
    let cur = self.offset + self.cursor_y;
    if let Some(&target) = self.bookmarks.iter().rfind(|&&b| b < cur) {
      self.push_nav_mark();
      self.offset = target;
      self.cursor_y = 0;
    }
  }

  /// Index into `sections` of the last section header at or above the current line.
  pub fn current_section_idx(&self) -> Option<usize> {
    let cur = self.current_line();
    self.sections.iter().rposition(|s| s.0 <= cur)
  }

  pub fn current_line(&self) -> usize {
    self.offset + self.cursor_y
  }

  pub fn total_lines(&self) -> usize {
    self.visual_lines.len()
  }

  pub fn content_height(&self) -> usize {
    let header = if self.meta.is_some() { 1 } else { 0 };
    let status = 1;
    let search = if self.mode == Mode::Search { 1 } else { 0 };
    self.height.saturating_sub(header + status + search)
  }

  pub fn update_search_matches(&mut self) {
    let q = self.search_query.to_lowercase();
    self.search_matches = if q.is_empty() {
      Vec::new()
    } else {
      self.visual_lines
        .iter()
        .enumerate()
        .filter(|(_, vl)| vl.text.to_lowercase().contains(&q))
        .map(|(i, _)| i)
        .collect()
    };
    self.search_idx = 0;
  }

  pub fn jump_to_match(&mut self, idx: usize) {
    if self.search_matches.is_empty() {
      return;
    }
    let line = self.search_matches[idx];
    self.offset = line;
    self.cursor_y = 0;
  }
}

/// Compute text column width given terminal width and TOC visibility.
fn content_width_for(terminal_width: usize, toc_visible: bool) -> usize {
  if toc_visible {
    // +1 for the border column.
    terminal_width.saturating_sub(TOC_WIDTH + 1)
  } else {
    terminal_width
  }
}

fn build_sections(visual_lines: &[VisualLine]) -> Vec<(usize, u8, String)> {
  visual_lines
    .iter()
    .enumerate()
    .filter_map(|(i, vl)| {
      if let VisualLineKind::Header(level) = &vl.kind {
        Some((i, *level, vl.text.clone()))
      } else {
        None
      }
    })
    .collect()
}
