pub mod bench;
mod commands;
mod config;
mod epub;
mod highlights;
mod html;
mod images;
mod keys;
mod markdown;
mod paper;
mod pdf;
mod persist;
mod progress;
mod render;
mod runtime;
mod state;
mod text_objects;
mod voice;

use crossterm::{
  event::{
    DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture, Event,
    KeyboardEnhancementFlags, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
  },
  execute,
  terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use doc_model::Block;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use state::Mode;
use std::collections::HashMap;
use std::io;
use std::sync::Arc;

// Public embed surface — host TUIs (trench) and the standalone tread
// binary both build against these.  See `crates/tread/CLAUDE.md` for
// the embed guide.  Stable from v1; signature changes are a contract
// break.
pub use commands::ReaderAction;
pub use images::{BurstTracker, ImageState};
pub use paper::{PaperData, fetch_any, fetch_paper, fetch_paper_refresh};
pub use state::{PaperMeta, Reader};
pub use voice::PlaybackController;
// Re-export the ui_theme crate so embedding hosts can name `tread::Theme`
// (and its `Color` enum) without taking a separate path-dep on the same
// crate.  When a host's own theme module diverged from tread's, an
// adapter on the host side maps host::Theme → tread::Theme — see
// trench/src/app.rs::theme_for_tread for the trench-side converter.
pub use ui_theme;
pub use ui_theme::Theme;

// Internal re-export so commands.rs continues to reach the clipboard
// write via `crate::osc52_yank` — the function itself now lives in
// `keys.rs`.
pub(crate) use keys::osc52_yank;

use runtime::ReaderRuntime;

/// True when the host terminal speaks the Kitty graphics protocol —
/// i.e. tread can render figures as actual pixel images via APC
/// escapes after each draw.  Wraps `kitty_graphics::detect()` so
/// embedding hosts (trench) don't need a separate path-dep on
/// `kitty-graphics`.  Standalone tread calls this in main.rs;
/// embedding hosts call it once at startup and pass the bool into
/// `Reader::init` plus the per-frame `tread::after_draw`.
pub fn detect_kitty_supported() -> bool {
  matches!(
    kitty_graphics::detect(),
    kitty_graphics::Capability::Supported
  )
}

/// Re-export of `kitty_graphics::in_zellij()` so embedding hosts
/// (trench) can surface the Zellij-over-iTerm2 graphics warning
/// without taking a direct dep on the kitty-graphics crate.
pub fn in_zellij() -> bool {
  kitty_graphics::in_zellij()
}

/// Re-export of `kitty_graphics::is_iterm2()`.  Used together with
/// `in_zellij()` to scope the figure-preview warning to the
/// host/multiplexer combo where Kitty graphics rendering is known
/// to fail silently.
pub fn is_iterm2() -> bool {
  kitty_graphics::is_iterm2()
}

/// Re-export of `kitty_graphics::tmux_passthrough_enabled()`.  Hosts
/// (trench) call this at startup to detect the silent-failure mode
/// where tmux is dropping every DCS passthrough envelope, and surface
/// a loud warning before alt-screen entry.  See the kitty-graphics
/// crate for the full semantics including the `None`-on-failure cases.
pub fn tmux_passthrough_enabled() -> Option<bool> {
  kitty_graphics::tmux_passthrough_enabled()
}

/// Extract an arXiv id from an URL or bare-id string.  Returns `None`
/// if the input doesn't look like an arXiv reference.  Hosts use this
/// to decide whether a paper should route through `fetch_paper`
/// (LaTeX source, full math/tables/figures) or fall back to plain-
/// text ingestion.  Wraps `arxiv_render::fetch::extract_id`.
pub fn extract_arxiv_id(url_or_id: &str) -> Option<String> {
  arxiv_render::fetch::extract_id(url_or_id)
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
  let kitty_supported = matches!(
    kitty_graphics::detect(),
    kitty_graphics::Capability::Supported
  );
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
  // One-shot figure-cache eviction.  Cheap (a read_dir + sort + a few
  // unlinks); harmless if the cache is under cap or doesn't exist.
  // Done before the runtime starts so any rasterisation during the
  // session writes into an already-trimmed dir.
  images::enforce_cache_cap();

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
  execute!(
    stdout,
    EnterAlternateScreen,
    EnableMouseCapture,
    EnableFocusChange
  )?;
  // Opt into the kitty keyboard protocol so Shift+Enter (and other
  // modified specials) are distinguishable from plain Enter.  Terminals
  // that don't speak the protocol silently ignore the push; on those,
  // `K` is the universal fallback for the citation-popup binding.
  let _ = execute!(
    stdout,
    PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
  );
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
  let reader = Reader::init(
    paper,
    meta,
    progress_key.clone(),
    size.width,
    size.height,
    kitty_supported,
    voice_controller,
  );

  let runtime = ReaderRuntime::new(reader, theme, kitty_supported);
  let (reader, result) = runtime.run(&mut terminal);

  // Persist reading progress, bookmarks, and highlights on clean exit.
  bench::time("shutdown_save", || reader.save_progress());
  bench::dump_summary();

  // Balance the keyboard-enhancement push.  Ignored on terminals
  // that didn't accept it.
  let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
  disable_raw_mode()?;
  execute!(
    terminal.backend_mut(),
    LeaveAlternateScreen,
    DisableMouseCapture,
    DisableFocusChange
  )?;
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
  let (reader_area, preview_area) = render::split_content_for_preview(content_area, reader);
  images::place_visible(reader, state, reader_area, kitty_supported);
  emit_preview(
    reader,
    state,
    preview_area.map(render::preview_image_area),
    area,
    kitty_supported,
  );
}

/// Emit the figure-preview placement.  Separated from `after_draw`'s
/// inline path so the burst gate can suppress one without the other —
/// the preview only changes on toggle / step, never on scroll, so it
/// has no reason to coalesce during scroll bursts.
fn emit_preview(
  reader: &Reader,
  state: &mut ImageState,
  preview_area: Option<Rect>,
  fallback_rect: Rect,
  kitty_supported: bool,
) {
  let kid = preview_area.and_then(|_| reader.current_figure_kitty_id());
  reader.set_preview_geometry(preview_area);
  let rect = preview_area.unwrap_or(fallback_rect);
  images::place_one_figure(reader, state, kid, rect, kitty_supported);
}

/// True when the current runtime should suppress image re-emission
/// during queued-input bursts.  This only applies to terminals that
/// lack a persistent Kitty image cache: on those hosts every placement
/// change forces a full `a=T` retransmit, which can overwhelm the
/// terminal during rapid scroll. Native Kitty keeps image bytes cached
/// and uses the cheap `a=p` path instead, so burst-skip would only add
/// flicker for no gain there.
pub fn burst_skip_enabled(kitty_supported: bool) -> bool {
  let user_disabled = std::env::var("TREAD_IMAGE_BURST_HIDE")
    .map(|v| v == "0")
    .unwrap_or(false);
  kitty_supported && !kitty_graphics::has_persistent_image_cache() && !user_disabled
}

/// Like [`after_draw`] but coalesces image emission during input bursts.
///
/// Hosts compute `burst_in_progress` from a [`BurstTracker`] kept
/// alongside the per-pane [`ImageState`]; on a non-cache host with the
/// gate enabled, this helper returns without emitting anything for the
/// frame.  The existing placement stays on the terminal (we don't
/// delete) so the prior image lingers in its old spot rather than
/// blinking out — far less jarring than the previous behaviour during
/// scroll on iTerm2.  When the burst ends, the next non-skipped frame
/// emits a normal delete+place and the image snaps to its new row.
pub fn after_draw_guarded(
  reader: &Reader,
  state: &mut ImageState,
  area: Rect,
  kitty_supported: bool,
  burst_in_progress: bool,
) {
  if !kitty_supported {
    return;
  }
  let (_, _, content_area, _, _) = render::split_layout(area, reader);
  let (reader_area, preview_area) = render::split_content_for_preview(content_area, reader);
  // Burst gate only suppresses the inline path: that's where a full
  // `a=T` retransmit fires every scroll line on iTerm2 and overwhelms
  // the terminal.  The preview figure only changes on `i` / `]f` /
  // `[f` — never on scroll — and its lazy fast path makes steady-
  // state emission ~zero-cost.  Suppressing it during scroll bursts
  // means the user can press `i` while scrolling and see no figure
  // until the burst window expires, which is the symptom we're
  // unblocking here.
  if !(burst_skip_enabled(kitty_supported) && burst_in_progress) {
    images::place_visible(reader, state, reader_area, kitty_supported);
  }
  emit_preview(
    reader,
    state,
    preview_area.map(render::preview_image_area),
    area,
    kitty_supported,
  );
}

/// Render one figure into a dedicated preview pane (or clear it).
///
/// Hosts pair this with `Reader::set_text_only(true)` to drive a
/// "figure preview" side pane: the reader pane stays text-only,
/// while `kitty_id = Some(id)` shows that figure scaled to fit
/// `area` and centered.  Pass `kitty_id = None` to clear any
/// active preview.  Stepping between figures (different `Some(id)`
/// across calls) deletes the old placement and emits the new one.
///
/// See `images::place_one_figure` for the full state machine.
pub fn place_one_figure(
  reader: &Reader,
  state: &mut ImageState,
  kitty_id: Option<u32>,
  area: Rect,
  kitty_supported: bool,
) {
  images::place_one_figure(reader, state, kitty_id, area, kitty_supported);
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
        if self.popup().is_some() {
          self.close_popup();
          return ReaderAction::Continue;
        }
        // Clone the active mode out of the borrow so we can call
        // `handle_*(self, ...)` (which needs `&mut self`).  Mode is
        // small (one enum tag plus a few primitives in the largest
        // variant) so the clone is negligible.
        let mode = self.mode().clone();
        match mode {
          Mode::Normal => {
            if keys::handle_normal(self, key.code, key.modifiers) {
              ReaderAction::Quit
            } else {
              ReaderAction::Continue
            }
          }
          Mode::Search => {
            keys::handle_search(self, key.code);
            ReaderAction::Continue
          }
          Mode::Visual { .. } => {
            keys::handle_visual(self, key.code);
            ReaderAction::Continue
          }
          Mode::AwaitingChar { kind } => {
            keys::handle_awaiting_char(self, key.code, kind);
            ReaderAction::Continue
          }
          Mode::AwaitingMarkName { for_set } => {
            keys::handle_awaiting_mark_name(self, key.code, for_set);
            ReaderAction::Continue
          }
          Mode::AwaitingG => {
            keys::handle_awaiting_g(self, key.code);
            ReaderAction::Continue
          }
          Mode::AwaitingBracket { forward } => {
            keys::handle_awaiting_bracket(self, key.code, forward);
            ReaderAction::Continue
          }
          Mode::AwaitingOperator { op } => {
            keys::handle_awaiting_operator(self, key.code, op);
            ReaderAction::Continue
          }
          Mode::AwaitingTextObject { op, around } => {
            keys::handle_awaiting_text_object(self, key.code, op, around);
            ReaderAction::Continue
          }
          Mode::Command => keys::handle_command(self, key.code),
        }
      }
      Event::Mouse(mouse) => {
        match mouse.kind {
          MouseEventKind::ScrollDown => {
            for _ in 0..3 {
              self.nav_down();
            }
          }
          MouseEventKind::ScrollUp => {
            for _ in 0..3 {
              self.nav_up();
            }
          }
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
    if self.voice_controller().is_none() {
      return false;
    }
    let prev_status = self.voice_status().clone();
    let prev_started_at = self.voice_started_at();
    let prev_chars_before = self.voice_chars_before();
    crate::state::voice_control::sync_voice_status(self);
    let mut changed = *self.voice_status() != prev_status
      || self.voice_started_at() != prev_started_at
      || self.voice_chars_before() != prev_chars_before;
    if self.continuous_reading()
      && matches!(prev_status, voice::PlaybackStatus::Playing)
      && matches!(self.voice_status(), voice::PlaybackStatus::Idle)
    {
      // Chunk just ended → roll forward to the next paragraph.
      let advanced =
        crate::state::voice_control::advance_to_next_paragraph_for_continuous_reading(self);
      if !advanced {
        self.stop_continuous_reading();
      }
      changed = true;
    }
    // While voice is actively playing, the active-word highlight
    // moves continuously based on wall-clock time, so every tick
    // changes visible state even if no field flipped.
    if matches!(
      self.voice_status(),
      voice::PlaybackStatus::Playing | voice::PlaybackStatus::Loading
    ) {
      changed = true;
    }
    changed
  }
}

#[cfg(test)]
mod acceptance_tests {
  use super::*;
  use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
  use doc_model::{Block, ImageItem, InlineSpan};
  use std::collections::HashMap;

  fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
  }

  fn ctrl(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
  }

  fn char_key(c: char) -> Event {
    key(KeyCode::Char(c))
  }

  fn send_chars(reader: &mut Reader, text: &str) {
    for c in text.chars() {
      reader.handle_event(char_key(c));
    }
  }

  fn acceptance_reader() -> Reader {
    let blocks = vec![
      Block::Header {
        level: 1,
        text: "Abstract".to_string(),
      },
      Block::Line("alpha beta gamma delta".to_string()),
      Block::Line("method paragraph with mark target".to_string()),
      Block::Anchor("sec:results".to_string()),
      Block::Header {
        level: 1,
        text: "Results".to_string(),
      },
      Block::StyledLine(vec![
        InlineSpan::plain("See "),
        InlineSpan::internal_link("Results", "sec:results"),
        InlineSpan::plain(" and "),
        InlineSpan::citation("[smith]", "smith"),
        InlineSpan::plain("."),
      ]),
      Block::Figure {
        rows: vec![vec![ImageItem {
          path: "figure-a.png".into(),
          kitty_id: 1,
          dims: Some((640, 360)),
        }]],
        alt: "first figure".to_string(),
        figure_id: 1,
        column_gaps_after: Vec::new(),
        header_rows: Vec::new(),
      },
      Block::Figure {
        rows: vec![vec![
          ImageItem {
            path: "figure-b.png".into(),
            kitty_id: 2,
            dims: Some((320, 240)),
          },
          ImageItem {
            path: "figure-c.png".into(),
            kitty_id: 3,
            dims: Some((320, 240)),
          },
        ]],
        alt: "row figure".to_string(),
        figure_id: 2,
        column_gaps_after: Vec::new(),
        header_rows: Vec::new(),
      },
      Block::Anchor("ref-smith".to_string()),
      Block::Line("Smith 2024. A useful reference.".to_string()),
    ];
    Reader::new_with_bibitems(
      blocks,
      100,
      28,
      HashMap::from([(
        "smith".to_string(),
        "Smith 2024. A useful reference.".to_string(),
      )]),
    )
  }

  #[test]
  fn acceptance_preview_scroll_and_resize_stays_coherent() {
    let mut reader = acceptance_reader();

    reader.set_figure_preview_active(true);
    assert!(reader.figure_preview_visible());
    assert_eq!(reader.current_figure_kitty_id(), Some(1));
    assert_eq!(reader.content_width(), 60);

    for _ in 0..8 {
      reader.handle_event(char_key('j'));
    }
    assert_eq!(reader.current_figure_kitty_id(), Some(1));
    assert!(reader.figure_preview_visible());

    reader.handle_event(Event::Resize(80, 20));
    assert_eq!(reader.width, 80);
    assert_eq!(reader.height, 20);
    assert_eq!(reader.content_width(), 48);
    assert!(reader.current_line() < reader.total_lines());

    reader.handle_event(char_key(']'));
    reader.handle_event(char_key('f'));
    assert_eq!(reader.current_figure_kitty_id(), Some(2));
  }

  #[test]
  fn acceptance_search_toc_and_command_mode_keep_navigation_sane() {
    let mut reader = acceptance_reader();

    reader.handle_event(char_key('/'));
    send_chars(&mut reader, "method");
    assert!(matches!(reader.mode(), Mode::Search));
    assert!(!reader.search_matches().is_empty());
    reader.handle_event(key(KeyCode::Enter));
    assert!(matches!(reader.mode(), Mode::Normal));
    assert!(
      reader.visual_lines()[reader.current_line()]
        .text
        .contains("method")
    );

    reader.handle_event(char_key('\\'));
    assert!(reader.toc_visible);
    assert_eq!(reader.content_width(), 71);

    reader.handle_event(char_key(':'));
    send_chars(&mut reader, "2");
    let action = reader.handle_event(key(KeyCode::Enter));
    assert!(matches!(action, ReaderAction::Continue));
    assert!(matches!(reader.mode(), Mode::Normal));
    assert_eq!(reader.current_line(), 1);
  }

  #[test]
  fn acceptance_marks_highlights_and_voice_keys_round_trip() {
    let mut reader = acceptance_reader();

    reader.handle_event(char_key('j'));
    let marked_line = reader.current_line();
    reader.handle_event(char_key('m'));
    reader.handle_event(char_key('a'));
    reader.handle_event(char_key('j'));
    assert_ne!(reader.current_line(), marked_line);
    reader.handle_event(char_key('\''));
    reader.handle_event(char_key('a'));
    assert_eq!(reader.current_line(), marked_line);

    reader.handle_event(char_key('V'));
    reader.handle_event(char_key('j'));
    reader.handle_event(char_key('H'));
    assert!(matches!(reader.mode(), Mode::Normal));
    assert!(
      !reader.highlights.highlights.is_empty(),
      "visual highlight should create persistent block-byte highlights"
    );
    let highlight_count = reader.highlights.highlights.len();

    reader.cursor_set_for_test(marked_line.saturating_sub(reader.offset()), 0);
    reader.handle_event(char_key('X'));
    assert!(
      reader.highlights.highlights.len() < highlight_count,
      "X should remove the highlight under the cursor"
    );

    reader.handle_event(char_key('r'));
    assert!(reader.reading_mode());
    assert!(!reader.tick(), "no controller means idle ticks are invisible");
    reader.handle_event(ctrl('p'));
    assert!(reader.continuous_reading());
    reader.handle_event(key(KeyCode::Esc));
    assert!(!reader.reading_mode());
    assert!(!reader.continuous_reading());
  }
}
