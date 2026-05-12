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

use doc_model::VisualLineKind;
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
  /// Ids whose image bytes the terminal currently has cached.  Set on
  /// first successful `a=T` transmission; consulted by the scroll-time
  /// re-place path to decide whether we can use the cheap `a=p`
  /// (placement-only, ~50 bytes) or have to re-transmit the full `a=T`
  /// payload (~400 KB base64 per image).  Cleared by `clear_all` —
  /// resize and focus-loss invalidate the assumption that the terminal
  /// still has our bytes.  Only ever populated when the host terminal
  /// is known to persist image data between frames; on iTerm2 (no
  /// persistent cache) this stays empty and every placement goes
  /// through the full-retransmit path, matching prior behaviour.
  transmitted_ids: HashSet<u32>,
}

impl ImageState {
  /// True when there's at least one image placed on screen from a
  /// previous frame.  Used by the burst-skip path in the standalone
  /// event loop to decide whether a frame that's skipping emission
  /// still owes a delete-all (so the old placement doesn't ghost
  /// over the text that just scrolled into its row).
  pub fn has_placements(&self) -> bool {
    !self.prev_visible.is_empty()
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
    let bytes_opt = state.bytes.entry(id).or_insert_with(|| {
      let result = path_for_load.as_ref().and_then(|p| {
        let r = resolve_png(p);
        if trace {
          match &r {
            Ok(b) => eprintln!("  load id={} path={:?} ok ({} bytes)", id, p, b.len()),
            Err(e) => eprintln!("  load id={} path={:?} ERR: {}", id, p, e),
          }
        }
        r.ok()
      });
      result
    });
    let Some(bytes) = bytes_opt.as_ref() else {
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
pub fn clear_all(state: &mut ImageState) {
  let mut batch = BatchEmitter::new();
  for id in state.prev_visible.drain() {
    let _ = batch.delete_placement(id);
  }
  let _ = batch.flush();
  state.last_emitted.clear();
  // After a resize / focus-loss / exit we no longer trust that the
  // terminal still has our image bytes cached.  Drop transmitted_ids
  // so the next placement falls back to a full `a=T` retransmit;
  // re-population happens automatically on that next emit.
  state.transmitted_ids.clear();
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
      std::fs::read(png)
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
}
