//! Table-group layout and visual-line emission.
//!
//! A "table group" is a run of consecutive `Block::Rule` and
//! `Block::Matrix` blocks.  Matrix-bearing groups render as booktabs-
//! style tables (with optional vertical rules); rule-only groups
//! render as plain horizontal separators.  The orchestrator calls
//! `emit_table_group_lines` once per group.

use crate::{Block, VisualLine, VisualLineKind, visual_width};

// Column separator widths.  Booktabs style by default (no vertical
// lines).  When a table has vertical rules, gaps widen to RULED_COL_SEP
// so " │ " fits without changing column alignment between ruled and
// non-ruled gaps.
const COL_SEP: usize = 2;
const RULED_COL_SEP: usize = 3;

/// Emit visual lines for a consecutive run of `Block::Rule` and
/// `Block::Matrix` blocks (a "table group").
///
/// Matrix-bearing groups render as a booktabs-style table with
/// horizontal rules (and optional vertical rules / cmidrule
/// suppression).  Rule-only groups render as plain horizontal
/// separators sized to the terminal width.
pub(crate) fn emit_table_group_lines(
    out: &mut Vec<VisualLine>,
    group: &[Block],
    base_block_idx: usize,
    terminal_width: usize,
) {
    let has_matrix = group.iter().any(|b| matches!(b, Block::Matrix { .. }));
    if has_matrix {
        render_table_group(group, base_block_idx, terminal_width, out);
    } else {
        for (k, _) in group.iter().enumerate() {
            out.push(VisualLine {
                block_idx: base_block_idx + k,
                line_in_block: 0,
                text: "─".repeat(terminal_width),
                kind: VisualLineKind::Rule,
                block_byte_start: 0,
                block_byte_end: 0,
            });
        }
    }
}

/// Translate raw column-rule positions (from a `tabular` spec) into active
/// column-boundary positions (after blank-column collapse).
///
/// `raw_rules[i] = k` means "vertical rule before raw column k".  Position 0
/// means the left edge, position `n_raw` means the right edge.  In active
/// space, position 0 is the left edge and position `active_cols.len()` is the
/// right edge.  Result is sorted and deduplicated.
fn translate_rules_to_active(
    raw_rules: &[usize],
    active_cols: &[usize],
    n_raw: usize,
) -> Vec<usize> {
    let n_active = active_cols.len();
    let mut active_rules: Vec<usize> = raw_rules
        .iter()
        .map(|&r| {
            if r == 0 {
                0
            } else if r >= n_raw {
                n_active
            } else {
                active_cols.partition_point(|&j| j < r)
            }
        })
        .collect();
    active_rules.sort_unstable();
    active_rules.dedup();
    active_rules
}

/// Build a horizontal rule line that includes intersection characters at
/// active-rule positions.  Corners use proper Unicode box-drawing forms
/// (`┌┐└┘`) when an edge rule meets a top/bottom horizontal; interiors use
/// the cross variant (`┬┼┴`).
///
/// Layout, matching `render_row_with_spans`:
/// - Left edge rule  → `corner + ─` (2 chars), aligns with `│ `.
/// - Internal rule   → `─ + cross + ─` (3 chars), aligns with ` │ `.
/// - Right edge rule → `─ + corner` (2 chars), aligns with ` │`.
fn make_rule_line(
    col_widths: &[usize],
    active_rules: &[usize],
    gap_width: usize,
    left_corner: char,
    cross: char,
    right_corner: char,
) -> String {
    let n_active = col_widths.len();
    let mut s = String::new();
    if active_rules.contains(&0) {
        s.push(left_corner);
        s.push('─');
    }
    for ai in 0..n_active {
        s.push_str(&"─".repeat(col_widths[ai]));
        if ai + 1 < n_active {
            if active_rules.contains(&(ai + 1)) {
                s.push('─');
                s.push(cross);
                s.push('─');
            } else {
                s.push_str(&"─".repeat(gap_width));
            }
        }
    }
    if active_rules.contains(&n_active) {
        s.push('─');
        s.push(right_corner);
    }
    s
}

/// Render a matrix-bearing table group.  Booktabs style by default
/// (horizontal rules only).  When the table's `vertical_rules` is
/// non-empty, also draws `│` between marked columns and `┬`/`┼`/`┴` at
/// the corresponding intersections in horizontal rules.
fn render_table_group(
    group: &[Block],
    base_block_idx: usize,
    terminal_width: usize,
    out: &mut Vec<VisualLine>,
) {
    type Row = Vec<(String, usize)>;
    let all_matrix_rows: Vec<&Vec<Row>> = group
        .iter()
        .filter_map(|b| {
            if let Block::Matrix { rows, .. } = b {
                Some(rows)
            } else {
                None
            }
        })
        .collect();

    // Vertical rules: read from the first Matrix in the group (head and data
    // Matrices for the same table carry identical rules).
    let raw_rules: &[usize] = group
        .iter()
        .find_map(|b| {
            if let Block::Matrix { vertical_rules, .. } = b {
                Some(vertical_rules.as_slice())
            } else {
                None
            }
        })
        .unwrap_or(&[]);

    // ncols = max total column positions (summing all spans in a row).
    let ncols = all_matrix_rows
        .iter()
        .flat_map(|rows| rows.iter())
        .map(|row| row.iter().map(|(_, span)| span).sum::<usize>())
        .max()
        .unwrap_or(0);

    if ncols == 0 {
        return;
    }

    // Collapse always-blank columns.  A column j is active if any cell covers it
    // (via its span) with non-empty content.
    let mut col_ever_nonempty = vec![false; ncols];
    for rows in &all_matrix_rows {
        for row in rows.iter() {
            let mut pos = 0usize;
            for (cell, span) in row.iter() {
                if !cell.trim().is_empty() {
                    for j in pos..(pos + span).min(ncols) {
                        col_ever_nonempty[j] = true;
                    }
                }
                pos += span;
            }
        }
    }
    let active_cols: Vec<usize> = (0..ncols).filter(|&j| col_ever_nonempty[j]).collect();
    let n_active = active_cols.len();
    if n_active == 0 {
        return;
    }

    // When the table has vertical rules, all gaps widen to 3 chars so " │ "
    // fits without changing column alignment between ruled and non-ruled gaps.
    let gap_width: usize = if raw_rules.is_empty() {
        COL_SEP
    } else {
        RULED_COL_SEP
    };
    let active_rules: Vec<usize> = translate_rules_to_active(raw_rules, &active_cols, ncols);

    // Collect widths from span=1 cells only (spanning cells don't dictate individual column widths).
    let mut col_all_widths: Vec<Vec<usize>> = vec![vec![]; n_active];
    for rows in &all_matrix_rows {
        for row in rows.iter() {
            let mut pos = 0usize;
            for (cell, span) in row.iter() {
                if *span == 1 {
                    let ai = active_cols.partition_point(|&j| j < pos);
                    if ai < n_active && active_cols[ai] == pos {
                        let w = visual_width(cell);
                        if w > 0 {
                            col_all_widths[ai].push(w);
                        }
                    }
                }
                pos += span;
            }
        }
    }

    // Cap outlier columns: if max > 2× second-max AND > 20 chars, constrain.
    let mut col_widths: Vec<usize> = (0..n_active)
        .map(|ai| {
            let ws = &col_all_widths[ai];
            if ws.is_empty() {
                return 3;
            }
            let max_w = *ws.iter().max().unwrap();
            if ws.len() < 2 {
                return max_w;
            }
            let second = ws
                .iter()
                .filter(|&&w| w < max_w)
                .max()
                .copied()
                .unwrap_or(max_w);
            if max_w > second * 2 && max_w > 20 {
                (second + 5).max(15)
            } else {
                max_w
            }
        })
        .collect();

    // Expand column widths so spanning headers are never truncated.
    for rows in &all_matrix_rows {
        for row in rows.iter() {
            let mut pos = 0usize;
            for (cell, span) in row.iter() {
                if *span > 1 {
                    let tw = visual_width(cell.trim());
                    let ai_start = active_cols.partition_point(|&j| j < pos);
                    let ai_end = active_cols.partition_point(|&j| j < pos + span);
                    if ai_end > ai_start {
                        let n = ai_end - ai_start;
                        let cur: usize = col_widths[ai_start..ai_end].iter().sum::<usize>()
                            + (n - 1) * gap_width;
                        if tw > cur {
                            let extra = tw - cur;
                            let per = extra.div_ceil(n);
                            for ai in ai_start..ai_end {
                                col_widths[ai] += per;
                            }
                        }
                    }
                }
                pos += span;
            }
        }
    }

    // Header zone: all Matrix blocks before the first Rule whose IMMEDIATE next Matrix
    // block is data-like (non-blank first col, all span=1).  Using "immediate" (first
    // Matrix found after the Rule) prevents looking past the sub-header row to data.
    let header_end = (0..group.len())
        .find(|&k| {
            if !matches!(group[k], Block::Rule) {
                return false;
            }
            match group[k + 1..]
                .iter()
                .find(|b| matches!(b, Block::Matrix { .. }))
            {
                Some(Block::Matrix { rows, .. }) => !rows_look_like_header(rows),
                _ => false,
            }
        })
        .unwrap_or(group.len());

    let last_matrix_k = group
        .iter()
        .rposition(|b| matches!(b, Block::Matrix { .. }));

    // Center the table within the terminal: total visual width is the sum of
    // column widths plus internal gaps plus 2 chars per edge rule (`│ ` / ` │`).
    // If the table fits with room to spare, prepend half the slack to every
    // emitted line; if it overflows, fall back to left-aligned (indent = 0).
    let total_width: usize =
        col_widths.iter().sum::<usize>() + (n_active.saturating_sub(1)) * gap_width;
    let edge_extra = (if active_rules.contains(&0) { 2 } else { 0 })
        + (if active_rules.contains(&n_active) {
            2
        } else {
            0
        });
    let table_full_width = total_width + edge_extra;
    let indent = terminal_width.saturating_sub(table_full_width) / 2;
    let pad: String = " ".repeat(indent);

    let top_rule = format!(
        "{pad}{}",
        make_rule_line(&col_widths, &active_rules, gap_width, '┌', '┬', '┐')
    );
    let mid_rule = format!(
        "{pad}{}",
        make_rule_line(&col_widths, &active_rules, gap_width, '├', '┼', '┤')
    );
    let bottom_rule = format!(
        "{pad}{}",
        make_rule_line(&col_widths, &active_rules, gap_width, '└', '┴', '┘')
    );

    let mut seq = 0usize;
    let mut seen_matrix = false;
    let mut top_done = false;

    for (k, block) in group.iter().enumerate() {
        let block_idx = base_block_idx + k;
        match block {
            Block::Rule => {
                let has_matrix_after = group[k + 1..]
                    .iter()
                    .any(|b| matches!(b, Block::Matrix { .. }));
                let is_cmidrule = k < header_end && seen_matrix && has_matrix_after;
                if !is_cmidrule {
                    let is_bottom = last_matrix_k.is_some_and(|lk| k > lk);
                    let text = if is_bottom {
                        bottom_rule.clone()
                    } else {
                        mid_rule.clone()
                    };
                    out.push(VisualLine {
                        block_idx,
                        line_in_block: seq,
                        text,
                        kind: VisualLineKind::Rule,
                        block_byte_start: 0,
                        block_byte_end: 0,
                    });
                    seq += 1;
                }
            }
            Block::Matrix { rows, .. } => {
                let is_header = k < header_end;
                seen_matrix = true;
                if !top_done {
                    out.push(VisualLine {
                        block_idx,
                        line_in_block: seq,
                        text: top_rule.clone(),
                        kind: VisualLineKind::Rule,
                        block_byte_start: 0,
                        block_byte_end: 0,
                    });
                    seq += 1;
                    top_done = true;
                }
                let n_rows = rows.len();
                for (ri, row) in rows.iter().enumerate() {
                    let row_text = render_row_with_spans(
                        row,
                        &active_cols,
                        &col_widths,
                        &active_rules,
                        gap_width,
                    );
                    let text = format!("{pad}{row_text}");
                    out.push(VisualLine {
                        block_idx,
                        line_in_block: seq,
                        text,
                        kind: VisualLineKind::MatrixLine {
                            is_first: ri == 0,
                            is_last: ri == n_rows - 1,
                            is_header,
                        },
                        block_byte_start: 0,
                        block_byte_end: 0,
                    });
                    seq += 1;
                }
            }
            _ => {}
        }
    }
}

/// A Matrix block is "header-like" if every row either has a spanning cell
/// OR has a blank first column.  Data rows have non-blank first columns and
/// all span=1.
fn rows_look_like_header(rows: &[Vec<(String, usize)>]) -> bool {
    rows.iter().all(|row| {
        row.iter().any(|(_, span)| *span > 1)
            || row.first().is_none_or(|(cell, _)| cell.trim().is_empty())
    })
}

/// Build the display string for one table row, respecting column spans.
/// Spanning cells are centered over their combined column width;
/// single cells are left-aligned.  Always-blank columns are skipped.
/// `active_rules` lists active-column boundary positions where `│` is drawn;
/// `gap_width` is the inter-column separator width (2 booktabs, 3 if ruled).
fn render_row_with_spans(
    row: &[(String, usize)],
    active_cols: &[usize],
    col_widths: &[usize],
    active_rules: &[usize],
    gap_width: usize,
) -> String {
    let n_active = active_cols.len();
    let mut result = String::new();

    if active_rules.contains(&0) {
        result.push_str("│ ");
    }

    let mut col_pos = 0usize;
    let mut first_part = true;

    for (cell_text, span) in row.iter() {
        let cell_end = col_pos + span;
        let ai_start = active_cols.partition_point(|&j| j < col_pos);
        let ai_end = active_cols.partition_point(|&j| j < cell_end);

        if ai_end > ai_start && ai_start < n_active {
            let n_covered = ai_end - ai_start;
            let display_width: usize =
                col_widths[ai_start..ai_end].iter().sum::<usize>() + (n_covered - 1) * gap_width;

            if !first_part {
                if active_rules.contains(&ai_start) {
                    result.push_str(" │ ");
                } else {
                    result.push_str(&" ".repeat(gap_width));
                }
            }
            first_part = false;

            let text = cell_text.trim();
            let tw = visual_width(text);
            let content = if *span > 1 && n_covered > 1 {
                if tw > display_width && display_width > 0 {
                    let t: String = text.chars().take(display_width.saturating_sub(1)).collect();
                    format!("{t}…")
                } else {
                    let pad = display_width - tw;
                    let pl = pad / 2;
                    let pr = pad - pl;
                    format!("{}{}{}", " ".repeat(pl), text, " ".repeat(pr))
                }
            } else if tw > display_width && display_width > 0 {
                let t: String = text.chars().take(display_width.saturating_sub(1)).collect();
                format!("{t}…")
            } else {
                format!("{:<width$}", text, width = display_width)
            };
            result.push_str(&content);
        }
        col_pos += span;
    }

    if active_rules.contains(&n_active) {
        result.push_str(" │");
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rule positions: 0 means left edge → 0 in active space; n_raw means
    /// right edge → n_active in active space; internal rules translate via
    /// partition_point on active_cols.  Result must be sorted and deduped
    /// so the contains() checks in row rendering work correctly.
    #[test]
    fn translate_rules_to_active_handles_edges_and_dedup() {
        // 4 raw columns, 3 active (column 2 collapsed).
        let active_cols = vec![0, 1, 3];
        let n_raw = 4;

        // Edges: 0 stays 0; n_raw stays n_active.
        let out = translate_rules_to_active(&[0, 4], &active_cols, n_raw);
        assert_eq!(out, vec![0, 3]);

        // Internal rule before raw col 3 → active position 2 (two active cols
        // before it: 0, 1).
        let out = translate_rules_to_active(&[3], &active_cols, n_raw);
        assert_eq!(out, vec![2]);

        // Dedup: two raw rules collapsing onto the same active position
        // (rule before raw col 2 = position 2; rule before raw col 3 also
        // = position 2 because col 2 is dropped) must collapse to one.
        let out = translate_rules_to_active(&[2, 3], &active_cols, n_raw);
        assert_eq!(out, vec![2]);

        // Sorted: unsorted input must come out sorted.
        let out = translate_rules_to_active(&[4, 0, 3], &active_cols, n_raw);
        assert_eq!(out, vec![0, 2, 3]);
    }

    /// A column that is empty in every row must be collapsed (dropped from
    /// active_cols).  Pre-existing regression source — booktabs tables
    /// frequently have padding columns that should not eat width.
    #[test]
    fn column_collapse_drops_always_blank_columns() {
        // 3-col table where column 1 is always blank.  emit_table_group_lines
        // should produce a render that doesn't reserve any width for col 1.
        let group = vec![Block::Matrix {
            rows: vec![
                vec![("A".into(), 1), ("".into(), 1), ("C".into(), 1)],
                vec![("D".into(), 1), ("".into(), 1), ("F".into(), 1)],
            ],
            vertical_rules: vec![],
        }];
        let mut out = Vec::new();
        emit_table_group_lines(&mut out, &group, 0, 80);

        // Find a matrix-data row; assert it does NOT contain three
        // gap-separated values (which would indicate col 1 was kept).
        let matrix_lines: Vec<&str> = out
            .iter()
            .filter(|vl| matches!(vl.kind, VisualLineKind::MatrixLine { .. }))
            .map(|vl| vl.text.as_str())
            .collect();
        assert!(!matrix_lines.is_empty(), "should have emitted matrix rows");
        for line in &matrix_lines {
            let trimmed = line.trim();
            // Active columns are 0 and 2 (col 1 dropped); rendered row
            // should be "A  C" / "D  F" — exactly one COL_SEP gap.
            // If col 1 were kept, there'd be two gaps and ~5+ spaces total.
            // Count internal whitespace runs of length >= 2 between
            // non-space tokens.
            let internal_gaps = trimmed
                .split(|c: char| !c.is_whitespace())
                .filter(|seg| seg.len() >= 2)
                .count();
            assert_eq!(
                internal_gaps, 1,
                "expected exactly 1 internal column gap (col 1 collapsed), \
                 got {} in line: {:?}",
                internal_gaps, line
            );
        }
    }

    /// When a `\multicolumn{N}` cell's text is wider than the sum of the
    /// columns it spans (plus inner gaps), those columns must be widened
    /// so the header isn't truncated.  This is the expand-widths-for-
    /// spanning-headers path; a regression silently truncates wide
    /// section headers in tables like Attention's Table 3.
    #[test]
    fn multicolumn_cell_expands_columns_when_text_overflows() {
        // 3-col table: row 1 has a 2-wide spanning header that's wider
        // than the natural width of its two columns.  Row 2 has narrow
        // single-cell data — so the natural widths would be tiny.
        let group = vec![Block::Matrix {
            rows: vec![
                vec![("Very Long Header Text".into(), 2), ("Z".into(), 1)],
                vec![("a".into(), 1), ("b".into(), 1), ("Z".into(), 1)],
            ],
            vertical_rules: vec![],
        }];
        let mut out = Vec::new();
        emit_table_group_lines(&mut out, &group, 0, 80);

        // The first matrix row's rendered text must contain the full
        // header — no truncation ellipsis.
        let first_row = out
            .iter()
            .find(|vl| {
                matches!(
                    vl.kind,
                    VisualLineKind::MatrixLine { is_first: true, .. }
                )
            })
            .expect("should have a first matrix row");
        assert!(
            first_row.text.contains("Very Long Header Text"),
            "header was truncated: {:?}",
            first_row.text
        );
    }
}
