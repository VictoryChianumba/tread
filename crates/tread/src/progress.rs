use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ReaderProgress {
  pub offset: usize,
}

fn progress_path() -> Option<PathBuf> {
  dirs::config_dir().map(|p| p.join("trench").join("reader_progress.json"))
}

pub fn load() -> HashMap<String, ReaderProgress> {
  let path = match progress_path() {
    Some(p) => p,
    None => return HashMap::new(),
  };
  let bytes = match fs::read(&path) {
    Ok(b) => b,
    Err(_) => return HashMap::new(),
  };
  serde_json::from_slice(&bytes).unwrap_or_default()
}

pub fn save(progress: &HashMap<String, ReaderProgress>) {
  let path = match progress_path() {
    Some(p) => p,
    None => return,
  };
  if let Some(parent) = path.parent() {
    let _ = fs::create_dir_all(parent);
  }
  if let Ok(json) = serde_json::to_vec_pretty(progress) {
    let _ = fs::write(&path, json);
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt;
      let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
  }
}
