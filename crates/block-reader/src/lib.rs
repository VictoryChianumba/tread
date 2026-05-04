mod bookmarks;
mod commands;
mod config;
mod highlights;
mod nav;
mod progress;
mod render;
mod state;

use crossterm::{
  event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers, MouseEventKind},
  execute,
  terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use doc_model::Block;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use commands::{CmdCtx, CommandResult};
use state::{FindKind, Mode, Reader};
use std::io;
use ui_theme::Theme;

pub use state::PaperMeta;

pub fn run(
  blocks: Vec<Block>,
  meta: Option<PaperMeta>,
  progress_key: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
  // Resolve theme via the new config layer: respect override, else follow
  // trench's theme, else fall back to the built-in dark default.
  let theme = config::resolve_theme();
  run_with_theme(blocks, meta, progress_key, theme)
}

pub fn run_with_theme(
  blocks: Vec<Block>,
  meta: Option<PaperMeta>,
  progress_key: Option<String>,
  theme: Theme,
) -> Result<(), Box<dyn std::error::Error>> {
  enable_raw_mode()?;
  let mut stdout = io::stdout();
  execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
  let backend = CrosstermBackend::new(stdout);
  let mut terminal = Terminal::new(backend)?;

  let size = terminal.size()?;
  let mut reader = Reader::new(blocks, size.width as usize, size.height as usize);
  reader.meta = meta;

  // Restore reading progress and bookmarks.
  if let Some(ref key) = progress_key {
    let map = progress::load();
    if let Some(p) = map.get(key) {
      let max_offset = reader.total_lines().saturating_sub(1);
      reader.offset = p.offset.min(max_offset);
    }
    reader.bookmarks = bookmarks::load(key).named;
    reader.highlights = highlights::load(key);
  }

  let ctx = CmdCtx { arxiv_id: progress_key.clone() };
  let result = event_loop(&mut terminal, &mut reader, theme, ctx);

  // Persist reading progress and bookmarks on clean exit.
  if let Some(ref key) = progress_key {
    let mut map = progress::load();
    map.insert(key.clone(), progress::ReaderProgress { offset: reader.offset });
    progress::save(&map);
    bookmarks::save(key, &bookmarks::BookmarkSet {
      marks: Vec::new(),
      named: reader.bookmarks.clone(),
    });
    highlights::save(key, &reader.highlights);
  }

  disable_raw_mode()?;
  execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
  terminal.show_cursor()?;

  result
}

fn event_loop(
  terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
  reader: &mut Reader,
  mut theme: Theme,
  ctx: CmdCtx,
) -> Result<(), Box<dyn std::error::Error>> {
  loop {
    terminal.draw(|f| render::draw(f, reader, &theme))?;

    match event::read()? {
      Event::Key(key) => {
        // Any keystroke clears the previous command's error and dismisses
        // any open popup so it doesn't linger across input.
        reader.cmd_error = None;
        if reader.popup.is_some() {
          reader.popup = None;
          continue;
        }
        match reader.mode {
          Mode::Normal => {
            if handle_normal(reader, key.code, key.modifiers) {
              break;
            }
          }
          Mode::Search => handle_search(reader, key.code),
          Mode::Visual { .. } => handle_visual(reader, key.code),
          Mode::AwaitingChar { kind } => handle_awaiting_char(reader, key.code, kind),
          Mode::AwaitingMarkName { for_set } => handle_awaiting_mark_name(reader, key.code, for_set),
          Mode::AwaitingG => handle_awaiting_g(reader, key.code),
          Mode::Command => match handle_command(reader, key.code, &ctx) {
            CommandResult::Continue => {}
            CommandResult::Quit => break,
            CommandResult::ChangeTheme(new) => theme = new,
            CommandResult::OpenHelp => reader.help_visible = true,
            CommandResult::Error(msg) => reader.cmd_error = Some(msg),
          },
        }
      }
      Event::Mouse(mouse) => match mouse.kind {
        MouseEventKind::ScrollDown => { for _ in 0..3 { reader.nav_down(); } }
        MouseEventKind::ScrollUp   => { for _ in 0..3 { reader.nav_up(); } }
        _ => {}
      },
      Event::Resize(w, h) => reader.resize(w as usize, h as usize),
      _ => {}
    }
  }
  Ok(())
}

fn take_count(reader: &mut Reader) -> usize {
  if reader.count_buf.is_empty() {
    1
  } else {
    let n: usize = reader.count_buf.parse().unwrap_or(1).max(1).min(9999);
    reader.count_buf.clear();
    n
  }
}

fn handle_normal(reader: &mut Reader, code: KeyCode, mods: KeyModifiers) -> bool {
  // Dismiss help overlay on any key.
  if reader.help_visible {
    reader.help_visible = false;
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
      for _ in 0..n { reader.nav_down(); }
    }
    KeyCode::Char('k') | KeyCode::Up => {
      let n = take_count(reader);
      for _ in 0..n { reader.nav_up(); }
    }
    KeyCode::Char('g') => {
      reader.count_buf.clear();
      reader.mode = Mode::AwaitingG;
    }
    KeyCode::Char('G') => {
      if reader.count_buf.is_empty() {
        reader.nav_bottom();
      } else {
        let n = take_count(reader);
        let target = n.saturating_sub(1).min(reader.total_lines().saturating_sub(1));
        reader.push_nav_mark();
        reader.offset = target;
        reader.cursor_y = 0;
      }
    }
    KeyCode::Char('d') if mods.contains(KeyModifiers::CONTROL) => {
      let n = take_count(reader);
      for _ in 0..n { reader.nav_half_page_down(); }
    }
    KeyCode::Char('u') if mods.contains(KeyModifiers::CONTROL) => {
      let n = take_count(reader);
      for _ in 0..n { reader.nav_half_page_up(); }
    }
    KeyCode::PageDown => {
      let n = take_count(reader);
      for _ in 0..n { reader.nav_page_down(); }
    }
    KeyCode::PageUp => {
      let n = take_count(reader);
      for _ in 0..n { reader.nav_page_up(); }
    }
    KeyCode::Char('}') => {
      let n = take_count(reader);
      for _ in 0..n { reader.jump_next_paragraph(); }
    }
    KeyCode::Char('{') => {
      let n = take_count(reader);
      for _ in 0..n { reader.jump_prev_paragraph(); }
    }
    KeyCode::Char('H') => { reader.count_buf.clear(); reader.jump_screen_top(); }
    KeyCode::Char('M') => { reader.count_buf.clear(); reader.jump_screen_middle(); }
    KeyCode::Char('L') => { reader.count_buf.clear(); reader.jump_screen_bottom(); }
    KeyCode::Char('z') => { reader.count_buf.clear(); reader.center_cursor(); }
    KeyCode::Char('h') | KeyCode::Left => {
      let n = take_count(reader);
      for _ in 0..n { reader.nav_left(); }
    }
    KeyCode::Char('l') | KeyCode::Right => {
      let n = take_count(reader);
      for _ in 0..n { reader.nav_right(); }
    }
    // Word motions — `w`/`W` forward, `b`/`B` back, `e`/`E` to word-end.
    KeyCode::Char('w') => { let n = take_count(reader); for _ in 0..n { reader.nav_word_forward(false); } reader.remember_column(); }
    KeyCode::Char('W') => { let n = take_count(reader); for _ in 0..n { reader.nav_word_forward(true); } reader.remember_column(); }
    KeyCode::Char('b') => { let n = take_count(reader); for _ in 0..n { reader.nav_word_back(false); } reader.remember_column(); }
    KeyCode::Char('B') => { let n = take_count(reader); for _ in 0..n { reader.nav_word_back(true); } reader.remember_column(); }
    KeyCode::Char('e') => { let n = take_count(reader); for _ in 0..n { reader.nav_word_end(false); } reader.remember_column(); }
    KeyCode::Char('E') => { let n = take_count(reader); for _ in 0..n { reader.nav_word_end(true); } reader.remember_column(); }
    // Line edges — `0` to byte 0, `^` to first non-blank, `$` to last char.
    KeyCode::Char('0') => { reader.count_buf.clear(); reader.nav_line_start(); reader.remember_column(); }
    KeyCode::Char('^') => { reader.count_buf.clear(); reader.nav_line_first_nonblank(); reader.remember_column(); }
    KeyCode::Char('$') => { reader.count_buf.clear(); reader.nav_line_end(); reader.remember_column(); }
    // Find char on current line — enters AwaitingChar mode for the next keystroke.
    KeyCode::Char('f') => { reader.count_buf.clear(); reader.mode = Mode::AwaitingChar { kind: FindKind::F }; }
    KeyCode::Char('F') => { reader.count_buf.clear(); reader.mode = Mode::AwaitingChar { kind: FindKind::ShiftF }; }
    KeyCode::Char('t') => { reader.count_buf.clear(); reader.mode = Mode::AwaitingChar { kind: FindKind::T }; }
    KeyCode::Char('T') => { reader.count_buf.clear(); reader.mode = Mode::AwaitingChar { kind: FindKind::ShiftT }; }
    // Matching brace — `%` jumps between paired brackets on the current line.
    KeyCode::Char('%') => { reader.count_buf.clear(); reader.nav_match_brace(); reader.remember_column(); }
    // Sentence motion — `)` next, `(` previous.  Cross-line.
    KeyCode::Char(')') => { let n = take_count(reader); for _ in 0..n { reader.nav_sentence_forward(); } reader.remember_column(); }
    KeyCode::Char('(') => { let n = take_count(reader); for _ in 0..n { reader.nav_sentence_back(); } reader.remember_column(); }
    // Remove highlight under cursor — eXcise.
    KeyCode::Char('X') => {
      reader.count_buf.clear();
      if let Some(vl) = reader.visual_lines.get(reader.current_line()) {
        if vl.block_byte_end > vl.block_byte_start {
          let local = reader.cursor_x.min(vl.block_byte_end - vl.block_byte_start - 1);
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
      reader.count_buf.clear();
      reader.cmd_buf.clear();
      reader.cmd_error = None;
      reader.mode = Mode::Command;
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
      let n = take_count(reader);
      for _ in 0..n { reader.jump_next_section(); }
    }
    KeyCode::Char('[') => {
      let n = take_count(reader);
      for _ in 0..n { reader.jump_prev_section(); }
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
      reader.count_buf.clear();
      reader.mode = Mode::AwaitingMarkName { for_set: true };
    }
    KeyCode::Char('\'') | KeyCode::Char('`') => {
      reader.count_buf.clear();
      reader.mode = Mode::AwaitingMarkName { for_set: false };
    }
    KeyCode::Char('y') => {
      reader.count_buf.clear();
      if let Some(vl) = reader.visual_lines.get(reader.current_line()) {
        let text = vl.text.clone();
        osc52_yank(&text);
      }
    }
    KeyCode::Char('v') => {
      reader.count_buf.clear();
      reader.visual_anchor = reader.current_line();
      reader.visual_anchor_x = reader.cursor_x;
      reader.mode = Mode::Visual { line_mode: false };
    }
    KeyCode::Char('V') => {
      reader.count_buf.clear();
      reader.visual_anchor = reader.current_line();
      reader.visual_anchor_x = 0;
      reader.mode = Mode::Visual { line_mode: true };
    }
    _ => { reader.count_buf.clear(); }
  }
  false
}

fn handle_search(reader: &mut Reader, code: KeyCode) {
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

fn handle_awaiting_char(reader: &mut Reader, code: KeyCode, kind: FindKind) {
  // One-shot: any keystroke ends AwaitingChar.  A Char(c) performs the find;
  // anything else (Esc, arrow keys, etc.) is a quiet cancel.
  if let KeyCode::Char(c) = code {
    if let Some(idx) = reader.find_char_in_line(c, kind) {
      reader.cursor_x = idx;
      reader.remember_column();
    }
  }
  reader.mode = Mode::Normal;
}

fn handle_command(reader: &mut Reader, code: KeyCode, ctx: &CmdCtx) -> CommandResult {
  match code {
    KeyCode::Esc => {
      reader.cmd_buf.clear();
      reader.mode = Mode::Normal;
      CommandResult::Continue
    }
    KeyCode::Enter => {
      let line = std::mem::take(&mut reader.cmd_buf);
      reader.mode = Mode::Normal;
      commands::execute(reader, ctx, &line)
    }
    KeyCode::Backspace => {
      if reader.cmd_buf.pop().is_none() {
        // Empty buffer: backspace exits command mode (matches search bar UX).
        reader.mode = Mode::Normal;
      }
      CommandResult::Continue
    }
    KeyCode::Char(c) => {
      reader.cmd_buf.push(c);
      CommandResult::Continue
    }
    _ => CommandResult::Continue,
  }
}

fn handle_awaiting_g(reader: &mut Reader, code: KeyCode) {
  // `gg` → top of doc; `ge` / `gE` → backward word-end.  Any other key cancels.
  match code {
    KeyCode::Char('g') => { reader.nav_top(); }
    KeyCode::Char('e') => { reader.nav_word_end_back(false); reader.remember_column(); }
    KeyCode::Char('E') => { reader.nav_word_end_back(true); reader.remember_column(); }
    _ => {}
  }
  reader.mode = Mode::Normal;
}

fn handle_awaiting_mark_name(reader: &mut Reader, code: KeyCode, for_set: bool) {
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
  reader.mode = Mode::Normal;
}

fn handle_visual(reader: &mut Reader, code: KeyCode) {
  match code {
    KeyCode::Esc | KeyCode::Char('v') | KeyCode::Char('V') => {
      reader.mode = Mode::Normal;
    }
    KeyCode::Char('j') | KeyCode::Down => reader.nav_down(),
    KeyCode::Char('k') | KeyCode::Up => reader.nav_up(),
    KeyCode::Char('h') | KeyCode::Left => reader.nav_left(),
    KeyCode::Char('l') | KeyCode::Right => reader.nav_right(),
    KeyCode::Char('y') => {
      let text = yank_selection(reader);
      osc52_yank(&text);
      reader.mode = Mode::Normal;
    }
    KeyCode::Char('H') => {
      commit_selection_as_highlights(reader);
      reader.mode = Mode::Normal;
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
  let is_line_mode = matches!(reader.mode, Mode::Visual { line_mode: true });
  let ax = reader.visual_anchor_x;
  let cx = reader.cursor_x;

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
    let Some(vl) = reader.visual_lines.get(i) else { continue };
    // Skip non-text blocks (Matrix, Rule, Blank) — they have zero byte range.
    if vl.block_byte_end == vl.block_byte_start { continue; }

    let local_start = if !is_line_mode && i == lo { start_x.min(vl.text.len()) } else { 0 };
    let local_end_excl = if !is_line_mode && i == hi {
      next_char_boundary(&vl.text, end_x_incl)
    } else {
      vl.text.len()
    };
    let local_start = snap_back_to_boundary(&vl.text, local_start);
    let local_end_excl = local_end_excl.min(vl.text.len());

    let byte_start = vl.block_byte_start + local_start;
    let byte_end = vl.block_byte_start + local_end_excl;
    if byte_end <= byte_start { continue; }

    match &mut current {
      Some((blk, _, end)) if *blk == vl.block_idx => {
        *end = byte_end;
      }
      _ => {
        if let Some((blk, s, e)) = current.take() {
          reader.highlights.add(Highlight { block_idx: blk, byte_start: s, byte_end: e });
        }
        current = Some((vl.block_idx, byte_start, byte_end));
      }
    }
  }
  if let Some((blk, s, e)) = current {
    reader.highlights.add(Highlight { block_idx: blk, byte_start: s, byte_end: e });
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
  let is_line_mode = matches!(reader.mode, Mode::Visual { line_mode: true });

  let lines: Vec<&str> = (lo..=hi)
    .filter_map(|i| reader.visual_lines.get(i))
    .map(|vl| vl.text.as_str())
    .collect();

  if is_line_mode || (lo == hi && reader.cursor_x == reader.visual_anchor_x) {
    lines.join("\n")
  } else {
    let first = lines.first().copied().unwrap_or("");
    let last = lines.last().copied().unwrap_or("");
    let ax = reader.visual_anchor_x;
    let cx = reader.cursor_x;
    if lo == hi {
      let (s, e) = (ax.min(cx), ax.max(cx) + 1);
      first.get(s..e.min(first.len())).unwrap_or("").to_string()
    } else {
      let (first_start, last_end) = if anchor <= cur { (ax, cx + 1) } else { (cx, ax + 1) };
      let mut parts = vec![first.get(first_start..).unwrap_or("").to_string()];
      if lines.len() > 2 {
        parts.extend(lines[1..lines.len() - 1].iter().map(|s| s.to_string()));
      }
      parts.push(last.get(..last_end.min(last.len())).unwrap_or("").to_string());
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
    out.push(if chunk.len() > 1 { T[((b1 & 15) << 2) | (b2 >> 6)] as char } else { '=' });
    out.push(if chunk.len() > 2 { T[b2 & 63] as char } else { '=' });
  }
  out
}
