// Pandoc-based LaTeX parser.
//
// Runs `pandoc -f latex -t json` as a subprocess, walks the resulting AST, and
// emits Vec<Block> — the same type produced by parse.rs.  The caller tries this
// path first; if Pandoc is unavailable or returns nothing, it falls back to the
// hand-rolled parser.

use doc_model::{Block, InlineSpan, LinkTarget};
use serde_json::Value;

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn try_pandoc(sources: &[(String, String)]) -> Result<Vec<Block>, String> {
    // Quick availability check — bail early if pandoc is not on PATH.
    std::process::Command::new("pandoc")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|_| "pandoc not found".to_string())?;

    // Write all source files to a temp dir so \input{} resolution works.
    // (Pandoc resolves \input{} relative to cwd, not the file path.)
    // Each file is preprocessed first to rewrite LaTeX commands that Pandoc
    // mishandles (e.g. \multirow drops content silently when on its own line).
    let tmp = tempfile::TempDir::new().map_err(|e| e.to_string())?;
    for (name, content) in sources {
        let dest = tmp.path().join(name);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let processed = preprocess_latex_source(content);
        std::fs::write(&dest, processed.as_bytes())
            .map_err(|e| format!("write {name}: {e}"))?;
    }

    let root = find_root_name(sources);

    let output = std::process::Command::new("pandoc")
        .args(["-f", "latex", "-t", "json", "--quiet", &root])
        .current_dir(tmp.path())
        .output()
        .map_err(|e| format!("pandoc exec: {e}"))?;

    if output.stdout.is_empty() {
        return Err(format!(
            "pandoc produced no output: {}",
            String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(200)
                .collect::<String>()
        ));
    }

    let ast: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("pandoc JSON: {e}"))?;

    // Walk every source for tabular layout info — Pandoc strips `|` characters
    // (vertical rules) and mid-body `\hline` directives, so we recover both
    // directly from the source.
    let mut tabular_specs: Vec<TableSpec> = sources
        .iter()
        .flat_map(|(_, content)| extract_tabular_specs(content))
        .collect();

    let mut blocks = Vec::new();
    if let Some(meta) = ast.get("meta") {
        blocks.extend(extract_meta_blocks(meta));
    }
    let mut counters = SectionCounters::new();
    if let Some(arr) = ast["blocks"].as_array() {
        blocks.extend(walk_blocks(arr, 0, &mut tabular_specs, &mut counters));
    }

    if blocks.iter().all(|b| matches!(b, Block::Blank)) {
        return Err("pandoc produced no content".to_string());
    }
    Ok(blocks)
}

// ── Root file detection ───────────────────────────────────────────────────────

fn find_root_name(sources: &[(String, String)]) -> String {
    for (name, content) in sources {
        if content.contains(r"\begin{document}") {
            return name.clone();
        }
    }
    sources
        .iter()
        .max_by_key(|(_, c)| c.len())
        .map(|(name, _)| name.clone())
        .unwrap_or_default()
}

// ── Metadata ──────────────────────────────────────────────────────────────────

fn extract_meta_blocks(meta: &Value) -> Vec<Block> {
    let mut out = vec![Block::Blank];

    if let Some(title_meta) = meta.get("title") {
        if let Some(inlines) = title_meta["c"].as_array() {
            let text = walk_inlines_text(inlines);
            if !text.is_empty() {
                out.push(Block::Header { level: 1, text });
            }
        }
    }

    if let Some(author_meta) = meta.get("author") {
        let names = extract_author_names(author_meta);
        if !names.is_empty() {
            out.push(Block::Line(names.join(", ")));
        }
    }

    out.push(Block::Blank);
    out
}

// Each author's name line is immediately followed by a Note or Span (footnote mark) in the
// flat MetaInlines block Pandoc produces for \author{Name\thanks{} \\ Inst \And ...}.
fn extract_author_names(author_meta: &Value) -> Vec<String> {
    let items = match author_meta["t"].as_str() {
        Some("MetaList") => author_meta["c"].as_array().map(|a| a.as_slice()).unwrap_or(&[]),
        _ => return vec![],
    };

    if items.len() > 1 {
        // Multiple MetaList items — each is one author; take text before first LineBreak.
        return items
            .iter()
            .filter_map(|item| {
                let inlines: Vec<Value> = item["c"]
                    .as_array()?
                    .iter()
                    .take_while(|n| n["t"].as_str() != Some("LineBreak"))
                    .cloned()
                    .collect();
                let text = walk_inlines_text(&inlines).trim().to_string();
                if text.is_empty() { None } else { Some(text) }
            })
            .collect();
    }

    // Single MetaList item: all authors are concatenated as one flat MetaInlines.
    // Split at LineBreak; name lines are identified by a trailing Note or Span marker.
    let inlines = match items.first().and_then(|i| i["c"].as_array()) {
        Some(v) => v,
        None => return vec![],
    };

    let mut segments: Vec<Vec<Value>> = vec![vec![]];
    for n in inlines {
        if n["t"].as_str() == Some("LineBreak") {
            segments.push(vec![]);
        } else {
            segments.last_mut().unwrap().push(n.clone());
        }
    }

    let names: Vec<String> = segments
        .iter()
        .filter_map(|seg| {
            // Skip email lines (contain a Code inline).
            if seg.iter().any(|n| n["t"].as_str() == Some("Code")) {
                return None;
            }
            // Name lines have a Note or Span footnote marker.
            let has_marker =
                seg.iter().any(|n| matches!(n["t"].as_str(), Some("Note") | Some("Span")));
            if !has_marker {
                return None;
            }
            // Extract text before the marker.
            let text_inlines: Vec<Value> = seg
                .iter()
                .take_while(|n| !matches!(n["t"].as_str(), Some("Note") | Some("Span")))
                .cloned()
                .collect();
            let text = walk_inlines_text(&text_inlines).trim().to_string();
            if text.is_empty() { None } else { Some(text) }
        })
        .collect();

    if names.is_empty() {
        // Fallback for papers without \thanks{} markers: take the first line.
        segments
            .first()
            .map(|seg| walk_inlines_text(seg).trim().to_string())
            .filter(|s| !s.is_empty())
            .into_iter()
            .collect()
    } else {
        names
    }
}

// ── Block walker ──────────────────────────────────────────────────────────────

/// Section / subsection / subsubsection counters threaded through `walk_blocks`
/// so that headers carry their LaTeX-style number prefix ("1  Introduction",
/// "2.1  Background", "2.1.3  Detailed Steps"). Pandoc emits unnumbered headers
/// (`\section*{}` → `class="unnumbered"`); those are skipped here and rendered
/// without a prefix. Mirrors the legacy parser convention at `parse.rs:805`.
struct SectionCounters {
    sec: [u32; 3],
}

impl SectionCounters {
    fn new() -> Self { Self { sec: [0; 3] } }

    /// Increment the counter at `level` (1–3) and reset deeper levels.
    /// Returns the formatted prefix string, e.g. "2", "2.1", "2.1.3".
    fn bump(&mut self, level: u8) -> String {
        let lv = level.clamp(1, 3) as usize;
        self.sec[lv - 1] += 1;
        for i in lv..3 { self.sec[i] = 0; }
        match lv {
            1 => format!("{}", self.sec[0]),
            2 => format!("{}.{}", self.sec[0], self.sec[1]),
            _ => format!("{}.{}.{}", self.sec[0], self.sec[1], self.sec[2]),
        }
    }
}

fn walk_blocks(
    nodes: &[Value],
    list_depth: u8,
    specs: &mut Vec<TableSpec>,
    counters: &mut SectionCounters,
) -> Vec<Block> {
    let mut out = Vec::new();

    for node in nodes {
        let t = node["t"].as_str().unwrap_or("");
        let c = &node["c"];

        match t {
            "Para" | "Plain" => {
                if let Some(inlines) = c.as_array() {
                    if let Some(b) = para_to_block(inlines) {
                        out.push(b);
                        out.push(Block::Blank);
                    }
                }
            }

            "Header" => {
                let level = c[0].as_u64().unwrap_or(1).min(3) as u8;
                // c[1] = attr = [id, classes, key_vals]. Pandoc tags `\section*{}`
                // and friends with class="unnumbered" — skip prefix for those.
                let unnumbered = c[1][1].as_array().map_or(false, |classes| {
                    classes.iter().any(|cl| cl.as_str() == Some("unnumbered"))
                });
                // attr.id is the LaTeX label (or Pandoc-generated slug).
                // Emit an Anchor so reader can resolve `\ref{}` jumps.
                if let Some(id) = c[1][0].as_str() {
                    if !id.is_empty() {
                        out.push(Block::Anchor(id.to_string()));
                    }
                }
                if let Some(inlines) = c[2].as_array() {
                    let raw = walk_inlines_text(inlines);
                    if !raw.is_empty() {
                        let text = if unnumbered {
                            raw
                        } else {
                            format!("{}  {}", counters.bump(level), raw)
                        };
                        out.push(Block::Header { level, text });
                    }
                }
            }

            // Suppress prose-level horizontal rules (bibliography separators, \hrule, etc.).
            // Block::Rule is reserved for programmatic table separators in parse_table().
            "HorizontalRule" => out.push(Block::Blank),

            "CodeBlock" => {
                let lang = c[0][1]
                    .as_array()
                    .and_then(|cls| cls.first())
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                if let Some(code) = c[1].as_str() {
                    if !code.is_empty() {
                        out.push(Block::CodeBlock {
                            lang,
                            lines: code.lines().map(|l| l.to_string()).collect(),
                        });
                    }
                }
            }

            "BlockQuote" => {
                if let Some(inner) = c.as_array() {
                    let spans: Vec<InlineSpan> = inner
                        .iter()
                        .flat_map(|b| match b["t"].as_str() {
                            Some("Para") | Some("Plain") => b["c"]
                                .as_array()
                                .map(|il| walk_inlines_spans(il))
                                .unwrap_or_default(),
                            _ => vec![],
                        })
                        .collect();
                    if !spans.is_empty() {
                        out.push(Block::Quote(spans));
                    }
                }
            }

            "BulletList" => {
                if let Some(items) = c.as_array() {
                    for item in items {
                        if let Some(item_blocks) = item.as_array() {
                            out.extend(list_item_blocks(item_blocks, list_depth, "•", specs, counters));
                        }
                    }
                }
            }

            "OrderedList" => {
                // c = [list_attrs, [[blocks], ...]]
                // list_attrs = [start_num, style, delimiter]
                let start = c[0][0].as_u64().unwrap_or(1) as usize;
                if let Some(items) = c[1].as_array() {
                    for (i, item) in items.iter().enumerate() {
                        let marker = format!("{}.", start + i);
                        if let Some(item_blocks) = item.as_array() {
                            out.extend(list_item_blocks(item_blocks, list_depth, &marker, specs, counters));
                        }
                    }
                }
            }

            "Table" => out.extend(parse_table(c, specs)),

            "Div" => {
                // c = [attr, [blocks]].  attr.id may carry a label
                // (e.g. table/figure/equation envs in newer Pandoc); emit
                // an Anchor so reader can resolve refs.  Bib entry divs
                // (id starts with "ref-") are picked up here too — the
                // reader's bib-entries collector keys off these ids.
                if let Some(id) = c[0][0].as_str() {
                    if !id.is_empty() {
                        out.push(Block::Anchor(id.to_string()));
                    }
                }
                if let Some(inner) = c[1].as_array() {
                    out.extend(walk_blocks(inner, list_depth, specs, counters));
                }
            }

            "Figure" => {
                // c = [attr, caption, [blocks]]
                let cap = extract_caption_text(&c[1]);
                if !cap.is_empty() {
                    out.push(Block::Line(format!("[Figure: {cap}]")));
                    out.push(Block::Blank);
                }
            }

            "LineBlock" => {
                // c = [[inlines], [inlines], ...]  — each line is an inline list
                if let Some(lines) = c.as_array() {
                    for line in lines {
                        if let Some(inlines) = line.as_array() {
                            let text = walk_inlines_text(inlines);
                            if !text.is_empty() {
                                out.push(Block::Line(text));
                            }
                        }
                    }
                }
            }

            // RawBlock, Null — skip
            _ => {}
        }
    }

    out
}

// ── Para → Block ──────────────────────────────────────────────────────────────

fn para_to_block(inlines: &[Value]) -> Option<Block> {
    // Check for a lone DisplayMath inline (possibly surrounded by whitespace).
    let meaningful: Vec<&Value> = inlines
        .iter()
        .filter(|n| !matches!(n["t"].as_str(), Some("Space") | Some("SoftBreak")))
        .collect();

    if meaningful.len() == 1 {
        let node = meaningful[0];
        if node["t"].as_str() == Some("Math")
            && node["c"][0]["t"].as_str() == Some("DisplayMath")
        {
            let latex = node["c"][1].as_str().unwrap_or("");
            let rendered = render_math(latex);
            let lines: Vec<String> = rendered.lines().map(|l| l.to_string()).collect();
            return Some(Block::DisplayMath { lines, num: None });
        }
    }

    let spans = walk_inlines_spans(inlines);
    if spans.is_empty() {
        return Some(Block::Blank);
    }

    // Always use StyledLine so build_visual_lines wraps the text to terminal_width.
    // Block::Line assumes the producer already wrapped it; Pandoc gives us full paragraphs.
    Some(Block::StyledLine(spans))
}

// ── List item helper ──────────────────────────────────────────────────────────

fn list_item_blocks(
    item_blocks: &[Value],
    depth: u8,
    marker: &str,
    specs: &mut Vec<TableSpec>,
    counters: &mut SectionCounters,
) -> Vec<Block> {
    let mut out = Vec::new();
    let mut emitted_item = false;

    for block in item_blocks {
        let t = block["t"].as_str().unwrap_or("");
        if !emitted_item && (t == "Para" || t == "Plain") {
            if let Some(inlines) = block["c"].as_array() {
                let content = walk_inlines_spans(inlines);
                if !content.is_empty() {
                    out.push(Block::ListItem {
                        depth,
                        marker: marker.to_string(),
                        content,
                    });
                    emitted_item = true;
                    continue;
                }
            }
        }
        // Nested lists or extra blocks inside an item.
        out.extend(walk_blocks(
            std::slice::from_ref(block),
            depth.saturating_add(1),
            specs,
            counters,
        ));
        emitted_item = true;
    }

    out
}

// ── Table parsing ─────────────────────────────────────────────────────────────

fn parse_table(c: &Value, specs: &mut Vec<TableSpec>) -> Vec<Block> {
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

    let head_size = head_rows.len();
    let total_rows = head_size + data_rows.len();

    // Caption goes ABOVE the table (academic convention for tables;
    // figures are the opposite, caption below).  Emitted as the first
    // block so the whole [Caption, Rule, Matrix, ...] group can be lifted
    // as a unit by future placement passes.
    if !caption.is_empty() {
        out.push(Block::Line(format!("[Table: {caption}]")));
    }

    if !head_rows.is_empty() {
        out.push(Block::Matrix { rows: head_rows, vertical_rules: vertical_rules.clone() });
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
            out.push(Block::Matrix { rows: data_rows, vertical_rules });
            out.push(Block::Rule);
        } else {
            let mut start = 0usize;
            for offset in &body_rule_offsets {
                if *offset > start {
                    let chunk: Vec<_> = data_rows[start..*offset].to_vec();
                    out.push(Block::Matrix { rows: chunk, vertical_rules: vertical_rules.clone() });
                    out.push(Block::Rule);
                }
                start = *offset;
            }
            if start < data_rows.len() {
                let chunk: Vec<_> = data_rows[start..].to_vec();
                out.push(Block::Matrix { rows: chunk, vertical_rules });
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
    TableSpec { col_count: table_cols, vertical_rules: Vec::new(), horizontal_rules: Vec::new() }
}

/// Count the leading rows of `rows` that look like header rows.  Stops at the
/// first data-like row.  Used to recover the head/body boundary when Pandoc
/// puts every row in the body (which happens for booktabs tables once cmidrule
/// commands have been stripped).
fn count_header_prefix(rows: &[Vec<(String, usize)>]) -> usize {
    rows.iter()
        .take_while(|r| !looks_like_data_row(r))
        .count()
}

/// A row is "data-like" if its leading non-empty cells fit the
/// `text-then-number` pattern of a typical data row: either the first
/// non-empty cell starts with a digit (e.g. "1", "23.75"), or the second
/// non-empty cell does (e.g. "ByteNet", "23.75").
fn looks_like_data_row(row: &[(String, usize)]) -> bool {
    let mut non_empty = row
        .iter()
        .map(|(t, _)| t.trim())
        .filter(|t| !t.is_empty());
    let first = match non_empty.next() {
        Some(s) => s,
        None => return false, // empty row — neither header nor data
    };
    if starts_with_ascii_digit(first) {
        return true;
    }
    if let Some(second) = non_empty.next() {
        if starts_with_ascii_digit(second) {
            return true;
        }
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
                    "Para" | "Plain" => {
                        b["c"].as_array().map(|il| walk_inlines_text(il))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

fn extract_caption_text(cap: &Value) -> String {
    // Pandoc 3.x serialises Caption as [short_or_null, [blocks]].
    // Older serialisations use {"t":"Caption","c":[short,[blocks]]}.
    let blocks = if cap.is_array() { &cap[1] } else { &cap["c"][1] };
    blocks
        .as_array()
        .map(|bs| {
            bs.iter()
                .filter_map(|b| match b["t"].as_str()? {
                    "Para" | "Plain" => {
                        b["c"].as_array().map(|il| walk_inlines_text(il))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

// ── Inline walkers ────────────────────────────────────────────────────────────

/// LaTeX prefix words that typically introduce a `\ref{...}` and should
/// share its link styling — so "Table 3", "Figure 4", "Section 6.1"
/// render as cohesive linked phrases instead of "(prose) (linked-number)".
/// Case-insensitive match; trailing dot on abbreviations matched.
const REF_PREFIX_WORDS: &[&str] = &[
    "table", "tab.", "tab",
    "figure", "fig.", "fig",
    "section", "sec.", "sec",
    "equation", "eq.", "eq",
    "algorithm", "alg.", "alg",
    "theorem", "thm.", "thm",
    "lemma", "lem.",
    "chapter", "chap.",
    "appendix", "app.",
    "definition", "def.",
    "proposition", "prop.",
    "corollary", "cor.",
];

/// Walk backward through `out` from the tail.  If the most recent prose
/// is whitespace followed by a span whose stripped text exactly matches
/// a `REF_PREFIX_WORDS` entry (case-insensitive), set `link_target` on
/// both that span and the whitespace span so a phrase like "Table 3"
/// gets uniform link styling.
fn extend_link_back_to_prefix(out: &mut [InlineSpan], target: &LinkTarget) {
    if out.is_empty() { return; }
    let mut idx = out.len();
    // Skip an optional trailing whitespace span.
    if idx > 0 && out[idx - 1].text.trim().is_empty() {
        idx -= 1;
    }
    if idx == 0 { return; }
    let candidate = &out[idx - 1];
    let trimmed = candidate.text.trim().to_ascii_lowercase();
    if !REF_PREFIX_WORDS.contains(&trimmed.as_str()) {
        return;
    }
    // Both the candidate and any whitespace span between it and the
    // forthcoming link get the link target.
    if out[idx - 1].link_target.is_none() {
        out[idx - 1].link_target = Some(target.clone());
    }
    if idx < out.len() && out[idx].link_target.is_none() {
        out[idx].link_target = Some(target.clone());
    }
}

fn walk_inlines_text(inlines: &[Value]) -> String {
    walk_inlines_spans(inlines)
        .into_iter()
        .map(|s| s.text)
        .collect()
}

fn walk_inlines_spans(inlines: &[Value]) -> Vec<InlineSpan> {
    let mut out = Vec::new();

    for node in inlines {
        let t = node["t"].as_str().unwrap_or("");
        let c = &node["c"];

        match t {
            "Str" => {
                if let Some(s) = c.as_str() {
                    // U+00A0 (non-breaking space from LaTeX ~) → regular space.
                    // Do NOT trim — trailing spaces from ~ before \ref{} are load-bearing;
                    // trimming them causes adjacent tokens to merge ("Figure1", "consumemodel").
                    let s = s.replace('\u{00A0}', " ");
                    if !s.is_empty() {
                        out.push(InlineSpan::plain(&s));
                    }
                }
            }

            "Space" | "SoftBreak" => out.push(InlineSpan::plain(" ")),
            "LineBreak" => out.push(InlineSpan::plain("\n")),

            "Emph" => {
                if let Some(inner) = c.as_array() {
                    for mut span in walk_inlines_spans(inner) {
                        span.italic = true;
                        out.push(span);
                    }
                }
            }

            "Strong" => {
                if let Some(inner) = c.as_array() {
                    for mut span in walk_inlines_spans(inner) {
                        span.bold = true;
                        out.push(span);
                    }
                }
            }

            "Underline" => {
                if let Some(inner) = c.as_array() {
                    for mut span in walk_inlines_spans(inner) {
                        span.underline = true;
                        out.push(span);
                    }
                }
            }

            "Strikeout" => {
                if let Some(inner) = c.as_array() {
                    for mut span in walk_inlines_spans(inner) {
                        span.strikethrough = true;
                        out.push(span);
                    }
                }
            }

            "SmallCaps" => {
                if let Some(inner) = c.as_array() {
                    out.extend(walk_inlines_spans(inner));
                }
            }

            // Wrap super/subscript in brackets so "d_k" renders as "d[k]" not "dk".
            "Superscript" => {
                if let Some(inner) = c.as_array() {
                    out.push(InlineSpan::plain("^("));
                    out.extend(walk_inlines_spans(inner));
                    out.push(InlineSpan::plain(")"));
                }
            }

            "Subscript" => {
                if let Some(inner) = c.as_array() {
                    out.push(InlineSpan::plain("_("));
                    out.extend(walk_inlines_spans(inner));
                    out.push(InlineSpan::plain(")"));
                }
            }

            "Code" => {
                // c = [attr, text]
                if let Some(text) = c[1].as_str() {
                    out.push(InlineSpan { monospace: true, ..InlineSpan::plain(text) });
                }
            }

            "Math" => {
                let kind = c[0]["t"].as_str().unwrap_or("InlineMath");
                let latex = c[1].as_str().unwrap_or("");
                if kind == "DisplayMath" {
                    // Normally caught in para_to_block; guard the in-line fall-through.
                    let rendered = render_math(latex);
                    out.push(InlineSpan::plain(format!("  {rendered}  ")));
                } else {
                    // Inline math: use strip_latex path so the result is always a
                    // single compact token — tui_math's vertical output breaks wrapping.
                    out.push(InlineSpan::plain(render_inline_math(latex)));
                }
            }

            "Link" => {
                // c = [attr, inlines, [url, title]]
                // External URLs surface as OSC 8 hyperlinks via `span.url`.
                // Internal `#anchor` refs become `LinkTarget::Internal(anchor)`
                // for jump-to-line by `Enter` in the reader.
                let raw_url = c[2][0].as_str().unwrap_or("");
                let url = if raw_url.starts_with("http://") || raw_url.starts_with("https://") {
                    Some(raw_url.to_string())
                } else {
                    None
                };
                let internal: Option<LinkTarget> = raw_url
                    .strip_prefix('#')
                    .filter(|a| !a.is_empty())
                    .map(|a| LinkTarget::Internal(a.to_string()));

                // For internal refs, papers typically write "Table~\ref{X}",
                // and Pandoc emits ["Table", " ", Link("3")].  Without
                // back-propagation, only "3" is styled — "Table 3" looks
                // half-linked.  Walk backward through whitespace and into
                // the previous prose span; if it ends with a known ref
                // prefix word, extend the link styling to cover it.
                if let Some(target) = &internal {
                    extend_link_back_to_prefix(&mut out, target);
                }

                if let Some(inner) = c[1].as_array() {
                    for mut span in walk_inlines_spans(inner) {
                        if span.url.is_none() {
                            span.url = url.clone();
                        }
                        if span.link_target.is_none() {
                            span.link_target = internal.clone();
                        }
                        out.push(span);
                    }
                }
            }

            "Quoted" => {
                // c = [quote_type, inlines]
                let (open, close) = match c[0]["t"].as_str() {
                    Some("DoubleQuote") => ("\u{201C}", "\u{201D}"),
                    _ => ("\u{2018}", "\u{2019}"),
                };
                out.push(InlineSpan::plain(open));
                if let Some(inner) = c[1].as_array() {
                    out.extend(walk_inlines_spans(inner));
                }
                out.push(InlineSpan::plain(close));
            }

            "Cite" => {
                // c = [citations, fallback_inlines]
                // Without `--citeproc`, Pandoc leaves the fallback as raw
                // LaTeX (e.g. RawInline "\\citep{X}") which we'd render as
                // nothing — citations would silently disappear.  Instead
                // we render the cite-keys ourselves: `[key]` or
                // `[key1, key2]` for multi-cite, styled as a link to the
                // bibliography.  `LinkTarget::Citation` carries the
                // *first* key; the popup/jump uses that one.
                let keys: Vec<String> = c[0]
                    .as_array()
                    .map(|cs| {
                        cs.iter()
                            .filter_map(|cit| cit["citationId"].as_str().map(|s| s.to_string()))
                            .filter(|s| !s.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                if keys.is_empty() {
                    // No citation keys — fall back to the inner inlines
                    // (RawInline most likely; produces nothing visible
                    // but won't crash).
                    if let Some(inner) = c[1].as_array() {
                        out.extend(walk_inlines_spans(inner));
                    }
                } else {
                    let primary = keys[0].clone();
                    let rendered = format!("[{}]", keys.join(", "));
                    out.push(InlineSpan::citation(rendered, primary));
                }
            }

            "Span" => {
                // c = [attr, inlines]  where attr = [id, [classes], [kvs]]
                // Skip footnote markers — they leak superscript letters/numbers into prose.
                let is_footnote = c[0][1].as_array().map_or(false, |cls| {
                    cls.iter().any(|cl| {
                        matches!(
                            cl.as_str(),
                            Some("footnote-ref") | Some("footnote-mark") | Some("footnote")
                        )
                    })
                });
                if !is_footnote {
                    if let Some(inner) = c[1].as_array() {
                        out.extend(walk_inlines_spans(inner));
                    }
                }
            }

            "Image" => {
                // c = [attr, alt_inlines, [url, title]]
                if let Some(alt) = c[1].as_array() {
                    let text = walk_inlines_text(alt);
                    if !text.is_empty() {
                        out.push(InlineSpan::plain(format!("[Image: {text}]")));
                    }
                }
            }

            // RawInline, Note — skip
            _ => {}
        }
    }

    // Collapse consecutive all-space spans into one, avoiding double-space gaps
    // that arise when "Figure\u{00A0}" + Space node both produce " ".
    dedup_spaces(out)
}

fn dedup_spaces(spans: Vec<InlineSpan>) -> Vec<InlineSpan> {
    let mut out = Vec::with_capacity(spans.len());
    let mut prev_is_space = false;
    for span in spans {
        let is_space = span.text.chars().all(|c| c == ' ');
        if is_space && prev_is_space {
            continue;
        }
        prev_is_space = is_space;
        out.push(span);
    }
    out
}

// ── Math rendering ─────────────────────────────────────────────────────────────

// Display math: full pipeline including tui_math vertical layout (multi-line OK).
fn render_math(latex: &str) -> String {
    let s = latex.trim();
    let inner = strip_env_wrapper(s).unwrap_or(s);
    math_render::render(math_render::MathInput::Latex(inner))
}

// Inline math: strip_latex only — tui_math's vertical output fragments into
// separate words when split_whitespace runs on the resulting span text.
fn render_inline_math(latex: &str) -> String {
    let s = latex.trim();
    let inner = strip_env_wrapper(s).unwrap_or(s);
    math_render::render_inline(inner)
}

// Strip \begin{env}...\end{env} wrappers that Pandoc includes in Math node content
// (e.g. \begin{align*}MultiHead...\end{align*} → just the body).
fn strip_env_wrapper(s: &str) -> Option<&str> {
    if !s.starts_with("\\begin{") {
        return None;
    }
    let close = s.find('}')?;
    let env = &s[7..close];
    let end_tag_len = 6 + env.len() + 1; // \end{ + env + }
    let body = s[close + 1..].trim_start_matches('\n');
    // Find last occurrence of \end{env} to handle nested envs correctly.
    let end_pos = body.rfind(&format!("\\end{{{}}}", env))?;
    let inner = body[..end_pos].trim();
    // Guard against infinite recursion: if inner still starts with the same env, give up.
    if inner.starts_with("\\begin{") && inner[7..].starts_with(env) {
        return None;
    }
    let _ = end_tag_len; // suppress unused warning
    Some(inner)
}

// ── Pre-Pandoc LaTeX preprocessing ────────────────────────────────────────────

/// Rewrite LaTeX constructs that Pandoc mishandles before sending to the parser.
///
/// Currently handles:
/// - `\multirow{N}{W}{TEXT}` → `TEXT`. Pandoc silently drops the content when
///   the multirow declaration appears on its own line before a row, so row
///   labels like `(A)`, `(B)` vanish from the output.
/// - `\cmidrule[W](T){N-M}` → ``. Pandoc drops the command name but keeps the
///   brace content as plain text (e.g. "2-3"), which leaks into the next cell.
fn preprocess_latex_source(src: &str) -> String {
    let after_multirow = strip_multirow(src);
    strip_cmidrule(&after_multirow)
}

fn strip_multirow(src: &str) -> String {
    let bytes = src.as_bytes();
    let cmd = b"\\multirow";
    let mut out = String::with_capacity(src.len());
    let mut copy_from = 0usize;
    let mut i = 0;
    while i + cmd.len() <= bytes.len() {
        if &bytes[i..i + cmd.len()] == cmd {
            let after = i + cmd.len();
            let bnd_ok = after >= bytes.len() || !bytes[after].is_ascii_alphanumeric();
            if bnd_ok {
                if let Some((arg3_start, arg3_end, end)) =
                    parse_three_brace_args(bytes, after)
                {
                    out.push_str(&src[copy_from..i]);
                    out.push_str(&src[arg3_start..arg3_end]);
                    i = end;
                    copy_from = end;
                    continue;
                }
            }
        }
        i += 1;
    }
    out.push_str(&src[copy_from..]);
    out
}

fn strip_cmidrule(src: &str) -> String {
    let bytes = src.as_bytes();
    let cmd = b"\\cmidrule";
    let mut out = String::with_capacity(src.len());
    let mut copy_from = 0usize;
    let mut i = 0;
    while i + cmd.len() <= bytes.len() {
        if &bytes[i..i + cmd.len()] == cmd {
            let after = i + cmd.len();
            let bnd_ok = after >= bytes.len() || !bytes[after].is_ascii_alphanumeric();
            if bnd_ok {
                if let Some(end) = parse_cmidrule_args(bytes, after) {
                    out.push_str(&src[copy_from..i]);
                    // Drop the command entirely — surrounding whitespace
                    // already separates it from neighbouring tokens.
                    i = end;
                    copy_from = end;
                    continue;
                }
            }
        }
        i += 1;
    }
    out.push_str(&src[copy_from..]);
    out
}

/// Parse `{...}{...}{...}` (skipping ASCII whitespace between groups) starting
/// at byte position `pos`. Returns `(arg3_content_start, arg3_content_end, end)`
/// where `end` is the byte position just past the closing brace of arg 3.
fn parse_three_brace_args(bytes: &[u8], mut pos: usize) -> Option<(usize, usize, usize)> {
    pos = skip_ascii_ws(bytes, pos);
    let close1 = match_brace(bytes, pos)?;
    pos = skip_ascii_ws(bytes, close1 + 1);
    let close2 = match_brace(bytes, pos)?;
    pos = skip_ascii_ws(bytes, close2 + 1);
    if pos >= bytes.len() || bytes[pos] != b'{' {
        return None;
    }
    let close3 = match_brace(bytes, pos)?;
    Some((pos + 1, close3, close3 + 1))
}

/// Parse `\cmidrule` arguments: optional `[width]`, optional `(trim)`, required
/// `{N-M}`. Returns the byte position just past the final `}`.
fn parse_cmidrule_args(bytes: &[u8], mut pos: usize) -> Option<usize> {
    pos = skip_ascii_ws(bytes, pos);
    if pos < bytes.len() && bytes[pos] == b'[' {
        pos = match_delim(bytes, pos, b'[', b']')? + 1;
        pos = skip_ascii_ws(bytes, pos);
    }
    if pos < bytes.len() && bytes[pos] == b'(' {
        pos = match_delim(bytes, pos, b'(', b')')? + 1;
        pos = skip_ascii_ws(bytes, pos);
    }
    if pos >= bytes.len() || bytes[pos] != b'{' {
        return None;
    }
    let close = match_brace(bytes, pos)?;
    Some(close + 1)
}

/// Find the matching close delimiter for an opening one.  Honours `\\` escapes
/// and supports nested delimiters of the same kind.
fn match_delim(bytes: &[u8], open: usize, open_ch: u8, close_ch: u8) -> Option<usize> {
    if open >= bytes.len() || bytes[open] != open_ch {
        return None;
    }
    let mut depth = 1;
    let mut i = open + 1;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' {
            i += 1;
            if i < bytes.len() {
                i += utf8_char_width(bytes[i]);
            }
            continue;
        }
        if b == open_ch {
            depth += 1;
        } else if b == close_ch {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += utf8_char_width(bytes[i]);
    }
    None
}

fn skip_ascii_ws(bytes: &[u8], mut p: usize) -> usize {
    while p < bytes.len() && bytes[p].is_ascii_whitespace() {
        p += 1;
    }
    p
}

/// Given that `bytes[open]` is `b'{'`, return the byte position of the matching
/// `}`. Honours `\\` escapes so `\\}` doesn't end the group.
fn match_brace(bytes: &[u8], open: usize) -> Option<usize> {
    if open >= bytes.len() || bytes[open] != b'{' {
        return None;
    }
    let mut depth = 1;
    let mut i = open + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                // Skip the backslash and the following codepoint (one Unicode
                // char, not necessarily one byte — `\é` etc. are valid LaTeX).
                i += 1;
                if i < bytes.len() {
                    i += utf8_char_width(bytes[i]);
                }
                continue;
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += utf8_char_width(bytes[i]);
    }
    None
}

// ── Tabular column-spec extraction (vertical & horizontal rules) ─────────────

/// Per-table layout info recovered from the LaTeX source.  Pandoc discards
/// both `|` characters (vertical rules) and mid-body `\hline`/`\specialrule`
/// directives (horizontal rules between data groups), so we extract both
/// directly from the source.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TableSpec {
    pub col_count: usize,
    /// Raw column indices BEFORE which a vertical rule appears (0 = left
    /// edge, col_count = right edge).
    pub vertical_rules: Vec<usize>,
    /// Source row indices BEFORE which a horizontal rule appears (0 = top of
    /// table, total_rows = bottom).  Top/bottom positions are deduplicated
    /// against the renderer's auto-emitted top/bottom rules; positions in
    /// between split the body into groups.
    pub horizontal_rules: Vec<usize>,
}

/// Walk a LaTeX source string and pull out the column spec and horizontal
/// rule positions of every `\begin{tabular}{...}` and `\begin{tabular*}{...}{...}`
/// (and the long* variants).  Specs we can't parse (e.g. those using the
/// `*{N}{...}` repeat operator) are skipped silently — the affected table
/// will render with no vertical/horizontal rules instead of risking misalignment.
pub(crate) fn extract_tabular_specs(src: &str) -> Vec<TableSpec> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        // Skip `%...\n` line comments — Pandoc ignores them, so we must too.
        // Without this, commented-out `\begin{tabular}{...}` lines (common in
        // arXiv sources) would pollute the spec queue and attach rules to
        // unrelated tables. `\%` is a literal percent — preserve it.
        if bytes[i] == b'%' && !preceded_by_odd_backslashes(bytes, i) {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // Match openers as byte slices — avoids string-slicing at arbitrary
        // positions (which would panic if `i` happened to land mid-codepoint).
        let opener = if bytes[i..].starts_with(b"\\begin{tabular*}") {
            Some((b"\\begin{tabular*}".len(), true))
        } else if bytes[i..].starts_with(b"\\begin{tabular}") {
            Some((b"\\begin{tabular}".len(), false))
        } else if bytes[i..].starts_with(b"\\begin{longtable*}") {
            Some((b"\\begin{longtable*}".len(), true))
        } else if bytes[i..].starts_with(b"\\begin{longtable}") {
            Some((b"\\begin{longtable}".len(), false))
        } else {
            None
        };
        if let Some((skip, has_width_arg)) = opener {
            let mut p = i + skip;
            p = skip_ascii_ws(bytes, p);
            if p < bytes.len() && bytes[p] == b'[' {
                if let Some(close) = match_delim(bytes, p, b'[', b']') {
                    p = skip_ascii_ws(bytes, close + 1);
                } else {
                    i += utf8_char_width(bytes[i]);
                    continue;
                }
            }
            if has_width_arg {
                if p >= bytes.len() || bytes[p] != b'{' {
                    i += utf8_char_width(bytes[i]);
                    continue;
                }
                let close = match match_brace(bytes, p) {
                    Some(c) => c,
                    None => { i += utf8_char_width(bytes[i]); continue; }
                };
                p = skip_ascii_ws(bytes, close + 1);
            }
            if p >= bytes.len() || bytes[p] != b'{' {
                i += utf8_char_width(bytes[i]);
                continue;
            }
            let close = match match_brace(bytes, p) {
                Some(c) => c,
                None => { i += utf8_char_width(bytes[i]); continue; }
            };
            // p+1 is past `{` (ASCII), close is the position of `}` (ASCII)
            // — both are guaranteed char boundaries, so this slice is safe.
            let spec_str = &src[p + 1..close];
            if let Some((col_count, vertical_rules)) = parse_column_spec(spec_str) {
                let content_start = close + 1;
                let (horizontal_rules, content_end) = scan_table_horizontal_rules(bytes, content_start);
                out.push(TableSpec { col_count, vertical_rules, horizontal_rules });
                i = content_end;
                continue;
            }
            i = close + 1;
            continue;
        }
        i += utf8_char_width(bytes[i]);
    }
    out
}

/// Walk a tabular env's content (starting just after the column spec's `}`)
/// up to its `\end{tabular...}` and return the row indices BEFORE which a
/// horizontal rule appears, plus the byte position just past the `\end`.
///
/// Recognised rule directives: `\hline`, `\midrule`, `\toprule`, `\bottomrule`,
/// and `\specialrule{...}{...}{...}`.  Skipped: `\rule{...}{...}` (a vertical
/// strut, not a rule), `\cmidrule(...){n-m}` and `\cline{n-m}` (partial rules
/// with no terminal equivalent).
fn scan_table_horizontal_rules(bytes: &[u8], start: usize) -> (Vec<usize>, usize) {
    let mut rules: Vec<usize> = Vec::new();
    let mut row_idx = 0usize;
    let mut i = start;
    while i < bytes.len() {
        // Stop at any \end{tabular...} or \end{longtable...}.
        for end_tag in [
            b"\\end{tabular}".as_slice(),
            b"\\end{tabular*}".as_slice(),
            b"\\end{longtable}".as_slice(),
            b"\\end{longtable*}".as_slice(),
        ] {
            if bytes[i..].starts_with(end_tag) {
                return (rules, i + end_tag.len());
            }
        }
        // Comments — Pandoc ignores, we must too.
        if bytes[i] == b'%' && !preceded_by_odd_backslashes(bytes, i) {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // Row terminator: `\\` followed by optional `[<dim>]` and optional `*`.
        if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
            i += 2;
            i = skip_ascii_ws(bytes, i);
            if i < bytes.len() && bytes[i] == b'[' {
                if let Some(close) = match_delim(bytes, i, b'[', b']') {
                    i = close + 1;
                }
            }
            if i < bytes.len() && bytes[i] == b'*' {
                i += 1;
            }
            row_idx += 1;
            continue;
        }
        // Rule directives.  Each requires a non-alphanumeric boundary so we
        // don't match `\hlinexxx` etc.
        let no_arg_directive = if bytes[i..].starts_with(b"\\hline") {
            Some(b"\\hline".len())
        } else if bytes[i..].starts_with(b"\\midrule") {
            Some(b"\\midrule".len())
        } else if bytes[i..].starts_with(b"\\toprule") {
            Some(b"\\toprule".len())
        } else if bytes[i..].starts_with(b"\\bottomrule") {
            Some(b"\\bottomrule".len())
        } else {
            None
        };
        if let Some(skip) = no_arg_directive {
            let after = i + skip;
            if after >= bytes.len() || !bytes[after].is_ascii_alphanumeric() {
                if rules.last() != Some(&row_idx) {
                    rules.push(row_idx);
                }
                i = after;
                continue;
            }
        }
        // \specialrule{}{}{}
        if bytes[i..].starts_with(b"\\specialrule") {
            let after = i + b"\\specialrule".len();
            if after >= bytes.len() || !bytes[after].is_ascii_alphanumeric() {
                if let Some((_, _, end)) = parse_three_brace_args(bytes, after) {
                    if rules.last() != Some(&row_idx) {
                        rules.push(row_idx);
                    }
                    i = end;
                    continue;
                }
            }
        }
        i += utf8_char_width(bytes[i]);
    }
    (rules, i)
}

/// Number of bytes in the UTF-8 codepoint that starts at this byte.  Defaults
/// to 1 for ill-formed leading bytes so the scanner makes progress instead of
/// looping forever.
fn utf8_char_width(b: u8) -> usize {
    if b < 0x80 { 1 }
    else if b < 0xC2 { 1 }
    else if b < 0xE0 { 2 }
    else if b < 0xF0 { 3 }
    else { 4 }
}

/// True iff `bytes[pos]` is preceded by an odd number of consecutive
/// backslashes — meaning the character at `pos` is escaped (`\%`, `\\\%`).
/// `\\%` and `\\\\%` are NOT escaped (the `\` themselves are escaped pairs).
fn preceded_by_odd_backslashes(bytes: &[u8], pos: usize) -> bool {
    let mut count = 0usize;
    let mut p = pos;
    while p > 0 && bytes[p - 1] == b'\\' {
        count += 1;
        p -= 1;
    }
    count % 2 == 1
}

/// Parse a LaTeX `tabular` column spec into its column count and the column
/// indices BEFORE which a `|` vertical rule appears.
///
/// Recognised primitives:
/// - `c`, `l`, `r`             — a column (count += 1)
/// - `p{...}`, `m{...}`, `b{...}` — a column with width arg (count += 1, skip arg)
/// - `|`                       — vertical rule before next column
/// - `>{...}`, `<{...}`,
///   `!{...}`, `@{...}`        — column-modifier macros (skip brace group)
/// - whitespace                — ignored
///
/// Returns `None` if the spec uses unsupported syntax (e.g. `*{N}{spec}` or
/// any character we don't recognise) — graceful degradation: the table will
/// render with no vertical rules.
pub(crate) fn parse_column_spec(spec: &str) -> Option<(usize, Vec<usize>)> {
    let bytes = spec.as_bytes();
    let mut col_count: usize = 0;
    let mut rules: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b' ' | b'\t' | b'\n' | b'\r' => {
                i += 1;
            }
            b'|' => {
                rules.push(col_count);
                i += 1;
            }
            b'c' | b'l' | b'r' => {
                col_count += 1;
                i += 1;
            }
            b'p' | b'm' | b'b' => {
                // Width-arg column.  Must be followed (after optional ws) by `{...}`.
                let mut p = i + 1;
                p = skip_ascii_ws(bytes, p);
                if p < bytes.len() && bytes[p] == b'{' {
                    let close = match_brace(bytes, p)?;
                    col_count += 1;
                    i = close + 1;
                } else {
                    return None;
                }
            }
            b'>' | b'<' | b'!' | b'@' => {
                // Modifier macro — skip the following `{...}` group.
                let mut p = i + 1;
                p = skip_ascii_ws(bytes, p);
                if p < bytes.len() && bytes[p] == b'{' {
                    let close = match_brace(bytes, p)?;
                    i = close + 1;
                } else {
                    return None;
                }
            }
            _ => {
                // Unknown character (`*`, custom column types, etc.) — bail out.
                return None;
            }
        }
    }
    Some((col_count, rules))
}

#[cfg(test)]
mod section_counter_tests {
    use super::SectionCounters;

    #[test]
    fn single_section_increments() {
        let mut c = SectionCounters::new();
        assert_eq!(c.bump(1), "1");
        assert_eq!(c.bump(1), "2");
        assert_eq!(c.bump(1), "3");
    }

    #[test]
    fn nested_levels_format() {
        let mut c = SectionCounters::new();
        assert_eq!(c.bump(1), "1");
        assert_eq!(c.bump(2), "1.1");
        assert_eq!(c.bump(2), "1.2");
        assert_eq!(c.bump(3), "1.2.1");
        assert_eq!(c.bump(3), "1.2.2");
    }

    #[test]
    fn outer_bump_resets_inner() {
        let mut c = SectionCounters::new();
        c.bump(1);            // 1
        c.bump(2);            // 1.1
        c.bump(3);            // 1.1.1
        assert_eq!(c.bump(1), "2");        // resets sub & subsub
        assert_eq!(c.bump(2), "2.1");      // sub starts fresh
        assert_eq!(c.bump(3), "2.1.1");    // subsub starts fresh
    }

    #[test]
    fn subsection_bump_resets_subsubsection() {
        let mut c = SectionCounters::new();
        c.bump(1);            // 1
        c.bump(2);            // 1.1
        c.bump(3);            // 1.1.1
        c.bump(3);            // 1.1.2
        assert_eq!(c.bump(2), "1.2");      // sub bump resets subsub
        assert_eq!(c.bump(3), "1.2.1");
    }

    #[test]
    fn level_clamped_to_three() {
        let mut c = SectionCounters::new();
        // Levels above 3 (e.g. \paragraph in LaTeX) are clamped to subsubsection.
        assert_eq!(c.bump(4), "0.0.1");
        assert_eq!(c.bump(5), "0.0.2");
    }
}

#[cfg(test)]
mod spec_parser_tests {
    use super::parse_column_spec;

    #[test]
    fn simple_no_rules() {
        assert_eq!(parse_column_spec("lccccc"), Some((6, vec![])));
    }

    #[test]
    fn two_rules() {
        // c|cccccc|ccc → 10 cols, rules before col 1 and col 7
        assert_eq!(parse_column_spec("c|cccccc|ccc"), Some((10, vec![1, 7])));
    }

    #[test]
    fn edge_rules() {
        // |l|c|r| → 3 cols, rules at 0,1,2,3
        assert_eq!(parse_column_spec("|l|c|r|"), Some((3, vec![0, 1, 2, 3])));
    }

    #[test]
    fn p_column_with_width() {
        assert_eq!(parse_column_spec("p{2cm}cc"), Some((3, vec![])));
    }

    #[test]
    fn whitespace_tolerant() {
        assert_eq!(parse_column_spec("c | c | c"), Some((3, vec![1, 2])));
    }

    #[test]
    fn star_repeat_unsupported() {
        // *{3}{c} repeat operator → bail out, return None
        assert!(parse_column_spec("*{3}{c}").is_none());
    }

    #[test]
    fn modifier_macro_skipped() {
        // >{\bfseries}c >{\itshape}c → 2 cols, no rules
        assert_eq!(parse_column_spec(">{\\bfseries}c>{\\itshape}c"), Some((2, vec![])));
    }

    /// Regression: extract_tabular_specs must not panic on sources containing
    /// multi-byte UTF-8 characters (e.g. "ä" in `\documentclass` comments).
    /// The previous byte-only `i += 1` advance landed mid-codepoint and the
    /// `let rest = &src[i..]` slice at the top of the loop panicked.
    #[test]
    fn utf8_safe_scanning() {
        let src = "% ä é ö\n\\begin{tabular}{c|c}\nfoo\n\\end{tabular}\n% ß";
        let specs = super::extract_tabular_specs(src);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].col_count, 2);
        assert_eq!(specs[0].vertical_rules, vec![1]);
    }

    /// Regression: commented-out `\begin{tabular}` must not produce a spec.
    /// In the Attention paper, `background.tex:40` has `%\begin{tabular}{l|c|c|c}`
    /// which falsely matched Table 1's column count and attached spurious
    /// vertical rules.
    #[test]
    fn line_comments_skipped() {
        let src = "%\\begin{tabular}{l|c|c|c}\n\\begin{tabular}{lc}\nfoo\n\\end{tabular}";
        let specs = super::extract_tabular_specs(src);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].col_count, 2);
        assert_eq!(specs[0].vertical_rules, Vec::<usize>::new());
    }

    /// `\%` is a LITERAL percent — must not trigger comment skipping.
    #[test]
    fn escaped_percent_not_a_comment() {
        let src = "100\\% \\begin{tabular}{cc}\nfoo\n\\end{tabular}";
        let specs = super::extract_tabular_specs(src);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].col_count, 2);
    }

    /// Horizontal rule scanning: \hline between data rows produces an entry
    /// in horizontal_rules at the row index immediately AFTER the row break.
    #[test]
    fn horizontal_rules_between_rows() {
        let src = "\\begin{tabular}{cc}\n\
                   \\toprule\n\
                   a & b \\\\\n\
                   \\midrule\n\
                   1 & 2 \\\\\n\
                   3 & 4 \\\\\n\
                   \\hline\n\
                   5 & 6 \\\\\n\
                   \\bottomrule\n\
                   \\end{tabular}";
        let specs = super::extract_tabular_specs(src);
        assert_eq!(specs.len(), 1);
        // Rules at: row 0 (toprule), row 1 (midrule between header and data),
        // row 3 (hline before "5 & 6"), row 4 (bottomrule after last row).
        assert_eq!(specs[0].horizontal_rules, vec![0, 1, 3, 4]);
    }

    /// `\rule{0pt}{2.0ex}` (vertical strut) must NOT be counted as a rule.
    #[test]
    fn rule_strut_not_counted() {
        let src = "\\begin{tabular}{cc}\\hline\\rule{0pt}{2.0ex}\na & b \\\\\\end{tabular}";
        let specs = super::extract_tabular_specs(src);
        assert_eq!(specs.len(), 1);
        // Only the \hline counts; the \rule strut is silently passed over.
        assert_eq!(specs[0].horizontal_rules, vec![0]);
    }

    /// Smoke test against a snippet representative of the Attention paper's
    /// background.tex, which has a commented-out 4-col tabular followed by no
    /// real tabular.  The commented-out one previously poisoned the queue.
    #[test]
    fn attention_background_no_phantom_specs() {
        let src = r"
% Some text
%\begin{tabular}{l|c|c|c}
% commented out content
\hline
%\end{tabular}
% More text
";
        assert!(super::extract_tabular_specs(src).is_empty());
    }

    #[test]
    fn match_brace_with_unicode_escape() {
        // `\é` — backslash followed by a 2-byte char should not panic.
        // Brace structure: `{ ä é hello }` → close at the trailing `}`.
        let src = "{ ä é hello }";
        let bytes = src.as_bytes();
        let close = super::match_brace(bytes, 0).expect("must find closing brace");
        assert_eq!(bytes[close], b'}');
        assert_eq!(close, bytes.len() - 1);
    }
}
