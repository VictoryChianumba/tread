use flate2::read::GzDecoder;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use tar::Archive;

const MAX_SOURCE_BYTES: usize = 50 * 1024 * 1024; // 50 MB
// Originally 30 MB on the rationale that "typical paper PDFs are 1-5 MB",
// but modern ML papers with embedded figures routinely cross 30 MB —
// 2605.04035 ships a 34 MB PDF.  When the cap fires the anchor-extraction
// path silently fails (best-effort), and worse: the cache never gets
// written, so every run pays the network cost.  Symmetric with
// MAX_SOURCE_BYTES so a paper with a working tarball almost always has
// a working PDF too.
const MAX_PDF_BYTES: usize = 50 * 1024 * 1024;
// ar5iv HTML is much smaller than the tarball — typical pages are
// 0.2–2 MB; the worst observed case in our benchmark set (1707.09763)
// was 6 MB.  Cap at 50 MB to leave headroom for outlier papers.
const MAX_AR5IV_BYTES: usize = 50 * 1024 * 1024;
const TIMEOUT_SECS: u64 = 30;

/// Image / asset extensions we lift out of the source tarball alongside
/// the `.tex` files.  Anything in this list is written to disk so the
/// reader can load and place it inline; everything else (auxiliary
/// `.bbl`, build outputs, fonts) is silently skipped.
const ASSET_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "pdf"];

/// What `fetch_source` hands back after one round-trip to arXiv:
/// LaTeX source as in-memory strings (so the parser can run without
/// touching disk again), plus the asset directory holding any image
/// files we extracted.  The asset directory survives for the lifetime
/// of the process — the reader reads PNG/PDF bytes from it on demand
/// when an image first scrolls into view.
pub struct FetchedSource {
  pub tex: Vec<(String, String)>,
  pub asset_dir: PathBuf,
}

/// Download the arXiv e-print source tarball for `id`, extract `.tex`
/// files into memory, and extract image / asset files into a stable
/// per-id directory under `~/.cache/tread/sources/<id>/`.
///
/// Returns `Err` if the network request fails or no `.tex` files are
/// found.  Image extraction failures are non-fatal — the run continues
/// without those figures.
pub fn fetch_source(id: &str) -> Result<FetchedSource, String> {
  fetch_source_with(id, false)
}

/// Same as `fetch_source` but bypasses the etag conditional cache and
/// pulls a fresh copy regardless of `If-None-Match` revalidation.  Used
/// by the in-reader `:refresh` command for the case where the user
/// suspects a paper has been revised and wants to force a re-pull
/// without exiting and unsetting the cache by hand.
pub fn fetch_source_refresh(id: &str) -> Result<FetchedSource, String> {
  fetch_source_with(id, true)
}

fn fetch_source_with(id: &str, force_refresh: bool) -> Result<FetchedSource, String> {
  let url = format!("https://arxiv.org/e-print/{id}");
  let bytes = cached_fetch_with(
    &url,
    &format!("{id}.tar.gz"),
    MAX_SOURCE_BYTES,
    force_refresh,
  )?;
  let asset_dir = prepare_asset_dir(id);
  let tex = extract_tex_files(&bytes, &asset_dir)?;
  Ok(FetchedSource { tex, asset_dir })
}

/// Per-id asset directory under XDG cache.  Created if missing, but
/// NOT wiped on re-runs: extracting fresh files with new mtimes
/// invalidates the downstream `pdf_to_png` cache (keyed on canonical
/// path + size + mtime), turning every "warm" run into a full
/// Poppler-rasterisation pass for every PDF figure.  Stale-asset
/// concern from a re-fetched paper version is handled in
/// `write_asset`, which only writes when the destination is absent
/// or differs in size from the tarball entry.  Falls back to
/// `$TMPDIR/tread/sources/<id>` when `HOME` is unset.
fn prepare_asset_dir(id: &str) -> PathBuf {
  let base = if let Some(home) = std::env::var_os("HOME") {
    PathBuf::from(home).join(".cache").join("tread").join("sources")
  } else {
    std::env::temp_dir().join("tread").join("sources")
  };
  let dir = base.join(id);
  let _ = std::fs::create_dir_all(&dir);
  dir
}

/// Download the rendered PDF for `id` and return its raw bytes.  Used for
/// table-placement anchor extraction — `pdftotext` reads the PDF and yields
/// prose in rendered reading order, with each `Table N:` caption appearing
/// next to the paragraph it visually follows in the typeset paper.
///
/// Returns `Err` on network failure, non-2xx status, or oversize response.
pub fn fetch_pdf(id: &str) -> Result<Vec<u8>, String> {
  fetch_pdf_with(id, false)
}

/// Like `fetch_source_refresh` but for the rendered PDF.  Always pulls
/// a fresh copy even when an etag would have produced a 304.
pub fn fetch_pdf_refresh(id: &str) -> Result<Vec<u8>, String> {
  fetch_pdf_with(id, true)
}

fn fetch_pdf_with(id: &str, force_refresh: bool) -> Result<Vec<u8>, String> {
  let url = format!("https://arxiv.org/pdf/{id}");
  cached_fetch_with(&url, &format!("{id}.pdf"), MAX_PDF_BYTES, force_refresh)
}

/// Download the LaTeXML-rendered HTML for `id` from ar5iv.  Returns the
/// raw HTML body as a UTF-8 string so the parser can hand it directly
/// to `ar5iv_parse::to_blocks` without touching disk a second time.
/// Pre-2007 papers use slash-style ids (`hep-th/9202088`); we keep the
/// slash in the cache basename so a single id maps to a single file.
pub fn fetch_ar5iv(id: &str) -> Result<String, String> {
  fetch_ar5iv_with(id, false)
}

/// Sibling of `fetch_ar5iv` that forces a fresh GET, used by `:refresh`.
pub fn fetch_ar5iv_refresh(id: &str) -> Result<String, String> {
  fetch_ar5iv_with(id, true)
}

fn fetch_ar5iv_with(id: &str, force_refresh: bool) -> Result<String, String> {
  let url = format!("https://ar5iv.labs.arxiv.org/html/{id}");
  let safe_basename = id.replace('/', "_");
  let bytes = cached_fetch_with(
    &url,
    &format!("{safe_basename}.html"),
    MAX_AR5IV_BYTES,
    force_refresh,
  )?;
  // ar5iv pages are always served as UTF-8.  Fall back to lossy decode
  // on the rare mojibake input rather than failing the whole load.
  match String::from_utf8(bytes) {
    Ok(s) => Ok(s),
    Err(e) => Ok(String::from_utf8_lossy(e.as_bytes()).into_owned()),
  }
}

/// Disk-backed conditional HTTP GET.
///
/// On warm runs, sends `If-None-Match: <cached ETag>`.  If the server
/// returns 304 Not Modified, we read the body from disk — saving the
/// full payload transfer (~7-10s for tarballs on slow links, ~3s for
/// PDFs).  arXiv serves stable ETags for both e-print and pdf URLs;
/// when a paper revises, the ETag flips and we re-download.  On
/// network failure we silently fall back to the cached body if any —
/// the reader stays usable offline once a paper has been opened once.
///
/// Cache layout under `~/.cache/tread/raw/`:
///   `<basename>`      — the response body bytes
///   `<basename>.etag` — the ETag string for conditional revalidation
///
/// Writes use the .tmp + rename pattern so a crash mid-write doesn't
/// produce a truncated cache file on disk.
///
/// TREAD_REFRESH=1 in env forces a full fetch (skips the conditional
/// header) so users can pull a new version of a paper without having
/// to find and delete the cache file by hand.  The in-reader `:refresh`
/// command sets `force_refresh = true` via `cached_fetch_with` directly,
/// scoped to a single fetch_paper invocation so it doesn't leak to
/// future :reload calls in the same session.
fn cached_fetch_with(
  url: &str,
  basename: &str,
  max_bytes: usize,
  force_refresh: bool,
) -> Result<Vec<u8>, String> {
  let force_refresh = force_refresh || std::env::var_os("TREAD_REFRESH").is_some();
  let cache_dir = raw_cache_dir();
  let body_path = cache_dir.as_ref().map(|d| d.join(basename));
  let etag_path = cache_dir
    .as_ref()
    .map(|d| d.join(format!("{basename}.etag")));

  let client = reqwest::blocking::Client::builder()
    .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
    .build()
    .map_err(|e| format!("failed to build HTTP client: {e}"))?;

  let mut req = client.get(url);
  let have_cached_body = body_path.as_ref().is_some_and(|p| p.exists());
  if !force_refresh
    && have_cached_body
    && let Some(ref ep) = etag_path
    && let Ok(stored) = std::fs::read_to_string(ep)
  {
    let trimmed = stored.trim().to_string();
    if !trimmed.is_empty() {
      req = req.header(reqwest::header::IF_NONE_MATCH, trimmed);
    }
  }

  let resp = match req.send() {
    Ok(r) => r,
    Err(e) => {
      // Offline-tolerant fallback: serve cached body if present.
      if let Some(ref p) = body_path
        && let Ok(b) = std::fs::read(p)
        && b.len() <= max_bytes
      {
        return Ok(b);
      }
      return Err(format!("request failed: {e}"));
    }
  };

  if resp.status().as_u16() == 304
    && let Some(ref p) = body_path
    && let Ok(b) = std::fs::read(p)
  {
    return Ok(b);
  }

  if !resp.status().is_success() {
    // 5xx / 429 / etc. — same fallback as network failure: prefer
    // stale data over no data.
    if let Some(ref p) = body_path
      && let Ok(b) = std::fs::read(p)
      && b.len() <= max_bytes
    {
      return Ok(b);
    }
    return Err(format!("HTTP {}: {url}", resp.status()));
  }

  let new_etag = resp
    .headers()
    .get(reqwest::header::ETAG)
    .and_then(|v| v.to_str().ok())
    .map(str::to_owned);
  let bytes = resp
    .bytes()
    .map_err(|e| format!("failed to read response: {e}"))?;
  if bytes.len() > max_bytes {
    return Err(format!("response too large: {} bytes", bytes.len()));
  }
  let bytes = bytes.to_vec();

  if let (Some(body), Some(etag_p)) = (&body_path, &etag_path) {
    if let Some(parent) = body.parent() {
      let _ = std::fs::create_dir_all(parent);
    }
    let body_tmp = append_ext(body, ".tmp");
    if std::fs::write(&body_tmp, &bytes).is_ok() {
      let _ = std::fs::rename(&body_tmp, body);
    }
    if let Some(etag_value) = new_etag {
      let etag_tmp = append_ext(etag_p, ".tmp");
      if std::fs::write(&etag_tmp, &etag_value).is_ok() {
        let _ = std::fs::rename(&etag_tmp, etag_p);
      }
    }
  }

  Ok(bytes)
}

fn raw_cache_dir() -> Option<PathBuf> {
  std::env::var_os("HOME").map(|h| {
    PathBuf::from(h)
      .join(".cache")
      .join("tread")
      .join("raw")
  })
}

/// Append `suffix` to a path's final component without `with_extension`'s
/// "replace the last extension" semantics — we want `foo.tar.gz` →
/// `foo.tar.gz.tmp`, not `foo.tar.tmp`.
fn append_ext(path: &Path, suffix: &str) -> PathBuf {
  let mut name = path
    .file_name()
    .map(OsString::from)
    .unwrap_or_default();
  name.push(suffix);
  path
    .parent()
    .map(|p| p.join(&name))
    .unwrap_or_else(|| PathBuf::from(&name))
}

fn extract_tex_files(bytes: &[u8], asset_dir: &Path) -> Result<Vec<(String, String)>, String> {
  // Try tar.gz (the common case).
  if let Ok(files) = try_tar_gz(bytes, asset_dir)
    && !files.is_empty() {
      return Ok(files);
    }

  // Some older submissions are a plain gzipped .tex file (not a tar).
  if let Ok(content) = try_plain_gz(bytes) {
    return Ok(vec![("main.tex".to_string(), content)]);
  }

  // Some submissions are uncompressed .tex.  Lossy-convert to handle
  // ISO-8859 / Latin-1 sources alongside the more common UTF-8 ones.
  let content = String::from_utf8_lossy(bytes);
  if content.contains("\\documentclass") || content.contains("\\begin{document}") {
    return Ok(vec![("main.tex".to_string(), content.into_owned())]);
  }

  Err("no .tex files found in source package".to_string())
}

fn try_tar_gz(bytes: &[u8], asset_dir: &Path) -> Result<Vec<(String, String)>, String> {
  let gz = GzDecoder::new(bytes);
  let mut archive = Archive::new(gz);
  let mut files = Vec::new();

  let entries = archive
    .entries()
    .map_err(|e| format!("tar entries error: {e}"))?;

  for entry in entries {
    let mut entry = entry.map_err(|e| format!("tar entry error: {e}"))?;
    let path = entry
      .path()
      .map_err(|e| format!("tar path error: {e}"))?
      .to_string_lossy()
      .to_string();

    if path.ends_with(".tex") || path.ends_with(".bbl") || path.ends_with(".bib") {
      let mut content = String::new();
      entry
        .read_to_string(&mut content)
        .map_err(|e| format!("read error for {path}: {e}"))?;
      // Preserve the full relative path (e.g. "content/intro/intro.tex") so that
      // Pandoc can resolve \input{} directives when run from the temp dir root.
      // Stripping to file_name() broke multi-file papers by flattening the tree.
      //
      // `.bbl` is BibTeX's precompiled output — its `\bibitem` syntax is
      // identical to inline thebibliography, so feeding it through the
      // same vec lets `extract_bibitems` pick up entries from papers that
      // ship .bbl instead of inline bibitems (e.g. GPT-3 / 2005.14165).
      //
      // `.bib` is the raw BibTeX database that bibtex would compile to a
      // .bbl.  Papers that ship the .bib but skip the compile step
      // (2605.04035) had zero bibitems otherwise.  Routed through
      // `bibtex::extract_bibtex_entries` inside `extract_bibitems`.
      files.push((path, content));
    } else if is_asset_path(&path) {
      // Write asset bytes to disk under asset_dir at the same relative
      // path the LaTeX source uses (e.g. "Figures/multi-head.png"), so
      // Block::Image::path can be resolved with a simple `asset_dir.join(rel)`.
      // Failures (zip slip, oversize, IO) are non-fatal — we just skip.
      let _ = write_asset(&mut entry, asset_dir, &path);
    }
  }

  Ok(files)
}

fn is_asset_path(path: &str) -> bool {
  let lower = path.to_ascii_lowercase();
  ASSET_EXTENSIONS.iter().any(|ext| lower.ends_with(&format!(".{ext}")))
}

/// Write one tar entry's bytes into `asset_dir` at the same relative
/// path it had in the tarball.  Rejects paths that try to escape the
/// asset directory (zip-slip protection) — arXiv tarballs shouldn't
/// contain `..` segments, but a hostile or malformed one could.
fn write_asset(
  entry: &mut tar::Entry<GzDecoder<&[u8]>>,
  asset_dir: &Path,
  rel: &str,
) -> std::io::Result<()> {
  let dest = asset_dir.join(rel);
  // Reject anything that escapes asset_dir.  starts_with checks the
  // prefix at component level, so "/a/b" doesn't match "/a/bb".
  if !dest.starts_with(asset_dir) {
    return Err(std::io::Error::other("asset path escapes cache dir"));
  }
  if let Some(parent) = dest.parent() {
    std::fs::create_dir_all(parent)?;
  }
  let mut buf = Vec::new();
  entry.read_to_end(&mut buf)?;
  // Skip the write when the destination already exists at the same
  // size — preserves the inode's mtime so the pdf_to_png cache key
  // stays valid across reruns.  Size-equality is a strong-enough
  // signal because arXiv tarballs are deterministic per-(id, version);
  // a content change ships as a new version with a different tarball.
  if let Ok(meta) = std::fs::metadata(&dest)
    && meta.len() == buf.len() as u64
  {
    return Ok(());
  }
  std::fs::write(&dest, &buf)?;
  Ok(())
}

fn try_plain_gz(bytes: &[u8]) -> Result<String, String> {
  let mut gz = GzDecoder::new(bytes);
  // read_to_string rejects non-UTF-8 input — older arXiv submissions
  // (1707.09763 is one) ship ISO-8859 / Latin-1 LaTeX files and would
  // be rejected as "no .tex files found".  Read raw bytes and lossy-
  // convert: non-UTF-8 octets become U+FFFD, which is harmless for the
  // mostly-ASCII LaTeX command stream and the parsers downstream.
  let mut buf = Vec::new();
  gz.read_to_end(&mut buf)
    .map_err(|e| format!("gz decode error: {e}"))?;
  let content = String::from_utf8_lossy(&buf).into_owned();
  if content.contains("\\documentclass") || content.contains("\\begin{document}") {
    Ok(content)
  } else {
    Err("not a tex file".to_string())
  }
}

/// Extract a clean arXiv ID from a URL or bare ID string.
/// Handles: `1706.03762`, `arxiv.org/abs/1706.03762`, `arxiv.org/pdf/1706.03762v2`
pub fn extract_id(input: &str) -> Option<String> {
  for prefix in &[
    "arxiv.org/abs/",
    "arxiv.org/pdf/",
    "arxiv.org/html/",
    "huggingface.co/papers/",
  ] {
    if let Some(pos) = input.find(prefix) {
      let rest = &input[pos + prefix.len()..];
      let id: String = rest
        .chars()
        .take_while(|&c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        .collect();
      return strip_version(&id);
    }
  }
  // Bare ID like "1706.03762" or "1706.03762v2".
  let candidate: String = input
    .chars()
    .take_while(|&c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    .collect();
  if candidate.contains('.') {
    return strip_version(&candidate);
  }
  None
}

fn strip_version(id: &str) -> Option<String> {
  if id.is_empty() {
    return None;
  }
  // Strip trailing "v<digits>" version suffix.
  if let Some(v_pos) = id.rfind('v') {
    let suffix = &id[v_pos + 1..];
    if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
      return Some(id[..v_pos].to_string());
    }
  }
  Some(id.to_string())
}
