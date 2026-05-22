//! Pandoc Table → `Block::Matrix` conversion.
//!
//! Handles the JSON AST's Table arm: extracts header / body / footer
//! row data, matches the table against a pre-extracted `TableSpec`
//! (vertical-rule and horizontal-rule info recovered from the LaTeX
//! source), and emits one `Block::Matrix` per header zone plus one
//! per data zone, separated by `Block::Rule` blocks.

use doc_model::{Alignment, Block};
use serde_json::Value;

use super::inline::walk_inlines_text;
use super::{SectionCounters, TableSpec};

/// Per-column alignment from Pandoc's `colspec` (`c[2]`): each entry is
/// `[alignment, colwidth]` where alignment is `{"t":"AlignLeft|Right|
/// Center|Default"}`.  `AlignDefault` and anything unrecognised → Left.
fn extract_alignments(colspec: &Value) -> Vec<Alignment> {
    let Some(cols) = colspec.as_array() else {
        return Vec::new();
    };
    cols.iter()
        .map(|cs| match cs.get(0).and_then(|a| a.get("t")).and_then(|t| t.as_str()) {
            Some("AlignRight") => Alignment::Right,
            Some("AlignCenter") => Alignment::Center,
            _ => Alignment::Left,
        })
        .collect()
}

pub(super) fn parse_table(
    c: &Value,
    specs: &mut Vec<TableSpec>,
    counters: &mut SectionCounters,
) -> Vec<Block> {
    // Pandoc 3.x: c = [attr, caption, colspec, head, [bodies…], foot]
    let mut out = Vec::new();

    let caption = extract_caption_text(&c[1]);

    // Head: c[3] = [row_attr, [rows]]
    let mut head_rows = extract_rows(&c[3][1]);

    // Bodies: c[4] = [[attr, head_col_count, head_rows, body_rows], …]
    let mut data_rows = Vec::new();
    if let Some(bodies) = c[4].as_array() {
        for body in bodies {
            data_rows.extend(extract_rows(&body[3]));
        }
    }

    // When Pandoc gives us no head (common after we strip cmidrule, since
    // Pandoc was using cmidrule as a soft head/body hint), promote any leading
    // header-like rows from the body into the head. This puts the midrule in
    // the right place — between the column labels and the actual data.
    //
    // Cap at MAX_PROMOTED so the heuristic fails safely on tables where data
    // rows happen to be text-heavy (e.g. complexity tables full of "O(...)"
    // expressions). Academic papers very rarely use more than 2 header rows.
    if head_rows.is_empty() {
        const MAX_PROMOTED: usize = 2;
        let promote = count_header_prefix(&data_rows).min(MAX_PROMOTED);
        if promote > 0 && promote < data_rows.len() {
            head_rows = data_rows.drain(..promote).collect();
        }
    }

    // Match this table to a TableSpec from the queue.  Pandoc gives us the
    // column count via c[2] (colspec array length).  We pop the next entry
    // whose column count matches, with a small look-ahead to tolerate cases
    // where source-list order differs from Pandoc's traversal order.
    let table_cols = c[2].as_array().map(|a| a.len()).unwrap_or(0);
    let spec = take_matching_spec(specs, table_cols);
    let vertical_rules = spec.vertical_rules;
    let alignments = extract_alignments(&c[2]);

    let head_size = head_rows.len();
    let total_rows = head_size + data_rows.len();

    // Bump the table counter regardless of caption presence so numbering
    // stays consistent even for caption-less tables.
    let n = counters.bump_table();
    // Caption goes ABOVE the table (academic convention for tables;
    // figures are the opposite, caption below).  Format: "[Table N: …]".
    if !caption.is_empty() {
        out.push(Block::Line(format!("[Table {n}: {caption}]")));
    } else {
        out.push(Block::Line(format!("[Table {n}]")));
    }

    if !head_rows.is_empty() {
        out.push(Block::Matrix {
            rows: head_rows,
            vertical_rules: vertical_rules.clone(),
            alignments: alignments.clone(),
        });
        out.push(Block::Rule);
    }

    // Translate source-row rule positions into body-row offsets.  Skip rules
    // at row 0 (toprule — already emitted by render's top_done logic),
    // at head_size (head/body boundary — already emitted above), and at
    // total_rows (bottomrule — emitted by the trailing Rule below).  What
    // remains splits the data Matrix into multiple Matrix+Rule chunks.
    let mut body_rule_offsets: Vec<usize> = spec
        .horizontal_rules
        .into_iter()
        .filter_map(|r| {
            if r > head_size && r < total_rows {
                Some(r - head_size)
            } else {
                None
            }
        })
        .collect();
    body_rule_offsets.sort_unstable();
    body_rule_offsets.dedup();

    if !data_rows.is_empty() {
        if body_rule_offsets.is_empty() {
            out.push(Block::Matrix {
                rows: data_rows,
                vertical_rules,
                alignments,
            });
            out.push(Block::Rule);
        } else {
            let mut start = 0usize;
            for offset in &body_rule_offsets {
                if *offset > start {
                    let chunk: Vec<_> = data_rows[start..*offset].to_vec();
                    out.push(Block::Matrix {
                        rows: chunk,
                        vertical_rules: vertical_rules.clone(),
                        alignments: alignments.clone(),
                    });
                    out.push(Block::Rule);
                }
                start = *offset;
            }
            if start < data_rows.len() {
                let chunk: Vec<_> = data_rows[start..].to_vec();
                out.push(Block::Matrix {
                    rows: chunk,
                    vertical_rules,
                    alignments,
                });
                out.push(Block::Rule);
            }
        }
    }

    // Trailing blank separates the table from the next paragraph of prose.
    // The caption sits above (emitted near the top of `out`).
    out.push(Block::Blank);

    out
}

/// Pop the first entry from `specs` whose column count equals `table_cols`,
/// looking ahead up to `LOOKAHEAD` entries.  Returns the matched
/// `TableSpec`, or a default (no rules) if no match is found.  Tolerant of
/// source-list ordering that doesn't perfectly mirror Pandoc's table
/// traversal — falls back to "no rules" rather than risk attaching the
/// wrong rules to a table.
fn take_matching_spec(specs: &mut Vec<TableSpec>, table_cols: usize) -> TableSpec {
    const LOOKAHEAD: usize = 3;
    let limit = specs.len().min(LOOKAHEAD);
    for k in 0..limit {
        if specs[k].col_count == table_cols {
            return specs.remove(k);
        }
    }
    TableSpec {
        col_count: table_cols,
        vertical_rules: Vec::new(),
        horizontal_rules: Vec::new(),
        column_gaps_after: Vec::new(),
    }
}

/// Count the leading rows of `rows` that look like header rows.  Stops at the
/// first data-like row.  Used to recover the head/body boundary when Pandoc
/// puts every row in the body (which happens for booktabs tables once cmidrule
/// commands have been stripped).
fn count_header_prefix(rows: &[Vec<(String, usize)>]) -> usize {
    rows.iter().take_while(|r| !looks_like_data_row(r)).count()
}

/// A row is "data-like" if its leading non-empty cells fit the
/// `text-then-number` pattern of a typical data row: either the first
/// non-empty cell starts with a digit (e.g. "1", "23.75"), or the second
/// non-empty cell does (e.g. "ByteNet", "23.75").
fn looks_like_data_row(row: &[(String, usize)]) -> bool {
    let mut non_empty = row.iter().map(|(t, _)| t.trim()).filter(|t| !t.is_empty());
    let first = match non_empty.next() {
        Some(s) => s,
        None => return false, // empty row — neither header nor data
    };
    if starts_with_ascii_digit(first) {
        return true;
    }
    if let Some(second) = non_empty.next()
        && starts_with_ascii_digit(second) {
            return true;
        }
    false
}

fn starts_with_ascii_digit(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_digit())
}

fn extract_rows(rows_json: &Value) -> Vec<Vec<(String, usize)>> {
    // Pandoc 3.x cell shape: cell = [attr, alignment, rowspan, colspan, [blocks]].
    // When a cell has rowspan > 1, subsequent rows omit cells in the columns the
    // rowspan covers — we have to insert blank placeholders so column alignment
    // is preserved (this is what \multirow{4}{*}{(A)} compiles to).
    let raw_rows = match rows_json.as_array() {
        Some(rs) => rs,
        None => return Vec::new(),
    };

    // carry[col] = number of further rows that column `col` is occupied by
    // an open rowspan from above.
    let mut carry: Vec<usize> = Vec::new();
    let mut out: Vec<Vec<(String, usize)>> = Vec::with_capacity(raw_rows.len());

    for row in raw_rows {
        let cells = match row[1].as_array() {
            Some(cs) => cs,
            None => {
                out.push(Vec::new());
                continue;
            }
        };

        let mut row_out: Vec<(String, usize)> = Vec::new();
        let mut col = 0usize;
        let mut cell_iter = cells.iter();

        loop {
            // If this column position is already claimed by a rowspan from
            // above, emit a blank filler and decrement the carry.
            if col < carry.len() && carry[col] > 0 {
                row_out.push((String::new(), 1));
                carry[col] -= 1;
                col += 1;
                continue;
            }
            // Otherwise consume the next user-supplied cell.
            let Some(cell) = cell_iter.next() else { break };
            let rowspan = cell[2].as_u64().unwrap_or(1).max(1) as usize;
            let colspan = cell[3].as_u64().unwrap_or(1).max(1) as usize;
            let text = extract_cell_text(&cell[4]);
            row_out.push((text, colspan));

            if rowspan > 1 {
                let end = col + colspan;
                if carry.len() < end {
                    carry.resize(end, 0);
                }
                for c in col..end {
                    carry[c] = rowspan - 1;
                }
            }
            col += colspan;
        }

        out.push(row_out);
    }

    out
}

fn extract_cell_text(blocks: &Value) -> String {
    blocks
        .as_array()
        .map(|bs| {
            bs.iter()
                .filter_map(|b| match b["t"].as_str()? {
                    "Para" | "Plain" => b["c"].as_array().map(|il| walk_inlines_text(il)),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

pub(super) fn extract_caption_text(cap: &Value) -> String {
    // Pandoc 3.x serialises Caption as [short_or_null, [blocks]].
    // Older serialisations use {"t":"Caption","c":[short,[blocks]]}.
    let blocks = if cap.is_array() {
        &cap[1]
    } else {
        &cap["c"][1]
    };
    blocks
        .as_array()
        .map(|bs| {
            bs.iter()
                .filter_map(|b| match b["t"].as_str()? {
                    "Para" | "Plain" => b["c"].as_array().map(|il| walk_inlines_text(il)),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}
