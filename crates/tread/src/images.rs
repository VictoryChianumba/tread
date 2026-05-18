//! Post-draw Kitty graphics emission for `Block::Image` rows.
//!
//! Ratatui's frame buffer is character-cell only — it has no concept of
//! pixel data and would mangle any escape sequence we tried to embed in
//! a `Span` (the cell-width accounting would corrupt).  So images live
//! outside the buffer: ratatui paints blank rows where the image goes,
//! and *after* `terminal.draw()` returns we walk the visible window for
//! Image VLs and emit Kitty `a=p` (place) escapes directly to stdout.
//!
//! The same pattern is used elsewhere in this crate for OSC 52 yank.
//!
//! ## Lifecycle
//!
//! - **First time visible**: read the PNG bytes from disk (converting
//!   PDFs via `pdftoppm` if necessary), `a=t` transmit them once, mark
//!   the kitty_id as cached for the rest of the session.
//! - **Each frame**: emit `a=d` (delete) for every cached id followed
//!   by `a=p` (place) at its current screen row.  We delete before
//!   placing because Kitty's `a=p` *adds* a placement instead of
//!   replacing — without the delete, scrolling stacks ghost images
//!   from previous offsets.
//! - **Scrolled out**: emit `a=d` for any id that was visible last
//!   frame but isn't this frame.
//!
//! ## Module layout
//!
//! - [`inline`] — main-pane placement (`place_visible`).
//! - [`preview`] — side-pane placement for one selected figure
//!   (`place_one_figure`).
//! - [`worker`] — background image-prep thread and the job/result
//!   pipeline that feeds both placement paths.
//! - [`png`] — source-path → PNG bytes, terminal-cap normalization,
//!   cross-session disk cache.
//!
//! `ImageState` and `BurstTracker` live here as the public seam.
//! `clear_all` lives here because it spans inline and preview.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

mod inline;
mod png;
mod preview;
mod worker;

pub use inline::place_visible;
pub use png::enforce_cache_cap;
pub use preview::place_one_figure;

pub(crate) use inline::clear_inline;
pub(crate) use preview::clear_preview;
pub(crate) use worker::{has_pending_jobs, poll_ready};

// `kitty_graphics::transmit_byte_cap()` returns the raw-PNG ceiling for
// the active terminal: the conservative 300 KB used to be a const here,
// but the cap is really an iTerm2 single-APC constraint — native Kitty
// (when not inside tmux) tolerates much larger payloads, so we let the
// detection layer decide.  See that function's docs for the full
// rationale and override behaviour.

/// Retry interval for image loads that failed at first sight.
/// See `ImageState::negative_loads`.
const NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(30);

/// Frame-to-frame image bookkeeping for the post-draw injector.
///
/// We cache PNG bytes in memory the first time an image is requested
/// so fast scrolling doesn't re-read 150 KB of disk per keystroke per
/// figure.  We don't cache *terminal-side* (the `a=T` protocol variant
/// re-transmits each frame) because iTerm2's Kitty implementation
/// doesn't keep an image store between frames — it only renders.
///
/// `last_emitted` is the lazy-transmission cache: when the next frame
/// would place an image at the same `(row, col, cols, rows)` as last
/// frame, the terminal already has it on screen and we skip the entire
/// delete+transmit cycle.  Saves ~210 KB of base64 per visible figure
/// per idle frame (mouse motion, focus events, key repeats that don't
/// change scroll position).
#[derive(Default)]
pub struct ImageState {
    /// Decoded PNG bytes by kitty_id.  Loaded lazily on first visibility.
    /// `None` marks an image we tried to load and failed (missing file,
    /// bad format) — caching the negative result avoids retrying every
    /// frame.
    pub(crate) bytes: HashMap<u32, Option<Vec<u8>>>,
    /// Kitty ids that had visible placements last frame.  Diffed against
    /// `current` each frame to find which ones just scrolled out.
    pub(crate) prev_visible: HashSet<u32>,
    /// Last `(abs_row, abs_col, cols, rows)` we emitted for each id.
    /// Compared against the current frame's intended placement; equal
    /// means "already on screen, don't re-emit."
    pub(crate) last_emitted: HashMap<u32, (u16, u16, u16, u16)>,
    /// Wall-clock timestamps for ids that failed to load.  Used to
    /// expire negative entries in `bytes` after `NEGATIVE_CACHE_TTL` so
    /// a file that becomes readable later (e.g. a still-downloading
    /// asset finishes, or the user fixes a permissions problem)
    /// recovers without restarting the reader — but not so often that
    /// rapid scroll keeps re-spawning `pdftoppm` for a genuinely
    /// missing figure.
    pub(crate) negative_loads: HashMap<u32, Instant>,
    /// Ids whose image bytes the terminal currently has cached.  Set on
    /// first successful `a=T` transmission; consulted by the scroll-time
    /// re-place path to decide whether we can use the cheap `a=p`
    /// (placement-only, ~50 bytes) or have to re-transmit the full `a=T`
    /// payload (~400 KB base64 per image).  Survives `clear_all`:
    /// `delete_placement` sends `a=d,d=i` which clears placements only
    /// and leaves stored image data in the terminal, so re-placing after
    /// a resize / focus-loss can still take the cheap `a=p` path.  Only
    /// ever populated when the host terminal is known to persist image
    /// data between frames; on iTerm2 (no persistent cache) this stays
    /// empty and every placement goes through the full-retransmit path.
    pub(crate) transmitted_ids: HashSet<u32>,
    /// Kitty ids currently shown in the preview pane (see
    /// `place_one_figure`).  Multi-part figures own multiple ids — one
    /// per `FigurePart` — so the preview tiler can place all subfigures
    /// of a single logical figure (Fig 3's N×M grid, subfigure rows,
    /// etc.) into one area.  Empty when no preview is active.  Distinct
    /// from `prev_visible`, which is only for inline images placed by
    /// `place_visible`.
    pub(crate) preview_ids: HashSet<u32>,
    /// Background image preparation worker.  Placement schedules jobs
    /// here and keeps the previous frame stable until bytes arrive.
    pub(crate) worker: Option<worker::ImageWorker>,
    /// Kitty ids currently queued or running on the image worker.
    pub(crate) pending_jobs: HashSet<u32>,
}

/// Input-pacing signal for the image burst-skip gate.
///
/// `after_draw_guarded` consults `in_burst()` to decide whether to skip
/// emission for the current frame.  Hosts call `note_event()` whenever
/// a navigation key dispatches to the focused reader — that timestamp
/// is the only signal `in_burst()` reads, so the tracker works in any
/// event-loop shape (poll-based, channel-based, hybrid).  The previous
/// `event::poll(Duration::ZERO)` heuristic only worked in loops that
/// hadn't pre-drained their event queue; this one doesn't care.
///
/// `burst_window` defaults to 100 ms — long enough to cover macOS
/// key-repeat (≈33 ms) with a few frames of headroom, short enough
/// that the gate releases promptly when the user stops scrolling.
pub struct BurstTracker {
    last_event_at: Option<Instant>,
    burst_window: Duration,
}

impl Default for BurstTracker {
    fn default() -> Self {
        Self {
            last_event_at: None,
            burst_window: Duration::from_millis(100),
        }
    }
}

impl BurstTracker {
    pub fn note_event(&mut self) {
        self.last_event_at = Some(Instant::now());
    }

    pub fn in_burst(&self) -> bool {
        self.last_event_at
            .map(|t| t.elapsed() < self.burst_window)
            .unwrap_or(false)
    }
}

/// Clear all on-screen placements.  Called on resize and on exit so
/// stale image artefacts don't bleed onto the next frame or back into
/// the user's shell after the alt-screen tears down.  Also resets
/// `last_emitted` since the cached coordinates are about to become
/// invalid (resize re-flows visual_lines; exit closes the alt screen).
///
/// `transmitted_ids` is deliberately preserved: `delete_placement`
/// uses `a=d,d=i` which removes placements without freeing the stored
/// image data, so the next emit after a resize / focus-loss can still
/// take the cheap `a=p` path on native Kitty instead of a full `a=T`
/// retransmit.
pub fn clear_all(state: &mut ImageState) {
    clear_inline(state);
    clear_preview(state);
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use doc_model::Block;
    use image::{DynamicImage, Rgb, RgbImage};
    use ratatui::layout::Rect;

    use super::inline::{clear_inline_inner, place_visible};
    use super::png::{encode_dynamic_image_png, normalize_png_for_terminal_with_limit,
                     normalized_cache_path};
    use super::preview::{clear_preview_inner, place_one_figure};
    use super::worker::{ImageJob, ImageLoadContext, ensure_image_bytes, poll_ready,
                        schedule_image_job};
    use super::ImageState;

    fn noisy_png(width: u32, height: u32) -> Vec<u8> {
        let img = RgbImage::from_fn(width, height, |x, y| {
            let x = x as u8;
            let y = y as u8;
            Rgb([
                x.wrapping_mul(31).wrapping_add(y.wrapping_mul(17)),
                x.wrapping_mul(13).wrapping_add(y.wrapping_mul(29)),
                x ^ y.wrapping_mul(47),
            ])
        });
        encode_dynamic_image_png(Path::new("synthetic.png"), &DynamicImage::ImageRgb8(img)).unwrap()
    }

    #[test]
    fn leaves_small_png_unchanged() {
        let png = noisy_png(32, 32);
        let out = normalize_png_for_terminal_with_limit(
            Path::new("small.png"),
            png.clone(),
            png.len() + 1,
        )
        .unwrap();
        assert_eq!(out, png);
    }

    #[test]
    fn image_job_result_populates_byte_cache() {
        let temp = std::env::temp_dir().join(format!(
            "tread-image-job-{}-{}.png",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let png = noisy_png(16, 16);
        std::fs::write(&temp, &png).unwrap();

        let mut state = ImageState::default();
        ensure_image_bytes(
            &mut state,
            ImageJob::resolve_png(7, temp.clone()),
            false,
            ImageLoadContext::Inline,
        );

        let _ = std::fs::remove_file(&temp);
        assert_eq!(
            state.bytes.get(&7).and_then(|value| value.as_ref()),
            Some(&png)
        );
        assert!(!state.negative_loads.contains_key(&7));
    }

    #[test]
    fn image_job_result_caches_failure_as_negative_bytes() {
        let mut state = ImageState::default();
        ensure_image_bytes(
            &mut state,
            ImageJob::resolve_png(8, PathBuf::from("/definitely/not/a/figure.png")),
            false,
            ImageLoadContext::Preview,
        );

        assert!(matches!(state.bytes.get(&8), Some(None)));
    }

    #[test]
    fn scheduled_image_job_completes_through_worker_poll() {
        let temp = std::env::temp_dir().join(format!(
            "tread-image-worker-{}-{}.png",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let png = noisy_png(16, 16);
        std::fs::write(&temp, &png).unwrap();

        let mut state = ImageState::default();
        schedule_image_job(
            &mut state,
            ImageJob::resolve_png(9, temp.clone()),
            false,
            ImageLoadContext::Inline,
        );
        assert!(state.pending_jobs.contains(&9));

        let mut changed = false;
        for _ in 0..50 {
            if poll_ready(&mut state) {
                changed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let _ = std::fs::remove_file(&temp);
        assert!(changed, "worker should return the prepared image");
        assert_eq!(
            state.bytes.get(&9).and_then(|value| value.as_ref()),
            Some(&png)
        );
        assert!(!state.pending_jobs.contains(&9));
    }

    #[test]
    fn downscales_oversized_png_to_budget() {
        let png = noisy_png(512, 512);
        assert!(
            png.len() > 20_000,
            "synthetic png should exceed test budget"
        );

        let out =
            normalize_png_for_terminal_with_limit(Path::new("large.png"), png, 20_000).unwrap();
        assert!(
            out.len() <= 20_000,
            "normalized png should respect byte budget"
        );

        let decoded = image::load_from_memory_with_format(&out, image::ImageFormat::Png).unwrap();
        assert!(decoded.width() < 512 || decoded.height() < 512);
    }

    #[test]
    fn place_one_figure_noop_when_unsupported() {
        use crate::state::Reader;
        let reader = Reader::new(vec![], 80, 24);
        let mut state = ImageState::default();
        place_one_figure(&reader, &mut state, Some(1), Rect::new(0, 0, 10, 10), false);
        assert!(state.preview_ids.is_empty());
        assert!(state.bytes.is_empty());
    }

    #[test]
    fn place_one_figure_unknown_id_is_silent() {
        use crate::state::Reader;
        let reader = Reader::new(vec![], 80, 24);
        let mut state = ImageState::default();
        place_one_figure(&reader, &mut state, Some(99), Rect::new(0, 0, 10, 10), true);
        assert!(state.preview_ids.is_empty());
        assert!(state.last_emitted.is_empty());
    }

    #[test]
    fn normalized_cache_key_changes_when_source_metadata_changes() {
        let temp = std::env::temp_dir().join(format!(
            "tread-norm-cache-{}-{}.png",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::write(&temp, b"one").unwrap();
        let first = normalized_cache_path(&temp, 1234).unwrap();
        std::fs::write(&temp, b"two two").unwrap();
        let second = normalized_cache_path(&temp, 1234).unwrap();
        let _ = std::fs::remove_file(&temp);
        assert_ne!(first, second);
    }

    // Suppression contract: when an image id is owned by the preview pane,
    // `place_visible` must not push it into `placements`.  Otherwise both
    // paths would emit `a=p,i=<id>` against the same implicit p=0 slot and
    // each emit would steal the other's on-screen placement.  We detect
    // skipping by observing that the lazy byte-load (inside the placements
    // loop) never ran for the suppressed id.
    #[test]
    fn place_visible_skips_inline_emit_for_preview_id() {
        use crate::state::Reader;
        let blocks = vec![
            Block::Figure {
                rows: vec![vec![doc_model::ImageItem {
                    path: std::path::PathBuf::from("/nonexistent/a.png"),
                    kitty_id: 1,
                    dims: Some((40, 20)),
                }]],
                alt: String::new(),
                figure_id: 1,
                column_gaps_after: Vec::new(),
                header_rows: Vec::new(),
            },
            Block::Figure {
                rows: vec![vec![doc_model::ImageItem {
                    path: std::path::PathBuf::from("/nonexistent/b.png"),
                    kitty_id: 2,
                    dims: Some((40, 20)),
                }]],
                alt: String::new(),
                figure_id: 2,
                column_gaps_after: Vec::new(),
                header_rows: Vec::new(),
            },
        ];
        let reader = Reader::new(blocks, 200, 80);
        let mut state = ImageState::default();
        state.preview_ids.insert(1);
        place_visible(&reader, &mut state, Rect::new(0, 0, 200, 80), true);
        assert!(
            !state.pending_jobs.contains(&1) && !state.bytes.contains_key(&1),
            "preview-owned id must not enter the inline image-prep path",
        );
        assert!(
            state.pending_jobs.contains(&2),
            "non-previewed id must still be scheduled by the inline path",
        );
    }

    // The dropped-diff filter: when the previewed image scrolls out of the
    // inline viewport, the cleanup logic must NOT call `delete_placement`
    // on it — that would wipe the preview placement too (Kitty's
    // `a=d,d=i` deletes all placements of an image id).  We detect a
    // would-be deletion by seeding `last_emitted` with a sentinel and
    // asserting it survives the call.
    #[test]
    fn place_visible_does_not_delete_preview_id_when_image_scrolls_off() {
        use crate::state::Reader;
        let reader = Reader::new(vec![], 80, 24);
        let mut state = ImageState::default();
        state.preview_ids.insert(7);
        state.prev_visible.insert(7);
        state.last_emitted.insert(7, (5, 10, 50, 30));
        place_visible(&reader, &mut state, Rect::new(0, 0, 80, 24), true);
        assert!(
            state.last_emitted.contains_key(&7),
            "preview id must survive the dropped-diff cleanup",
        );
        assert!(state.preview_ids.contains(&7));
    }

    #[test]
    fn clear_inline_preserves_preview_placement_state() {
        let mut state = ImageState::default();
        state.preview_ids.insert(7);
        state.prev_visible.insert(3);
        state.last_emitted.insert(3, (1, 2, 3, 4));
        state.last_emitted.insert(7, (5, 6, 7, 8));

        clear_inline_inner(&mut state, false);

        assert!(state.preview_ids.contains(&7));
        assert!(!state.prev_visible.contains(&3));
        assert!(!state.last_emitted.contains_key(&3));
        assert_eq!(state.last_emitted.get(&7), Some(&(5, 6, 7, 8)));
    }

    #[test]
    fn clear_preview_preserves_inline_placement_state() {
        let mut state = ImageState::default();
        state.preview_ids.insert(7);
        state.prev_visible.insert(3);
        state.last_emitted.insert(3, (1, 2, 3, 4));
        state.last_emitted.insert(7, (5, 6, 7, 8));

        clear_preview_inner(&mut state, false);

        assert!(state.preview_ids.is_empty());
        assert!(state.prev_visible.contains(&3));
        assert_eq!(state.last_emitted.get(&3), Some(&(1, 2, 3, 4)));
        assert!(!state.last_emitted.contains_key(&7));
    }
}
