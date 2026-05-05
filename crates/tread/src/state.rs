use std::collections::HashMap;

use doc_model::{Block, VisualLine, VisualLineKind, build_visual_lines};

use crate::highlights::HighlightSet;

pub const TOC_WIDTH: usize = 28;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FindKind {
  /// `f<c>` — forward to next occurrence.
  F,
  /// `F<c>` — backward to previous occurrence.
  ShiftF,
  /// `t<c>` — forward, land *before* the match.
  T,
  /// `T<c>` — backward, land *after* the match.
  ShiftT,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
  Normal,
  Search,
  Visual { line_mode: bool },
  /// One-shot mode: the next `KeyCode::Char(c)` is consumed as the find
  /// target.  Any other key returns to Normal without moving.
  AwaitingChar { kind: FindKind },
  /// One-shot mode: the next `KeyCode::Char(letter)` is consumed as a
  /// mark identifier.  When `for_set` is true the mark is saved at the
  /// current line; when false the cursor jumps to that mark (no-op if
  /// the mark is unset).  Any non-Char key cancels.
  AwaitingMarkName { for_set: bool },
  /// One-shot mode after the user pressed `g`.  Awaits the second
  /// keystroke for vim's `g`-prefixed motions: `gg` → top, `ge` /
  /// `gE` → backward word-end (small / big).  Any other key cancels.
  AwaitingG,
  /// `:`-prefixed Ex-command input.  `cmd_buf` on Reader holds the
  /// in-progress command line; Esc cancels, Enter dispatches via
  /// `commands::execute`.
  Command,
  /// After pressing an operator (currently only `y`).  Awaits the
  /// follow-up: another `y` to apply to the current line, `i`/`a` to
  /// enter text-object mode, or any other key cancels.
  AwaitingOperator { op: Operator },
  /// After `yi` or `ya`.  Awaits the text-object spec character (`w`,
  /// `"`, `(`, `p`, `s`, etc.) and dispatches to `text_objects::*`.
  AwaitingTextObject { op: Operator, around: bool },
}

/// Which operator is currently pending.  In a read-only reader only
/// `Yank` is meaningful — `d`/`c`/`x` would mutate the buffer.  Kept as
/// an enum so the dispatch surface is uniform with vim's model and the
/// state machine can be extended without rewiring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
  Yank,
}

/// A modal popup surfaced by `:marks`, `:highlights`, `:about`,
/// `:placement`, etc.  `Reader.popup` is `Some(...)` while one is open;
/// any keystroke dismisses it.
#[derive(Debug, Clone)]
pub struct PopupContent {
  pub title: String,
  pub lines: Vec<String>,
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
  /// Letter-keyed bookmarks (vim-style marks).  `m<letter>` sets,
  /// `'<letter>` jumps.  Persisted per arXiv ID.
  pub bookmarks: HashMap<char, usize>,
  /// Persistent character-range highlights.  Stored at block-byte
  /// granularity so they survive resize.  Loaded on entry, saved on exit.
  pub highlights: HighlightSet,
  /// Resolution map for `\ref{X}` jumps: label → first visual-line index
  /// of the labeled element.  Built from `Block::Anchor` markers in
  /// `Reader::new`.  For ref-targets we want the line *before* the
  /// labeled element when possible (so the equation/figure/table is
  /// fully visible after the jump); see `follow_link_target`.
  pub label_lines: HashMap<String, usize>,
  /// Bibliography entry text by cite-key.  Pandoc bib divs use
  /// `id="ref-<key>"`; we capture the rendered entry text for popup
  /// display by `:K` / `Shift+Enter` on a citation.
  pub bib_entries: HashMap<String, String>,
  /// Bibliography entry first-VL index by cite-key.  `Enter` on a
  /// citation jumps here (line *before* the entry).
  pub bib_entry_lines: HashMap<String, usize>,
  /// Effective byte column of the cursor on the current line.  Always
  /// represents the rendered position — horizontal motions write here.
  pub cursor_x: usize,
  /// "Desired" column carried across `j`/`k` line changes so that
  /// returning to a long line restores the original column.  Matches
  /// vim's `curswant`.  Set by horizontal motions; consulted by vertical
  /// motions via `clamp_cursor_after_line_change`.
  pub desired_column: usize,
  /// Absolute line index where visual selection started.
  pub visual_anchor: usize,
  /// Column index where visual selection started.
  pub visual_anchor_x: usize,
  /// Accumulated digit prefix for count motions (e.g. "5" before `j`).
  pub count_buf: String,
  /// In-progress text after `:` in Command mode.
  pub cmd_buf: String,
  /// One-line error message shown in the status line after a command
  /// failed (e.g. unknown command, unknown theme).  Cleared on next event.
  pub cmd_error: Option<String>,
  /// Active modal popup (e.g. `:marks` listing).  Any keystroke dismisses.
  pub popup: Option<PopupContent>,
  /// Resolved on-disk path for every `Block::Image` in the document,
  /// keyed by its `kitty_id`.  Built once at construction; consulted
  /// post-draw to load PNG bytes for terminals that speak the Kitty
  /// graphics protocol.  Paths that fail to resolve are silently
  /// skipped — the caption row always renders, so degradation is
  /// graceful.
  pub image_paths: HashMap<u32, std::path::PathBuf>,

  // ── Voice / TTS playback state ──────────────────────────────────────────
  // All fields are `None` / `false` / `Idle` when voice is inactive.  The
  // controller is wired post-construction in `lib.rs::run` once the env
  // API key is read; tests can leave it `None`.
  /// Background TTS playback controller, or `None` when audio init failed.
  pub voice_controller: Option<crate::voice::PlaybackController>,
  /// Last-synced playback status; refreshed each tick from the
  /// controller's shared `Arc<Mutex>`.
  pub voice_status: crate::voice::PlaybackStatus,
  /// Pending error from the playback thread (e.g. ElevenLabs auth
  /// failure, audio device missing).  Cleared after display in the
  /// status bar.
  pub voice_error: Option<String>,
  /// First / last visual-line index of the paragraph currently being
  /// read.  Used for line dimming and word-position bookkeeping.
  pub voice_para_start: usize,
  pub voice_para_end: usize,
  /// Wall-clock instant when the current chunk's audio started playing.
  /// `None` when nothing is playing.  Combined with a fixed chars-per-
  /// second rate, this drives the "active word" highlight.
  pub voice_started_at: Option<std::time::Instant>,
  /// Cumulative character count from chunks that completed BEFORE the
  /// current one, so word-position math knows what offset to start at.
  pub voice_chars_before: usize,
  /// True while the user is in voice mode (`r`/`R`/`Ctrl+P` started a
  /// playback session).  Allows navigation to keep working while audio
  /// plays, and gates `Space`/`c`/`Esc` voice handlers.
  pub reading_mode: bool,
  /// True when continuous reading is active — on chunk-end, advance to
  /// the next paragraph and start playing it.
  pub continuous_reading: bool,
}

impl Reader {
  /// Construct a Reader without any pre-scanned bibitems.  Used by
  /// internal tests; production callers go through
  /// `new_with_bibitems` so cite-key popups have data.
  #[allow(dead_code)]
  pub fn new(blocks: Vec<Block>, width: usize, height: usize) -> Self {
    Self::new_with_bibitems(blocks, width, height, HashMap::new())
  }

  pub fn new_with_bibitems(
    blocks: Vec<Block>,
    width: usize,
    height: usize,
    bibitems: HashMap<String, String>,
  ) -> Self {
    let cw = content_width_for(width, false);
    let visual_lines = build_visual_lines(&blocks, cw, height);
    let sections = build_sections(&visual_lines);
    let (label_lines, mut bib_entries, bib_entry_lines) = build_link_indexes(&blocks, &visual_lines);
    // Pre-scanned bibitems from source override anything we picked up via
    // Block::Anchor("ref-…") — Pandoc's bibliography Paras don't carry
    // cite-keys, so this is the authoritative source.
    for (k, v) in bibitems {
      bib_entries.insert(k, v);
    }
    let mut image_paths = HashMap::new();
    for block in &blocks {
      match block {
        Block::Image { kitty_id, path, .. } => {
          image_paths.insert(*kitty_id, path.clone());
        }
        Block::ImageRow { items, .. } => {
          for item in items {
            image_paths.insert(item.kitty_id, item.path.clone());
          }
        }
        _ => {}
      }
    }
    Self {
      blocks,
      visual_lines,
      sections,
      label_lines,
      bib_entries,
      bib_entry_lines,
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
      bookmarks: HashMap::new(),
      highlights: HighlightSet::default(),
      cursor_x: 0,
      desired_column: 0,
      visual_anchor: 0,
      visual_anchor_x: 0,
      count_buf: String::new(),
      cmd_buf: String::new(),
      cmd_error: None,
      popup: None,
      image_paths,
      // Voice fields default to "no playback in progress."  The
      // controller is wired in lib.rs::run after the API key + config
      // are loaded; leaving it None here lets tests skip audio entirely.
      voice_controller: None,
      voice_status: crate::voice::PlaybackStatus::Idle,
      voice_error: None,
      voice_para_start: 0,
      voice_para_end: 0,
      voice_started_at: None,
      voice_chars_before: 0,
      reading_mode: false,
      continuous_reading: false,
    }
  }

  /// Replace the loaded paper with freshly-fetched blocks + bibitems
  /// in-place, preserving user state where it still makes sense.  Used
  /// by `:reload`.  We keep:
  ///   - `offset`, `cursor_y`, `cursor_x`, `desired_column` (clamped to new bounds)
  ///   - `bookmarks` and `highlights` (block-byte addressed; survive
  ///     re-parse iff the document structure is unchanged.  If the paper
  ///     has been edited upstream, some marks may land on different
  ///     content — acceptable for v1)
  ///   - `mode`, `search_query`, `search_matches`, `search_idx`, `nav_history`,
  ///     `toc_visible`, `meta`, `count_buf`, `cmd_buf`, `cmd_error`, `popup`
  ///
  /// Re-derives: `visual_lines`, `sections`, `label_lines`, `bib_entries`,
  /// `bib_entry_lines`, `image_paths`.
  pub fn reload_with(&mut self, blocks: Vec<Block>, bibitems: HashMap<String, String>) {
    self.blocks = blocks;
    let cw = self.content_width();
    self.visual_lines = build_visual_lines(&self.blocks, cw, self.height);
    self.sections = build_sections(&self.visual_lines);
    let (label_lines, mut bib_entries, bib_entry_lines) =
      build_link_indexes(&self.blocks, &self.visual_lines);
    for (k, v) in bibitems {
      bib_entries.insert(k, v);
    }
    self.label_lines = label_lines;
    self.bib_entries = bib_entries;
    self.bib_entry_lines = bib_entry_lines;
    let mut image_paths = HashMap::new();
    for block in &self.blocks {
      match block {
        Block::Image { kitty_id, path, .. } => {
          image_paths.insert(*kitty_id, path.clone());
        }
        Block::ImageRow { items, .. } => {
          for item in items {
            image_paths.insert(item.kitty_id, item.path.clone());
          }
        }
        _ => {}
      }
    }
    self.image_paths = image_paths;
    self.clamp_position();
  }

  /// Effective text column width after subtracting the TOC panel (if visible).
  pub fn content_width(&self) -> usize {
    content_width_for(self.width, self.toc_visible)
  }

  pub fn resize(&mut self, width: usize, height: usize) {
    self.width = width;
    self.height = height;
    let cw = self.content_width();
    self.visual_lines = build_visual_lines(&self.blocks, cw, height);
    self.sections = build_sections(&self.visual_lines);
    let (ll, be, bel) = build_link_indexes(&self.blocks, &self.visual_lines);
    self.label_lines = ll;
    self.bib_entries = be;
    self.bib_entry_lines = bel;
    self.clamp_position();
  }

  pub fn toggle_toc(&mut self) {
    self.toc_visible = !self.toc_visible;
    let cw = self.content_width();
    self.visual_lines = build_visual_lines(&self.blocks, cw, self.height);
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
      self.clamp_cursor_after_line_change();
    }
  }

  pub fn toggle_help(&mut self) {
    self.help_visible = !self.help_visible;
  }

  /// Set mark `letter` at the current line, replacing any prior value.
  /// Only ASCII letters (a–z, A–Z) are valid; other chars are silently
  /// rejected so the user gets no surprise mark on a stray punctuation key.
  pub fn set_mark(&mut self, letter: char) {
    if !letter.is_ascii_alphabetic() { return; }
    let line = self.offset + self.cursor_y;
    self.bookmarks.insert(letter, line);
  }

  /// Jump to mark `letter`.  No-op if the mark is unset or the letter
  /// is invalid.  Pushes the current position onto the back-nav stack
  /// so `Ctrl+O` returns here.
  pub fn jump_to_mark(&mut self, letter: char) {
    if !letter.is_ascii_alphabetic() { return; }
    let Some(&target) = self.bookmarks.get(&letter) else { return };
    let total = self.total_lines();
    if target >= total { return; }
    self.push_nav_mark();
    self.offset = target;
    self.cursor_y = 0;
    self.clamp_cursor_after_line_change();
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
    self.clamp_cursor_after_line_change();
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

/// Build the cross-reference resolution maps from `Block::Anchor`
/// markers and bibliography div ids.  Returns `(label_lines,
/// bib_entries, bib_entry_lines)`.
///
/// - `label_lines`: each Anchor associates with the first VL of the
///   *next* visible block.  For ref-targets we want the equation /
///   figure / table fully visible, so callers (`follow_link_target`)
///   subtract one when jumping.
/// - `bib_entries`: keys are cite-keys (Pandoc strips the `ref-`
///   prefix); value is the joined text of the entry block.
/// - `bib_entry_lines`: same keys, value is the first VL index of the
///   entry — used by `Enter` (jump-to-bib).
fn build_link_indexes(
  blocks: &[Block],
  visual_lines: &[VisualLine],
) -> (HashMap<String, usize>, HashMap<String, String>, HashMap<String, usize>) {
  // Map block_idx → first VL with that block_idx.  O(n) once.
  let mut block_to_vl: HashMap<usize, usize> = HashMap::new();
  for (vl_idx, vl) in visual_lines.iter().enumerate() {
    block_to_vl.entry(vl.block_idx).or_insert(vl_idx);
  }

  let mut label_lines = HashMap::new();
  let mut bib_entries = HashMap::new();
  let mut bib_entry_lines = HashMap::new();

  for (bi, block) in blocks.iter().enumerate() {
    if let Block::Anchor(label) = block {
      // Walk forward from bi+1 to find the next visible block.
      let target_block = (bi + 1..blocks.len()).find(|&j| {
        !matches!(blocks[j], Block::Anchor(_) | Block::Blank)
      });
      let target_vl = target_block.and_then(|j| block_to_vl.get(&j).copied());
      if let Some(vl) = target_vl {
        // Pandoc bib divs have id="ref-<key>".  Strip the prefix and
        // also capture the entry text for popup display.
        if let Some(key) = label.strip_prefix("ref-") {
          bib_entry_lines.insert(key.to_string(), vl);
          if let Some(j) = target_block {
            let entry_text = block_text(&blocks[j]);
            if !entry_text.is_empty() {
              bib_entries.insert(key.to_string(), entry_text);
            }
          }
        } else {
          label_lines.insert(label.clone(), vl);
        }
      }
    }
  }
  (label_lines, bib_entries, bib_entry_lines)
}

/// Extract the rendered text of a block for bib-entry popup display.
/// Strips inline styling — popup is plain text only.
fn block_text(block: &Block) -> String {
  match block {
    Block::Line(s) => s.clone(),
    Block::StyledLine(spans) => spans.iter().map(|s| s.text.as_str()).collect(),
    Block::Header { text, .. } => text.clone(),
    Block::ListItem { content, .. } => content.iter().map(|s| s.text.as_str()).collect(),
    Block::Quote(spans) => spans.iter().map(|s| s.text.as_str()).collect(),
    _ => String::new(),
  }
}
