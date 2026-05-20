//! PDF → PNG rasterisation via Poppler's `pdftoppm`.
//!
//! Many arXiv papers ship vector figures as PDFs (TikZ output, matplotlib
//! PDF backend, exported Inkscape diagrams).  The Kitty graphics protocol
//! only accepts raster formats, so we rasterise once into a session cache
//! and reuse the PNG for the rest of the run.
//!
//! `pdftoppm` is part of Poppler — same dependency as `pdftotext`, which
//! the reader already shells to for table-anchor extraction.  If a user
//! has the latter, they almost certainly have the former.
//!
//! Cache layout: `<cache_dir>/<fnv1a-of-canonical-path>-1.png`.  Paths
//! are hashed (not URL-encoded) so the filename is bounded and safe on
//! every filesystem; the `-1` suffix is `pdftoppm`'s own page-number
//! convention, which we leave as-is so a future enhancement to render
//! page 2+ doesn't have to migrate the cache layout.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

/// Render resolution.  150 DPI is the matplotlib/seaborn default and
/// roughly matches retina cell sizes — pixelation kicks in only at very
/// large terminal windows where users can re-export at higher DPI by
/// hand.
const RENDER_DPI: u32 = 150;

/// Rasterise the first page of `input` (a PDF) into a PNG inside
/// `cache_dir`, returning the PNG path.
///
/// Cached by an FNV-1a hash of the canonical input path — repeated
/// calls for the same PDF return immediately without re-running
/// `pdftoppm`.  Errors if Poppler isn't installed, the input isn't a
/// readable PDF, or the rasteriser doesn't produce the expected file.
pub fn pdf_to_png(input: &Path, cache_dir: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(cache_dir)?;
    let key = cache_key(input);
    let target = cache_dir.join(format!("{key}-1.png"));
    if target.exists() {
        return Ok(target);
    }
    // pdftoppm takes an output *prefix* (no extension) and appends
    // "-<page>.png" itself.  So we pass `<cache>/<key>` and read back
    // `<cache>/<key>-1.png`.
    let prefix = cache_dir.join(&key);
    let status = Command::new("pdftoppm")
        .arg("-png")
        .arg("-r")
        .arg(RENDER_DPI.to_string())
        .arg("-f")
        .arg("1")
        .arg("-l")
        .arg("1")
        .arg(input)
        .arg(&prefix)
        .status()
        .map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("failed to spawn pdftoppm (is poppler installed?): {e}"),
            )
        })?;
    if !status.success() {
        return Err(io::Error::other(format!("pdftoppm exited with {status}")));
    }
    if !target.exists() {
        return Err(io::Error::other(format!(
            "pdftoppm completed but expected output missing: {}",
            target.display()
        )));
    }
    Ok(target)
}

/// Read the first page's pixel `(width, height)` **without rasterising**,
/// by asking `pdfinfo` for the page size in points and scaling by the
/// same `RENDER_DPI` `pdf_to_png` uses.  The reported dimensions match
/// the PNG a later lazy rasterisation will produce — what layout needs
/// is the aspect ratio, and points→pixels is a uniform scale that
/// preserves it.
///
/// This is the cheap companion to `pdf_to_png`: the layout pass needs a
/// figure's dimensions up front to reserve its footprint, but rendering
/// every PDF figure eagerly via `pdftoppm` costs seconds on figure-heavy
/// papers.  `pdfinfo` reads structure only and returns in milliseconds,
/// so the expensive rasterisation can stay lazy (on first scroll into
/// view) while layout is still aspect-correct.
///
/// We read the **MediaBox**, not `pdfinfo`'s top-line "Page size" — that
/// summary reports the CropBox, but `pdf_to_png` invokes `pdftoppm`
/// without `-cropbox`, so the rasteriser renders the full MediaBox.
/// Keying off the MediaBox keeps these dims identical (to rounding) with
/// the eventual PNG; the CropBox can differ in both scale and aspect
/// (e.g. a 960×540 MediaBox cropped to 786×404), which would distort the
/// reserved footprint.
///
/// `pdfinfo` ships in the same Poppler package as the `pdftoppm` this
/// module already depends on.  Returns `None` when it isn't installed,
/// the input isn't a readable PDF, or the MediaBox line can't be parsed;
/// callers then fall back to a default footprint, exactly as for any
/// other dimension-less image.
pub fn pdf_page_dims(input: &Path) -> Option<(u32, u32)> {
    let output = Command::new("pdfinfo").arg("-box").arg(input).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let (w_pts, h_pts) = parse_mediabox_pts(&stdout)?;
    let scale = RENDER_DPI as f64 / 72.0;
    let w = (w_pts * scale).round() as u32;
    let h = (h_pts * scale).round() as u32;
    if w == 0 || h == 0 {
        return None;
    }
    Some((w, h))
}

/// Parse the `MediaBox: <x0> <y0> <x1> <y1>` line from `pdfinfo -box`
/// output, returning `(x1 - x0, y1 - y0)` in points.  Coordinates are
/// fractional in general (`pdfinfo -box` prints two decimals), so parse
/// as `f64`.  Returns `None` when the line is absent or malformed.
fn parse_mediabox_pts(pdfinfo_stdout: &str) -> Option<(f64, f64)> {
    let after_label = pdfinfo_stdout
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("MediaBox:"))?;
    let mut nums = after_label
        .split_whitespace()
        .filter_map(|tok| tok.parse::<f64>().ok());
    let x0 = nums.next()?;
    let y0 = nums.next()?;
    let x1 = nums.next()?;
    let y1 = nums.next()?;
    let (w, h) = (x1 - x0, y1 - y0);
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    Some((w, h))
}

/// FNV-1a 64-bit hash of the canonicalised input path mixed with the
/// source file's size and mtime, so a refreshed PDF at the same path
/// doesn't keep serving the older rasterised PNG forever.  Stable
/// across std versions and processes — unlike `DefaultHasher`, which
/// std reserves the right to swap out.  Falls back to the raw path if
/// `canonicalize` fails (e.g. file not yet created); falls back to
/// path-only if `metadata` fails, so the function never panics and
/// existing synthetic-path tests still produce deterministic keys.
fn cache_key(input: &Path) -> String {
    let canonical = std::fs::canonicalize(input).unwrap_or_else(|_| input.to_path_buf());
    let path_bytes = canonical.as_os_str().to_string_lossy();
    let freshness = freshness_token(&canonical);
    format!(
        "{:016x}",
        fnv1a_64(format!("{path_bytes}|{freshness}").as_bytes())
    )
}

/// `len|mtime_secs|mtime_subsec_nanos` for cache-key freshness mixing.
/// Returns an empty string when metadata is unavailable so callers stay
/// infallible — a stat failure just collapses the key to path-only,
/// which matches prior behaviour.
fn freshness_token(path: &Path) -> String {
    let Ok(meta) = std::fs::metadata(path) else {
        return String::new();
    };
    let modified = meta
        .modified()
        .ok()
        .and_then(|ts| ts.duration_since(UNIX_EPOCH).ok())
        .unwrap_or_default();
    format!(
        "{}|{}|{}",
        meta.len(),
        modified.as_secs(),
        modified.subsec_nanos(),
    )
}

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

    #[test]
    fn fnv1a_known_values() {
        // FNV-1a 64 reference vectors.
        assert_eq!(fnv1a_64(b""), 0xcbf29ce484222325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63dc4c8601ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x85944171f73967e8);
    }

    #[test]
    fn parse_mediabox_full_page() {
        let out = "Page size:       786.05 x 403.634 pts\n\
                   MediaBox:            0.00     0.00   960.00   540.00\n\
                   CropBox:           103.62    54.93   889.67   458.56\n";
        // MediaBox wins over the CropBox-derived "Page size" line.
        assert_eq!(parse_mediabox_pts(out), Some((960.0, 540.0)));
    }

    #[test]
    fn parse_mediabox_offset_origin() {
        // Non-zero origin: dims are the box extent, not the far corner.
        let out = "MediaBox:           10.00    20.00   110.00   220.00\n";
        assert_eq!(parse_mediabox_pts(out), Some((100.0, 200.0)));
    }

    #[test]
    fn parse_mediabox_absent_returns_none() {
        assert_eq!(parse_mediabox_pts("Pages:           1\n"), None);
    }

    #[test]
    fn parse_mediabox_degenerate_returns_none() {
        assert_eq!(parse_mediabox_pts("MediaBox: 0 0 0 0\n"), None);
    }

    #[test]
    fn cache_key_is_deterministic() {
        let p = Path::new("/tmp/some/path.pdf");
        assert_eq!(cache_key(p), cache_key(p));
    }

    #[test]
    fn cache_key_distinguishes_paths() {
        assert_ne!(
            cache_key(Path::new("/tmp/a.pdf")),
            cache_key(Path::new("/tmp/b.pdf")),
        );
    }

    #[test]
    fn cache_key_changes_when_source_metadata_changes() {
        use std::time::SystemTime;
        let temp = std::env::temp_dir().join(format!(
            "tread-pdf-cache-key-{}-{}.pdf",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::write(&temp, b"one").unwrap();
        let first = cache_key(&temp);
        std::fs::write(&temp, b"two two two").unwrap();
        let second = cache_key(&temp);
        let _ = std::fs::remove_file(&temp);
        assert_ne!(first, second);
    }

    /// Real-PDF smoke test.  Skipped by default — run with
    /// `TREAD_PDF_SMOKE=/abs/path/to/some.pdf cargo test -p kitty-graphics
    /// -- --ignored real_pdf_smoke --nocapture` to verify the full
    /// pdftoppm pipeline against a real file on disk.
    #[test]
    #[ignore]
    fn real_pdf_smoke() {
        let Ok(input) = std::env::var("TREAD_PDF_SMOKE") else {
            panic!("set TREAD_PDF_SMOKE=/path/to/some.pdf");
        };
        let cache = std::env::temp_dir().join("tread-pdf-smoke-test");
        let _ = std::fs::remove_dir_all(&cache);
        let png = pdf_to_png(Path::new(&input), &cache).expect("conversion");
        assert!(png.exists(), "PNG should exist at {}", png.display());
        assert!(
            png.metadata().unwrap().len() > 100,
            "PNG should not be empty"
        );
        eprintln!(
            "OK: {} bytes at {}",
            png.metadata().unwrap().len(),
            png.display()
        );
        // Second call should hit the cache without re-running pdftoppm.
        let png2 = pdf_to_png(Path::new(&input), &cache).expect("cached conversion");
        assert_eq!(png, png2);
    }
}
