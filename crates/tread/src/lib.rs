mod bookmarks;
mod commands;
mod config;
mod highlights;
mod images;
mod nav;
mod progress;
mod render;
mod state;
mod text_objects;
mod voice;
mod voice_control;

use crossterm::{
  event::{self, DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture, Event, KeyCode, KeyModifiers, KeyboardEnhancementFlags, MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags},
  execute,
  terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use doc_model::Block;
use std::collections::HashMap;
use std::sync::Arc;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use state::{FindKind, Mode, Operator};
use std::io;

// Public embed surface — host TUIs (trench) and the standalone tread
// binary both build against these.  See `crates/tread/CLAUDE.md` for
// the embed guide.  Stable from v1; signature changes are a contract
// break.
pub use state::{Reader, PaperMeta};
pub use commands::ReaderAction;
pub use images::ImageState;
pub use voice::PlaybackController;
// Re-export the ui_theme crate so embedding hosts can name `tread::Theme`
// (and its `Color` enum) without taking a separate path-dep on the same
// crate.  When a host's own theme module diverged from tread's, an
// adapter on the host side maps host::Theme → tread::Theme — see
// trench/src/app.rs::theme_for_tread for the trench-side converter.
pub use ui_theme;
pub use ui_theme::Theme;

/// What `fetch_paper` returns: everything the reader needs to build/refresh
/// its block tree and bib lookups for one arXiv paper.  The `asset_dir`
/// is kept on disk for image lookups — the reader doesn't need to track it
/// explicitly, but holding the value here lets callers know it exists.
pub struct PaperData {
  pub blocks: Vec<Block>,
  pub bibitems: HashMap<String, String>,
  pub asset_dir: std::path::PathBuf,
}

impl PaperData {
  /// Build a `PaperData` from pre-extracted plain-text lines.  Used by
  /// host TUIs (trench) to embed tread for non-arXiv content sources —
  /// PDF / EPUB / HTML / local files where the host has already done
  /// the extraction and only needs a typed reader on the resulting
  /// `Vec<String>`.  Each input line becomes one `Block::Line`; no
  /// math, no tables, no images, no cross-refs.  `:reload`, `:cite`,
  /// `:url`, `:open` are no-ops with a sensible error (they all
  /// require an arxiv_id, which `Reader::init` leaves unset for this
  /// path — pass `progress_key = None`).
  ///
  /// All non-prose Reader features still work: navigation, marks,
  /// highlights, search, themes, voice, text objects.
  pub fn from_plain_lines(lines: Vec<String>) -> Self {
    Self {
      blocks: lines.into_iter().map(Block::Line).collect(),
      bibitems: HashMap::new(),
      asset_dir: std::path::PathBuf::new(),
    }
  }
}

/// Fetch + parse + post-process a paper.  Mirrors the bootstrap steps
/// in `main.rs` so both initial load and `:reload` go through the same
/// pipeline: fetch the e-print tarball, parse to blocks, resolve image
/// paths (or degrade to captions on non-graphics terminals), lift table
/// floats to PDF-rendered position.  Network-bound; runs synchronously
/// so the caller will block for ~2s on a typical paper.
pub fn fetch_paper(id: &str, kitty_supported: bool) -> Result<PaperData, String> {
  use arxiv_render::{fetch, parse, pdf_anchors, placement};

  let fetched = fetch::fetch_source(id)?;
  let sources = fetched.tex;
  let asset_dir = fetched.asset_dir;
  let bibitems = arxiv_render::extract_bibitems(&sources);
  let mut blocks = parse::to_blocks(sources);
  if kitty_supported {
    arxiv_render::absolutize_image_paths(&mut blocks, &asset_dir);
  } else {
    arxiv_render::degrade_images_to_captions(&mut blocks);
  }
  // Best-effort PDF anchor extraction; failures leave blocks in source order.
  let anchors = match fetch::fetch_pdf(id) {
    Ok(pdf) => pdf_anchors::extract_anchors(&pdf),
    Err(_) => Vec::new(),
  };
  let blocks = placement::lift_tables(blocks, &anchors);
  Ok(PaperData { blocks, bibitems, asset_dir })
}

pub fn run(
  blocks: Vec<Block>,
  meta: Option<PaperMeta>,
  progress_key: Option<String>,
  bibitems: HashMap<String, String>,
) -> Result<(), Box<dyn std::error::Error>> {
  // Resolve theme via the new config layer: respect override, else follow
  // trench's theme, else fall back to the built-in dark default.
  let theme = config::resolve_theme();
  let kitty_supported = matches!(kitty_graphics::detect(), kitty_graphics::Capability::Supported);
  run_with_theme(blocks, meta, progress_key, bibitems, theme, kitty_supported)
}

pub fn run_with_theme(
  blocks: Vec<Block>,
  meta: Option<PaperMeta>,
  progress_key: Option<String>,
  bibitems: HashMap<String, String>,
  theme: Theme,
  kitty_supported: bool,
) -> Result<(), Box<dyn std::error::Error>> {
  // Standalone tread: tread itself owns terminal setup and teardown.
  // When embedded in a host TUI (trench), the host owns terminal state
  // and constructs the Reader directly via `Reader::init`.
  enable_raw_mode()?;
  let mut stdout = io::stdout();
  // Focus events let us detect tmux pane switches: when our pane
  // loses focus we delete all image placements (otherwise iTerm2
  // keeps them painted at absolute screen coordinates that now
  // belong to a different tmux pane).  Requires `set -g focus-events
  // on` in the user's tmux config; if absent, FocusLost never fires
  // and behaviour is what we had before — image bleed across panes.
  execute!(stdout, EnterAlternateScreen, EnableMouseCapture, EnableFocusChange)?;
  // Opt into the kitty keyboard protocol so Shift+Enter (and other
  // modified specials) are distinguishable from plain Enter.  Terminals
  // that don't speak the protocol silently ignore the push; on those,
  // `K` is the universal fallback for the citation-popup binding.
  let _ = execute!(stdout, PushKeyboardEnhancementFlags(
    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
  ));
  let backend = CrosstermBackend::new(stdout);
  let mut terminal = Terminal::new(backend)?;

  // Build the voice playback controller from saved config + the
  // env-only ELEVENLABS_API_KEY.  Wrapped in `Arc` so the embed surface
  // signature is uniform between standalone and host-embedded use
  // (trench clones the same Arc into each tab — see
  // `tread::build_voice_controller`).
  let voice_controller = Some(build_voice_controller());

  let size = terminal.size()?;
  let paper = PaperData {
    blocks,
    bibitems,
    asset_dir: std::path::PathBuf::new(),
  };
  let mut reader = Reader::init(
    paper,
    meta,
    progress_key.clone(),
    size.width,
    size.height,
    kitty_supported,
    voice_controller,
  );

  let result = standalone_loop(&mut terminal, &mut reader, theme, kitty_supported);

  // Persist reading progress, bookmarks, and highlights on clean exit.
  reader.save_progress();

  // Balance the keyboard-enhancement push.  Ignored on terminals
  // that didn't accept it.
  let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
  disable_raw_mode()?;
  execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture, DisableFocusChange)?;
  terminal.show_cursor()?;

  result
}

/// Post-draw hook: emit Kitty graphics escapes for visible image VLs.
/// Hosts call this after `terminal.draw(|f| tread::render::draw(...))`
/// returns.  Walks the visible window each frame; lazy emission inside
/// `place_visible` skips unchanged placements so the hot path is cheap
/// on idle frames.  No-op when `kitty_supported == false` — keeps
/// non-graphics terminals from paying any cost here.
pub fn after_draw(reader: &Reader, state: &mut ImageState, area: Rect, kitty_supported: bool) {
  if !kitty_supported {
    return;
  }
  let (_, _, content_area, _, _) = render::split_layout(area, reader);
  images::place_visible(reader, state, content_area, kitty_supported);
}

/// Public draw entry point — wraps `render::draw` so hosts don't need
/// to import `render`.  Hosts pass the sub-rectangle they want the
/// reader to occupy (e.g. one half of a split pane); standalone tread
/// passes `frame.area()` for the whole terminal.
pub fn draw(frame: &mut ratatui::Frame, area: Rect, reader: &Reader, theme: &Theme) {
  render::draw(frame, area, reader, theme);
}

/// Clear the host-owned image cache.  Call after `:reload` returns
/// `ReaderAction::Reload`, on terminal resize, and on `Event::FocusLost`
/// (tmux pane switch).  Idempotent and cheap.
pub fn clear_images(state: &mut ImageState) {
  images::clear_all(state);
}

/// Build a shared TTS playback controller from the saved voice config
/// plus the env-only `ELEVENLABS_API_KEY`.  Hosts call this once at
/// app startup and `clone()` the returned `Arc` into every Reader tab
/// — the underlying audio thread + `rodio::Sink` are shared, so only
/// one paper speaks at a time across tabs (cross-tab preemption is
/// handled by the session-id machinery in `voice/playback.rs`).
///
/// Construction never fails: if the audio device isn't available, the
/// playback thread surfaces the error via `take_error()` instead of
/// panicking.  Standalone tread embeds the same call in
/// `run_with_theme`.
pub fn build_voice_controller() -> Arc<PlaybackController> {
  let cfg = config::load();
  let voice_cfg = cfg.voice.clone().unwrap_or_default();
  let api_key = std::env::var("ELEVENLABS_API_KEY").ok();
  let provider = voice::make_provider(&voice_cfg, api_key.as_deref());
  Arc::new(PlaybackController::new(provider))
}

impl Reader {
  /// Embed-surface event handler.  Hosts call this for every crossterm
  /// `Event` they want the reader to process — keystrokes, mouse,
  /// resize, focus events.  Returns a `ReaderAction` the host reacts
  /// to: `Quit` to close the reader, `ChangeTheme(t)` to update the
  /// active theme, `Reload` to clear image caches, `Error(msg)` to
  /// optionally surface in a host-side status bar (the reader's own
  /// status bar already shows `cmd_error`).  `OpenHelp` is a side-
  /// effect-only action — `help_visible` has already been set on the
  /// Reader; the host can ignore it.
  ///
  /// Resize: this method calls `Reader::resize`, but the host should
  /// also call `tread::clear_images(&mut state)` because reflow can
  /// move every image VL.  FocusLost: same — the host should clear
  /// the image cache.  Both can be done before or after this call.
  pub fn handle_event(&mut self, ev: Event) -> ReaderAction {
    match ev {
      Event::Key(key) => {
        // Any keystroke clears the previous command's error and dismisses
        // any open popup so it doesn't linger across input.
        self.cmd_error = None;
        if self.popup.is_some() {
          self.popup = None;
          return ReaderAction::Continue;
        }
        match self.mode {
          Mode::Normal => {
            if handle_normal(self, key.code, key.modifiers) {
              ReaderAction::Quit
            } else {
              ReaderAction::Continue
            }
          }
          Mode::Search => { handle_search(self, key.code); ReaderAction::Continue }
          Mode::Visual { .. } => { handle_visual(self, key.code); ReaderAction::Continue }
          Mode::AwaitingChar { kind } => {
            handle_awaiting_char(self, key.code, kind);
            ReaderAction::Continue
          }
          Mode::AwaitingMarkName { for_set } => {
            handle_awaiting_mark_name(self, key.code, for_set);
            ReaderAction::Continue
          }
          Mode::AwaitingG => { handle_awaiting_g(self, key.code); ReaderAction::Continue }
          Mode::AwaitingOperator { op } => {
            handle_awaiting_operator(self, key.code, op);
            ReaderAction::Continue
          }
          Mode::AwaitingTextObject { op, around } => {
            handle_awaiting_text_object(self, key.code, op, around);
            ReaderAction::Continue
          }
          Mode::Command => handle_command(self, key.code),
        }
      }
      Event::Mouse(mouse) => {
        match mouse.kind {
          MouseEventKind::ScrollDown => { for _ in 0..3 { self.nav_down(); } }
          MouseEventKind::ScrollUp => { for _ in 0..3 { self.nav_up(); } }
          _ => {}
        }
        ReaderAction::Continue
      }
      Event::Resize(w, h) => {
        // Reflow visual_lines for the new size.  The host is responsible
        // for clearing its image cache — see method docs.
        self.resize(w, h);
        ReaderAction::Continue
      }
      // FocusLost / FocusGained / Paste: no Reader-side state change
      // here.  Hosts should clear the image cache on FocusLost so
      // placements don't bleed across tmux panes; the next draw will
      // re-emit visible images via `after_draw` anyway.
      _ => ReaderAction::Continue,
    }
  }

  /// Per-frame tick: sync voice playback state and run continuous-
  /// reading auto-advance.  Hosts call this once per frame (typically
  /// on `event::poll` timeout in their main loop) so the loading
  /// spinner animates and the active-word highlight repaints during
  /// playback.  Cheap when `voice_controller` is None.
  ///
  /// Returns `true` if the tick mutated user-visible state (voice
  /// status / active word / paragraph advance) so the host can mark
  /// its own dirty flag and redraw on the next frame.  Returns
  /// `false` on a fully idle tick — hosts can skip redrawing.
  pub fn tick(&mut self) -> bool {
    if self.voice_controller.is_none() {
      return false;
    }
    let prev_status = self.voice_status.clone();
    let prev_started_at = self.voice_started_at;
    let prev_chars_before = self.voice_chars_before;
    voice_control::sync_voice_status(self);
    let mut changed = self.voice_status != prev_status
      || self.voice_started_at != prev_started_at
      || self.voice_chars_before != prev_chars_before;
    if self.continuous_reading
      && matches!(prev_status, voice::PlaybackStatus::Playing)
      && matches!(self.voice_status, voice::PlaybackStatus::Idle)
    {
      // Chunk just ended → roll forward to the next paragraph.
      let advanced = voice_control::advance_to_next_paragraph_for_continuous_reading(self);
      if !advanced {
        self.continuous_reading = false;
      }
      changed = true;
    }
    // While voice is actively playing, the active-word highlight
    // moves continuously based on wall-clock time, so every tick
    // changes visible state even if no field flipped.
    if matches!(self.voice_status, voice::PlaybackStatus::Playing | voice::PlaybackStatus::Loading) {
      changed = true;
    }
    changed
  }
}

/// Standalone-binary event loop.  Drives the embed surface from inside
/// tread itself so the standalone path and the embedded path share one
/// implementation.  Trench has its own equivalent loop that interleaves
/// reader events with feed events.
fn standalone_loop(
  terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
  reader: &mut Reader,
  mut theme: Theme,
  kitty_supported: bool,
) -> Result<(), Box<dyn std::error::Error>> {
  let mut img_state = images::ImageState::default();
  loop {
    terminal.draw(|f| render::draw(f, f.area(), reader, &theme))?;

    if kitty_supported {
      let size = terminal.size()?;
      let area = Rect::new(0, 0, size.width, size.height);
      after_draw(reader, &mut img_state, area, kitty_supported);
    }

    if !event::poll(std::time::Duration::from_millis(100))? {
      // Idle tick — refresh voice state, then loop back to redraw.
      reader.tick();
      continue;
    }

    let ev = event::read()?;
    // Image-cache cleanup events: host-side mirror of the contract.
    let needs_image_clear = matches!(&ev, Event::Resize(_, _) | Event::FocusLost);

    match reader.handle_event(ev) {
      ReaderAction::Quit => break,
      ReaderAction::ChangeTheme(t) => theme = t,
      ReaderAction::Reload => {
        // Paper just rebuilt with fresh blocks → all kitty_ids are new.
        // Drop on-screen image placements so we don't try to reuse
        // stale ids the terminal still has cached.
        images::clear_all(&mut img_state);
      }
      ReaderAction::Error(msg) => reader.cmd_error = Some(msg),
      ReaderAction::OpenHelp => reader.help_visible = true,
      ReaderAction::Continue => {}
    }

    if needs_image_clear {
      images::clear_all(&mut img_state);
    }
  }
  // Clear any lingering image placements so they don't bleed onto the
  // user's shell after we leave the alt screen.
  images::clear_all(&mut img_state);
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

  // Voice keys take priority — `r`, `R`, `Ctrl+P` enter or restart
  // reading mode; `Space`, `c`, `Esc` only fire while reading_mode is
  // true.  Putting this BEFORE the digit accumulator and the global
  // `Esc`-quits match means reading-mode Esc stops audio without
  // also quitting the reader.
  let key_event = crossterm::event::KeyEvent::new(code, mods);
  if voice_control::handle_voice_keys(reader, key_event) {
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
      // Vim operator-pending: bare `y` waits for a follow-up.  `yy`
      // yanks the current line, `yi<obj>` / `ya<obj>` yanks a text
      // object.  Any other key cancels and returns to Normal.
      reader.count_buf.clear();
      reader.mode = Mode::AwaitingOperator { op: Operator::Yank };
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

fn handle_command(reader: &mut Reader, code: KeyCode) -> ReaderAction {
  match code {
    KeyCode::Esc => {
      reader.cmd_buf.clear();
      reader.mode = Mode::Normal;
      ReaderAction::Continue
    }
    KeyCode::Enter => {
      let line = std::mem::take(&mut reader.cmd_buf);
      reader.mode = Mode::Normal;
      commands::execute(reader, &line)
    }
    KeyCode::Backspace => {
      if reader.cmd_buf.pop().is_none() {
        // Empty buffer: backspace exits command mode (matches search bar UX).
        reader.mode = Mode::Normal;
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

fn handle_awaiting_operator(reader: &mut Reader, code: KeyCode, op: Operator) {
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
      reader.mode = Mode::Normal;
    }
    KeyCode::Char('i') => {
      reader.mode = Mode::AwaitingTextObject { op, around: false };
    }
    KeyCode::Char('a') => {
      reader.mode = Mode::AwaitingTextObject { op, around: true };
    }
    _ => {
      reader.mode = Mode::Normal;
    }
  }
}

fn handle_awaiting_text_object(reader: &mut Reader, code: KeyCode, op: Operator, around: bool) {
  // Look up the text object spec character and produce the yank target.
  // `b` aliases parens, `B` aliases braces (vim convention).
  let yanked: Option<String> = match code {
    KeyCode::Char('w') => text_objects::word(reader, false, around),
    KeyCode::Char('W') => text_objects::word(reader, true, around),
    KeyCode::Char('"') => text_objects::quote(reader, '"', around),
    KeyCode::Char('\'') => text_objects::quote(reader, '\'', around),
    KeyCode::Char('`') => text_objects::quote(reader, '`', around),
    KeyCode::Char('(') | KeyCode::Char(')') | KeyCode::Char('b') => text_objects::pair(reader, '(', ')', around),
    KeyCode::Char('[') | KeyCode::Char(']') => text_objects::pair(reader, '[', ']', around),
    KeyCode::Char('{') | KeyCode::Char('}') | KeyCode::Char('B') => text_objects::pair(reader, '{', '}', around),
    KeyCode::Char('p') => text_objects::paragraph(reader, around),
    KeyCode::Char('s') => text_objects::sentence(reader, around),
    _ => None,
  };
  if let Some(text) = yanked {
    match op {
      Operator::Yank => osc52_yank(&text),
    }
  }
  reader.mode = Mode::Normal;
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
    if reader.cursor_x >= byte && reader.cursor_x < next {
      return span.link_target.clone();
    }
    byte = next;
  }
  None
}

/// Enter dispatch: jump to whatever the cursor is sitting on.  No-op if
/// nothing is there.  Pushes onto the back-nav stack so `Ctrl+O` rewinds.
fn follow_link_at_cursor(reader: &mut Reader) {
  let Some(target) = link_at_cursor(reader) else { return };
  let line = match &target {
    doc_model::LinkTarget::Internal(label) => reader.label_lines.get(label).copied(),
    doc_model::LinkTarget::Citation(key) => reader.bib_entry_lines.get(key).copied(),
  };
  let Some(line) = line else { return };
  // Land one line before the labeled element so it's fully visible
  // below the cursor (vim convention for jumps).  Clamp at 0.
  let target_line = line.saturating_sub(1);
  reader.push_nav_mark();
  jump_to_line_for_link(reader, target_line);
}

/// `K` / `Shift+Enter`: show the citation entry in a popup.  Only acts
/// on `LinkTarget::Citation`; `Internal` targets are silently ignored
/// (Enter is the right key for those).
fn popup_citation_at_cursor(reader: &mut Reader) {
  let Some(target) = link_at_cursor(reader) else { return };
  let key = match target {
    doc_model::LinkTarget::Citation(k) => k,
    _ => return,
  };
  let entry = reader.bib_entries.get(&key).cloned()
    .unwrap_or_else(|| "(no entry available)".to_string());
  // Wrap to ~60 chars for readability in the popup.
  let lines: Vec<String> = wrap_for_popup(&entry, 60);
  reader.popup = Some(state::PopupContent {
    title: format!("[{key}]"),
    lines,
  });
}

fn jump_to_line_for_link(reader: &mut Reader, line: usize) {
  let total = reader.total_lines();
  if total == 0 { return; }
  let line = line.min(total - 1);
  let ch = reader.content_height();
  if line >= reader.offset && line < reader.offset + ch {
    reader.cursor_y = line - reader.offset;
  } else {
    reader.offset = line;
    reader.cursor_y = 0;
  }
  reader.cursor_x = 0;
  reader.desired_column = 0;
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
  if !line.is_empty() { out.push(line); }
  if out.is_empty() { out.push(String::new()); }
  out
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
