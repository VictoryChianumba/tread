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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use doc_model::{Block, VisualLineKind, compute_cell_footprint};
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
  /// Kitty id currently shown in a preview pane (see
  /// `place_one_figure`).  Tracked so a subsequent call with a
  /// different id knows what to delete first.  `None` when no
  /// preview is active.  Distinct from `prev_visible`, which is
  /// only for inline images placed by `place_visible`.
  preview_id: Option<u32>,
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
    Self { last_event_at: None, burst_window: Duration::from_millis(100) }
  }
}

impl BurstTracker {
  pub fn note_event(&mut self) {
    self.last_event_at = Some(Instant::now());
  }

  pub fn in_burst(&self) -> bool {
    self.last_event_at.map(|t| t.elapsed() < self.burst_window).unwrap_or(false)
  }
}

/// Walk the visible window for Image VLs and emit Kitty escapes to
/// transmit (first time) and place each one at its current screen row.
/// No-op if `supported == false` — keeps the hot path cheap on
/// non-graphics terminals and on prose-only papers.
pub fn place_visible(
  reader: &Reader,
  state: &mut ImageState,
  content_area: Rect,
  supported: bool,
) {
  if !supported {
    return;
  }

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
      VisualLineKind::Image { kitty_id, cols, rows, is_first } => {
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
            placements.push((*kitty_id, abs_row, *rows, abs_col, *cols));
          }
        }
      }
      VisualLineKind::ImageRow { items, rows, is_first } => {
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
              placements.push((*id, abs_row, *rows, col, *item_cols));
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
      content_area.x, content_area.y, content_area.width, content_area.height,
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
  let dropped: Vec<u32> = state.prev_visible.difference(&current).copied().collect();
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
    let path_for_load = reader.image_paths.get(&id).cloned();
    // Trigger the lazy load inside a tight scope so the mutable
    // borrow on `state.bytes` releases before the failure-path stamp
    // touches `state.negative_loads` below.
    {
      state.bytes.entry(id).or_insert_with(|| {
        path_for_load.as_ref().and_then(|p| {
          let r = resolve_png(p);
          if trace {
            match &r {
              Ok(b) => eprintln!("  load id={} path={:?} ok ({} bytes)", id, p, b.len()),
              Err(e) => eprintln!("  load id={} path={:?} ERR: {}", id, p, e),
            }
          }
          r.ok()
        })
      });
    }
    let Some(bytes) = state.bytes.get(&id).and_then(|v| v.as_ref()) else {
      state.negative_loads.entry(id).or_insert_with(Instant::now);
      if trace { eprintln!("  skip id={} (no bytes)", id); }
      continue;
    };
    // Lazy emission: skip the delete+transmit cycle when the placement
    // hasn't moved since last frame.  Idle events (mouse motion, focus,
    // un-handled keys) don't change scroll position, so most frames
    // hit this fast path and pay zero terminal-IO for image upkeep.
    let placement_key = (abs_row, abs_col, cols, rows);
    if state.last_emitted.get(&id) == Some(&placement_key) {
      if trace { eprintln!("  cached id={} at row={} col={}", id, abs_row + 1, abs_col + 1); }
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
        eprintln!("  place id={} at row={} col={} cells={}x{} (cached)",
          id, abs_row + 1, abs_col + 1, cols, rows);
      }
      let _ = batch.place_by_id(id, cols, rows, abs_row + 1, abs_col + 1);
    } else {
      if trace {
        eprintln!("  emit id={} at row={} col={} cells={}x{}",
          id, abs_row + 1, abs_col + 1, cols, rows);
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
  let mut batch = BatchEmitter::new();
  for id in state.prev_visible.drain() {
    let _ = batch.delete_placement(id);
  }
  // Preview placements aren't tracked in `prev_visible` (they go
  // through `place_one_figure`), so delete them explicitly or the
  // figure would linger at stale coords across a resize.
  if let Some(id) = state.preview_id.take() {
    let _ = batch.delete_placement(id);
  }
  let _ = batch.flush();
  state.last_emitted.clear();
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
  let trace = std::env::var_os("TREAD_TRACE_IMAGES").is_some();
  let mut batch = BatchEmitter::new();

  // If we had a previous preview and the host is asking for a
  // different one (or clearing), delete the old placement first so
  // the terminal doesn't stack ghosts.
  if let Some(prev) = state.preview_id
    && Some(prev) != kitty_id
  {
    if trace { eprintln!("preview: delete prev id={prev}"); }
    let _ = batch.delete_placement(prev);
    state.last_emitted.remove(&prev);
    state.preview_id = None;
  }

  let Some(id) = kitty_id else {
    let _ = batch.flush();
    return;
  };

  let Some((path, dims)) = find_figure_meta(reader, id) else {
    if trace { eprintln!("preview: no figure meta for id={id}"); }
    let _ = batch.flush();
    return;
  };

  // Lazy load PNG bytes into the same `state.bytes` cache the inline
  // path uses.  Scoped so the mutable borrow releases before we
  // stamp `negative_loads` on failure.
  {
    state.bytes.entry(id).or_insert_with(|| {
      let r = resolve_png(&path);
      if trace {
        match &r {
          Ok(b) => eprintln!("preview: load id={id} ok ({} bytes)", b.len()),
          Err(e) => eprintln!("preview: load id={id} ERR: {e}"),
        }
      }
      r.ok()
    });
  }
  let Some(bytes) = state.bytes.get(&id).and_then(|v| v.as_ref()) else {
    state.negative_loads.entry(id).or_insert_with(Instant::now);
    let _ = batch.flush();
    return;
  };

  // Aspect-preserved cell footprint that fits inside `area`, then
  // center both axes.
  let (cols, rows) = compute_cell_footprint(
    dims,
    area.width as usize,
    area.height,
  );
  if cols == 0 || rows == 0 {
    let _ = batch.flush();
    return;
  }
  let pad_col = area.width.saturating_sub(cols) / 2;
  let pad_row = area.height.saturating_sub(rows) / 2;
  let abs_row = area.y.saturating_add(pad_row);
  let abs_col = area.x.saturating_add(pad_col);
  let placement_key = (abs_row, abs_col, cols, rows);

  // Same lazy fast path as `place_visible`: identical placement +
  // identical id since last frame means the terminal already has
  // this on screen and we owe zero IO this frame.
  if state.last_emitted.get(&id) == Some(&placement_key) {
    if trace { eprintln!("preview: cached id={id} at row={} col={}", abs_row + 1, abs_col + 1); }
    state.preview_id = Some(id);
    let _ = batch.flush();
    return;
  }

  let _ = batch.delete_placement(id);
  let has_cache = kitty_graphics::has_persistent_image_cache();
  let already_transmitted = has_cache && state.transmitted_ids.contains(&id);
  if already_transmitted {
    if trace {
      eprintln!("preview: place id={id} at row={} col={} cells={cols}x{rows} (cached)", abs_row + 1, abs_col + 1);
    }
    let _ = batch.place_by_id(id, cols, rows, abs_row + 1, abs_col + 1);
  } else {
    if trace {
      eprintln!("preview: emit id={id} at row={} col={} cells={cols}x{rows}", abs_row + 1, abs_col + 1);
    }
    let _ = batch.transmit_and_place(id, bytes, cols, rows, abs_row + 1, abs_col + 1);
    if has_cache {
      state.transmitted_ids.insert(id);
    }
  }
  state.last_emitted.insert(id, placement_key);
  state.preview_id = Some(id);
  let _ = batch.flush();
}

/// Walk `reader.blocks` for a figure by kitty_id, returning its
/// on-disk path and pixel dims.  O(blocks) but blocks is short and
/// preview-step events are infrequent — no need to pre-index.
fn find_figure_meta(reader: &Reader, kitty_id: u32) -> Option<(std::path::PathBuf, Option<(u32, u32)>)> {
  for block in &reader.blocks {
    match block {
      Block::Image { kitty_id: id, path, dims, .. } if *id == kitty_id => {
        return Some((path.clone(), *dims));
      }
      Block::ImageRow { items, .. } => {
        for item in items {
          if item.kitty_id == kitty_id {
            return Some((item.path.clone(), item.dims));
          }
        }
      }
      _ => {}
    }
  }
  None
}

/// Resolve an image source path to PNG bytes.  PNGs read directly;
/// PDFs go through Poppler via `kitty_graphics::pdf::pdf_to_png` and
/// are cached under `~/.cache/tread/figures` so a second visit doesn't
/// re-rasterise.  JPEGs are decoded in-process and re-encoded as PNG
/// for Kitty (which only accepts PNG payloads); the result is cached
/// in `ImageState.bytes` per-session.  GIFs and other formats remain
/// unsupported until a paper actually contains one.
fn resolve_png(path: &Path) -> std::io::Result<Vec<u8>> {
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
      img
        .write_to(&mut png_bytes, image::ImageFormat::Png)
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
    if cached.len() >= 8
      && &cached[..8] == PNG_SIGNATURE
      && cached.len() <= max_bytes
    {
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
    let next_width = ((width as f64 * scale).round() as u32).max(1).min(width - 1);
    let next_height = ((height as f64 * scale).round() as u32).max(1).min(height - 1);

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
  img
    .write_to(&mut png_bytes, image::ImageFormat::Png)
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
  let modified = metadata.modified().ok()
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
    let out = normalize_png_for_terminal_with_limit(Path::new("small.png"), png.clone(), png.len() + 1)
      .unwrap();
    assert_eq!(out, png);
  }

  #[test]
  fn downscales_oversized_png_to_budget() {
    let png = noisy_png(512, 512);
    assert!(png.len() > 20_000, "synthetic png should exceed test budget");

    let out = normalize_png_for_terminal_with_limit(Path::new("large.png"), png, 20_000).unwrap();
    assert!(out.len() <= 20_000, "normalized png should respect byte budget");

    let decoded = image::load_from_memory_with_format(&out, image::ImageFormat::Png).unwrap();
    assert!(decoded.width() < 512 || decoded.height() < 512);
  }

  #[test]
  fn place_one_figure_noop_when_unsupported() {
    use crate::state::Reader;
    let reader = Reader::new(vec![], 80, 24);
    let mut state = ImageState::default();
    place_one_figure(&reader, &mut state, Some(1), Rect::new(0, 0, 10, 10), false);
    assert!(state.preview_id.is_none());
    assert!(state.bytes.is_empty());
  }

  #[test]
  fn place_one_figure_unknown_id_is_silent() {
    use crate::state::Reader;
    let reader = Reader::new(vec![], 80, 24);
    let mut state = ImageState::default();
    place_one_figure(&reader, &mut state, Some(99), Rect::new(0, 0, 10, 10), true);
    assert!(state.preview_id.is_none());
    assert!(state.last_emitted.is_empty());
  }

  #[test]
  fn find_figure_meta_locates_image_and_image_row() {
    use crate::state::Reader;
    use doc_model::ImageItem;
    let blocks = vec![
      Block::Line("intro".to_string()),
      Block::Image {
        path: std::path::PathBuf::from("/tmp/a.png"),
        alt: String::new(),
        kitty_id: 1,
        dims: Some((100, 80)),
        stack_total: 1,
      },
      Block::ImageRow {
        items: vec![ImageItem {
          path: std::path::PathBuf::from("/tmp/b.png"),
          kitty_id: 2,
          dims: Some((50, 50)),
        }],
        alt: String::new(),
      },
    ];
    let reader = Reader::new(blocks, 80, 24);
    let (p1, d1) = find_figure_meta(&reader, 1).unwrap();
    assert_eq!(p1, std::path::PathBuf::from("/tmp/a.png"));
    assert_eq!(d1, Some((100, 80)));
    let (p2, d2) = find_figure_meta(&reader, 2).unwrap();
    assert_eq!(p2, std::path::PathBuf::from("/tmp/b.png"));
    assert_eq!(d2, Some((50, 50)));
    assert!(find_figure_meta(&reader, 99).is_none());
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
}
