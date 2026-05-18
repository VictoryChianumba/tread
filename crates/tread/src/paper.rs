//! Paper ingestion: `PaperData` value, the per-source builders
//! (`from_*`), and the URL-driven entry point (`fetch_any`).  Extracted
//! from `lib.rs` so the crate root only carries the public-surface API
//! and the runtime wiring; the parsing / fetching plumbing lives here.

use crate::{bench, epub, html, markdown, pdf};
use doc_model::Block;
use std::collections::HashMap;

/// What `fetch_paper` returns: everything the reader needs to build/refresh
/// its block tree and bib lookups for one arXiv paper.  The `asset_dir`
/// is kept on disk for image lookups — the reader doesn't need to track it
/// explicitly, but holding the value here lets callers know it exists.
pub struct PaperData {
  pub blocks: Vec<Block>,
  pub bibitems: HashMap<String, String>,
  pub asset_dir: std::path::PathBuf,
}

impl PaperData {
  /// Build a `PaperData` from pre-extracted plain-text lines.  Used by
  /// host TUIs (trench) to embed tread for non-arXiv content sources —
  /// PDF / EPUB / HTML / local files where the host has already done
  /// the extraction and only needs a typed reader on the resulting
  /// `Vec<String>`.  Each input line becomes one `Block::Line`; no
  /// math, no tables, no images, no cross-refs.  `:reload`, `:cite`,
  /// `:url`, `:open` are no-ops with a sensible error (they all
  /// require an arxiv_id, which `Reader::init` leaves unset for this
  /// path — pass `progress_key = None`).
  ///
  /// All non-prose Reader features still work: navigation, marks,
  /// highlights, search, themes, voice, text objects.
  pub fn from_plain_lines(lines: Vec<String>) -> Self {
    Self {
      blocks: lines.into_iter().map(Block::Line).collect(),
      bibitems: HashMap::new(),
      asset_dir: std::path::PathBuf::new(),
    }
  }

  /// Build a `PaperData` from a markdown source string.  Headers,
  /// lists, code blocks, blockquotes, links, bold/italic/inline-code
  /// styling, and rules all map to their `doc-model::Block` /
  /// `InlineSpan` equivalents.  Unlocks GitHub READMEs, generic
  /// markdown READMEs, blog posts that ship as `.md`, and any host
  /// content already in markdown form.
  ///
  /// Out of scope (deliberately):
  /// - Math: markdown has no standard syntax; mixing TeX with
  ///   shell-prompt-style code samples in fences is fragile.  v2
  ///   if needed.
  /// - Image fetching: tread doesn't speak HTTP for image bodies, so
  ///   `![alt](url)` degrades to an italic `[Image: alt]` placeholder.
  ///   Hosts that want pixel images should pre-fetch and inject via
  ///   the arxiv-style image_paths flow (v2).
  /// - Tables: pulldown-cmark emits them but tread's current Matrix
  ///   block expects LaTeX-style cells; v2 backlog.
  pub fn from_markdown(md: &str) -> Self {
    Self {
      blocks: markdown::parse(md),
      bibitems: HashMap::new(),
      asset_dir: std::path::PathBuf::new(),
    }
  }

  /// Build a `PaperData` from in-memory PDF bytes.  Wraps the
  /// `pdf_extract` crate (same dep `cli-pdf-to-text` used in trench's
  /// old pipeline) so a host can pass bytes from its HTTP layer
  /// without writing to disk.  Each output line maps to a
  /// `Block::Line` — heuristic structure detection (headers, lists,
  /// columns) is deferred to v2 to avoid mis-parsing reading order.
  ///
  /// Errors when the buffer is empty, corrupt, or password-protected.
  /// Hosts can fall back to `from_plain_lines` with a notification.
  pub fn from_pdf_bytes(bytes: &[u8]) -> Result<Self, String> {
    let blocks = pdf::pdf_to_blocks(bytes)?;
    Ok(Self {
      blocks,
      bibitems: HashMap::new(),
      asset_dir: std::path::PathBuf::new(),
    })
  }

  /// Build a `PaperData` from in-memory EPUB bytes.  Wraps the
  /// `epub` and `html2text` crates (same deps `cli-epub-to-text`
  /// used) so a host can pass a buffer fetched from any source.
  /// Each spine item becomes a chapter with a `Block::Header` (using
  /// the EPUB's NavPoint label when available, else the idref),
  /// followed by line-by-line content.  Paragraph breaks are
  /// preserved as `Block::Blank`.
  ///
  /// Out of scope (v2 backlog):
  /// - Inline images: EPUB images live as separate manifest
  ///   resources, would need integration with the image_paths flow.
  /// - Encryption: surfaces as parse error; tread doesn't support
  ///   DRM key handling.
  /// - Adaptive wrap: html2text wraps at a fixed 110 cols today;
  ///   v2 routes raw HTML through `from_html` once that lands.
  pub fn from_epub_bytes(bytes: &[u8]) -> Result<Self, String> {
    let blocks = epub::epub_to_blocks(bytes)?;
    Ok(Self {
      blocks,
      bibitems: HashMap::new(),
      asset_dir: std::path::PathBuf::new(),
    })
  }

  /// Build a `PaperData` from an HTML document.  Walks the parsed
  /// DOM (via `html5ever` through the `scraper` crate) and emits
  /// the same Block tree the other paths produce — headers, lists,
  /// code blocks, blockquotes, images-as-placeholders, links carry
  /// their `href` on the span, inline bold/italic/monospace/strike.
  ///
  /// Hosts that pre-clean noisy pages with `readability` should
  /// pass the cleaned HTML here.  Tread doesn't error on malformed
  /// HTML — html5ever applies the standard browser repair, so at
  /// worst this returns an empty Vec.
  ///
  /// Out of scope (v2 backlog):
  /// - Tables: render flattened (one row per paragraph) rather
  ///   than as Block::Matrix; cell-shape conversion is its own task.
  /// - Inline images: degrade to `[Image: alt]` placeholders.
  ///   Hosts that want pixel rendering need to download bytes and
  ///   integrate with the image_paths flow.
  pub fn from_html(html: &str) -> Self {
    Self {
      blocks: html::html_to_blocks(html),
      bibitems: HashMap::new(),
      asset_dir: std::path::PathBuf::new(),
    }
  }

  /// Build a `PaperData` from a local file path.  Reads the file
  /// once, sniffs the extension (case-insensitive), and dispatches
  /// to the matching `from_*` builder.  Unknown / missing extensions
  /// fall back to `from_plain_lines` so any text file still opens.
  ///
  /// Extension dispatch:
  /// - `.md`, `.markdown`           → from_markdown
  /// - `.pdf`                       → from_pdf_bytes
  /// - `.epub`                      → from_epub_bytes
  /// - `.html`, `.htm`              → from_html
  /// - `.txt` / unknown / no ext    → from_plain_lines
  ///
  /// Errors propagate from the underlying builder (parse failures,
  /// empty buffers, …) plus an I/O error if the file can't be read.
  /// Hosts that want richer error context wrap this with their own.
  pub fn from_path(path: impl AsRef<std::path::Path>) -> Result<Self, String> {
    let path = path.as_ref();
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let ext = path
      .extension()
      .and_then(|e| e.to_str())
      .map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
      Some("pdf") => Self::from_pdf_bytes(&bytes),
      Some("epub") => Self::from_epub_bytes(&bytes),
      Some("md") | Some("markdown") => {
        let text = String::from_utf8(bytes)
          .map_err(|e| format!("{}: invalid UTF-8 ({e})", path.display()))?;
        Ok(Self::from_markdown(&text))
      }
      Some("html") | Some("htm") => {
        let text = String::from_utf8(bytes)
          .map_err(|e| format!("{}: invalid UTF-8 ({e})", path.display()))?;
        Ok(Self::from_html(&text))
      }
      _ => {
        let text = String::from_utf8(bytes)
          .map_err(|e| format!("{}: invalid UTF-8 ({e})", path.display()))?;
        let lines: Vec<String> = text.split('\n').map(|l| l.trim_end().to_string()).collect();
        Ok(Self::from_plain_lines(lines))
      }
    }
  }
}

/// Fetch + parse + post-process a paper.  Mirrors the bootstrap steps
/// in `main.rs` so both initial load and `:reload` go through the same
/// pipeline: fetch the e-print tarball, parse to blocks, resolve image
/// paths (or degrade to captions on non-graphics terminals), lift table
/// floats to PDF-rendered position.  Network-bound; runs synchronously
/// so the caller will block for ~2s on a typical paper.
pub fn fetch_paper(id: &str, kitty_supported: bool) -> Result<PaperData, String> {
  fetch_paper_inner(id, kitty_supported, false)
}

/// Same as `fetch_paper` but forces both the source tarball and the
/// rendered PDF to bypass the etag conditional cache.  Used by the
/// in-reader `:refresh` command for the case where a paper has been
/// revised on arXiv and the user wants a guaranteed-fresh copy.
pub fn fetch_paper_refresh(id: &str, kitty_supported: bool) -> Result<PaperData, String> {
  fetch_paper_inner(id, kitty_supported, true)
}

fn fetch_paper_inner(
  id: &str,
  kitty_supported: bool,
  force_refresh: bool,
) -> Result<PaperData, String> {
  use arxiv_render::{fetch, parse, pdf_anchors, placement};

  // Two pairs of independent work overlap in this scope:
  //
  //   1. Source tarball and PDF are independent HTTPS GETs against
  //      arxiv.org; total network phase = max(both) instead of sum.
  //   2. parse_to_blocks (Pandoc subprocess on the source bytes) and
  //      extract_anchors (pdftotext subprocess on the PDF bytes) are
  //      also independent — they consume different inputs and meet
  //      only at lift_tables.  Running them in parallel saves
  //      ~min(parse, anchors) ≈ 500–600ms off the warm-cache wall.
  //
  // The anchor thread starts as soon as fetch_pdf completes (it
  // joins pdf_handle from inside) and runs alongside whatever the
  // main thread is doing — fetch_source, parse, absolutize.  arXiv
  // tolerates the two-concurrent-request pattern; trench's openreview
  // audit hit the same shape.
  std::thread::scope(|s| {
    let pdf_handle = s.spawn(|| {
      bench::time("fetch_pdf", || {
        if force_refresh {
          fetch::fetch_pdf_refresh(id)
        } else {
          fetch::fetch_pdf(id)
        }
      })
    });
    let anchor_handle = s.spawn(move || {
      let pdf_result = pdf_handle
        .join()
        .unwrap_or_else(|_| Err("pdf fetch thread panicked".to_string()));
      match pdf_result {
        Ok(pdf) => bench::time("extract_anchors", || pdf_anchors::extract_anchors(&pdf)),
        Err(_) => Vec::new(),
      }
    });

    let fetched = bench::time("fetch_source", || {
      if force_refresh {
        fetch::fetch_source_refresh(id)
      } else {
        fetch::fetch_source(id)
      }
    })?;
    let sources = fetched.tex;
    let asset_dir = fetched.asset_dir;
    let bibitems = bench::time("extract_bibitems", || {
      arxiv_render::extract_bibitems(&sources)
    });
    let mut blocks = bench::time("parse_to_blocks", || parse::to_blocks(sources));
    if kitty_supported {
      bench::time("absolutize_image_paths", || {
        arxiv_render::absolutize_image_paths(&mut blocks, &asset_dir)
      });
    } else {
      arxiv_render::degrade_images_to_captions(&mut blocks);
    }
    let anchors = anchor_handle.join().unwrap_or_else(|_| Vec::new());
    let blocks = bench::time("lift_tables", || placement::lift_tables(blocks, &anchors));
    bench::emit_fields(
      "fetch_paper_done",
      &[
        ("blocks", blocks.len() as i64),
        ("bibitems", bibitems.len() as i64),
        ("anchors", anchors.len() as i64),
      ],
    );
    Ok(PaperData {
      blocks,
      bibitems,
      asset_dir,
    })
  })
}

/// Fetch a URL and parse into `PaperData`, auto-detecting the format.
/// One-call entry point for embedding hosts that don't want to manage
/// the URL → format → builder chain themselves; standalone tread
/// uses it for `tread <https://...>` invocations.
///
/// Detection order:
/// 1. arXiv URL pattern via `extract_arxiv_id` → `fetch_paper`
///    (gets the full LaTeX path with math / tables / figures).
/// 2. URL path extension (`.pdf`, `.epub`, `.md`, `.markdown`,
///    `.html`, `.htm`, `.txt`) — fast and reliable when present.
/// 3. HTTP `Content-Type` header — fallback for clean URLs that
///    don't expose a file extension.
/// 4. Default: treat the body as HTML.  Browsers do the same when
///    Content-Type is missing or generic.
///
/// Network-bound; runs synchronously like `fetch_paper` does.
/// Hosts that need non-blocking ingestion should background this
/// on a worker thread (see trench's existing fulltext pipeline).
pub fn fetch_any(url: &str) -> Result<PaperData, String> {
  // 1. arXiv fast path.  fetch_paper handles its own HTTP and
  // returns a richer PaperData (bibitems, asset_dir for images).
  if let Some(id) = crate::extract_arxiv_id(url) {
    let kitty_supported = matches!(
      kitty_graphics::detect(),
      kitty_graphics::Capability::Supported
    );
    return fetch_paper(&id, kitty_supported);
  }

  // 2. URL extension sniff before the HTTP roundtrip.
  let path_part = url.split('?').next().unwrap_or(url);
  let ext = std::path::Path::new(path_part)
    .extension()
    .and_then(|e| e.to_str())
    .map(|e| e.to_ascii_lowercase());

  // 3. Fetch the body.  Defenses against pathological servers (C5):
  // - 30s timeout matches arxiv-render's fetch — long enough for slow
  //   academic mirrors, short enough to avoid hanging the TUI when a
  //   server stalls mid-transfer.
  // - Redirect cap matches reqwest's default (10) but stated
  //   explicitly so future reqwest version bumps can't surprise us.
  // - Body cap: take (MAX + 1) bytes via the Read implementation, then
  //   reject if we got more than MAX — distinguishes "body exactly
  //   equals MAX" (allowed) from "server streaming more than we'll
  //   accept" (rejected before OOM).  Doesn't help against a server
  //   that streams forever AT a reasonable rate within the timeout;
  //   for that we rely on the timeout to fire.
  const FETCH_ANY_TIMEOUT_SECS: u64 = 30;
  const FETCH_ANY_MAX_REDIRECTS: usize = 10;
  const FETCH_ANY_MAX_BYTES: usize = 64 * 1024 * 1024; // 64 MB
  let client = reqwest::blocking::Client::builder()
    .timeout(std::time::Duration::from_secs(FETCH_ANY_TIMEOUT_SECS))
    .redirect(reqwest::redirect::Policy::limited(FETCH_ANY_MAX_REDIRECTS))
    .build()
    .map_err(|e| format!("build HTTP client for {url}: {e}"))?;
  let resp = client
    .get(url)
    .send()
    .map_err(|e| format!("fetch {url}: {e}"))?;
  if !resp.status().is_success() {
    return Err(format!("fetch {url}: HTTP {}", resp.status()));
  }
  let content_type = resp
    .headers()
    .get(reqwest::header::CONTENT_TYPE)
    .and_then(|v| v.to_str().ok())
    .unwrap_or("")
    .to_ascii_lowercase();

  use std::io::Read;
  let mut bytes = Vec::new();
  resp
    .take((FETCH_ANY_MAX_BYTES + 1) as u64)
    .read_to_end(&mut bytes)
    .map_err(|e| format!("read body of {url}: {e}"))?;
  if bytes.len() > FETCH_ANY_MAX_BYTES {
    return Err(format!(
      "{url}: response exceeds {FETCH_ANY_MAX_BYTES} bytes; refusing to load"
    ));
  }

  // 4. Pick a parser based on extension first, then Content-Type,
  // then fall through to HTML.
  let format = ext
    .as_deref()
    .and_then(format_from_extension)
    .or_else(|| format_from_content_type(&content_type))
    .unwrap_or(Format::Html);

  match format {
    Format::Pdf => PaperData::from_pdf_bytes(&bytes),
    Format::Epub => PaperData::from_epub_bytes(&bytes),
    Format::Markdown => {
      let text = String::from_utf8(bytes).map_err(|e| format!("{url}: invalid UTF-8 ({e})"))?;
      Ok(PaperData::from_markdown(&text))
    }
    Format::Html => {
      let text = String::from_utf8(bytes).map_err(|e| format!("{url}: invalid UTF-8 ({e})"))?;
      Ok(PaperData::from_html(&text))
    }
    Format::PlainText => {
      let text = String::from_utf8(bytes).map_err(|e| format!("{url}: invalid UTF-8 ({e})"))?;
      let lines: Vec<String> = text.split('\n').map(|l| l.trim_end().to_string()).collect();
      Ok(PaperData::from_plain_lines(lines))
    }
  }
}

#[derive(Clone, Copy)]
enum Format {
  Pdf,
  Epub,
  Markdown,
  Html,
  PlainText,
}

fn format_from_extension(ext: &str) -> Option<Format> {
  Some(match ext {
    "pdf" => Format::Pdf,
    "epub" => Format::Epub,
    "md" | "markdown" => Format::Markdown,
    "html" | "htm" => Format::Html,
    "txt" => Format::PlainText,
    _ => return None,
  })
}

fn format_from_content_type(ct: &str) -> Option<Format> {
  // Strip any `; charset=…` suffix browsers append.
  let base = ct.split(';').next().unwrap_or("").trim();
  Some(match base {
    "application/pdf" => Format::Pdf,
    "application/epub+zip" => Format::Epub,
    "text/markdown" | "text/x-markdown" => Format::Markdown,
    "text/html" | "application/xhtml+xml" => Format::Html,
    "text/plain" => Format::PlainText,
    _ => return None,
  })
}

#[cfg(test)]
mod fetch_any_tests {
  use super::*;

  fn fmt_disc(f: Format) -> &'static str {
    match f {
      Format::Pdf => "pdf",
      Format::Epub => "epub",
      Format::Markdown => "markdown",
      Format::Html => "html",
      Format::PlainText => "txt",
    }
  }

  #[test]
  fn extension_dispatch_covers_known_formats() {
    let cases = [
      ("pdf", "pdf"),
      ("epub", "epub"),
      ("md", "markdown"),
      ("markdown", "markdown"),
      ("html", "html"),
      ("htm", "html"),
      ("txt", "txt"),
    ];
    for (ext, want) in cases {
      let got = format_from_extension(ext).unwrap_or_else(|| panic!("ext {ext} returned None"));
      assert_eq!(fmt_disc(got), want, "extension {ext}");
    }
  }

  #[test]
  fn extension_unknown_returns_none() {
    assert!(format_from_extension("xyz").is_none());
    assert!(format_from_extension("").is_none());
  }

  #[test]
  fn content_type_dispatch_covers_known_mimes() {
    let cases = [
      ("application/pdf", "pdf"),
      ("application/epub+zip", "epub"),
      ("text/markdown", "markdown"),
      ("text/x-markdown", "markdown"),
      ("text/html", "html"),
      ("application/xhtml+xml", "html"),
      ("text/plain", "txt"),
    ];
    for (ct, want) in cases {
      let got = format_from_content_type(ct).unwrap_or_else(|| panic!("ct {ct} returned None"));
      assert_eq!(fmt_disc(got), want, "ct {ct}");
    }
  }

  #[test]
  fn content_type_charset_suffix_ignored() {
    let f = format_from_content_type("text/html; charset=utf-8")
      .expect("charset-suffixed text/html should resolve");
    assert_eq!(fmt_disc(f), "html");
  }

  #[test]
  fn content_type_unknown_returns_none() {
    assert!(format_from_content_type("application/octet-stream").is_none());
    assert!(format_from_content_type("").is_none());
  }
}
