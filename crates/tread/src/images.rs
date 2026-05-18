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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use doc_model::{VisualLineKind, compute_cell_footprint};
use image::imageops::FilterType;
use kitty_graphics::transmit::BatchEmitter;
use ratatui::layout::Rect;

use crate::state::Reader;

// `kitty_graphics::transmit_byte_cap()` returns the raw-PNG ceiling for
// the active terminal: the conservative 300 KB used to be a const here,
// but the cap is really an iTerm2 single-APC constraint — native Kitty
// (when not inside tmux) tolerates much larger payloads, so we let the
// detection layer decide.  See that function's docs for the full
// rationale and override behaviour.

/// Retry interval for image loads that failed at first sight.
/// See `ImageState::negative_loads`.
const NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub(crate) struct ImageJob {
    kitty_id: u32,
    path: PathBuf,
}

impl ImageJob {
    fn resolve_png(kitty_id: u32, path: PathBuf) -> Self {
        Self { kitty_id, path }
    }
}

#[derive(Debug)]
pub(crate) struct ImageResult {
    kitty_id: u32,
    png_bytes: Result<Vec<u8>, String>,
}

struct ImageWorker {
    jobs: Sender<ImageJob>,
    results: Receiver<ImageResult>,
}

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
    bytes: HashMap<u32, Option<Vec<u8>>>,
    /// Kitty ids that had visible placements last frame.  Diffed against
    /// `current` each frame to find which ones just scrolled out.
    prev_visible: HashSet<u32>,
    /// Last `(abs_row, abs_col, cols, rows)` we emitted for each id.
    /// Compared against the current frame's intended placement; equal
    /// means "already on screen, don't re-emit."
    last_emitted: HashMap<u32, (u16, u16, u16, u16)>,
    /// Wall-clock timestamps for ids that failed to load.  Used to
    /// expire negative entries in `bytes` after `NEGATIVE_CACHE_TTL` so
    /// a file that becomes readable later (e.g. a still-downloading
    /// asset finishes, or the user fixes a permissions problem)
    /// recovers without restarting the reader — but not so often that
    /// rapid scroll keeps re-spawning `pdftoppm` for a genuinely
    /// missing figure.
    negative_loads: HashMap<u32, Instant>,
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
    transmitted_ids: HashSet<u32>,
    /// Kitty ids currently shown in the preview pane (see
    /// `place_one_figure`).  Multi-part figures own multiple ids — one
    /// per `FigurePart` — so the preview tiler can place all subfigures
    /// of a single logical figure (Fig 3's N×M grid, subfigure rows,
    /// etc.) into one area.  Empty when no preview is active.  Distinct
    /// from `prev_visible`, which is only for inline images placed by
    /// `place_visible`.
    preview_ids: HashSet<u32>,
    /// Background image preparation worker.  Placement schedules jobs
    /// here and keeps the previous frame stable until bytes arrive.
    worker: Option<ImageWorker>,
    /// Kitty ids currently queued or running on the image worker.
    pending_jobs: HashSet<u32>,
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

/// Walk the visible window for Image VLs and emit Kitty escapes to
/// transmit (first time) and place each one at its current screen row.
/// No-op if `supported == false` — keeps the hot path cheap on
/// non-graphics terminals and on prose-only papers.
pub fn place_visible(reader: &Reader, state: &mut ImageState, content_area: Rect, supported: bool) {
    if !supported {
        return;
    }
    let _span = crate::bench::Span::new("place_visible");
    poll_ready(state);

    // Expire stale negative-load entries so a file that became readable
    // after we first tried it (still-downloading asset, transient
    // permissions error) gets a fresh attempt on the next frame.  We
    // drop the matching `bytes` entry too so the `or_insert_with` loop
    // below actually retries; if the load fails again the negative
    // stamp is reapplied and the next retry is another window away.
    let now = Instant::now();
    let expired: Vec<u32> = state
        .negative_loads
        .iter()
        .filter(|(_, t)| now.duration_since(**t) >= NEGATIVE_CACHE_TTL)
        .map(|(id, _)| *id)
        .collect();
    for id in expired {
        state.negative_loads.remove(&id);
        // Only evict the `bytes` entry if it still represents a failure;
        // a successful load between stamp and expiry must not be wiped.
        if matches!(state.bytes.get(&id), Some(None)) {
            state.bytes.remove(&id);
        }
    }

    // Collect first-row Image VLs that fall inside content_area, plus
    // the set of all kitty_ids touching the visible window (which may
    // include rows of an image whose first row scrolled off the top).
    let total = reader.visual_lines.len();
    let height = content_area.height as usize;
    let cell_cols = content_area.width;

    // Each placement: (id, abs_row, rows, abs_col, cols).  Cell footprint
    // (cols × rows) was chosen at build_visual_lines time to preserve
    // each image's aspect ratio; we just read it from the VL kind here.
    let mut placements: Vec<(u32, u16, u16, u16, u16)> = Vec::new();
    let mut current: HashSet<u32> = HashSet::new();
    let _ = cell_cols; // total content width — kept for the trace log only.
    for screen_row in 0..height {
        let vl_idx = reader.offset + screen_row;
        if vl_idx >= total {
            break;
        }
        let abs_row = content_area.y + screen_row as u16;
        // The image must fit *fully* inside the content area for us to
        // render it.  Kitty graphics has no partial-from-top OR partial-
        // from-bottom clipping — the image is anchored at one row and
        // extends downward.  If its bottom would draw past the content
        // area, that overflow lands in the status bar / next pane.  So we
        // gate placement on "first row in viewport AND last row in
        // viewport"; the moment either edge scrolls past the boundary,
        // `current` excludes the id, the dropped-set fires, the image
        // disappears cleanly.  See v2.md for the scale-to-fit alternative.
        let viewport_bottom = content_area.y.saturating_add(content_area.height);
        match &reader.visual_lines[vl_idx].kind {
            VisualLineKind::Image {
                kitty_id,
                cols,
                rows,
                is_first,
            } => {
                if *is_first {
                    let last_row = abs_row.saturating_add(*rows);
                    if last_row <= viewport_bottom {
                        // Center horizontally within the content area — same
                        // visual treatment as tables and display math.  When the
                        // image's cell width matches content_area.width, padding
                        // is zero and the image starts flush left, naturally.
                        let pad = content_area.width.saturating_sub(*cols) / 2;
                        let abs_col = content_area.x.saturating_add(pad);
                        current.insert(*kitty_id);
                        // Suppress inline emission for the figure currently owned
                        // by the preview pane.  Both paths share the same Kitty
                        // image id with an implicit p=0 placement, so emitting
                        // here would relocate (and visibly steal) the preview's
                        // placement.  We still record it in `current` so the
                        // dropped-diff below treats the id as "still around".
                        if !state.preview_ids.contains(kitty_id) {
                            placements.push((*kitty_id, abs_row, *rows, abs_col, *cols));
                        }
                    }
                }
            }
            VisualLineKind::ImageRow {
                items,
                rows,
                is_first,
            } => {
                if *is_first {
                    let last_row = abs_row.saturating_add(*rows);
                    if last_row <= viewport_bottom {
                        for (id, _) in items {
                            current.insert(*id);
                        }
                        // Center the WHOLE row: sum of sibling cols vs content
                        // width gives the slack to split as left padding.  Items
                        // then place at consecutive cumulative offsets.
                        let total_cols: u16 = items.iter().map(|(_, c)| *c).sum();
                        let pad = content_area.width.saturating_sub(total_cols) / 2;
                        let mut col = content_area.x.saturating_add(pad);
                        for (id, item_cols) in items {
                            if !state.preview_ids.contains(id) {
                                placements.push((*id, abs_row, *rows, col, *item_cols));
                            }
                            col = col.saturating_add(*item_cols);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Trace logging — gated by env var so it only fires when we're
    // actively debugging.  Run with `TREAD_TRACE_IMAGES=1 ... 2>/tmp/br.log`
    // to capture without polluting the alt screen.
    let trace = std::env::var_os("TREAD_TRACE_IMAGES").is_some();
    let mut batch = BatchEmitter::new();
    if trace && !placements.is_empty() {
        eprintln!(
            "trace: offset={} content_area=({},{},{}x{}) placements={:?}",
            reader.offset,
            content_area.x,
            content_area.y,
            content_area.width,
            content_area.height,
            placements,
        );
        for &(id, _, _, _, _) in &placements {
            let path = reader.image_paths.get(&id);
            let load_status = match state.bytes.get(&id) {
                Some(Some(b)) => format!("loaded ({} bytes)", b.len()),
                Some(None) => "LOAD FAILED (cached negative)".to_string(),
                None => "not loaded yet".to_string(),
            };
            eprintln!("  id={} path={:?} status={}", id, path, load_status);
        }
    }

    // Delete placements for images that just scrolled out entirely.
    // Kitty otherwise leaves the image painted over the area where new
    // text is about to appear.  Also clear them from `last_emitted` so
    // the next time they scroll back in we know to re-emit (the terminal
    // has discarded the image at this point).
    //
    // The preview id is excluded from this drop set: deleting the image
    // would wipe the preview pane's placement too (Kitty's `a=d,d=i`
    // deletes ALL placements for an image id, not just the inline one).
    // The preview path owns its own lifecycle via `place_one_figure`.
    let dropped: Vec<u32> = state
        .prev_visible
        .difference(&current)
        .copied()
        .filter(|id| !state.preview_ids.contains(id))
        .collect();
    for id in dropped {
        let _ = batch.delete_placement(id);
        state.last_emitted.remove(&id);
    }

    // Two emission paths, picked per-terminal:
    //
    // - **Persistent-cache hosts** (native Kitty): the first time an id
    //   is visible we send `a=T` (transmit-and-display); the terminal
    //   keeps the bytes.  On every subsequent re-placement we emit just
    //   `delete + a=p` — ~100 bytes per scroll line instead of ~400 KB.
    //   This is what makes continuous-scroll feel fluid on image-heavy
    //   papers; without it, every `j` keystroke re-base64s the whole PNG
    //   to stdout and the page visibly catches up.
    // - **Non-cache hosts** (iTerm2 ≥ 3.5): no persistent cache means
    //   `a=p` for an id not transmitted in the same frame silently
    //   no-ops.  We stay on the full-retransmit path: `delete + a=T`
    //   every time placement changes.  Same behaviour as before this
    //   split landed.
    //
    // Failures (missing file, unreadable bytes, unsupported format) are
    // memoised as `None` in `state.bytes` and silently skipped on every
    // future frame — the caption row beneath the image is the
    // user-visible fallback.
    let has_cache = kitty_graphics::has_persistent_image_cache();
    for &(id, abs_row, rows, abs_col, cols) in &placements {
        if !state.bytes.contains_key(&id) {
            if let Some(path) = reader.image_paths.get(&id).cloned() {
                schedule_image_job(
                    state,
                    ImageJob::resolve_png(id, path),
                    trace,
                    ImageLoadContext::Inline,
                );
            } else {
                state.bytes.insert(id, None);
            }
        }
        if state.pending_jobs.contains(&id) {
            continue;
        }
        let Some(bytes) = state.bytes.get(&id).and_then(|v| v.as_ref()) else {
            state.negative_loads.entry(id).or_insert_with(Instant::now);
            if trace {
                eprintln!("  skip id={} (no bytes)", id);
            }
            continue;
        };
        // Lazy emission: skip the delete+transmit cycle when the placement
        // hasn't moved since last frame.  Idle events (mouse motion, focus,
        // un-handled keys) don't change scroll position, so most frames
        // hit this fast path and pay zero terminal-IO for image upkeep.
        let placement_key = (abs_row, abs_col, cols, rows);
        if state.last_emitted.get(&id) == Some(&placement_key) {
            if trace {
                eprintln!(
                    "  cached id={} at row={} col={}",
                    id,
                    abs_row + 1,
                    abs_col + 1
                );
            }
            continue;
        }
        let _ = batch.delete_placement(id);
        // Cursor positioning travels inside the same passthrough envelope
        // as the APC — see `transmit_and_place` / `place_by_id` doc
        // comments for the tmux DCS details.  Both row and col are
        // 1-indexed (ANSI CUP convention).
        let already_transmitted = has_cache && state.transmitted_ids.contains(&id);
        if already_transmitted {
            if trace {
                eprintln!(
                    "  place id={} at row={} col={} cells={}x{} (cached)",
                    id,
                    abs_row + 1,
                    abs_col + 1,
                    cols,
                    rows
                );
            }
            let _ = batch.place_by_id(id, cols, rows, abs_row + 1, abs_col + 1);
        } else {
            if trace {
                eprintln!(
                    "  emit id={} at row={} col={} cells={}x{}",
                    id,
                    abs_row + 1,
                    abs_col + 1,
                    cols,
                    rows
                );
            }
            let _ = batch.transmit_and_place(id, bytes, cols, rows, abs_row + 1, abs_col + 1);
            if has_cache {
                state.transmitted_ids.insert(id);
            }
        }
        state.last_emitted.insert(id, placement_key);
    }

    let _ = batch.flush();
    state.prev_visible = current;
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

pub(crate) fn clear_inline(state: &mut ImageState) {
    clear_inline_inner(state, true);
}

fn clear_inline_inner(state: &mut ImageState, emit: bool) {
    let ids: Vec<u32> = state.prev_visible.drain().collect();
    if emit {
        let mut batch = BatchEmitter::new();
        for id in &ids {
            let _ = batch.delete_placement(*id);
        }
        let _ = batch.flush();
    }
    state
        .last_emitted
        .retain(|id, _| state.preview_ids.contains(id));
}

pub(crate) fn clear_preview(state: &mut ImageState) {
    clear_preview_inner(state, true);
}

fn clear_preview_inner(state: &mut ImageState, emit: bool) {
    if state.preview_ids.is_empty() {
        return;
    }
    let ids: Vec<u32> = state.preview_ids.drain().collect();
    if emit {
        let mut batch = BatchEmitter::new();
        for id in &ids {
            let _ = batch.delete_placement(*id);
        }
        let _ = batch.flush();
    }
    for id in &ids {
        state.last_emitted.remove(id);
    }
}

/// Render one figure into a dedicated preview pane (or clear it).
///
/// Pair with `Reader::set_text_only(true)` so the main reader pane
/// stays text-only while this draws the user's currently-selected
/// figure into `area`.  Pass `Some(kitty_id)` to show a figure;
/// pass `None` to clear any active preview placement.
///
/// Stepping (host calls with a different `Some(id)`) deletes the
/// previously-previewed placement and emits the new one.  When the
/// id and area are unchanged from the prior frame, hits the same
/// lazy fast path as `place_visible` and emits nothing.
///
/// State on `ImageState` is shared with `place_visible`:
/// - `bytes` byte-cache, `last_emitted` placement-cache, and
///   `transmitted_ids` terminal-side cache are all reused.
/// - `negative_loads` TTL applies the same way: a figure that
///   failed to load gets retried every `NEGATIVE_CACHE_TTL`.
pub fn place_one_figure(
    reader: &Reader,
    state: &mut ImageState,
    kitty_id: Option<u32>,
    area: Rect,
    supported: bool,
) {
    if !supported {
        return;
    }
    poll_ready(state);
    let trace = std::env::var_os("TREAD_TRACE_IMAGES").is_some();
    let mut batch = BatchEmitter::new();

    // Resolve the requested representative id to the whole figure entry.
    // The preview tiles every part of the figure (multi-row, multi-col)
    // inside `area`, not just the representative — that's what makes a
    // composite figure like Fig 3 render as one whole image.
    let target_ids: HashSet<u32> = match kitty_id.and_then(|id| reader.figure_entry_for(id)) {
        Some(entry) => entry.parts().map(|p| p.kitty_id).collect(),
        None => HashSet::new(),
    };

    // Delete placements for parts that are no longer in the current
    // figure (host stepped to a different figure, or cleared preview).
    let to_drop: Vec<u32> = state
        .preview_ids
        .difference(&target_ids)
        .copied()
        .collect();
    for prev in &to_drop {
        if trace {
            eprintln!("preview: delete prev id={prev}");
        }
        let _ = batch.delete_placement(*prev);
        state.last_emitted.remove(prev);
        state.preview_ids.remove(prev);
    }

    let Some(rep_id) = kitty_id else {
        let _ = batch.flush();
        return;
    };
    let Some(entry) = reader.figure_entry_for(rep_id) else {
        if trace {
            eprintln!("preview: no figure entry for id={rep_id}");
        }
        let _ = batch.flush();
        return;
    };

    // Layout: divide `area` vertically into one strip per row of the
    // figure, then within each strip divide horizontally into one cell
    // per side-by-side part.  Within each cell we use the existing
    // aspect-preserving footprint math, so subfigures of different
    // shapes coexist without distortion.
    let row_count = entry.rows.len() as u16;
    if row_count == 0 || area.height == 0 || area.width == 0 {
        let _ = batch.flush();
        return;
    }
    let max_row_height = area.height / row_count;
    if max_row_height == 0 {
        let _ = batch.flush();
        return;
    }
    let has_cache = kitty_graphics::has_persistent_image_cache();
    let mut new_preview_ids: HashSet<u32> = HashSet::new();
    for (row_idx, row) in entry.rows.iter().enumerate() {
        if row.is_empty() {
            continue;
        }
        let item_count = row.len() as u16;
        let cell_width_budget = area.width / item_count;
        if cell_width_budget == 0 {
            continue;
        }
        // Two-pass row layout (mirrors `build_visual_lines::ImageRow`):
        // 1. Find the common row height — the largest aspect-correct
        //    height across siblings under the per-cell width budget.
        //    The cap is `max_row_height` so a single tall sibling can't
        //    crowd out the rest of the stack.
        // 2. Derive each sibling's width from that shared height.
        // Result: items pack tight at uniform height, no internal cell
        // padding, matching how the source figure renders on the page.
        let mut row_height: u16 = 1;
        for part in row {
            let (_c, r) =
                compute_cell_footprint(part.dims, cell_width_budget as usize, max_row_height);
            if r > row_height {
                row_height = r;
            }
        }
        row_height = row_height.min(max_row_height);
        let item_cols: Vec<u16> = row
            .iter()
            .map(|part| match part.dims {
                Some((w, h)) if h > 0 && w > 0 => {
                    let derived = ((row_height as u32 * w * 2) / h) as u16;
                    derived.min(cell_width_budget).max(1)
                }
                _ => cell_width_budget,
            })
            .collect();
        let total_cols: u16 = item_cols.iter().sum();
        let row_start_x = area
            .x
            .saturating_add(area.width.saturating_sub(total_cols) / 2);
        let row_y = area.y.saturating_add(row_idx as u16 * max_row_height);
        let row_pad_y = max_row_height.saturating_sub(row_height) / 2;
        let mut cur_x = row_start_x;
        for (part, cols) in row.iter().zip(item_cols.iter()) {
            let cols = *cols;
            let rows = row_height;
            let abs_row = row_y.saturating_add(row_pad_y);
            let abs_col = cur_x;
            cur_x = cur_x.saturating_add(cols);
            let placement_key = (abs_row, abs_col, cols, rows);
            let id = part.kitty_id;

            // Lazy byte load — same job pipeline as the inline path.
            if !state.bytes.contains_key(&id) {
                schedule_image_job(
                    state,
                    ImageJob::resolve_png(id, part.path.clone()),
                    trace,
                    ImageLoadContext::Preview,
                );
            }
            if state.pending_jobs.contains(&id) {
                new_preview_ids.insert(id);
                continue;
            }
            let Some(bytes) = state.bytes.get(&id).and_then(|v| v.as_ref()) else {
                state.negative_loads.entry(id).or_insert_with(Instant::now);
                continue;
            };

            // Lazy fast path: same placement as last frame → skip emit.
            if state.last_emitted.get(&id) == Some(&placement_key) {
                if trace {
                    eprintln!(
                        "preview: cached id={id} at row={} col={}",
                        abs_row + 1,
                        abs_col + 1
                    );
                }
                new_preview_ids.insert(id);
                continue;
            }
            let _ = batch.delete_placement(id);
            let already_transmitted = has_cache && state.transmitted_ids.contains(&id);
            if already_transmitted {
                if trace {
                    eprintln!(
                        "preview: place id={id} at row={} col={} cells={cols}x{rows} (cached)",
                        abs_row + 1,
                        abs_col + 1
                    );
                }
                let _ = batch.place_by_id(id, cols, rows, abs_row + 1, abs_col + 1);
            } else {
                if trace {
                    eprintln!(
                        "preview: emit id={id} at row={} col={} cells={cols}x{rows}",
                        abs_row + 1,
                        abs_col + 1
                    );
                }
                let _ = batch.transmit_and_place(id, bytes, cols, rows, abs_row + 1, abs_col + 1);
                if has_cache {
                    state.transmitted_ids.insert(id);
                }
            }
            state.last_emitted.insert(id, placement_key);
            new_preview_ids.insert(id);
        }
    }
    state.preview_ids = new_preview_ids;
    let _ = batch.flush();
}

#[derive(Debug, Clone, Copy)]
enum ImageLoadContext {
    Inline,
    Preview,
}

fn run_image_job(job: ImageJob) -> ImageResult {
    ImageResult {
        kitty_id: job.kitty_id,
        png_bytes: resolve_png(&job.path).map_err(|err| err.to_string()),
    }
}

fn spawn_image_worker() -> Option<ImageWorker> {
    let (job_tx, job_rx) = mpsc::channel::<ImageJob>();
    let (result_tx, result_rx) = mpsc::channel::<ImageResult>();
    let spawn_result = thread::Builder::new()
        .name("tread-image-worker".to_string())
        .spawn(move || {
            while let Ok(job) = job_rx.recv() {
                let result = run_image_job(job);
                if result_tx.send(result).is_err() {
                    break;
                }
            }
        });

    if spawn_result.is_ok() {
        Some(ImageWorker {
            jobs: job_tx,
            results: result_rx,
        })
    } else {
        None
    }
}

fn apply_image_result(state: &mut ImageState, result: ImageResult) {
    state.pending_jobs.remove(&result.kitty_id);
    match result.png_bytes {
        Ok(bytes) => {
            state.negative_loads.remove(&result.kitty_id);
            state.bytes.insert(result.kitty_id, Some(bytes));
        }
        Err(_) => {
            state.bytes.insert(result.kitty_id, None);
        }
    }
}

pub(crate) fn poll_ready(state: &mut ImageState) -> bool {
    let Some(worker) = state.worker.as_ref() else {
        return false;
    };

    let mut disconnected = false;
    let mut results = Vec::new();
    loop {
        match worker.results.try_recv() {
            Ok(result) => results.push(result),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                disconnected = true;
                break;
            }
        }
    }

    let changed = !results.is_empty();
    for result in results {
        apply_image_result(state, result);
    }
    if disconnected {
        state.worker = None;
        state.pending_jobs.clear();
    }
    changed
}

pub(crate) fn has_pending_jobs(state: &ImageState) -> bool {
    !state.pending_jobs.is_empty()
}

fn schedule_image_job(
    state: &mut ImageState,
    job: ImageJob,
    trace: bool,
    context: ImageLoadContext,
) {
    let id = job.kitty_id;
    if state.bytes.contains_key(&id) || state.pending_jobs.contains(&id) {
        return;
    }

    if state.worker.is_none() {
        state.worker = spawn_image_worker();
    }

    if let Some(worker) = &state.worker {
        match worker.jobs.send(job) {
            Ok(()) => {
                state.pending_jobs.insert(id);
                if trace {
                    match context {
                        ImageLoadContext::Inline => eprintln!("  schedule image job id={id}"),
                        ImageLoadContext::Preview => {
                            eprintln!("preview: schedule image job id={id}")
                        }
                    }
                }
            }
            Err(err) => {
                ensure_image_bytes(state, err.0, trace, context);
            }
        }
    } else {
        ensure_image_bytes(state, job, trace, context);
    }
}

fn ensure_image_bytes(
    state: &mut ImageState,
    job: ImageJob,
    trace: bool,
    context: ImageLoadContext,
) {
    if state.bytes.contains_key(&job.kitty_id) {
        return;
    }

    let id = job.kitty_id;
    let path = job.path.clone();
    let result = run_image_job(job);
    if trace {
        match &result.png_bytes {
            Ok(bytes) => match context {
                ImageLoadContext::Inline => {
                    eprintln!(
                        "  load id={} path={:?} ok ({} bytes)",
                        id,
                        path,
                        bytes.len()
                    )
                }
                ImageLoadContext::Preview => {
                    eprintln!("preview: load id={id} ok ({} bytes)", bytes.len())
                }
            },
            Err(err) => match context {
                ImageLoadContext::Inline => {
                    eprintln!("  load id={} path={:?} ERR: {}", id, path, err)
                }
                ImageLoadContext::Preview => eprintln!("preview: load id={id} ERR: {err}"),
            },
        }
    }
    apply_image_result(state, result);
}

/// Resolve an image source path to PNG bytes.  PNGs read directly;
/// PDFs go through Poppler via `kitty_graphics::pdf::pdf_to_png` and
/// are cached under `~/.cache/tread/figures` so a second visit doesn't
/// re-rasterise.  JPEGs are decoded in-process and re-encoded as PNG
/// for Kitty (which only accepts PNG payloads); the result is cached
/// in `ImageState.bytes` per-session.  GIFs and other formats remain
/// unsupported until a paper actually contains one.
fn resolve_png(path: &Path) -> std::io::Result<Vec<u8>> {
    let _span = crate::bench::Span::new("image_resolve_png");
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let png_bytes = match ext.as_str() {
        "png" => std::fs::read(path),
        "pdf" => {
            let cache = cache_dir();
            let png = kitty_graphics::pdf::pdf_to_png(path, &cache)?;
            let png_bytes = std::fs::read(&png)?;
            return normalize_png_for_terminal(&png, png_bytes);
        }
        "jpg" | "jpeg" => {
            let img = image::open(path)
                .map_err(|e| std::io::Error::other(format!("decode jpeg {path:?}: {e}")))?;
            let mut png_bytes = std::io::Cursor::new(Vec::new());
            img.write_to(&mut png_bytes, image::ImageFormat::Png)
                .map_err(|e| std::io::Error::other(format!("encode png {path:?}: {e}")))?;
            Ok(png_bytes.into_inner())
        }
        other => Err(std::io::Error::other(format!(
            "unsupported image format: {other}"
        ))),
    }?;
    normalize_png_for_terminal(path, png_bytes)
}

fn normalize_png_for_terminal(path: &Path, png_bytes: Vec<u8>) -> std::io::Result<Vec<u8>> {
    normalize_png_for_terminal_with_limit(path, png_bytes, kitty_graphics::transmit_byte_cap())
}

fn normalize_png_for_terminal_with_limit(
    path: &Path,
    mut png_bytes: Vec<u8>,
    max_bytes: usize,
) -> std::io::Result<Vec<u8>> {
    if png_bytes.len() <= max_bytes {
        return Ok(png_bytes);
    }

    let trace = std::env::var_os("TREAD_TRACE_IMAGES").is_some();

    // Cross-session disk cache for downscaled PNGs.  Lanczos3 +
    // re-encode is the dominant first-visibility CPU cost on
    // figure-heavy iTerm2 scrolls (~30-50% of busy samples after the
    // burst-skip change exposed it as the new top leaf).  Each (source
    // path, max_bytes) tuple has a deterministic output, so save the
    // result alongside the PDF rasterization cache and skip the work
    // on every later session.  Cap-suffixed filename keeps Kitty's
    // wider cap and iTerm2's 300 KB cap from colliding on the same
    // path.  The cache key also includes source-file freshness
    // metadata, so a refreshed asset at the same path doesn't keep
    // serving an older normalized PNG forever.  `normalized_cache_path`
    // returns None when the source path can't be canonicalized — e.g.
    // test fixtures that don't exist on disk — so tests don't pollute
    // the real user cache.
    let cache_path = normalized_cache_path(path, max_bytes);
    if let Some(ref cp) = cache_path
        && let Ok(cached) = std::fs::read(cp)
    {
        // PNG signature check + size bound: filters out partial writes,
        // foreign files at the same hash slot, and cached outputs from a
        // larger-cap session that wouldn't fit our current budget.  Any
        // miss falls through to re-encode and re-cache below.
        const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
        if cached.len() >= 8 && &cached[..8] == PNG_SIGNATURE && cached.len() <= max_bytes {
            if trace {
                eprintln!(
                    "  norm-cache hit {:?}: {} bytes (cap {})",
                    path,
                    cached.len(),
                    max_bytes
                );
            }
            return Ok(cached);
        }
    }

    let mut img = image::load_from_memory_with_format(&png_bytes, image::ImageFormat::Png)
        .map_err(|e| std::io::Error::other(format!("decode png {path:?}: {e}")))?;

    for attempt in 1..=6 {
        if png_bytes.len() <= max_bytes {
            break;
        }

        let width = img.width();
        let height = img.height();
        if width <= 1 || height <= 1 {
            break;
        }

        let scale = ((max_bytes as f64 / png_bytes.len() as f64).sqrt() * 0.90).clamp(0.10, 0.95);
        let next_width = ((width as f64 * scale).round() as u32)
            .max(1)
            .min(width - 1);
        let next_height = ((height as f64 * scale).round() as u32)
            .max(1)
            .min(height - 1);

        let resized = img.resize(next_width, next_height, FilterType::Lanczos3);
        let next_png = encode_dynamic_image_png(path, &resized)?;

        if trace {
            eprintln!(
                "  downscale {:?}: {}x{} {} bytes -> {}x{} {} bytes (attempt {})",
                path,
                width,
                height,
                png_bytes.len(),
                resized.width(),
                resized.height(),
                next_png.len(),
                attempt
            );
        }

        if next_png.len() >= png_bytes.len() {
            break;
        }

        png_bytes = next_png;
        img = resized;
    }

    // Best-effort cache write — failure here just means the next
    // session will redo the work, no functional impact this session.
    // Skip when the source path didn't canonicalize (tests with
    // synthetic paths) or when normalization couldn't get the bytes
    // under the cap (no point caching an oversized result; next
    // session would just reject it).  Writes go through a temp file +
    // rename so readers never observe a truncated cache artifact.
    if let Some(ref cp) = cache_path
        && png_bytes.len() <= max_bytes
    {
        let _ = write_atomic(cp, &png_bytes);
        if trace {
            eprintln!(
                "  norm-cache write {:?}: {} bytes (cap {})",
                path,
                png_bytes.len(),
                max_bytes
            );
        }
    }

    Ok(png_bytes)
}

fn encode_dynamic_image_png(path: &Path, img: &image::DynamicImage) -> std::io::Result<Vec<u8>> {
    let mut png_bytes = std::io::Cursor::new(Vec::new());
    img.write_to(&mut png_bytes, image::ImageFormat::Png)
        .map_err(|e| std::io::Error::other(format!("encode png {path:?}: {e}")))?;
    Ok(png_bytes.into_inner())
}

fn cache_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".cache")
            .join("tread")
            .join("figures");
    }
    std::env::temp_dir().join("tread-figures")
}

/// Maximum bytes retained in the figure cache.  Both PDF rasterisations
/// (`<key>-1.png`, often 1-3 MB each, up to ~20 MB for dense
/// engineering diagrams) and downscaled "norm" PNGs (~200-300 KB each)
/// live here.  At ~1 MB average, 500 MB holds figures for roughly 500
/// papers — comfortably above what a daily reader accumulates between
/// runs.  Eviction is age-based (oldest mtime first), which approximates
/// LRU for our workload since cache hits don't update mtime: a
/// frequently-revisited paper has the same mtime as its first rasterisation,
/// but a never-revisited paper from months ago has the same mtime too —
/// and the never-revisited one is cheaper to lose.  Both write paths
/// (kitty-graphics's pdf_to_png and tread's normalize_png_for_terminal)
/// re-create on miss, so eviction is always safe.
const FIGURE_CACHE_CAP_BYTES: u64 = 500 * 1024 * 1024;

/// Walk the figure cache, sort by mtime ascending, delete oldest files
/// until total size is at or under `FIGURE_CACHE_CAP_BYTES`.  Idempotent
/// and best-effort: every step (read_dir, stat, remove) is fallible and
/// silently skipped on failure rather than blocking startup.  Called
/// once per process, before any image-cache writes — both the PDF and
/// "norm" caches recreate on miss so even an over-eager eviction is
/// recoverable at the cost of one rasterisation.
pub fn enforce_cache_cap() {
  let _span = crate::bench::Span::new("figure_cache_enforce");
  let dir = cache_dir();
  let Ok(read) = std::fs::read_dir(&dir) else {
    return; // dir doesn't exist or unreadable; nothing to do
  };
  let mut files: Vec<(PathBuf, u64, std::time::SystemTime)> = read
    .filter_map(|e| e.ok())
    .filter_map(|e| {
      let path = e.path();
      let meta = e.metadata().ok()?;
      if !meta.is_file() {
        return None;
      }
      let mtime = meta.modified().ok()?;
      Some((path, meta.len(), mtime))
    })
    .collect();
  let total: u64 = files.iter().map(|(_, sz, _)| sz).sum();
  if total <= FIGURE_CACHE_CAP_BYTES {
    crate::bench::emit_fields(
      "figure_cache_under_cap",
      &[
        ("total_bytes", total as i64),
        ("cap_bytes", FIGURE_CACHE_CAP_BYTES as i64),
        ("files", files.len() as i64),
      ],
    );
    return;
  }
  files.sort_by_key(|(_, _, t)| *t);
  let mut to_evict = total - FIGURE_CACHE_CAP_BYTES;
  let mut evicted_bytes: u64 = 0;
  let mut evicted_count: u64 = 0;
  for (path, sz, _) in &files {
    if to_evict == 0 {
      break;
    }
    if std::fs::remove_file(path).is_ok() {
      to_evict = to_evict.saturating_sub(*sz);
      evicted_bytes += *sz;
      evicted_count += 1;
    }
  }
  crate::bench::emit_fields(
    "figure_cache_evicted",
    &[
      ("evicted_bytes", evicted_bytes as i64),
      ("evicted_files", evicted_count as i64),
      ("kept_bytes", (total - evicted_bytes) as i64),
      ("cap_bytes", FIGURE_CACHE_CAP_BYTES as i64),
    ],
  );
}

/// Filesystem path for the cached normalized PNG produced from
/// `(source freshness, max_bytes)`.  Returns `None` when `source`
/// doesn't canonicalize or stat cleanly (test fixtures with
/// non-existent synthetic paths, or genuinely missing files) —
/// caching is best-effort and not having a real source means we
/// have nothing stable to key on.
///
/// Filename embeds both the FNV-1a hash of the canonical source path
/// plus freshness metadata and the cap, so:
/// - cap changes (between terminals with different
///   `transmit_byte_cap` values) don't collide on the same slot
/// - refreshed source assets at the same path don't keep serving
///   stale normalized PNGs
///
/// Matches the PDF rasterization cache's naming style.
fn normalized_cache_path(source: &Path, max_bytes: usize) -> Option<PathBuf> {
    let canonical = std::fs::canonicalize(source).ok()?;
    let freshness = source_fingerprint(&canonical)?;
    let key = fnv1a_64(freshness.as_bytes());
    Some(cache_dir().join(format!("{key:016x}-{max_bytes}.norm.png")))
}

fn source_fingerprint(source: &Path) -> Option<String> {
    let metadata = std::fs::metadata(source).ok()?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|ts| ts.duration_since(UNIX_EPOCH).ok())
        .unwrap_or_default();
    Some(format!(
        "{}|{}|{}|{}",
        source.as_os_str().to_string_lossy(),
        metadata.len(),
        modified.as_secs(),
        modified.subsec_nanos(),
    ))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let Some(parent) = path.parent() else {
        return std::fs::write(path, bytes);
    };
    std::fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("cache");
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let tmp = parent.join(format!(".{file_name}.tmp-{}-{stamp}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&tmp);
            Err(err)
        }
    }
}

/// FNV-1a 64-bit hash — same algorithm and constants as
/// `kitty_graphics::pdf::cache_key` for consistency across the
/// project's caching layers.  Stable across std versions (which
/// `DefaultHasher` is not).
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use doc_model::Block;
    use image::{DynamicImage, Rgb, RgbImage};

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
            },
            Block::Figure {
                rows: vec![vec![doc_model::ImageItem {
                    path: std::path::PathBuf::from("/nonexistent/b.png"),
                    kitty_id: 2,
                    dims: Some((40, 20)),
                }]],
                alt: String::new(),
                figure_id: 2,
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
