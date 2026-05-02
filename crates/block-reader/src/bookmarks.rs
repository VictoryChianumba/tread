use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default)]
pub struct BookmarkSet {
  pub marks: Vec<usize>,
}

fn path(key: &str) -> Option<PathBuf> {
  dirs::config_dir().map(|p| p.join("trench").join(format!("bookmarks_{key}.json")))
}

pub fn load(key: &str) -> BookmarkSet {
  let Some(p) = path(key) else { return BookmarkSet::default() };
  let Ok(data) = std::fs::read_to_string(&p) else { return BookmarkSet::default() };
  serde_json::from_str(&data).unwrap_or_default()
}

pub fn save(key: &str, set: &BookmarkSet) {
  let Some(p) = path(key) else { return };
  if let Some(dir) = p.parent() {
    let _ = std::fs::create_dir_all(dir);
  }
  if let Ok(data) = serde_json::to_string(set) {
    let _ = std::fs::write(&p, data);
  }
}
