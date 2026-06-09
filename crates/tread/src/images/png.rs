//! PNG resolution, terminal-cap normalization, and on-disk cache.
//!
//! Source assets reach us as `.png`, `.pdf`, or `.jpg/.jpeg`.  Kitty
//! graphics only accepts PNG payloads, and there's an upper byte limit
//! per APC envelope that varies by terminal / tmux state (see
//! `kitty_graphics::transmit_byte_cap`).  This module is the single
//! place that:
//!
//! 1. Resolves a source path to PNG bytes (Poppler / decode-and-
//!    re-encode for JPEG).
//! 2. Downscales to fit the byte cap via Lanczos3 + re-encode.
//! 3. Caches both PDF rasterisations and normalized PNGs on disk so a
//!    second session doesn't redo the work.
//! 4. Evicts the cache by mtime once it exceeds `FIGURE_CACHE_CAP_BYTES`.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use image::imageops::FilterType;

/// Resolve an image source path to PNG bytes.  PNGs read directly;
/// PDFs go through Poppler via `kitty_graphics::pdf::pdf_to_png` and
/// are cached under `~/.cache/tread/figures` so a second visit doesn't
/// re-rasterise.  JPEGs are decoded in-process and re-encoded as PNG
/// for Kitty (which only accepts PNG payloads); the result is cached
/// in `ImageState.bytes` per-session.  GIFs and other formats remain
/// unsupported until a paper actually contains one.
pub(crate) fn resolve_png(path: &Path) -> std::io::Result<Vec<u8>> {
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

pub(crate) fn normalize_png_for_terminal_with_limit(
    path: &Path,
    mut png_bytes: Vec<u8>,
    max_bytes: usize,
) -> std::io::Result<Vec<u8>> {
    let trace = std::env::var_os("TREAD_TRACE_IMAGES").is_some();

    // Cross-session disk cache for fully-processed PNGs — flattened onto an
    // opaque backdrop (below) and downscaled to fit the byte cap.  Lanczos3
    // + re-encode is the dominant first-visibility CPU cost on figure-heavy
    // iTerm2 scrolls (~30-50% of busy samples after the burst-skip change
    // exposed it as the new top leaf); flattening adds a decode + composite
    // on top.  Each (source path, max_bytes) tuple has a deterministic
    // output, so save the result alongside the PDF rasterization cache and
    // skip the work on every later session.  Checked up front (not just on
    // the over-cap path) so a flatten-only result is served from disk too.
    // Cap-suffixed filename keeps Kitty's wider cap and iTerm2's 300 KB cap
    // from colliding on the same path.  The cache key also includes
    // source-file freshness metadata, so a refreshed asset at the same path
    // doesn't keep serving an older normalized PNG forever.
    // `normalized_cache_path` returns None when the source path can't be
    // canonicalized — e.g. test fixtures that don't exist on disk — so
    // tests don't pollute the real user cache.
    let cache_path = normalized_cache_path(path, max_bytes);
    if let Some(ref cp) = cache_path
        && let Ok(cached) = std::fs::read(cp)
    {
        // PNG signature check + size bound: filters out partial writes,
        // foreign files at the same hash slot, and cached outputs from a
        // larger-cap session that wouldn't fit our current budget.  Any
        // miss falls through to re-process and re-cache below.
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

    // arXiv figures are authored for white paper; a transparent backdrop
    // lets a dark terminal show through and swallows dark-ink labels and
    // arrows (the source PNGs are commonly RGBA out of Poppler).  The
    // header gate keeps fully-opaque PNGs and JPEG-sourced RGB on the cheap
    // path — no decode, no re-encode — when they also fit the byte cap.
    let needs_flatten = png_may_have_alpha(&png_bytes);
    if !needs_flatten && png_bytes.len() <= max_bytes {
        return Ok(png_bytes);
    }

    let mut img = image::load_from_memory_with_format(&png_bytes, image::ImageFormat::Png)
        .map_err(|e| std::io::Error::other(format!("decode png {path:?}: {e}")))?;

    // Tracks whether we changed the bytes at all; gates the cache write so a
    // pass-through (header flagged possible alpha but every pixel was opaque,
    // and already under cap) doesn't write a redundant cache artifact.
    let mut did_work = false;

    if needs_flatten
        && let Some(flat) = flatten_alpha_over(&img, FIGURE_BACKDROP)
    {
        png_bytes = encode_dynamic_image_png(path, &flat)?;
        img = flat;
        did_work = true;
        if trace {
            eprintln!("  flatten {path:?}: composited onto opaque backdrop");
        }
    }

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
        did_work = true;
    }

    // Best-effort cache write — failure here just means the next
    // session will redo the work, no functional impact this session.
    // Skip when the source path didn't canonicalize (tests with
    // synthetic paths) or when normalization couldn't get the bytes
    // under the cap (no point caching an oversized result; next
    // session would just reject it).  Writes go through a temp file +
    // rename so readers never observe a truncated cache artifact.  Skip the
    // write when we did no work (pass-through bytes are already the source).
    if let Some(ref cp) = cache_path
        && did_work
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

pub(crate) fn encode_dynamic_image_png(
    path: &Path,
    img: &image::DynamicImage,
) -> std::io::Result<Vec<u8>> {
    let mut png_bytes = std::io::Cursor::new(Vec::new());
    img.write_to(&mut png_bytes, image::ImageFormat::Png)
        .map_err(|e| std::io::Error::other(format!("encode png {path:?}: {e}")))?;
    Ok(png_bytes.into_inner())
}

/// Opaque colour every figure with transparency is composited onto.  arXiv
/// figures are drawn for white paper, so white keeps dark-ink labels,
/// arrows, and rules legible regardless of the reader's theme.
const FIGURE_BACKDROP: [u8; 3] = [255, 255, 255];

/// Cheap pre-decode gate: does this PNG's header indicate it *might* carry
/// per-pixel transparency?  Reads the IHDR colour-type byte (offset 25 in a
/// well-formed PNG) so fully-opaque RGB / grayscale figures — and
/// JPEG-sourced PNGs we re-encoded as RGB — skip the decode-and-composite
/// path entirely.  Conservative: an unrecognisable header returns `true`
/// and lets the decoder + opaque-pixel scan have the final say.
fn png_may_have_alpha(bytes: &[u8]) -> bool {
    const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 26 || &bytes[..8] != PNG_SIGNATURE {
        return true;
    }
    match bytes[25] {
        4 | 6 => true,             // grayscale+alpha / truecolour+alpha
        3 => png_has_trns(bytes),  // palette: transparency lives in a tRNS chunk
        _ => false,                // grayscale (0) / truecolour (2): opaque
    }
}

/// Whether a palette PNG declares a `tRNS` transparency chunk.  `tRNS`
/// always precedes the first `IDAT`, so the scan stops there rather than
/// walking compressed image data (where the four bytes could appear by
/// chance).
fn png_has_trns(bytes: &[u8]) -> bool {
    let end = bytes
        .windows(4)
        .position(|w| w == b"IDAT")
        .unwrap_or(bytes.len());
    bytes[..end].windows(4).any(|w| w == b"tRNS")
}

/// Fraction of the figure's shorter side added as a solid-`bg` margin on
/// every edge when flattening, plus the clamp that keeps it sane across the
/// range of asset resolutions we see.  Without it, content authored flush to
/// a transparent canvas (labels, arrowheads) ends up touching the boundary
/// of the synthesized white box once the dark backdrop is gone.
const FIGURE_PAD_FRACTION: f32 = 0.04;
const FIGURE_PAD_MIN: u32 = 8;
const FIGURE_PAD_MAX: u32 = 64;

/// Alpha-composite `img` over a solid `bg`, inset inside a `bg` margin, and
/// return an opaque RGB image — or `None` when every pixel is already fully
/// opaque (so the caller keeps the original encoded bytes — no needless
/// re-encode or size change).  Dropping the alpha channel on the way out
/// guarantees the terminal can't re-expose the dark backdrop on a future
/// placement; the margin keeps figure content off the box edge.
fn flatten_alpha_over(img: &image::DynamicImage, bg: [u8; 3]) -> Option<image::DynamicImage> {
    use image::{GenericImageView, Rgb, RgbImage};

    let rgba = img.to_rgba8();
    if rgba.pixels().all(|p| p.0[3] == 255) {
        return None;
    }
    let (w, h) = img.dimensions();
    let pad = ((w.min(h) as f32 * FIGURE_PAD_FRACTION).round() as u32)
        .clamp(FIGURE_PAD_MIN, FIGURE_PAD_MAX);

    // Canvas starts as a solid backdrop, so the padding band — and any gaps
    // the source leaves transparent — are already the fill colour.
    let mut out = RgbImage::from_pixel(w + pad * 2, h + pad * 2, Rgb(bg));
    for (x, y, px) in rgba.enumerate_pixels() {
        let [r, g, b, a] = px.0;
        let a = a as u16;
        let inv = 255 - a;
        // Source-over composite: out = fg·α + bg·(1−α), integer-rounded.
        let blend = |fg: u8, bg: u8| (((fg as u16 * a + bg as u16 * inv) + 127) / 255) as u8;
        out.put_pixel(
            x + pad,
            y + pad,
            Rgb([blend(r, bg[0]), blend(g, bg[1]), blend(b, bg[2])]),
        );
    }
    Some(image::DynamicImage::ImageRgb8(out))
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
pub(crate) fn normalized_cache_path(source: &Path, max_bytes: usize) -> Option<PathBuf> {
    let canonical = std::fs::canonicalize(source).ok()?;
    let freshness = source_fingerprint(&canonical)?;
    let key = fnv1a_64(freshness.as_bytes());
    // Version tag in the suffix orphans cache artifacts from older
    // processing pipelines (eviction later reclaims them) so a stale hit
    // can't re-introduce a fixed defect.  Bumps so far:
    //   .norm  → pre-flatten (transparent; dark-bg bleed)
    //   .norm2 → flattened but no padding margin
    //   .norm3 → flattened + padding margin (current)
    Some(cache_dir().join(format!("{key:016x}-{max_bytes}.norm3.png")))
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
    use image::{DynamicImage, Rgba, RgbaImage};

    /// Encode a 1x1 image of the given colour type so `png_may_have_alpha`
    /// has a real header to read.
    fn encode_png(img: DynamicImage) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    #[test]
    fn alpha_gate_skips_opaque_rgb() {
        let rgb = encode_png(DynamicImage::ImageRgb8(image::RgbImage::new(2, 2)));
        assert!(!png_may_have_alpha(&rgb), "RGB (type 2) has no alpha channel");

        let mut rgba = RgbaImage::new(2, 2);
        rgba.put_pixel(0, 0, Rgba([0, 0, 0, 128]));
        let rgba = encode_png(DynamicImage::ImageRgba8(rgba));
        assert!(png_may_have_alpha(&rgba), "RGBA (type 6) may carry alpha");
    }

    #[test]
    fn alpha_gate_treats_unknown_header_as_maybe() {
        assert!(png_may_have_alpha(b"not a png"));
    }

    #[test]
    fn flatten_composites_semi_transparent_over_white() {
        use image::GenericImageView;

        // Pure red at 50% alpha over white → (255, 128, 128) under
        // source-over with round-half-up: 255·0.5 + 255·0.5 = 255 for red,
        // 0·0.5 + 255·0.5 ≈ 128 for green/blue.  A wide-enough source so the
        // single content pixel survives at a known, unpadded coordinate.
        let mut rgba = RgbaImage::new(4, 4);
        rgba.put_pixel(2, 2, Rgba([255, 0, 0, 128]));
        let img = DynamicImage::ImageRgba8(rgba);

        let flat = flatten_alpha_over(&img, [255, 255, 255]).expect("had alpha");
        // Output is padded; the content pixel lands at (2 + pad, 2 + pad).
        let pad = (flat.width() - 4) / 2;
        let px = flat.to_rgba8().get_pixel(2 + pad, 2 + pad).0;
        assert_eq!(px[3], 255, "output must be fully opaque");
        assert_eq!(px[0], 255);
        assert!((127..=129).contains(&px[1]), "got {}", px[1]);
        assert_eq!(px[1], px[2]);

        // Corner is pure padding — the backdrop colour, untouched.
        let corner = flat.to_rgba8().get_pixel(0, 0).0;
        assert_eq!(corner, [255, 255, 255, 255], "margin should be solid backdrop");
    }

    #[test]
    fn resolve_png_flattens_transparent_source_end_to_end() {
        // The real bug: ar5iv figure assets arrive as type-6 RGBA PNGs, and
        // a fully-transparent backdrop let the dark terminal bleed through.
        // Drive the whole resolve path and assert the output carries no
        // alpha channel (colour type 0 or 2), so no backdrop can show.
        let mut rgba = RgbaImage::new(4, 4);
        rgba.put_pixel(0, 0, Rgba([20, 20, 20, 0])); // transparent dark "label"
        rgba.put_pixel(1, 1, Rgba([0, 0, 0, 128])); // semi-transparent ink
        let png = encode_png(DynamicImage::ImageRgba8(rgba));
        assert_eq!(png[25], 6, "fixture should be RGBA");

        let dir = std::env::temp_dir().join(format!("tread-flatten-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("x1.png");
        std::fs::write(&src, &png).unwrap();

        let out = resolve_png(&src).expect("resolve");
        let _ = std::fs::remove_dir_all(&dir);

        // Colour type 4 (gray+alpha) and 6 (RGBA) both retain a channel the
        // terminal could composite the dark bg into; the flattened output
        // must be one of the alpha-free types.
        assert!(
            matches!(out[25], 0 | 2),
            "flattened output should have no alpha channel, got colour type {}",
            out[25],
        );
    }

    #[test]
    fn flatten_is_noop_when_fully_opaque() {
        let mut rgba = RgbaImage::new(2, 2);
        for p in rgba.pixels_mut() {
            *p = Rgba([10, 20, 30, 255]);
        }
        let img = DynamicImage::ImageRgba8(rgba);
        assert!(
            flatten_alpha_over(&img, [255, 255, 255]).is_none(),
            "opaque input should pass through untouched",
        );
    }
}
