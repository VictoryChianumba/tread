//! Extract per-table placement anchors from a PDF.
//!
//! Tables in LaTeX are floating environments — the source position where
//! `\begin{table}` is written rarely matches the visual position the
//! typesetter chooses in the rendered PDF.  Pandoc preserves source order,
//! so our parsed `Block::Matrix` groups land at the wrong spot.  This
//! module recovers the *rendered* position by reading the paper's PDF as
//! plain text and locating each `Table N:` caption inside the prose flow.
//!
//! The returned anchors carry a fingerprint of the prose paragraph that
//! immediately precedes each caption in reading order; the placement pass
//! (in `placement.rs`) then matches that fingerprint against our parsed
//! blocks and lifts the corresponding Matrix group to that position.
//!
//! Depends on `pdftotext` (Poppler, `brew install poppler`).  Falls back
//! gracefully — if the binary is missing or the PDF can't be parsed, we
//! return an empty Vec and the placement pass becomes a no-op.

use std::io::Write;
use std::process::{Command, Stdio};

/// One table's placement signal extracted from the PDF.
#[derive(Debug, Clone)]
pub struct TableAnchor {
  /// Number from the `Table N:` caption — matches the Nth Matrix group
  /// emitted by our parser (both walk the document body in source order).
  pub table_number: usize,
  /// 64-bit hash of the first ~7 ASCII-only lowercased words of the prose
  /// paragraph that immediately precedes the caption in PDF reading order.
  /// Used by the placement pass to locate the same paragraph in our block
  /// list.
  pub anchor_fingerprint: u64,
  /// Human-readable copy of the same words.  Diagnostics only.
  pub anchor_preview: String,
}

/// Extract anchors from PDF bytes.  Returns empty Vec on any failure
/// (missing `pdftotext`, malformed PDF, no captions found) — callers
/// should treat that as "no placement signal available".
pub fn extract_anchors(pdf_bytes: &[u8]) -> Vec<TableAnchor> {
  let text = match pdf_to_text(pdf_bytes) {
    Ok(t) => t,
    Err(_) => return Vec::new(),
  };
  parse_anchors(&text)
}

/// Pipe `pdf_bytes` to `pdftotext -layout - -` and capture stdout.
/// `-layout` keeps two-column papers flowing left-then-right per page,
/// which is what we want for sequential prose extraction.
fn pdf_to_text(pdf_bytes: &[u8]) -> Result<String, String> {
  let mut child = Command::new("pdftotext")
    .args(["-layout", "-", "-"])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .spawn()
    .map_err(|e| format!("failed to spawn pdftotext: {e}"))?;

  if let Some(mut stdin) = child.stdin.take() {
    stdin
      .write_all(pdf_bytes)
      .map_err(|e| format!("failed to pipe PDF to pdftotext: {e}"))?;
  }

  let output = child
    .wait_with_output()
    .map_err(|e| format!("pdftotext wait error: {e}"))?;
  if !output.status.success() {
    return Err(format!("pdftotext exited with status {}", output.status));
  }
  String::from_utf8(output.stdout)
    .map_err(|e| format!("pdftotext output not UTF-8: {e}"))
}

fn parse_anchors(text: &str) -> Vec<TableAnchor> {
  let lines: Vec<&str> = text.lines().collect();
  let mut out = Vec::new();

  for (idx, line) in lines.iter().enumerate() {
    if let Some(n) = parse_table_caption_line(line) {
      if let Some((preview, hash)) = anchor_before(&lines, idx) {
        out.push(TableAnchor {
          table_number: n,
          anchor_fingerprint: hash,
          anchor_preview: preview,
        });
      }
    }
  }
  out
}

/// If `line` begins with `Table N:` (after optional leading whitespace),
/// return `Some(N)`.  Otherwise `None`.
fn parse_table_caption_line(line: &str) -> Option<usize> {
  let trimmed = line.trim_start();
  let rest = trimmed.strip_prefix("Table ")?;
  let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
  if digits.is_empty() {
    return None;
  }
  let after = &rest[digits.len()..];
  if !after.starts_with(':') {
    return None;
  }
  digits.parse().ok()
}

/// Walk backwards from `idx` skipping blank lines, page numbers, footnote
/// leak-through, and numbered section headings.  Return the first
/// multi-word prose line as `(preview, fingerprint)`.
///
/// We fingerprint the *last* line of the paragraph that ends right before
/// the caption — i.e. mid-paragraph content, not the paragraph start.
/// `pdftotext` gives us no reliable inter-paragraph blank lines in body
/// prose, so we cannot walk further back to the paragraph's first line.
/// The placement pass works around this by performing **substring
/// matching**: the snippet from the PDF's last line will appear verbatim
/// inside the LaTeX-parsed paragraph that owns it.
fn anchor_before(lines: &[&str], idx: usize) -> Option<(String, u64)> {
  let mut i = idx;
  while i > 0 {
    i -= 1;
    if is_skippable_noise(lines[i]) {
      continue;
    }
    let words = first_n_words(lines[i], 7);
    if words.split_whitespace().count() < 3 {
      // Single short tokens aren't real anchors — keep walking back.
      continue;
    }
    let hash = fingerprint(&words);
    return Some((words, hash));
  }
  None
}

/// Maximum leading whitespace allowed on an anchor line.  In `pdftotext`
/// `-layout` mode body prose is flush-left or near-flush; significantly
/// indented lines are footnote bodies, table cells, or centered text —
/// all unsuitable as anchors.
const MAX_ANCHOR_INDENT: usize = 4;

/// True for lines that aren't real anchor candidates.  Catches:
/// - blanks
/// - lone page numbers (1–4 digits alone)
/// - footnote leakage (`5 We used values…`)
/// - numbered section headings (`5.4 Regularization`, `6 Results`)
/// - heavily indented lines (footnote bodies whose digit marker landed on
///   a previous line under `-layout` extraction)
fn is_skippable_noise(line: &str) -> bool {
  let s = line.trim();
  if s.is_empty() {
    return true;
  }
  // Page number: 1–4 digits, nothing else.
  if s.len() <= 4 && s.chars().all(|c| c.is_ascii_digit()) {
    return true;
  }
  // Heavy leading indent → not body prose.
  let leading = line.chars().take_while(|c| *c == ' ').count();
  if leading > MAX_ANCHOR_INDENT {
    return true;
  }
  // Numbered prefix followed by whitespace: footnote OR section heading.
  let mut chars = s.chars();
  let first = match chars.next() {
    Some(c) => c,
    None => return false,
  };
  if !first.is_ascii_digit() {
    return false;
  }
  let mut consumed = 1;
  for c in chars.by_ref() {
    if c.is_ascii_digit() || c == '.' {
      consumed += 1;
      if consumed > 6 { return false; }
      continue;
    }
    return c.is_whitespace();
  }
  false
}

/// First `n` ASCII-letter words of `line`, lowercased and space-joined.
/// Drops digits, punctuation, and non-ASCII so PDF/LaTeX renderings of the
/// same paragraph (which differ in math, refs, accents) yield the same
/// fingerprint.
pub(crate) fn first_n_words(line: &str, n: usize) -> String {
  let mut out = Vec::with_capacity(n);
  let mut cur = String::new();
  for ch in line.chars() {
    if ch.is_ascii_alphabetic() {
      cur.push(ch.to_ascii_lowercase());
    } else if !cur.is_empty() {
      out.push(std::mem::take(&mut cur));
      if out.len() == n { break; }
    }
  }
  if !cur.is_empty() && out.len() < n {
    out.push(cur);
  }
  out.join(" ")
}

/// Stable 64-bit FNV-1a hash.  Sufficient for "find the matching paragraph
/// in this document" — papers have ~hundreds of paragraphs, collisions at
/// 64 bits are negligible.
pub(crate) fn fingerprint(s: &str) -> u64 {
  let mut h: u64 = 0xcbf29ce484222325;
  for b in s.as_bytes() {
    h ^= *b as u64;
    h = h.wrapping_mul(0x100000001b3);
  }
  h
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_caption_line() {
    assert_eq!(parse_table_caption_line("Table 1: Maximum path lengths"), Some(1));
    assert_eq!(parse_table_caption_line("  Table 42: Foo"), Some(42));
    assert_eq!(parse_table_caption_line("Table 2 summarizes our results"), None);
    assert_eq!(parse_table_caption_line("Tablet 1: nope"), None);
    assert_eq!(parse_table_caption_line("Table: missing number"), None);
  }

  #[test]
  fn skippable_noise_detection() {
    // Page numbers
    assert!(is_skippable_noise("7"));
    assert!(is_skippable_noise("  42  "));
    assert!(!is_skippable_noise("12345")); // 5 digits — too long for a page number

    // Footnote leakage (works regardless of capitalization of body text)
    assert!(is_skippable_noise("5 We used values of 2.8, 3.7, 6.0 and 9.5 TFLOPS"));
    assert!(is_skippable_noise("12 the lowercase prefix also matches"));

    // Numbered section headings
    assert!(is_skippable_noise("5.4 Regularization"));
    assert!(is_skippable_noise("6 Results"));
    assert!(is_skippable_noise("6.2 Model Variations"));

    // Real prose — must not be skipped
    assert!(!is_skippable_noise("We employ three types of regularization"));
    assert!(!is_skippable_noise("To evaluate the importance of different components"));

    // Edge cases
    assert!(is_skippable_noise(""));
    assert!(is_skippable_noise("   "));
  }

  #[test]
  fn first_words_normalize() {
    assert_eq!(
      first_n_words("We employ three types of regularization during training:", 5),
      "we employ three types of"
    );
    assert_eq!(
      first_n_words("§3.4 Section header — with weird chars", 4),
      "section header with weird"
    );
  }

  #[test]
  fn anchor_walks_back_past_noise() {
    let text = "\
We employ three types of regularization during training:

7

Table 2: The Transformer achieves better BLEU scores
";
    let anchors = parse_anchors(text);
    assert_eq!(anchors.len(), 1);
    assert_eq!(anchors[0].table_number, 2);
    assert!(anchors[0].anchor_preview.starts_with("we employ three"));
  }

  /// Smoke test against the actual Attention paper PDF.  Run with:
  ///   ATTENTION_PDF=/tmp/tread-pdf-investigation/attention.pdf \
  ///     cargo test -p arxiv-render attention_pdf_anchors -- --ignored --nocapture
  /// Skipped by default because it requires both the PDF on disk and
  /// `pdftotext` installed.
  #[test]
  #[ignore]
  fn attention_pdf_anchors() {
    let path = std::env::var("ATTENTION_PDF").expect("ATTENTION_PDF unset");
    let bytes = std::fs::read(&path).expect("read PDF");
    let anchors = extract_anchors(&bytes);
    println!("found {} anchor(s):", anchors.len());
    for a in &anchors {
      println!("  Table {} → 0x{:016x}  «{}»",
        a.table_number, a.anchor_fingerprint, a.anchor_preview);
    }
    // Attention is All You Need has 4 tables.
    assert_eq!(anchors.len(), 4, "expected 4 tables in 1706.03762");
    let nums: Vec<usize> = anchors.iter().map(|a| a.table_number).collect();
    assert_eq!(nums, vec![1, 2, 3, 4], "expected captions in numerical order");
  }

  #[test]
  fn anchor_skips_footnote_leak() {
    let text = "\
To evaluate the importance of different components of the Transformer

5 We used values of 2.8, 3.7, 6.0 and 9.5 TFLOPS for K80, K40, M40 and P100, respectively

8

Table 3: Variations on the Transformer architecture
";
    let anchors = parse_anchors(text);
    assert_eq!(anchors.len(), 1);
    assert_eq!(anchors[0].table_number, 3);
    assert!(anchors[0].anchor_preview.starts_with("to evaluate the importance"));
  }
}
