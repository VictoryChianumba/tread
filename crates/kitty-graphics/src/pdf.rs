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
    .arg("-r").arg(RENDER_DPI.to_string())
    .arg("-f").arg("1")
    .arg("-l").arg("1")
    .arg(input)
    .arg(&prefix)
    .status()
    .map_err(|e| io::Error::new(
      e.kind(),
      format!("failed to spawn pdftoppm (is poppler installed?): {e}"),
    ))?;
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

/// FNV-1a 64-bit hash of the canonicalised input path.  Stable across
/// std versions and processes — unlike `DefaultHasher`, which std
/// reserves the right to swap out.  Falls back to the raw path if
/// `canonicalize` fails (e.g. file not yet created), so the function
/// never panics on hashing.
fn cache_key(input: &Path) -> String {
  let canonical = std::fs::canonicalize(input).unwrap_or_else(|_| input.to_path_buf());
  let bytes = canonical.as_os_str().to_string_lossy();
  format!("{:016x}", fnv1a_64(bytes.as_bytes()))
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
    assert!(png.metadata().unwrap().len() > 100, "PNG should not be empty");
    eprintln!("OK: {} bytes at {}", png.metadata().unwrap().len(), png.display());
    // Second call should hit the cache without re-running pdftoppm.
    let png2 = pdf_to_png(Path::new(&input), &cache).expect("cached conversion");
    assert_eq!(png, png2);
  }
}
