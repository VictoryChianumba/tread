use flate2::read::GzDecoder;
use std::io::Read;
use tar::Archive;

const MAX_SOURCE_BYTES: usize = 50 * 1024 * 1024; // 50 MB
const TIMEOUT_SECS: u64 = 30;

/// Download the arXiv e-print source tarball for `id` and return all `.tex`
/// file contents as `(filename, content)` pairs.
///
/// Returns `Err` if the network request fails or no `.tex` files are found.
pub fn fetch_source(id: &str) -> Result<Vec<(String, String)>, String> {
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

  extract_tex_files(&bytes)
}

fn extract_tex_files(bytes: &[u8]) -> Result<Vec<(String, String)>, String> {
  // Try tar.gz (the common case).
  if let Ok(files) = try_tar_gz(bytes) {
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

fn try_tar_gz(bytes: &[u8]) -> Result<Vec<(String, String)>, String> {
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

    if !path.ends_with(".tex") {
      continue;
    }

    let mut content = String::new();
    entry
      .read_to_string(&mut content)
      .map_err(|e| format!("read error for {path}: {e}"))?;

    let filename = std::path::Path::new(&path)
      .file_name()
      .map(|n| n.to_string_lossy().to_string())
      .unwrap_or(path.clone());

    files.push((filename, content));
  }

  Ok(files)
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
