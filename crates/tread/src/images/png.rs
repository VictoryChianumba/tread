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

pub(crate) fn encode_dynamic_image_png(
    path: &Path,
    img: &image::DynamicImage,
) -> std::io::Result<Vec<u8>> {
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
pub(crate) fn normalized_cache_path(source: &Path, max_bytes: usize) -> Option<PathBuf> {
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
