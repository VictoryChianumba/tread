//! Minimal PNG header parser — width/height only.
//!
//! Pulled in to avoid the `image` or `png` crate dependency just for
//! reading two `u32`s.  PNG files start with an 8-byte signature, then
//! an IHDR chunk whose first 8 data bytes are width and height (big
//! endian, per the spec).  Total bytes we need: 24.

use std::path::Path;

/// Read a PNG file's pixel dimensions (width, height) by parsing only
/// the first 24 bytes of the file.  Returns `None` on any failure
/// (file missing, not a PNG, truncated header) — caller is expected
/// to fall back to default cell footprint sizing.
pub fn dimensions(path: &Path) -> Option<(u32, u32)> {
  use std::io::Read;
  let mut buf = [0u8; 24];
  let mut f = std::fs::File::open(path).ok()?;
  f.read_exact(&mut buf).ok()?;
  // PNG signature.
  if &buf[0..8] != b"\x89PNG\r\n\x1a\n" {
    return None;
  }
  // First chunk after signature must be IHDR.  buf[8..12] = chunk length,
  // buf[12..16] = chunk type, buf[16..20] = width, buf[20..24] = height.
  if &buf[12..16] != b"IHDR" {
    return None;
  }
  let width = u32::from_be_bytes([buf[16], buf[17], buf[18], buf[19]]);
  let height = u32::from_be_bytes([buf[20], buf[21], buf[22], buf[23]]);
  if width == 0 || height == 0 {
    return None;
  }
  Some((width, height))
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::io::Write;

  /// Build a minimal valid PNG header for testing — signature + IHDR
  /// chunk with given width/height.  Body of IHDR (depth, color type,
  /// etc.) doesn't have to be valid for our header reader.
  fn synthesize_png_header(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    out.extend_from_slice(&[0, 0, 0, 13]);  // IHDR length
    out.extend_from_slice(b"IHDR");
    out.extend_from_slice(&w.to_be_bytes());
    out.extend_from_slice(&h.to_be_bytes());
    out.extend_from_slice(&[8, 6, 0, 0, 0]); // depth, color, etc.
    out
  }

  #[test]
  fn reads_width_and_height() {
    let path = std::env::temp_dir().join("kitty-graphics-test-1234x567.png");
    std::fs::File::create(&path).unwrap()
      .write_all(&synthesize_png_header(1234, 567)).unwrap();
    assert_eq!(dimensions(&path), Some((1234, 567)));
    let _ = std::fs::remove_file(&path);
  }

  #[test]
  fn rejects_non_png() {
    let path = std::env::temp_dir().join("kitty-graphics-test-not-a-png");
    std::fs::write(&path, b"hello world this is not a png").unwrap();
    assert_eq!(dimensions(&path), None);
    let _ = std::fs::remove_file(&path);
  }

  #[test]
  fn rejects_missing_file() {
    let path = std::env::temp_dir().join("kitty-graphics-does-not-exist.png");
    let _ = std::fs::remove_file(&path);
    assert_eq!(dimensions(&path), None);
  }
}
