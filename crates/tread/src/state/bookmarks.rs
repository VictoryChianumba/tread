//! Bookmark seam (ADR-0004 Seam 4).
//!
//! `Reader.bookmarks` is private; the impl-Reader methods below own
//! every read and write.  Storage moved from
//! `HashMap<char, usize>` (visual-line index — brittle across resize)
//! to `HashMap<char, Bookmark>`, mirroring how `HighlightSet` already
//! addresses positions by `(block_idx, byte_in_block)`.
//!
//! Reflow safety: when the user resizes the terminal, `visual_lines`
//! rebuilds and every line's index shifts, but the underlying block
//! and the byte offset within it stay stable — so the bookmark still
//! resolves to the same word in the same paragraph regardless of
//! wrap width.
//!
//! On-disk back-compat: legacy files store `named: HashMap<char, usize>`
//! (the visual-line index recorded at the time of save).  We accept
//! either form during load via a `#[serde(untagged)]` enum; legacy
//! values are translated to `Bookmark` against the current
//! `visual_lines` layout in `load_bookmarks_from_disk`.  The next
//! `save_bookmarks_to_disk` rewrites the file in the new format, so
//! the migration is one-shot per paper.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::Reader;

/// Block-byte-addressed bookmark.  `byte_in_block` indexes into the
/// parent block's canonical text — the same addressing scheme used by
/// `Highlight` and `VisualLine.block_byte_start / block_byte_end`.
/// A single point, not a range: jumps land on the visual line
/// containing this byte and place the cursor at the corresponding
/// column.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bookmark {
    pub block_idx: usize,
    pub byte_in_block: usize,
}

/// On-disk shape.  `marks` is the long-obsolete anonymous-bookmark
/// list, preserved so old files don't lose data on first read.
/// `named` is the active map; values are either `Bookmark` (current
/// format) or `usize` (legacy visual-line index, translated on load).
#[derive(Serialize, Deserialize, Default, Debug)]
pub struct BookmarkSet {
    #[serde(default)]
    pub marks: Vec<usize>,
    #[serde(default)]
    pub named: HashMap<char, BookmarkValue>,
}

/// Untagged enum so existing JSON files with `usize` values keep
/// loading.  New files always emit the `Block` variant.
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
#[serde(untagged)]
pub enum BookmarkValue {
    /// Current format.  `Bookmark` is a struct, so the JSON value is
    /// an object — distinguishable from the legacy integer form.
    Block(Bookmark),
    /// Legacy: a bare visual-line index.  Translated to `Block` in
    /// `Reader::load_bookmarks_from_disk` once `visual_lines` exists.
    LegacyLineIdx(usize),
}

fn path(key: &str) -> Option<PathBuf> {
    dirs::config_dir().map(|p| p.join("one-research").join(format!("bookmarks_{key}.json")))
}

pub fn load(key: &str) -> BookmarkSet {
    let Some(p) = path(key) else {
        return BookmarkSet::default();
    };
    crate::persist::load_json(&p)
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

impl Reader {
    /// Set mark `letter` at the current cursor position.  ASCII letters
    /// only — other chars are silently rejected so a stray punctuation
    /// key can't accidentally land a mark on the bar.  Replaces any
    /// prior value for the same letter.  Block-byte addressed so the
    /// mark survives a later resize.
    pub fn set_mark(&mut self, letter: char) {
        if !letter.is_ascii_alphabetic() {
            return;
        }
        let Some(vl) = self.visual_lines.get(self.current_line()) else {
            return;
        };
        // Non-text VLs (Image, Rule, Blank) have block_byte_end == block_byte_start.
        // A bookmark on those is meaningless because the byte address
        // doesn't pin a glyph — skip silently rather than store a degenerate.
        if vl.block_byte_end == vl.block_byte_start {
            return;
        }
        let byte_in_block = vl
            .block_byte_start
            .saturating_add(self.cursor_x().min(vl.block_byte_end - vl.block_byte_start - 1));
        self.bookmarks.insert(
            letter,
            Bookmark {
                block_idx: vl.block_idx,
                byte_in_block,
            },
        );
    }

    /// Jump to mark `letter`.  No-op if the mark is unset, the letter
    /// is invalid, or the referenced block no longer exists (which
    /// happens after `:reload` if the paper changed).  Pushes the
    /// current position onto `nav_history` first so `Ctrl+O` returns.
    pub fn jump_to_mark(&mut self, letter: char) {
        if !letter.is_ascii_alphabetic() {
            return;
        }
        let Some(bm) = self.bookmarks.get(&letter).copied() else {
            return;
        };
        let Some((vl_idx, col)) = self.resolve_bookmark(&bm) else {
            return;
        };
        self.push_nav_mark();
        self.jump_to_line(vl_idx);
        self.set_cursor_x(col);
    }

    /// Remove mark `letter`, returning whether anything was removed.
    /// Used by the `:delmarks {a}` Ex-command.
    pub fn remove_mark(&mut self, letter: char) -> bool {
        self.bookmarks.remove(&letter).is_some()
    }

    /// Iterator of `(letter, visual-line index)` pairs for every
    /// currently-set mark, sorted by letter.  Marks whose target
    /// block no longer exists in `visual_lines` yield no entry.  Used
    /// by `:marks` to render the popup list.
    pub fn marks_iter(&self) -> impl Iterator<Item = (char, usize)> + '_ {
        let mut entries: Vec<(char, Bookmark)> =
            self.bookmarks.iter().map(|(&c, &bm)| (c, bm)).collect();
        entries.sort_by_key(|(c, _)| *c);
        entries
            .into_iter()
            .filter_map(|(c, bm)| self.resolve_bookmark(&bm).map(|(vl_idx, _)| (c, vl_idx)))
    }

    /// Whether the visual line at `vl_idx` is the landing line for any
    /// currently-set mark.  Used by `render.rs` to draw the bookmark
    /// gutter glyph.  Linear in number of marks — fine since vim's
    /// 26-letter alphabet caps the map.
    pub fn is_line_bookmarked(&self, vl_idx: usize) -> bool {
        self.bookmarks
            .values()
            .filter_map(|bm| self.resolve_bookmark(bm))
            .any(|(idx, _)| idx == vl_idx)
    }

    /// Resolve a `Bookmark` to `(visual_line_index, byte_column)` in
    /// the current layout.  Returns `None` when the block was removed
    /// (post-`:reload`) or when the target byte falls outside any VL
    /// (shouldn't happen for a well-formed bookmark, but guard
    /// against legacy/corrupt entries).
    fn resolve_bookmark(&self, bm: &Bookmark) -> Option<(usize, usize)> {
        self.visual_lines
            .iter()
            .enumerate()
            .find(|(_, vl)| {
                vl.block_idx == bm.block_idx
                    && bm.byte_in_block >= vl.block_byte_start
                    && bm.byte_in_block < vl.block_byte_end
            })
            .map(|(idx, vl)| (idx, bm.byte_in_block - vl.block_byte_start))
    }

    /// Load bookmarks from disk for `key`, translating any legacy
    /// `usize` (visual-line index) values into `Bookmark`s against the
    /// current `visual_lines` layout.  Legacy entries that no longer
    /// resolve to a VL (e.g. document shrank) are silently dropped —
    /// stale marks are less useful than the alternative of corrupting
    /// the in-memory map with degenerate entries.
    pub(super) fn load_bookmarks_from_disk(&mut self, key: &str) {
        let mut set = load(key);
        // Fold the legacy anonymous `marks` list into named entries
        // starting at 'a', mirroring the pre-Seam-4 behaviour.
        if !set.marks.is_empty() {
            let legacy: Vec<usize> = std::mem::take(&mut set.marks);
            let mut next_letter: u8 = b'a';
            for line in legacy {
                while next_letter <= b'z' && set.named.contains_key(&(next_letter as char)) {
                    next_letter += 1;
                }
                if next_letter > b'z' {
                    break;
                }
                set.named
                    .insert(next_letter as char, BookmarkValue::LegacyLineIdx(line));
                next_letter += 1;
            }
        }
        self.bookmarks.clear();
        for (letter, value) in set.named {
            match value {
                BookmarkValue::Block(bm) => {
                    self.bookmarks.insert(letter, bm);
                }
                BookmarkValue::LegacyLineIdx(vl_idx) => {
                    // Translate vl_idx → block-byte by looking up the
                    // line in the CURRENT layout.  Drop the entry if
                    // the index is out of range or sits on a non-text
                    // line.
                    if let Some(vl) = self.visual_lines.get(vl_idx)
                        && vl.block_byte_end > vl.block_byte_start
                    {
                        self.bookmarks.insert(
                            letter,
                            Bookmark {
                                block_idx: vl.block_idx,
                                byte_in_block: vl.block_byte_start,
                            },
                        );
                    }
                }
            }
        }
    }

    /// Persist the current bookmark map.  Always writes the new
    /// `Block` form; legacy `LegacyLineIdx` values can only enter the
    /// map via `load_bookmarks_from_disk` and they're translated to
    /// `Block` there, so save never round-trips the legacy form.
    pub(super) fn save_bookmarks_to_disk(&self, key: &str) {
        let named: HashMap<char, BookmarkValue> = self
            .bookmarks
            .iter()
            .map(|(&c, &bm)| (c, BookmarkValue::Block(bm)))
            .collect();
        save(
            key,
            &BookmarkSet {
                marks: Vec::new(),
                named,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_set_round_trips_through_json() {
        // The on-disk format keeps backward compatibility with files
        // written before Seam 4 — those entries are bare integers.
        let json = r#"{"marks": [], "named": {"a": 42}}"#;
        let set: BookmarkSet = serde_json::from_str(json).expect("legacy parse");
        let entry = set.named.get(&'a').expect("entry");
        assert!(matches!(entry, BookmarkValue::LegacyLineIdx(42)));
    }

    #[test]
    fn new_set_round_trips_through_json() {
        let json = r#"{"marks": [], "named": {"a": {"block_idx": 3, "byte_in_block": 12}}}"#;
        let set: BookmarkSet = serde_json::from_str(json).expect("new parse");
        let entry = set.named.get(&'a').expect("entry");
        match entry {
            BookmarkValue::Block(bm) => {
                assert_eq!(bm.block_idx, 3);
                assert_eq!(bm.byte_in_block, 12);
            }
            _ => panic!("expected Block variant"),
        }
    }
}
