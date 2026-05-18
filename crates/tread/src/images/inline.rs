//! Inline image placement.
//!
//! Walks the visible window for `VisualLineKind::Image` /
//! `VisualLineKind::ImageRow` rows and emits Kitty `a=p` placements at
//! the cells ratatui has already blanked.  See ADR-0003 for the bypass-
//! the-frame-buffer rationale.

use std::collections::HashSet;
use std::time::Instant;

use doc_model::VisualLineKind;
use kitty_graphics::transmit::BatchEmitter;
use ratatui::layout::Rect;

use super::{ImageState, NEGATIVE_CACHE_TTL};
use super::worker::{ImageJob, ImageLoadContext, poll_ready, schedule_image_job};
use crate::state::Reader;

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

pub(crate) fn clear_inline(state: &mut ImageState) {
    clear_inline_inner(state, true);
}

pub(crate) fn clear_inline_inner(state: &mut ImageState, emit: bool) {
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
