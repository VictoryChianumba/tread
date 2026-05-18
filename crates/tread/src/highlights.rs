use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// One persistent character-range highlight, stored at block-byte
/// granularity so that it survives terminal-resize reflow.
///
/// `byte_start` and `byte_end` index into the parent block's *canonical*
/// text — the same byte addressing that `VisualLine.block_byte_start /
/// block_byte_end` describe.  See `doc-model::VisualLine` for the
/// definition per block kind.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Highlight {
    pub block_idx: usize,
    pub byte_start: usize,
    pub byte_end: usize,
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct HighlightSet {
    pub highlights: Vec<Highlight>,
}

impl HighlightSet {
    /// Insert a highlight, merging it with any adjacent or overlapping
    /// highlight in the same block to keep the set minimal.
    pub fn add(&mut self, mut h: Highlight) {
        if h.byte_end <= h.byte_start {
            return;
        }
        // Merge with overlapping/adjacent entries.  Two highlights merge when
        // they share a block and their ranges touch (`a.end >= b.start` and
        // `a.start <= b.end`, treating end as inclusive of the merge point).
        let mut i = 0;
        while i < self.highlights.len() {
            let other = self.highlights[i];
            if other.block_idx == h.block_idx
                && other.byte_end >= h.byte_start
                && other.byte_start <= h.byte_end
            {
                h.byte_start = h.byte_start.min(other.byte_start);
                h.byte_end = h.byte_end.max(other.byte_end);
                self.highlights.swap_remove(i);
                continue; // re-inspect the swapped-in element
            }
            i += 1;
        }
        self.highlights.push(h);
    }

    /// Remove the first highlight whose byte range contains `byte`.
    /// Returns whether anything was removed.
    pub fn remove_at(&mut self, block_idx: usize, byte: usize) -> bool {
        if let Some(idx) = self
            .highlights
            .iter()
            .position(|h| h.block_idx == block_idx && byte >= h.byte_start && byte < h.byte_end)
        {
            self.highlights.remove(idx);
            true
        } else {
            false
        }
    }

    /// Iterator over highlights overlapping the given block-byte range.
    pub fn overlapping(
        &self,
        block_idx: usize,
        range: std::ops::Range<usize>,
    ) -> impl Iterator<Item = &Highlight> {
        self.highlights.iter().filter(move |h| {
            h.block_idx == block_idx && h.byte_start < range.end && h.byte_end > range.start
        })
    }
}

fn path(key: &str) -> Option<PathBuf> {
    dirs::config_dir().map(|p| p.join("trench").join(format!("highlights_{key}.json")))
}

pub fn load(key: &str) -> HighlightSet {
    let Some(p) = path(key) else {
        return HighlightSet::default();
    };
    crate::persist::load_json(&p)
}

pub fn save(key: &str, set: &HighlightSet) {
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

    fn h(block_idx: usize, byte_start: usize, byte_end: usize) -> Highlight {
        Highlight {
            block_idx,
            byte_start,
            byte_end,
        }
    }

    #[test]
    fn add_inserts_simple() {
        let mut s = HighlightSet::default();
        s.add(h(0, 5, 10));
        assert_eq!(s.highlights, vec![h(0, 5, 10)]);
    }

    #[test]
    fn add_rejects_zero_length() {
        let mut s = HighlightSet::default();
        s.add(h(0, 5, 5));
        assert!(s.highlights.is_empty());
    }

    #[test]
    fn add_merges_adjacent_in_same_block() {
        let mut s = HighlightSet::default();
        s.add(h(0, 0, 5));
        s.add(h(0, 5, 10)); // touches at byte 5 — should merge
        assert_eq!(s.highlights, vec![h(0, 0, 10)]);
    }

    #[test]
    fn add_merges_overlapping_in_same_block() {
        let mut s = HighlightSet::default();
        s.add(h(0, 0, 6));
        s.add(h(0, 4, 10)); // overlaps 4..6 — should merge
        assert_eq!(s.highlights, vec![h(0, 0, 10)]);
    }

    #[test]
    fn add_does_not_merge_across_blocks() {
        let mut s = HighlightSet::default();
        s.add(h(0, 0, 5));
        s.add(h(1, 0, 5)); // same range, different block — keep separate
        assert_eq!(s.highlights.len(), 2);
    }

    #[test]
    fn add_chains_multi_merge() {
        let mut s = HighlightSet::default();
        s.add(h(0, 0, 5));
        s.add(h(0, 10, 15));
        s.add(h(0, 4, 11)); // bridges both — merges all three into one
        assert_eq!(s.highlights, vec![h(0, 0, 15)]);
    }

    #[test]
    fn remove_at_hits_and_misses() {
        let mut s = HighlightSet::default();
        s.add(h(0, 5, 10));
        assert!(!s.remove_at(0, 4)); // before range — miss
        assert!(!s.remove_at(0, 10)); // exclusive end — miss
        assert!(!s.remove_at(1, 7)); // wrong block — miss
        assert!(s.remove_at(0, 7)); // inside — hit
        assert!(s.highlights.is_empty());
    }

    #[test]
    fn overlapping_filters() {
        let mut s = HighlightSet::default();
        s.add(h(0, 5, 10));
        s.add(h(0, 20, 25));
        s.add(h(1, 5, 10));
        let in_range: Vec<_> = s.overlapping(0, 0..30).copied().collect();
        assert_eq!(in_range, vec![h(0, 5, 10), h(0, 20, 25)]);
        let in_block_one: Vec<_> = s.overlapping(1, 0..30).copied().collect();
        assert_eq!(in_block_one, vec![h(1, 5, 10)]);
        let nothing: Vec<_> = s.overlapping(0, 11..19).copied().collect();
        assert!(nothing.is_empty());
    }
}
