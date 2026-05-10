//! Lift each `Block::Matrix` group to the position the rendered PDF
//! places it.
//!
//! Input: the parsed block list (in LaTeX source order), plus per-table
//! placement anchors extracted from the PDF (see `pdf_anchors.rs`).
//! Output: the same block list with table groups moved to immediately
//! after the prose paragraph that owns them in PDF reading order.
//!
//! Matching is by **substring**, not exact paragraph fingerprint.  The
//! anchor phrase is the first 7 ASCII-only words of the PDF line that
//! sits right before the caption — that line is mid-paragraph wrap from
//! `pdftotext`, but the same words appear verbatim inside the
//! corresponding LaTeX-parsed paragraph block.  Matching by substring
//! tolerates the wrap-vs-whole difference cleanly.
//!
//! Robustness rules:
//! - Each anchor maps to one group (by 1-based table number → Nth group).
//! - If no parsed block matches the anchor phrase, the group is left at
//!   its source-order position.  Same if the table number doesn't have a
//!   corresponding group.  Failure is silent and per-table.
//! - Groups whose target equals their current location are not moved.

use doc_model::Block;

use crate::pdf_anchors::{TableAnchor, fingerprint, first_n_words};

/// Lift table groups in `blocks` to the positions implied by `anchors`.
/// Groups without a matching anchor stay where they are.  Returns a new
/// `Vec<Block>` regardless of whether any moves happened.
pub fn lift_tables(blocks: Vec<Block>, anchors: &[TableAnchor]) -> Vec<Block> {
  if anchors.is_empty() {
    return blocks;
  }
  let groups = identify_groups(&blocks);
  if groups.is_empty() {
    return blocks;
  }

  // For each anchor, find the target block index in `blocks` that should
  // immediately precede the corresponding group.  Targets must be outside
  // every group (we never insert a table inside another table).
  let mut targets: Vec<Option<usize>> = vec![None; groups.len()];
  for anchor in anchors {
    let group_idx = match anchor.table_number.checked_sub(1) {
      Some(i) if i < groups.len() => i,
      _ => {
        // log::debug! instead of eprintln! — when this crate is
        // embedded in a TUI host (trench) that has already entered
        // alt-screen, eprintln writes land directly on the terminal
        // and bypass ratatui's buffer, leaving stray text the next
        // diff-paint never overwrites.
        log::debug!(
          "placement: Table {} has no matching parsed Matrix group ({} groups in source)",
          anchor.table_number, groups.len()
        );
        continue;
      }
    };
    match find_anchor_target(&blocks, &groups, &anchor.anchor_preview) {
      Some(t) => {
        targets[group_idx] = Some(t);
        log::debug!(
          "placement: Table {} → block {} (anchor «{}»)",
          anchor.table_number, t, anchor.anchor_preview
        );
      }
      None => {
        log::debug!(
          "placement: Table {} anchor «{}» did not match any block; leaving in source order",
          anchor.table_number, anchor.anchor_preview
        );
      }
    }
  }

  // Rebuild block list: skip blocks belonging to groups that will be
  // re-inserted; emit each kept block in turn; after each kept block,
  // append any group whose target is that block's index.
  let mut group_block_set = std::collections::HashSet::new();
  for (gi, g) in groups.iter().enumerate() {
    if targets[gi].is_some() {
      for b in g.start..g.end {
        group_block_set.insert(b);
      }
    }
  }

  // Collect each moving group's block sequence (cloned) keyed by target
  // index so we can splice them in.
  let mut to_insert: std::collections::HashMap<usize, Vec<Vec<Block>>> = Default::default();
  for (gi, g) in groups.iter().enumerate() {
    if let Some(t) = targets[gi] {
      let slice: Vec<Block> = blocks[g.start..g.end].to_vec();
      to_insert.entry(t).or_default().push(slice);
    }
  }

  let mut out: Vec<Block> = Vec::with_capacity(blocks.len() + 4);
  for (i, block) in blocks.iter().enumerate() {
    if group_block_set.contains(&i) {
      continue;
    }
    out.push(block.clone());
    if let Some(groups_here) = to_insert.remove(&i) {
      for g in groups_here {
        out.extend(g);
      }
    }
  }
  out
}

/// Half-open range of a contiguous table group inside the block list.
#[derive(Debug, Clone, Copy)]
struct GroupRange { start: usize, end: usize }

/// Identify table groups in encounter order.  A group is a contiguous
/// run of blocks containing at least one `Block::Matrix`, optionally
/// preceded by its caption `Block::Line("[Table: ...]")` and followed by
/// its trailing `Block::Blank`.
///
/// Walks forward; when it sees a Matrix it expands the bounds backward
/// (to capture an immediately-preceding caption Line) and forward (to
/// capture the trailing Rule chain plus one Blank).
fn identify_groups(blocks: &[Block]) -> Vec<GroupRange> {
  let mut groups = Vec::new();
  let mut i = 0;
  while i < blocks.len() {
    if !matches!(blocks[i], Block::Matrix { .. }) {
      i += 1;
      continue;
    }
    let mut start = i;
    // Capture preceding caption line (placed above the table since the
    // caption-above convention).  Newer caption format is "[Table N:".
    if start > 0 {
      if let Block::Line(s) = &blocks[start - 1] {
        if s.starts_with("[Table") {
          start -= 1;
        }
      }
    }
    // Capture any preceding `Block::Anchor` (the table's `\label`
    // marker emitted from a Pandoc Div with id="tab:X") so it travels
    // with the lifted table — without this, references to the table
    // resolve to whatever block the orphaned anchor ends up next to.
    while start > 0 && matches!(blocks[start - 1], Block::Anchor(_)) {
      start -= 1;
    }
    // Walk forward through Rule/Matrix sequence.
    let mut end = i;
    while end < blocks.len() && matches!(blocks[end], Block::Matrix { .. } | Block::Rule) {
      end += 1;
    }
    // Include trailing Blank if present.
    if end < blocks.len() && matches!(blocks[end], Block::Blank) {
      end += 1;
    }
    groups.push(GroupRange { start, end });
    i = end;
  }
  groups
}

/// Find the index of the block in `blocks` whose normalised text contains
/// `phrase`.  Skips any block inside an existing group (we never anchor
/// one table inside another's group).  Returns the first match in
/// document order.
fn find_anchor_target(blocks: &[Block], groups: &[GroupRange], phrase: &str) -> Option<usize> {
  if phrase.is_empty() {
    return None;
  }
  let in_group = |idx: usize| groups.iter().any(|g| idx >= g.start && idx < g.end);
  for (i, block) in blocks.iter().enumerate() {
    if in_group(i) {
      continue;
    }
    let text = block_prose_text(block);
    if text.is_empty() {
      continue;
    }
    let normalised = normalise(&text);
    if normalised.contains(phrase) {
      return Some(i);
    }
  }
  None
}

/// Extract the prose text of a block for substring matching.  Returns
/// empty string for non-text blocks (Matrix, Rule, etc.) so they can't
/// be matched.
fn block_prose_text(block: &Block) -> String {
  match block {
    Block::Line(s) => s.clone(),
    Block::StyledLine(spans) => spans.iter().map(|s| s.text.clone()).collect::<String>(),
    Block::Header { text, .. } => text.clone(),
    Block::ListItem { content, .. } => content.iter().map(|s| s.text.clone()).collect::<String>(),
    Block::Quote(spans) => spans.iter().map(|s| s.text.clone()).collect::<String>(),
    _ => String::new(),
  }
}

/// Normalise prose text into the same shape as the PDF anchor phrase:
/// lowercase ASCII letters separated by single spaces, dropping digits,
/// punctuation, math, accented forms, and any content within `[...]`
/// brackets (citations, ref-tags).  The PDF-anchor side rendered numeric
/// citations as digits which were already dropped — but the LaTeX side
/// renders citations as `[cite-key]` whose alphabetic key would otherwise
/// interrupt the anchor phrase match.  Stripping bracket groups makes
/// both sides converge to the same alpha-only form.
fn normalise(text: &str) -> String {
  let mut out = String::with_capacity(text.len());
  let mut prev_space = true;
  let mut depth: i32 = 0;
  for ch in text.chars() {
    match ch {
      '[' => {
        depth += 1;
        if !prev_space { out.push(' '); prev_space = true; }
      }
      ']' if depth > 0 => {
        depth -= 1;
        if !prev_space { out.push(' '); prev_space = true; }
      }
      _ if depth > 0 => { /* inside brackets — skip */ }
      c if c.is_ascii_alphabetic() => {
        out.push(c.to_ascii_lowercase());
        prev_space = false;
      }
      _ => {
        if !prev_space { out.push(' '); prev_space = true; }
      }
    }
  }
  if out.ends_with(' ') {
    out.pop();
  }
  out
}

// Silence unused-warning when `fingerprint` and `first_n_words` aren't
// referenced from this file directly — they're part of the public API
// of `pdf_anchors` and are reachable from tests via the import above.
#[allow(dead_code)]
fn _force_use() {
  let _ = fingerprint;
  let _ = first_n_words;
}

#[cfg(test)]
mod tests {
  use super::*;
  use doc_model::{Block, InlineSpan};

  fn line(s: &str) -> Block { Block::Line(s.to_string()) }
  fn matrix() -> Block {
    Block::Matrix {
      rows: vec![vec![("cell".to_string(), 1)]],
      vertical_rules: vec![],
    }
  }

  #[test]
  fn identify_simple_group() {
    let blocks = vec![
      line("Some prose."),
      Block::Blank,
      line("[Table: caption]"),
      matrix(),
      Block::Rule,
      Block::Blank,
      line("More prose."),
    ];
    let groups = identify_groups(&blocks);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].start, 2); // caption
    assert_eq!(groups[0].end, 6);   // past Blank
  }

  #[test]
  fn substring_match_finds_paragraph() {
    let blocks = vec![
      line("Section heading"),
      Block::StyledLine(vec![InlineSpan::plain(
        "We employ three types of regularization during training: residual dropout, label smoothing, and attention dropout."
      )]),
    ];
    let target = find_anchor_target(&blocks, &[], "we employ three types of regularization during");
    assert_eq!(target, Some(1));
  }

  #[test]
  fn lift_moves_group_after_anchor() {
    let blocks = vec![
      line("Anchor paragraph contains: in different ways measuring the change in performance."),
      line("Other paragraph."),
      line("[Table: caption]"),
      matrix(),
      Block::Rule,
      Block::Blank,
      line("After-table paragraph."),
    ];
    let anchors = vec![TableAnchor {
      table_number: 1,
      anchor_fingerprint: 0,
      anchor_preview: "in different ways measuring the change in".to_string(),
    }];
    let out = lift_tables(blocks, &anchors);
    // Group should now sit after block 0 (the anchor paragraph).
    assert!(matches!(&out[0], Block::Line(s) if s.starts_with("Anchor paragraph")));
    assert!(matches!(&out[1], Block::Line(s) if s.starts_with("[Table:")));
    assert!(matches!(&out[2], Block::Matrix { .. }));
  }

  #[test]
  fn lift_preserves_unmatched_groups() {
    let blocks = vec![
      line("Anchor matches phrase one."),
      line("[Table: caption]"),
      matrix(),
      Block::Blank,
      line("Other text."),
    ];
    // Anchor phrase doesn't match anything.
    let anchors = vec![TableAnchor {
      table_number: 1,
      anchor_fingerprint: 0,
      anchor_preview: "no such phrase exists in this document".to_string(),
    }];
    let out = lift_tables(blocks, &anchors);
    // Group remains in its original position.
    assert!(matches!(&out[1], Block::Line(s) if s.starts_with("[Table:")));
    assert!(matches!(&out[2], Block::Matrix { .. }));
  }
}
