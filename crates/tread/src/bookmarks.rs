use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Per-paper bookmark storage.  `named` is the active map (vim-style:
/// `m<letter>` sets, `'<letter>` jumps).  `marks` is a legacy field from
/// the single-anonymous-bookmark era; it's still read so old JSON files
/// don't lose data — `migrate` folds any leftover entries into `named`.
#[derive(Serialize, Deserialize, Default, Debug)]
pub struct BookmarkSet {
  #[serde(default)]
  pub marks: Vec<usize>,
  #[serde(default)]
  pub named: HashMap<char, usize>,
}

impl BookmarkSet {
  /// Migrate legacy anonymous bookmarks into the named map under
  /// sequential letter keys (a, b, c, …).  Skips any letter already in
  /// use by `named`.  After migration `marks` is cleared so save() won't
  /// write the legacy field back.
  pub fn migrate(&mut self) {
    if self.marks.is_empty() { return; }
    let legacy: Vec<usize> = std::mem::take(&mut self.marks);
    let mut next_letter: u8 = b'a';
    for line in legacy {
      // Skip past letters that are already taken or past 'z'.
      while next_letter <= b'z' && self.named.contains_key(&(next_letter as char)) {
        next_letter += 1;
      }
      if next_letter > b'z' { break; } // overflow — drop the rest
      self.named.insert(next_letter as char, line);
      next_letter += 1;
    }
  }
}

fn path(key: &str) -> Option<PathBuf> {
  dirs::config_dir().map(|p| p.join("trench").join(format!("bookmarks_{key}.json")))
}

pub fn load(key: &str) -> BookmarkSet {
  let Some(p) = path(key) else { return BookmarkSet::default() };
  let Ok(data) = std::fs::read_to_string(&p) else { return BookmarkSet::default() };
  let mut set: BookmarkSet = serde_json::from_str(&data).unwrap_or_default();
  set.migrate();
  set
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn migrate_legacy_to_letters() {
    let mut set = BookmarkSet { marks: vec![10, 20, 30], named: HashMap::new() };
    set.migrate();
    assert!(set.marks.is_empty());
    assert_eq!(set.named.get(&'a'), Some(&10));
    assert_eq!(set.named.get(&'b'), Some(&20));
    assert_eq!(set.named.get(&'c'), Some(&30));
  }

  #[test]
  fn migrate_skips_taken_letters() {
    let mut named = HashMap::new();
    named.insert('a', 99);
    named.insert('c', 77);
    let mut set = BookmarkSet { marks: vec![10, 20], named };
    set.migrate();
    // 'a' and 'c' are taken; legacy entries land at 'b' and 'd'.
    assert_eq!(set.named.get(&'a'), Some(&99));
    assert_eq!(set.named.get(&'b'), Some(&10));
    assert_eq!(set.named.get(&'c'), Some(&77));
    assert_eq!(set.named.get(&'d'), Some(&20));
  }

  #[test]
  fn migrate_overflow_dropped() {
    // 27 legacy marks; only 26 letters exist.
    let marks: Vec<usize> = (100..127).collect();
    let mut set = BookmarkSet { marks, named: HashMap::new() };
    set.migrate();
    assert_eq!(set.named.len(), 26);
    assert_eq!(set.named.get(&'z'), Some(&125));
  }
}
