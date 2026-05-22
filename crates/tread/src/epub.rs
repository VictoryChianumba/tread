//! EPUB → tread `Block` conversion.
//!
//! Wraps the `epub` crate (same dep `cli-epub-to-text` used) +
//! tread's own `html` module so chapter XHTML walks through the
//! same DOM walker as web HTML — meaning headings / lists / code
//! blocks / blockquotes / inline styles all render structured at
//! the terminal's actual width, not pre-wrapped to a fixed 110.
//!
//! Output strategy: each spine item (chapter) becomes
//!   - `Block::Header { level: 1, text: <chapter title or fallback> }`
//!   - the blocks emitted by `html::html_to_blocks` for that chapter
//!   - `Block::Blank` between chapters
//!
//! Chapter titles come from the EPUB navigation table when we can
//! match the spine `idref` to a `NavPoint`; otherwise fall back to
//! the idref string.

use std::collections::HashMap;
use std::io::Cursor;

use doc_model::Block;
use epub::doc::EpubDoc;

/// Convert EPUB bytes to a tread `Block` stream.  Errors when the
/// buffer isn't a valid EPUB, can't be unzipped, or contains no
/// extractable content.  Encrypted EPUBs surface as parse errors.
pub fn epub_to_blocks(bytes: &[u8]) -> Result<Vec<Block>, String> {
    if bytes.is_empty() {
        return Err("empty EPUB buffer".to_string());
    }
    let cursor = Cursor::new(bytes.to_vec());
    let mut doc = EpubDoc::from_reader(cursor).map_err(|e| format!("EPUB parse failed: {e}"))?;

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
        let xhtml = match std::str::from_utf8(&resource) {
            Ok(s) => s,
            Err(_) => continue, // non-UTF-8 chapter — skip rather than fail whole doc
        };

        let chapter_blocks = crate::html::html_to_blocks(xhtml);
        if chapter_blocks.is_empty() {
            continue;
        }

        let header_text = titles.get(idref).cloned().unwrap_or_else(|| idref.clone());
        out.push(Block::Header {
            level: 1,
            text: header_text,
            number: None,
        });
        out.push(Block::Blank);
        out.extend(chapter_blocks);
        // Blank between chapters.  Consecutive Blank blocks render as
        // one visible empty row, so this is safe even if the chapter
        // ended with a blank.
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
    fn visit(points: &[epub::doc::NavPoint], out: &mut HashMap<String, String>) {
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
