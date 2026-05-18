//! Owning event-loop / dirty-tracking runtime that drives the
//! standalone-tread binary.  Embedding hosts (trench) don't use this —
//! they own their own crossterm loop and call `Reader::handle_event`
//! and `tread::after_draw` directly.  Extracted from `lib.rs` so the
//! crate root only carries the public-surface API; the loop wiring,
//! dirty-region tracking, and idle-poll cadence live here.

use crate::commands::ReaderAction;
use crate::images::{self, ImageState};
use crate::state::Reader;
use crate::voice;
use crate::{bench, render};
use crossterm::event::{self, Event};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use std::io;
use std::time::Duration;
use ui_theme::Theme;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReaderEvent {
  Input,
  Resize,
  Tick,
  ReloadComplete,
  ImageReady,
  ConfigChange,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DirtyState {
  pub content: bool,
  pub status: bool,
  pub layout: bool,
  pub images: bool,
  pub voice: bool,
}

impl DirtyState {
  pub fn all() -> Self {
    Self {
      content: true,
      status: true,
      layout: true,
      images: true,
      voice: true,
    }
  }

  pub fn any(self) -> bool {
    self.content || self.status || self.layout || self.images || self.voice
  }

  pub fn clear_after_draw(&mut self) {
    *self = Self::default();
  }

  pub fn mark(&mut self, event: ReaderEvent) {
    match event {
      ReaderEvent::Input => {
        self.content = true;
        self.status = true;
      }
      ReaderEvent::Resize => {
        self.content = true;
        self.status = true;
        self.layout = true;
        self.images = true;
      }
      ReaderEvent::Tick => {
        self.status = true;
        self.voice = true;
      }
      ReaderEvent::ReloadComplete => {
        self.content = true;
        self.status = true;
        self.layout = true;
        self.images = true;
      }
      ReaderEvent::ImageReady => {
        self.images = true;
      }
      ReaderEvent::ConfigChange => {
        self.content = true;
        self.status = true;
      }
    }
  }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ReaderUpdate {
  pub dirty: DirtyState,
  pub quit: bool,
}

impl ReaderUpdate {
  pub fn quit() -> Self {
    Self {
      quit: true,
      dirty: DirtyState::default(),
    }
  }

  pub fn from_event(event: ReaderEvent) -> Self {
    let mut dirty = DirtyState::default();
    dirty.mark(event);
    Self { dirty, quit: false }
  }
}

pub(crate) struct ReaderRuntime {
  reader: Reader,
  img_state: ImageState,
  theme: Theme,
  kitty_supported: bool,
  dirty: DirtyState,
  first_draw_done: bool,
  pending_event_start: Option<std::time::Instant>,
  // Wall-clock of the most recent user-driven event (key / mouse /
  // resize / focus).  Drives the idle-poll cadence in `poll_timeout`:
  // if more than IDLE_THRESHOLD has passed without input, we drop the
  // poll timeout from 16ms (60 wakeups/s, ~zero-latency redraw) to
  // 250ms (4 wakeups/s) so a paper open in the background doesn't
  // burn battery.  The next keypress pays at most one idle-poll cycle
  // of input latency — acceptable cost for an order-of-magnitude
  // wakeup reduction during long reading sessions.
  last_input_at: Option<std::time::Instant>,
}

impl ReaderRuntime {
  pub fn new(reader: Reader, theme: Theme, kitty_supported: bool) -> Self {
    Self {
      reader,
      img_state: ImageState::default(),
      theme,
      kitty_supported,
      dirty: DirtyState::all(),
      first_draw_done: false,
      pending_event_start: None,
      last_input_at: None,
    }
  }

  pub fn run(
    mut self,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
  ) -> (Reader, Result<(), Box<dyn std::error::Error>>) {
    let result = self.run_inner(terminal);
    // Clear any lingering image placements so they don't bleed onto the
    // user's shell after we leave the alt screen.
    images::clear_all(&mut self.img_state);
    (self.reader, result)
  }

  fn run_inner(
    &mut self,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
  ) -> Result<(), Box<dyn std::error::Error>> {
    // TREAD_BENCH_AUTOQUIT=<secs>: clean exit after N seconds so
    // cold/warm benchmark runs terminate deterministically without
    // manual `:q`.
    let autoquit_deadline = std::env::var("TREAD_BENCH_AUTOQUIT")
      .ok()
      .and_then(|s| s.parse::<f64>().ok())
      .map(|secs| std::time::Instant::now() + Duration::from_secs_f64(secs));

    // TREAD_BENCH_JCOUNT=<n>: after the first frame, inject N
    // synthetic `j` keypresses one per loop iteration.  Bypasses
    // crossterm so we don't need a PTY — measures pure scroll-loop
    // cost (handle_event → apply_update → draw_if_dirty).  Combine
    // with TREAD_BENCH=<path> to capture frame + event_to_frame
    // distributions under sustained input.
    let mut remaining_synthetic_j: u32 = std::env::var("TREAD_BENCH_JCOUNT")
      .ok()
      .and_then(|s| s.parse().ok())
      .unwrap_or(0);

    loop {
      self.draw_if_dirty(terminal)?;

      if let Some(deadline) = autoquit_deadline
        && std::time::Instant::now() >= deadline
      {
        bench::emit_us("autoquit", 0);
        break;
      }

      if self.first_draw_done && remaining_synthetic_j > 0 {
        remaining_synthetic_j -= 1;
        let ev = Event::Key(crossterm::event::KeyEvent::new(
          crossterm::event::KeyCode::Char('j'),
          crossterm::event::KeyModifiers::NONE,
        ));
        self.pending_event_start = Some(std::time::Instant::now());
        let update = self.handle_event(ev);
        self.apply_update(update);
        if update.quit {
          break;
        }
        continue;
      }

      if event::poll(self.poll_timeout())? {
        let ev = event::read()?;
        if matches!(
          &ev,
          Event::Key(_) | Event::Mouse(_) | Event::Resize(_, _)
        ) {
          self.last_input_at = Some(std::time::Instant::now());
        }
        if matches!(
          &ev,
          Event::Key(_) | Event::Resize(_, _) | Event::FocusLost | Event::FocusGained
        ) {
          self.pending_event_start = Some(std::time::Instant::now());
        }
        let update = self.handle_event(ev);
        self.apply_update(update);
        if update.quit {
          break;
        }
      } else if images::poll_ready(&mut self.img_state) {
        self.apply_update(ReaderUpdate::from_event(ReaderEvent::ImageReady));
      } else if self.reader.tick() {
        self.apply_update(ReaderUpdate::from_event(ReaderEvent::Tick));
      }
    }
    Ok(())
  }

  fn draw_if_dirty(
    &mut self,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
  ) -> Result<(), Box<dyn std::error::Error>> {
    if !self.dirty.any() {
      return Ok(());
    }
    let frame_start = std::time::Instant::now();
    let mut drawn_area = Rect::default();
    terminal.draw(|f| {
      drawn_area = f.area();
      render::draw(f, drawn_area, &self.reader, &self.theme);
    })?;

    if self.kitty_supported {
      let burst_in_progress =
        crate::burst_skip_enabled(self.kitty_supported) && event::poll(Duration::ZERO)?;
      crate::after_draw_guarded(
        &self.reader,
        &mut self.img_state,
        drawn_area,
        self.kitty_supported,
        burst_in_progress,
      );
    }
    self.dirty.clear_after_draw();

    let frame_us = frame_start.elapsed().as_micros().min(u32::MAX as u128) as u32;
    bench::record_frame(frame_us);
    if let Some(t0) = self.pending_event_start.take() {
      let lat = t0.elapsed().as_micros().min(u32::MAX as u128) as u32;
      bench::record_event_to_frame(lat);
    }
    if !self.first_draw_done {
      self.first_draw_done = true;
      bench::emit_us("startup_to_interactive", 0);
    }
    Ok(())
  }

  fn poll_timeout(&self) -> Duration {
    // After this much silence we assume the reader is parked and
    // drop poll cadence from 16ms to 250ms.  Under that, every key
    // still drives a redraw within one poll cycle of receipt; the
    // worst-case input latency on resume is the idle timeout itself,
    // ~250ms.
    const IDLE_THRESHOLD: Duration = Duration::from_secs(1);
    const IDLE_POLL: Duration = Duration::from_millis(250);
    const ACTIVE_POLL: Duration = Duration::from_millis(16);

    if self.dirty.any() {
      Duration::ZERO
    } else if images::has_pending_jobs(&self.img_state) {
      ACTIVE_POLL
    } else if matches!(
      self.reader.voice_status(),
      voice::PlaybackStatus::Playing | voice::PlaybackStatus::Loading
    ) {
      Duration::from_millis(33)
    } else if self
      .last_input_at
      .is_none_or(|t| t.elapsed() > IDLE_THRESHOLD)
    {
      IDLE_POLL
    } else {
      ACTIVE_POLL
    }
  }

  fn handle_event(&mut self, ev: Event) -> ReaderUpdate {
    let event_kind = match &ev {
      Event::Resize(_, _) => ReaderEvent::Resize,
      Event::Key(_) | Event::Mouse(_) | Event::Paste(_) => ReaderEvent::Input,
      _ => ReaderEvent::Input,
    };
    let needs_image_clear = matches!(&ev, Event::Resize(_, _) | Event::FocusLost);

    let action = self.reader.handle_event(ev);
    let mut update = match action {
      ReaderAction::Quit => ReaderUpdate::quit(),
      ReaderAction::ChangeTheme(t) => {
        self.theme = t;
        ReaderUpdate::from_event(ReaderEvent::ConfigChange)
      }
      ReaderAction::Reload => {
        images::clear_all(&mut self.img_state);
        ReaderUpdate::from_event(ReaderEvent::ReloadComplete)
      }
      ReaderAction::Error(msg) => {
        self.reader.cmd_error = Some(msg);
        ReaderUpdate::from_event(ReaderEvent::Input)
      }
      ReaderAction::OpenHelp => {
        self.reader.help_visible = true;
        ReaderUpdate::from_event(ReaderEvent::Input)
      }
      ReaderAction::Continue => ReaderUpdate::from_event(event_kind),
    };

    if needs_image_clear {
      images::clear_all(&mut self.img_state);
      update.dirty.mark(ReaderEvent::Resize);
    }
    update
  }

  fn apply_update(&mut self, update: ReaderUpdate) {
    self.dirty.content |= update.dirty.content;
    self.dirty.status |= update.dirty.status;
    self.dirty.layout |= update.dirty.layout;
    self.dirty.images |= update.dirty.images;
    self.dirty.voice |= update.dirty.voice;
  }
}

#[cfg(test)]
mod runtime_tests {
  use super::*;

  #[test]
  fn dirty_state_marks_redraw_for_visible_events() {
    let cases = [
      (ReaderEvent::Input, (true, true, false, false, false)),
      (ReaderEvent::Resize, (true, true, true, true, false)),
      (ReaderEvent::ReloadComplete, (true, true, true, true, false)),
      (ReaderEvent::ImageReady, (false, false, false, true, false)),
      (ReaderEvent::ConfigChange, (true, true, false, false, false)),
      (ReaderEvent::Tick, (false, true, false, false, true)),
    ];

    for (event, expected) in cases {
      let mut dirty = DirtyState::default();
      dirty.mark(event);
      assert_eq!(
        (
          dirty.content,
          dirty.status,
          dirty.layout,
          dirty.images,
          dirty.voice
        ),
        expected,
        "event {event:?}"
      );
    }
  }

  #[test]
  fn reader_update_quit_does_not_force_redraw() {
    let update = ReaderUpdate::quit();
    assert!(update.quit);
    assert!(!update.dirty.any());
  }
}
