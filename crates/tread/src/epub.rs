//! EPUB → tread `Block` conversion.
//!
//! Wraps the same `epub` + `html2text` crates the old
//! `cli-epub-to-text` used.  `epub::EpubDoc::from_reader` accepts a
//! `Cursor<Vec<u8>>`, so the host can pass an in-memory buffer with
//! no temp-file dancing — same shape as `from_pdf_bytes`.
//!
//! Output strategy: each spine item (chapter) becomes
//!   - `Block::Header { level: 1, text: <chapter title or fallback> }`
//!   - `Block::Line` per non-empty text line from html2text
//!   - `Block::Blank` between empty-line groups (paragraph breaks)
//!   - `Block::Blank` between chapters
//!
//! `html2text` already wraps to a fixed column width — we keep that
//! width modest (110) so most terminals render without further wrap.
//! v2: feed raw HTML to `from_html` once that lands so the wrap
//! width matches the reader's pane width dynamically.
//!
//! Chapter titles come from the EPUB navigation table when we can
//! match the spine `idref` to a `NavPoint`; otherwise fall back to
//! the idref string (better than nothing and consistent with what
//! cli-epub-to-text exposed).

use std::collections::HashMap;
use std::io::Cursor;

use doc_model::Block;
use epub::doc::EpubDoc;

const HTML_WRAP_COLS: usize = 110;

/// Convert EPUB bytes to a tread `Block` stream.  Errors when the
/// buffer isn't a valid EPUB, can't be unzipped, or contains no
/// extractable content.  Encrypted EPUBs surface as parse errors.
pub fn epub_to_blocks(bytes: &[u8]) -> Result<Vec<Block>, String> {
  if bytes.is_empty() {
    return Err("empty EPUB buffer".to_string());
  }
  let cursor = Cursor::new(bytes.to_vec());
  let mut doc = EpubDoc::from_reader(cursor)
    .map_err(|e| format!("EPUB parse failed: {e}"))?;

  // Build idref → label map from the NavPoint tree so chapter
  // headers carry meaningful titles.  EPUBs that don't ship a
  // navigation map fall through to using the idref string itself.
  let titles = collect_nav_titles(&doc);

  let spine = doc.spine.clone();
  let mut out: Vec<Block> = Vec::new();

  for spine_item in &spine {
    let idref = &spine_item.idref;
    let resource = match doc.get_resource(idref) {
      Some((bytes, _)) => bytes,
      None => continue, // skip silently — non-essential or malformed entry
    };
    let text = match html2text::from_read(resource.as_slice(), HTML_WRAP_COLS) {
      Ok(t) => t,
      Err(_) => continue, // skip an unparseable chapter rather than failing whole doc
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
      continue;
    }

    let header_text = titles
      .get(idref)
      .cloned()
      .unwrap_or_else(|| idref.clone());
    out.push(Block::Header { level: 1, text: header_text });
    out.push(Block::Blank);

    // Convert one chapter's text into Line/Blank blocks.  Sequences
    // of blank input lines collapse to a single Block::Blank so
    // paragraph spacing stays even.
    let mut last_was_blank = false;
    for line in trimmed.lines() {
      let stripped = line.trim_end();
      if stripped.is_empty() {
        if !last_was_blank {
          out.push(Block::Blank);
          last_was_blank = true;
        }
      } else {
        out.push(Block::Line(stripped.to_string()));
        last_was_blank = false;
      }
    }
    // Blank between chapters.  If the last block we emitted was
    // already a blank we still want a clear chapter break, so push
    // unconditionally — render-time consecutive blanks render as
    // one visible empty row.
    out.push(Block::Blank);
  }

  if out.is_empty() {
    return Err("no readable content in EPUB spine".to_string());
  }
  Ok(out)
}

/// Walk the EPUB's NavPoint tree and collect a map of spine `idref`
/// → human-readable label so chapter headers aren't bare opaque ids.
/// NavPoint URLs include the resource href; we map both the URL's
/// file portion (which usually matches an idref) and the raw idref
/// so lookups are flexible.
fn collect_nav_titles<R: std::io::Read + std::io::Seek>(
  doc: &EpubDoc<R>,
) -> HashMap<String, String> {
  fn visit(
    points: &[epub::doc::NavPoint],
    out: &mut HashMap<String, String>,
  ) {
    for np in points {
      let url = np.content.to_string_lossy().to_string();
      // Strip fragment and any leading directory so we can match
      // against the spine's idref-shaped strings.
      let key = url.split('#').next().unwrap_or(&url);
      let key = key.rsplit('/').next().unwrap_or(key);
      // Strip extension as a fuzzy fallback for idref matching.
      let key_no_ext = key.rsplit_once('.').map(|(s, _)| s).unwrap_or(key);
      out.insert(key.to_string(), np.label.clone());
      out.insert(key_no_ext.to_string(), np.label.clone());
      visit(&np.children, out);
    }
  }
  let mut map = HashMap::new();
  visit(&doc.toc, &mut map);
  map
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn empty_buffer_errors_clearly() {
    let err = epub_to_blocks(&[]).unwrap_err();
    assert!(err.contains("empty"));
  }

  #[test]
  fn invalid_bytes_error_propagates() {
    let err = epub_to_blocks(b"not an epub at all").unwrap_err();
    assert!(err.contains("parse failed"), "unexpected error: {err}");
  }
}
