mod bookmarks;
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
use state::{Mode, Reader};
use std::io;
use ui_theme::Theme;

pub use state::PaperMeta;

pub fn run(
  blocks: Vec<Block>,
  meta: Option<PaperMeta>,
  progress_key: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
  let theme = ui_theme::ThemeId::Dark.theme();
  run_with_theme(blocks, meta, progress_key, &theme)
}

pub fn run_with_theme(
  blocks: Vec<Block>,
  meta: Option<PaperMeta>,
  progress_key: Option<String>,
  theme: &Theme,
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
    reader.bookmarks = bookmarks::load(key).marks;
  }

  let result = event_loop(&mut terminal, &mut reader, theme);

  // Persist reading progress and bookmarks on clean exit.
  if let Some(ref key) = progress_key {
    let mut map = progress::load();
    map.insert(key.clone(), progress::ReaderProgress { offset: reader.offset });
    progress::save(&map);
    bookmarks::save(key, &bookmarks::BookmarkSet { marks: reader.bookmarks.clone() });
  }

  disable_raw_mode()?;
  execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
  terminal.show_cursor()?;

  result
}

fn event_loop(
  terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
  reader: &mut Reader,
  theme: &Theme,
) -> Result<(), Box<dyn std::error::Error>> {
  loop {
    terminal.draw(|f| render::draw(f, reader, theme))?;

    match event::read()? {
      Event::Key(key) => match reader.mode {
        Mode::Normal => {
          if handle_normal(reader, key.code, key.modifiers) {
            break;
          }
        }
        Mode::Search => handle_search(reader, key.code),
        Mode::Visual { .. } => handle_visual(reader, key.code),
      },
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
      reader.nav_top();
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
      reader.count_buf.clear();
      reader.cursor_x = reader.cursor_x.saturating_sub(1);
    }
    KeyCode::Char('l') | KeyCode::Right => {
      reader.count_buf.clear();
      if let Some(vl) = reader.visual_lines.get(reader.current_line()) {
        reader.cursor_x = (reader.cursor_x + 1).min(vl.text.len().saturating_sub(1));
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
    KeyCode::Char('t') => {
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
      reader.toggle_bookmark();
    }
    KeyCode::Char('\'') => {
      reader.count_buf.clear();
      reader.next_bookmark();
    }
    KeyCode::Char('`') => {
      reader.count_buf.clear();
      reader.prev_bookmark();
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

fn handle_visual(reader: &mut Reader, code: KeyCode) {
  match code {
    KeyCode::Esc | KeyCode::Char('v') | KeyCode::Char('V') => {
      reader.mode = Mode::Normal;
    }
    KeyCode::Char('j') | KeyCode::Down => reader.nav_down(),
    KeyCode::Char('k') | KeyCode::Up => reader.nav_up(),
    KeyCode::Char('h') | KeyCode::Left => {
      reader.cursor_x = reader.cursor_x.saturating_sub(1);
    }
    KeyCode::Char('l') | KeyCode::Right => {
      if let Some(vl) = reader.visual_lines.get(reader.current_line()) {
        reader.cursor_x = (reader.cursor_x + 1).min(vl.text.len().saturating_sub(1));
      }
    }
    KeyCode::Char('y') => {
      let text = yank_selection(reader);
      osc52_yank(&text);
      reader.mode = Mode::Normal;
    }
    _ => {}
  }
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

fn osc52_yank(text: &str) {
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
