//! PDF → tread `Block` conversion.
//!
//! Wraps the `pdf_extract` crate (same dependency `cli-pdf-to-text`
//! used in trench's old reader pipeline) so a host can hand tread an
//! in-memory PDF buffer and get back the same Block tree the arxiv
//! and markdown paths produce.  Used by `PaperData::from_pdf_bytes`.
//!
//! Output strategy: one `Block::Line` per source-line.  PDF text
//! extraction is whitespace-noisy (column reflow, hyphen breaks,
//! page footers interleaved with content) — heuristic structure
//! detection (header / list / etc.) is deliberately deferred to v2.
//! Plain Block::Line preserves the user's reading order without
//! introducing parse mistakes.
//!
//! Encrypted PDFs surface a clear error rather than silently failing.

use doc_model::Block;

/// Convert PDF bytes to a tread `Block` stream.  Returns an error
/// string if the buffer doesn't parse as a PDF (corrupt, truncated,
/// password-protected, or empty).
pub fn pdf_to_blocks(bytes: &[u8]) -> Result<Vec<Block>, String> {
    if bytes.is_empty() {
        return Err("empty PDF buffer".to_string());
    }
    let text = pdf_extract::extract_text_from_mem(bytes)
        .map_err(|e| format!("PDF extraction failed: {e}"))?;
    Ok(text_to_blocks(&text))
}

/// Split extracted text into `Block::Line`s.  Trailing whitespace is
/// trimmed because pdf_extract often leaves trailing spaces from
/// column-aligned text; leading whitespace is kept so list/code
/// indentation survives.
fn text_to_blocks(text: &str) -> Vec<Block> {
    text.split('\n')
        .map(|line| Block::Line(line.trim_end().to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buffer_errors_clearly() {
        let err = pdf_to_blocks(&[]).unwrap_err();
        assert!(err.contains("empty"), "unexpected error: {err}");
    }

    #[test]
    fn invalid_bytes_error_propagates() {
        let err = pdf_to_blocks(b"not a pdf file at all").unwrap_err();
        assert!(
            err.contains("PDF extraction failed"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn text_to_blocks_preserves_line_count() {
        let blocks = text_to_blocks("first\nsecond\nthird");
        assert_eq!(blocks.len(), 3);
        if let Block::Line(s) = &blocks[0] {
            assert_eq!(s, "first");
        } else {
            panic!("expected Block::Line");
        }
    }

    #[test]
    fn trailing_whitespace_trimmed() {
        let blocks = text_to_blocks("hello   \nworld  ");
        if let Block::Line(s) = &blocks[0] {
            assert_eq!(s, "hello");
        }
        if let Block::Line(s) = &blocks[1] {
            assert_eq!(s, "world");
        }
    }

    #[test]
    fn leading_whitespace_kept() {
        // Indentation survives so list-like or code-like content keeps
        // its structure.
        let blocks = text_to_blocks("    indented");
        if let Block::Line(s) = &blocks[0] {
            assert_eq!(s, "    indented");
        }
    }
}
