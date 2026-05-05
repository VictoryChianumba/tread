use flate2::read::GzDecoder;
use std::io::Read;
use std::path::{Path, PathBuf};
use tar::Archive;

const MAX_SOURCE_BYTES: usize = 50 * 1024 * 1024; // 50 MB
const MAX_PDF_BYTES: usize = 30 * 1024 * 1024; // 30 MB — typical paper PDFs are 1–5 MB
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
  let url = format!("https://arxiv.org/e-print/{id}");

  let client = reqwest::blocking::Client::builder()
    .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
    .build()
    .map_err(|e| format!("failed to build HTTP client: {e}"))?;

  let resp = client
    .get(&url)
    .send()
    .map_err(|e| format!("request failed: {e}"))?;

  if !resp.status().is_success() {
    return Err(format!("HTTP {}: {url}", resp.status()));
  }

  // Read response bytes up to the cap.
  let mut bytes = Vec::new();
  let reader = resp
    .bytes()
    .map_err(|e| format!("failed to read response: {e}"))?;
  // reqwest bytes() returns the full Bytes object — convert and cap.
  if reader.len() > MAX_SOURCE_BYTES {
    return Err(format!("source too large: {} bytes", reader.len()));
  }
  bytes.extend_from_slice(&reader);

  let asset_dir = prepare_asset_dir(id);
  let tex = extract_tex_files(&bytes, &asset_dir)?;
  Ok(FetchedSource { tex, asset_dir })
}

/// Per-id asset directory under XDG cache.  Wiped on every fetch so a
/// re-download always reflects the latest tarball — preventing a
/// stale figure from a previous version sticking around when the
/// paper is re-fetched.  Falls back to `$TMPDIR/tread/sources/<id>`
/// when `HOME` is unset.
fn prepare_asset_dir(id: &str) -> PathBuf {
  let base = if let Some(home) = std::env::var_os("HOME") {
    PathBuf::from(home).join(".cache").join("tread").join("sources")
  } else {
    std::env::temp_dir().join("tread").join("sources")
  };
  let dir = base.join(id);
  let _ = std::fs::remove_dir_all(&dir);
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
  let url = format!("https://arxiv.org/pdf/{id}");

  let client = reqwest::blocking::Client::builder()
    .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
    .build()
    .map_err(|e| format!("failed to build HTTP client: {e}"))?;

  let resp = client
    .get(&url)
    .send()
    .map_err(|e| format!("PDF request failed: {e}"))?;

  if !resp.status().is_success() {
    return Err(format!("HTTP {}: {url}", resp.status()));
  }

  let bytes = resp
    .bytes()
    .map_err(|e| format!("failed to read PDF response: {e}"))?;
  if bytes.len() > MAX_PDF_BYTES {
    return Err(format!("PDF too large: {} bytes", bytes.len()));
  }
  Ok(bytes.to_vec())
}

fn extract_tex_files(bytes: &[u8], asset_dir: &Path) -> Result<Vec<(String, String)>, String> {
  // Try tar.gz (the common case).
  if let Ok(files) = try_tar_gz(bytes, asset_dir) {
    if !files.is_empty() {
      return Ok(files);
    }
  }

  // Some older submissions are a plain gzipped .tex file (not a tar).
  if let Ok(content) = try_plain_gz(bytes) {
    return Ok(vec![("main.tex".to_string(), content)]);
  }

  // Some submissions are uncompressed .tex.
  if let Ok(content) = std::str::from_utf8(bytes) {
    if content.contains("\\documentclass") || content.contains("\\begin{document}") {
      return Ok(vec![("main.tex".to_string(), content.to_string())]);
    }
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

    if path.ends_with(".tex") {
      let mut content = String::new();
      entry
        .read_to_string(&mut content)
        .map_err(|e| format!("read error for {path}: {e}"))?;
      // Preserve the full relative path (e.g. "content/intro/intro.tex") so that
      // Pandoc can resolve \input{} directives when run from the temp dir root.
      // Stripping to file_name() broke multi-file papers by flattening the tree.
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
  std::fs::write(&dest, &buf)?;
  Ok(())
}

fn try_plain_gz(bytes: &[u8]) -> Result<String, String> {
  let mut gz = GzDecoder::new(bytes);
  let mut content = String::new();
  gz.read_to_string(&mut content)
    .map_err(|e| format!("gz decode error: {e}"))?;
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
