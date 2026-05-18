//! Preview-pane image placement.
//!
//! Tiles every part of one figure (multi-row, multi-column) inside
//! a `Rect` carved out as the preview pane.  Shares `FigureEntry::layout`
//! with `render::draw_preview_pane` so column positions and row heights
//! cannot drift between the text and pixel paths.  See ADR-0002.

use std::collections::HashSet;
use std::time::Instant;

use kitty_graphics::transmit::BatchEmitter;
use ratatui::layout::Rect;

use super::ImageState;
use super::worker::{ImageJob, ImageLoadContext, poll_ready, schedule_image_job};
use crate::state::Reader;

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

    // Single source of truth for figure layout — same call is made by
    // the header text renderer in `render::draw_preview_pane`.  Sharing
    // ensures the headers sit exactly above the columns they describe.
    let layout = entry.layout(area);
    let has_cache = kitty_graphics::has_persistent_image_cache();
    let mut new_preview_ids: HashSet<u32> = HashSet::new();
    for row_placement in &layout.image_rows {
        if row_placement.items.is_empty() || row_placement.height == 0 {
            continue;
        }
        for placement in &row_placement.items {
            let crate::state::PartPlacement {
                kitty_id: id,
                abs_col,
                abs_row,
                cols,
                rows,
            } = *placement;
            // Look up the source path on the entry so byte loading
            // matches the placement's kitty_id one-for-one.
            let part_path = entry
                .parts()
                .find(|p| p.kitty_id == id)
                .map(|p| p.path.clone());
            let placement_key = (abs_row, abs_col, cols, rows);

            // Lazy byte load — same job pipeline as the inline path.
            if !state.bytes.contains_key(&id) {
                if let Some(path) = part_path.clone() {
                    schedule_image_job(
                        state,
                        ImageJob::resolve_png(id, path),
                        trace,
                        ImageLoadContext::Preview,
                    );
                }
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

pub(crate) fn clear_preview(state: &mut ImageState) {
    clear_preview_inner(state, true);
}

pub(crate) fn clear_preview_inner(state: &mut ImageState, emit: bool) {
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
