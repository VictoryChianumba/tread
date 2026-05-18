//! Voice / TTS key handling and bookkeeping for the reader.
//!
//! Free functions operating on `&Reader` / `&mut Reader`.  Imports the
//! `voice/` submodule's `PlaybackController` for actual audio work and
//! manages the per-frame syncing of its background-thread state into
//! Reader fields used by the renderer (line dimming, word highlight).
//!
//! Ported from `cli-text-reader/src/editor/voice_control.rs`.  Adapted
//! for tread's visual-line architecture: source-line indices are
//! replaced with visual-line indices throughout.  The empirical 13
//! chars/sec speech rate is unchanged.

use std::sync::Arc;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::Reader;
use crate::text_objects::{cursor_to_paragraph_end, paragraph_with_range};
use crate::voice::{PlaybackController, PlaybackStatus};

// ── Seam surface (ADR-0004 Seam 3) ─────────────────────────────────────
//
// Voice state lives in ten fields on `Reader` that move in lockstep.
// External callers (`lib.rs::tick`, `render.rs`) reach in through the
// getters below; the only write outside `state/` is the `tick()`
// continuous-reading shutoff, routed through `stop_continuous_reading`.
// Multi-field writes (start a chunk, exit a session, sync from the
// controller) stay as free functions inside this module because they're
// only ever called from this module — pushing them through the impl
// surface would force a borrow-split through methods just to satisfy
// the seam.
impl Reader {
    /// Whether voice is currently rendering effects (line dimming,
    /// active-word highlight).  Polled by the host event loop's idle-
    /// tick scheduler to decide whether to redraw on a wall-clock tick.
    pub fn voice_status(&self) -> &PlaybackStatus {
        &self.voice_status
    }

    /// Shared playback controller.  `None` when audio init failed at
    /// startup or the host disabled voice entirely.  Returned as an
    /// `Arc` clone so callers can hold a reference across `&mut self`
    /// operations on the Reader without fighting the borrow checker.
    pub fn voice_controller(&self) -> Option<Arc<PlaybackController>> {
        self.voice_controller.clone()
    }

    /// Wall-clock instant when the current audio chunk started, or
    /// `None` when nothing is playing.  Drives the moving "active word"
    /// highlight by combining with a fixed chars-per-second rate.
    pub fn voice_started_at(&self) -> Option<Instant> {
        self.voice_started_at
    }

    /// Cumulative character count from chunks that completed BEFORE the
    /// current one.  Word-position math adds this to elapsed time × 13
    /// chars/sec to estimate the active word.
    pub fn voice_chars_before(&self) -> usize {
        self.voice_chars_before
    }

    /// First / last visual-line index of the paragraph currently being
    /// read.  Used for paragraph dimming and word-position bookkeeping.
    pub fn voice_para_range(&self) -> (usize, usize) {
        (self.voice_para_start, self.voice_para_end)
    }

    /// True while the user is in voice mode (`r` / `R` / `Ctrl+P`
    /// started a playback session).  Gates `Space`/`c`/`Esc` voice key
    /// handling so navigation keeps working alongside playback.
    pub fn reading_mode(&self) -> bool {
        self.reading_mode
    }

    /// True iff continuous reading is active — on chunk-end, the
    /// reader auto-advances to the next paragraph and starts playing.
    pub fn continuous_reading(&self) -> bool {
        self.continuous_reading
    }

    /// Stop continuous auto-advance, leaving the current chunk's
    /// playback state alone.  Used when `advance_to_next_paragraph_for_continuous_reading`
    /// runs out of document — we want to keep reading_mode on so the
    /// status line keeps showing "READING" until the final chunk
    /// finishes, but no further paragraphs should auto-start.
    pub fn stop_continuous_reading(&mut self) {
        self.continuous_reading = false;
    }
}

/// Voice key handler.  Called BEFORE other key handlers in normal
/// mode so reading-mode `Esc` takes precedence over reader-quit.
/// Returns `true` when the keystroke was consumed by voice.
pub fn handle_voice_keys(reader: &mut Reader, key: KeyEvent) -> bool {
    match key.code {
        // r — enter reading mode (if not already), or re-read current paragraph
        KeyCode::Char('r') => {
            if !reader.reading_mode {
                reader.reading_mode = true;
            } else {
                if let Some(vc) = &reader.voice_controller {
                    if !matches!(reader.voice_status, PlaybackStatus::Idle) {
                        vc.stop();
                    }
                }
                reader.continuous_reading = false;
                if let Some((text, start, end)) = paragraph_with_range(reader, false) {
                    if !text.trim().is_empty() {
                        voice_start(reader, text, start, end);
                    }
                }
            }
            true
        }

        // R — silently enter reading mode and read cursor → end of current paragraph
        KeyCode::Char('R') => {
            reader.reading_mode = true;
            reader.continuous_reading = false;
            if let Some(vc) = &reader.voice_controller {
                if !matches!(reader.voice_status, PlaybackStatus::Idle) {
                    vc.stop();
                }
            }
            if let Some((text, start, end)) = cursor_to_paragraph_end(reader) {
                if !text.trim().is_empty() {
                    voice_start(reader, text, start, end);
                }
            }
            true
        }

        // Ctrl+P — start continuous reading from cursor to end of document
        KeyCode::Char('p')
            if reader.reading_mode && key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            reader.continuous_reading = true;
            if let Some(vc) = &reader.voice_controller {
                if !matches!(reader.voice_status, PlaybackStatus::Idle) {
                    vc.stop();
                }
            }
            if let Some((text, start, end)) = paragraph_with_range(reader, false) {
                if !text.trim().is_empty() {
                    voice_start(reader, text, start, end);
                }
            }
            true
        }

        // Space — pause / resume (only in reading mode)
        KeyCode::Char(' ') if reader.reading_mode => {
            sync_voice_status(reader);
            match reader.voice_status {
                PlaybackStatus::Playing => {
                    if let Some(vc) = &reader.voice_controller {
                        vc.pause();
                    }
                }
                PlaybackStatus::Paused => {
                    if let Some(vc) = &reader.voice_controller {
                        vc.resume();
                    }
                }
                PlaybackStatus::Loading | PlaybackStatus::Idle => {}
            }
            true
        }

        // c — re-centre viewport on cursor (no playback effect)
        KeyCode::Char('c') if reader.reading_mode => {
            reader.center_cursor();
            true
        }

        // Esc — stop playback and exit reading mode entirely
        KeyCode::Esc if reader.reading_mode => {
            if let Some(vc) = &reader.voice_controller {
                vc.stop();
                reader.voice_started_at = None;
            }
            reader.voice_started_session = None;
            reader.reading_mode = false;
            reader.continuous_reading = false;
            true
        }

        _ => false,
    }
}

/// Continuous reading: walk to the next non-blank visual line below
/// the just-finished paragraph and start playback there.  Returns
/// false at end-of-document so the caller knows to stop.
pub fn advance_to_next_paragraph_for_continuous_reading(reader: &mut Reader) -> bool {
    let total = reader.total_lines();
    let mut next = reader.voice_para_end + 1;
    while next < total
        && reader
            .visual_lines
            .get(next)
            .map(|vl| vl.text.trim().is_empty())
            .unwrap_or(true)
    {
        next += 1;
    }
    if next >= total {
        return false;
    }

    // Move cursor to the new paragraph (centred in viewport).
    reader.center_on_line(next);

    let Some((text, start, end)) = paragraph_with_range(reader, false) else {
        return false;
    };
    if text.trim().is_empty() {
        return false;
    }
    voice_start(reader, text, start, end);
    true
}

/// True iff voice is actually rendering effects right now (paragraph
/// dimming + word highlight).  Returns false when status is not
/// Playing OR the cursor has navigated outside the playback range.
pub fn voice_rendering_active(reader: &Reader) -> bool {
    if !matches!(reader.voice_status, PlaybackStatus::Playing) {
        return false;
    }
    let cursor_line = reader.offset() + reader.cursor_y();
    let detached = reader.reading_mode
        && (cursor_line < reader.voice_para_start || cursor_line > reader.voice_para_end);
    !detached
}

/// Best-guess (line, byte_start, byte_end) of the word being spoken
/// right now, based on elapsed-time × empirical 13 chars/sec.  Returns
/// `None` when no playback is active or the cursor is detached.
pub fn active_voice_word(reader: &Reader) -> Option<(usize, usize, usize)> {
    if !voice_rendering_active(reader) {
        return None;
    }
    let estimated_char_offset = if let Some(started) = reader.voice_started_at {
        let elapsed_chars = (started.elapsed().as_secs_f32() * 13.0) as usize;
        reader.voice_chars_before.saturating_add(elapsed_chars)
    } else {
        0
    };
    let total = reader.total_lines();
    let paragraph_end = reader.voice_para_end.min(total.saturating_sub(1));
    let mut char_pos = 0usize;
    for vl_idx in reader.voice_para_start..=paragraph_end {
        let line = &reader.visual_lines[vl_idx].text;
        let line_end = char_pos + line.len();
        if estimated_char_offset <= line_end {
            let column = estimated_char_offset
                .saturating_sub(char_pos)
                .min(line.len());
            let (word_start, word_end) = find_word_at(line, column);
            return Some((vl_idx, word_start, word_end));
        }
        char_pos = line_end + 1; // +1 for the `\n` separator
    }
    None
}

/// Status-bar label for the current playback state, or `None` if
/// nothing is happening worth showing.  The Loading variant returns a
/// rotating Braille-spinner glyph driven by wall-clock so the badge
/// animates while the network round-trip happens.
pub fn voice_status_label(reader: &Reader) -> Option<String> {
    if let Some(err) = &reader.voice_error {
        return Some(format!("[Voice: {err}]"));
    }
    match reader.voice_status {
        PlaybackStatus::Loading => {
            use std::time::{SystemTime, UNIX_EPOCH};
            const FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let frame = FRAMES[(ms / 100) as usize % FRAMES.len()];
            Some(format!("[{frame} Loading]"))
        }
        PlaybackStatus::Playing => Some("[♪ Playing]".to_string()),
        PlaybackStatus::Paused => Some("[⏸ Paused]".to_string()),
        PlaybackStatus::Idle => None,
    }
}

/// Should the renderer render this VL with the dim foreground?
/// True when playback is active AND this line is OUTSIDE the
/// currently-spoken paragraph range.
pub fn voice_line_dimmed(reader: &Reader, vl_idx: usize) -> bool {
    voice_rendering_active(reader)
        && (vl_idx < reader.voice_para_start || vl_idx > reader.voice_para_end)
}

/// Find the word boundaries in `s` containing byte position `col`.
/// Word chars: alphanumeric + apostrophes (ASCII and Unicode right
/// single quote ’).  Returns `(start, end)` byte offsets.
fn find_word_at(s: &str, col: usize) -> (usize, usize) {
    let col = col.min(s.len());
    let col = (0..=col)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0);
    let is_word = |c: char| c.is_alphanumeric() || c == '\'' || c == '\u{2019}';
    let start = s[..col]
        .rfind(|c: char| !is_word(c))
        .map(|i| i + s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1))
        .unwrap_or(0);
    let end = s[col..]
        .find(|c: char| !is_word(c))
        .map(|i| col + i)
        .unwrap_or(s.len());
    if start >= end {
        let next = ((col + 1)..=s.len())
            .find(|&i| s.is_char_boundary(i))
            .unwrap_or(s.len());
        (col, next)
    } else {
        (start, end)
    }
}

/// Per-tick sync of background-thread state into Reader fields.
/// Pulls status, error, and per-chunk playing info from the
/// `PlaybackController`'s shared `Arc<Mutex>` state.  Loading→Idle is
/// suppressed so a quick network round-trip doesn't flicker the
/// status bar through Idle before reaching Playing.
///
/// Cross-tab preemption: when this Reader requested playback it
/// stamped `voice_started_session` from the controller's session
/// counter.  If the controller is now playing a *different* session
/// (because another Reader, in another tab, called `start()` after
/// us), the controller's `session_id()` no longer matches ours —
/// silently exit reading mode.  Status / dim / word-highlight all
/// drop to inactive without surfacing an error.
pub fn sync_voice_status(reader: &mut Reader) {
    let Some(vc) = &reader.voice_controller else {
        return;
    };

    // Preemption check: a Reader that started playback owns the
    // controller until either it ends naturally (session→None) or
    // another Reader bumps the session.  If we held a session and the
    // controller no longer reflects it, fold our reading mode silently.
    if let Some(my_session) = reader.voice_started_session {
        let current = vc.session_id();
        if !matches!(current, Some(id) if id == my_session) && current.is_some() {
            // A different Reader is now playing.  Exit reading mode with
            // no error — this is expected behaviour, not a failure.
            reader.reading_mode = false;
            reader.continuous_reading = false;
            reader.voice_status = PlaybackStatus::Idle;
            reader.voice_started_at = None;
            reader.voice_started_session = None;
            return;
        }
        // current == None means natural end of playback (could be ours
        // ending, in which case the rest of this function handles the
        // Playing→Idle transition).  current == Some(my_session) means
        // we're still the active speaker.  Both fall through.
    }

    let controller_status = vc.status();
    let should_update = !matches!(
        (&reader.voice_status, &controller_status),
        (PlaybackStatus::Loading, PlaybackStatus::Idle)
    );
    if should_update {
        reader.voice_status = controller_status;
    }
    if let Some(err) = vc.take_error() {
        reader.voice_error = Some(err);
        reader.voice_status = PlaybackStatus::Idle;
        reader.voice_started_at = None;
    }
    if let Ok(info_guard) = vc.playing_info.lock() {
        if let Some(info) = info_guard.as_ref() {
            reader.voice_para_start = info.doc_start_line;
            reader.voice_para_end = info.doc_end_line;
            reader.voice_started_at = Some(info.started_at);
            reader.voice_chars_before = info.chars_before_chunk;
        }
    }
    if matches!(
        reader.voice_status,
        PlaybackStatus::Idle | PlaybackStatus::Paused
    ) {
        reader.voice_started_at = None;
    }
}

/// Initiate playback of `text` covering the visual-line range
/// `[doc_start_line, doc_end_line]`.  No-op (with error message) when
/// the controller isn't initialised — typically because audio init
/// failed at startup.  Records the new session id on the Reader so
/// `sync_voice_status` can detect cross-tab preemption.
pub fn voice_start(reader: &mut Reader, text: String, doc_start_line: usize, doc_end_line: usize) {
    if let Some(vc) = &reader.voice_controller {
        reader.voice_status = PlaybackStatus::Loading;
        reader.voice_error = None;
        reader.voice_para_start = doc_start_line;
        reader.voice_para_end = doc_end_line;
        reader.voice_started_at = None;
        reader.voice_chars_before = 0;
        let session_id = vc.start(text, doc_start_line, doc_end_line);
        reader.voice_started_session = Some(session_id);
    } else {
        reader.voice_error = Some("voice not initialised".to_string());
    }
}
