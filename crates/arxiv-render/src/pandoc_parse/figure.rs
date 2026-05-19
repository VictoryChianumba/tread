//! Pandoc `Figure` → `Block::Figure` extraction.
//!
//! Lives behind a single entry point ([`extract_figure`]) so the parent
//! `walk_blocks` match arm stays small.  See ADR-0001 for the model:
//! one Pandoc Figure node produces one `Block::Figure` carrying the
//! full 2D image grid, caption, figure id, recovered column gaps, and
//! tabular header rows.  Layout-detection precedence (also in the ADR):
//!
//! 1. Multiple top-level blocks → side-by-side (minipage / subfigure).
//! 2. Single block with `\\` LineBreak inlines between images → stacked.
//! 3. Single block where any image has `width=\textwidth` /
//!    `\linewidth` (or scalar ≥ 0.85) → stacked.
//! 4. Otherwise → side-by-side.
//!
//! Helpers in this module are figure-exclusive; shared parsers like
//! [`super::walk_inlines_text`] and [`super::extract_caption_text`]
//! live in the parent.

use doc_model::{Block, HeaderCell, ImageItem};
use serde_json::Value;

use super::inline::walk_inlines_text;
use super::table::extract_caption_text;
use super::{SectionCounters, TableSpec};

/// Lift one Pandoc `Figure` node into a `Vec<Block>` consisting of:
/// - an optional `Block::Anchor` for `\label{fig:X}` jumps
/// - either a `Block::Figure` (with the 2D image grid, caption, id,
///   column gaps, and header rows) or — when the figure carries no
///   resolvable images — a textual `[Figure N: …]` placeholder line
/// - a trailing `Block::Blank` so prose after the figure starts on a
///   fresh paragraph
///
/// `c` is the Pandoc Figure node's payload, in the AST shape
/// `[attr, caption, [blocks]]`.  `counters` provides the figure number
/// and per-image Kitty id allocation.  `specs` is the queue of
/// pre-extracted `\begin{tabular}{...}` column specs; if the figure
/// contains a `Table` node, we pop the matching spec so the column-
/// group `@{\hspace{...}}` separators survive into `column_gaps_after`.
pub(super) fn extract_figure(
    c: &Value,
    counters: &mut SectionCounters,
    specs: &mut Vec<TableSpec>,
) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();

    // Pandoc's Figure node carries the LaTeX `\label{}` at attr.id
    // (c[0][0]).  Emit an Anchor before the caption so `\ref{fig:X}`
    // resolves at runtime.
    if let Some(id) = c[0][0].as_str() {
        if !id.is_empty() {
            out.push(Block::Anchor(id.to_string()));
        }
    }
    let cap = extract_caption_text(&c[1]);
    let n = counters.bump_figure();
    let alt = if cap.is_empty() {
        format!("Figure {n}")
    } else {
        format!("Figure {n}: {cap}")
    };

    // Detect stacked vs side-by-side by walking c[2] (the figure's
    // content blocks).  Empirically, Pandoc emits:
    //   - `\\` between images → one Para block with inline LineBreaks
    //     separating the Images → STACKED.
    //   - Minipages / subfigures → multiple Div blocks each with one
    //     Image inside → SIDE-BY-SIDE.
    //
    // The result is a vec of vecs: outer = stack rows, inner = side-by-
    // side siblings within a row.
    let blocks_in_figure: Vec<Value> = c[2].as_array().cloned().unwrap_or_default();
    let mut figure_table_spec: Option<TableSpec> = None;
    let mut figure_header_rows: Vec<Vec<HeaderCell>> = Vec::new();
    let groups: Vec<Vec<String>> = if blocks_in_figure.len() == 1 {
        let block = &blocks_in_figure[0];
        let t = block["t"].as_str();
        if matches!(t, Some("Para") | Some("Plain")) {
            let inlines = block["c"].as_array().cloned().unwrap_or_default();
            // (2) Explicit `\\` separator wins.
            let by_linebreak = split_inlines_by_linebreak(&inlines);
            if by_linebreak.len() > 1 {
                by_linebreak
            } else {
                // (3) Width-attribute inference.
                let with_widths = collect_images_with_width(&inlines);
                if with_widths.iter().any(|(_, fw)| *fw) {
                    with_widths.into_iter().map(|(s, _)| vec![s]).collect()
                } else {
                    // (4) Default side-by-side row.
                    let srcs: Vec<String> = with_widths.into_iter().map(|(s, _)| s).collect();
                    if srcs.is_empty() {
                        Vec::new()
                    } else {
                        vec![srcs]
                    }
                }
            }
        } else if t == Some("Table") {
            // Pandoc emits N-row tabulars as a Table node — one row per
            // source `\\`-separated line.  Walk row by row and emit one
            // image-row per Table row so the grid's vertical structure
            // survives (recovered by the `\resizebox` strip; otherwise
            // flattens into one super-wide ImageRow with N×M slivers).
            let (header_rows, rows) = walk_table_rows_for_images(block);
            figure_header_rows = header_rows;
            // Match the spec by column count of the widest row (the
            // natural tabular column count after Pandoc drops header-
            // only rows).  Unlike non-figure tables — which consume
            // specs from the head of the queue in source order —
            // figure tables are rare and the multi-file source-
            // collection step doesn't preserve cross-file ordering, so
            // we scan the whole queue.  Prefer an entry with non-empty
            // `column_gaps_after` when one exists: that's the spec from
            // a figure tabular that explicitly groups columns with
            // `@{\hspace}`.
            let max_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
            if max_cols > 0 {
                let pick = specs
                    .iter()
                    .position(|s| s.col_count == max_cols && !s.column_gaps_after.is_empty())
                    .or_else(|| specs.iter().position(|s| s.col_count == max_cols));
                if let Some(idx) = pick {
                    figure_table_spec = Some(specs.remove(idx));
                }
            }
            rows
        } else {
            // Div or other wrapper: treat as one row of images.
            let mut imgs = Vec::new();
            walk_for_images(std::slice::from_ref(block), &mut imgs);
            if imgs.is_empty() {
                Vec::new()
            } else {
                vec![imgs]
            }
        }
    } else {
        // (1) Multiple top-level blocks (minipage / subfigure): combine
        // all images into one side-by-side row.
        let mut all_imgs = Vec::new();
        for block in &blocks_in_figure {
            walk_for_images(std::slice::from_ref(block), &mut all_imgs);
        }
        if all_imgs.is_empty() {
            Vec::new()
        } else {
            vec![all_imgs]
        }
    };

    if groups.is_empty() {
        if !cap.is_empty() {
            out.push(Block::Line(format!("[Figure {n}: {cap}]")));
        } else {
            out.push(Block::Line(format!("[Figure {n}]")));
        }
    } else {
        // One Pandoc Figure node → one Block::Figure.  The 2D `rows`
        // grid mirrors the source's stacked-vs-side-by-side structure
        // exactly, so downstream consumers (FigureIndex,
        // build_visual_lines, the preview tiler) don't have to
        // reconstruct it.
        let rows: Vec<Vec<ImageItem>> = groups
            .into_iter()
            .map(|group| {
                group
                    .into_iter()
                    .map(|src| ImageItem {
                        path: std::path::PathBuf::from(src),
                        kitty_id: counters.next_kitty_id(),
                        dims: None, // populated post-parse by absolutize_image_paths
                    })
                    .collect()
            })
            .collect();
        let column_gaps_after = figure_table_spec
            .map(|s| s.column_gaps_after)
            .unwrap_or_default();
        out.push(Block::Figure {
            rows,
            alt,
            figure_id: n,
            column_gaps_after,
            header_rows: figure_header_rows,
        });
    }
    out.push(Block::Blank);
    out
}

// ── Figure-exclusive walkers ─────────────────────────────────────────────────

/// Walk inlines collecting `(src, is_full_width)` per Image.  The width
/// flag is read from the LaTeX `width=` attribute that Pandoc preserves
/// verbatim on the Image's attr triple — `\textwidth` or `\linewidth`
/// (with no scalar prefix or scalar ≥ 0.85) means the image will
/// dominate its row and so should stack rather than share.
///
/// Walks recursively into Spans and other inline wrappers so the
/// `{Span(Image)}` pattern Pandoc uses for `{\includegraphics{}}` is
/// handled correctly.
fn collect_images_with_width(inlines: &[Value]) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    walk_for_images_with_width(inlines, &mut out);
    out
}

fn walk_for_images_with_width(nodes: &[Value], out: &mut Vec<(String, bool)>) {
    for node in nodes {
        if node["t"].as_str() == Some("Image") {
            let c = &node["c"];
            // c = [attr, alt_inlines, [src, title]]
            // attr = [id, classes, key-value pairs]
            if let Some(src) = c[2][0].as_str() {
                if !src.is_empty() {
                    let full = c[0][2]
                        .as_array()
                        .map(|kvs| {
                            kvs.iter().any(|kv| {
                                kv[0].as_str() == Some("width")
                                    && kv[1].as_str().is_some_and(is_full_width)
                            })
                        })
                        .unwrap_or(false);
                    out.push((src.to_string(), full));
                    continue;
                }
            }
        }
        if let Some(arr) = node["c"].as_array() {
            walk_for_images_with_width(arr, out);
        }
        if let Some(arr) = node.as_array() {
            walk_for_images_with_width(arr, out);
        }
    }
}

/// Decide whether a LaTeX width spec (preserved verbatim by Pandoc)
/// indicates the image is meant to fill the available row.  Treats
/// `\textwidth` and `\linewidth` as the markers; an absent scalar
/// prefix means 1.0 (full), a present scalar ≥ 0.85 still counts as
/// effectively full (e.g. `0.95\textwidth`), anything smaller is
/// fractional.
fn is_full_width(s: &str) -> bool {
    let trimmed = s.trim();
    let marker_idx = trimmed
        .find("\\textwidth")
        .or_else(|| trimmed.find("\\linewidth"))
        .or_else(|| trimmed.find("\\columnwidth"));
    let Some(idx) = marker_idx else { return false };
    let prefix = trimmed[..idx].trim();
    if prefix.is_empty() {
        return true;
    }
    prefix.parse::<f32>().map(|v| v >= 0.85).unwrap_or(false)
}

/// Walk a list of Pandoc inlines and split into groups separated by
/// `LineBreak` inlines.  Each returned inner Vec is the images on one
/// "row" — i.e. siblings that should render side-by-side.  Used for
/// the `\includegraphics{}\\\includegraphics{}` pattern, which Pandoc
/// emits as a single Para containing `[Image, LineBreak, Image]`.
fn split_inlines_by_linebreak(inlines: &[Value]) -> Vec<Vec<String>> {
    let mut groups: Vec<Vec<String>> = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    for node in inlines {
        if node["t"].as_str() == Some("LineBreak") {
            if !cur.is_empty() {
                groups.push(std::mem::take(&mut cur));
            }
            continue;
        }
        // Anything else: walk for nested images and add to current row.
        walk_for_images(std::slice::from_ref(node), &mut cur);
    }
    if !cur.is_empty() {
        groups.push(cur);
    }
    groups
}

/// Walk a Pandoc `Table` node and split rows by whether they contain
/// images.  Returns `(headers, image_rows)`.
///
/// - Header rows (no Image inlines) keep their cell text and col_span
///   so the preview can render them above the image grid, aligned with
///   the columns they describe.
/// - Image rows return one source list each, suitable for emitting
///   `\includegraphics` calls per cell.
///
/// Pandoc Table layout (AST: `Table Attr Caption [ColSpec] TableHead [TableBody] TableFoot`):
/// - `c[0]` attr · `c[1]` caption · `c[2]` colspecs
/// - `c[3]` head = `[attr, [row]]`
/// - `c[4]` bodies = `[[attr, row_head_cols, [intermediate_head_row], [body_row]], ...]`
/// - `c[5]` foot = `[attr, [row]]`
///
/// A row is `[attr, [cell]]`; a cell is `[Attr, Alignment, RowSpan,
/// ColSpan, [Block]]` (5 elements in the newer Pandoc Table format).
/// We hand each cell's content blocks to `walk_for_images` to detect
/// images, and to `walk_inlines_text` (via the cell's nested inlines)
/// for the header text.
fn walk_table_rows_for_images(table: &Value) -> (Vec<Vec<HeaderCell>>, Vec<Vec<String>>) {
    let mut headers: Vec<Vec<HeaderCell>> = Vec::new();
    let mut image_rows: Vec<Vec<String>> = Vec::new();
    let push_row = |row: &Value,
                    headers: &mut Vec<Vec<HeaderCell>>,
                    image_rows: &mut Vec<Vec<String>>| {
        let cells = match row
            .as_array()
            .and_then(|r| r.get(1))
            .and_then(Value::as_array)
        {
            Some(c) => c,
            None => return,
        };
        let mut row_imgs: Vec<String> = Vec::new();
        walk_for_images(cells, &mut row_imgs);
        if !row_imgs.is_empty() {
            image_rows.push(row_imgs);
            return;
        }
        // No images in this row → treat as a header row.  Capture cell
        // text + col_span so the preview can position labels.
        let mut row_headers: Vec<HeaderCell> = Vec::new();
        for cell in cells {
            // Pandoc cell shape: [Attr, Alignment, RowSpan, ColSpan, [Block]].
            // The 4th element is col_span (integer); older Pandoc
            // versions emit a 4-tuple without it, in which case span
            // defaults to 1.
            let col_span = cell
                .as_array()
                .and_then(|c| c.get(3))
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .max(1) as u16;
            let content = cell
                .as_array()
                .and_then(|c| c.get(4))
                .cloned()
                .unwrap_or(Value::Null);
            // Each cell content is `[Block]` — walk each block for
            // inline text and flatten to a single line.
            let mut text = String::new();
            if let Some(blocks) = content.as_array() {
                for block in blocks {
                    let inlines = block["c"].as_array().cloned().unwrap_or_default();
                    text.push_str(&walk_inlines_text(&inlines));
                }
            }
            let text = text.trim().to_string();
            row_headers.push(HeaderCell { text, col_span });
        }
        // Drop rows whose cells are all empty — Pandoc often emits a
        // trailing all-blank row for the `\\[Xpt]` spacing after the
        // last header row.
        if row_headers.iter().any(|c| !c.text.is_empty()) {
            headers.push(row_headers);
        }
    };
    // Head rows.
    if let Some(head_rows) = table["c"][3][1].as_array() {
        for row in head_rows {
            push_row(row, &mut headers, &mut image_rows);
        }
    }
    // Bodies (intermediate-head-rows then body-rows for each body).
    if let Some(bodies) = table["c"][4].as_array() {
        for body in bodies {
            if let Some(rows) = body.get(2).and_then(Value::as_array) {
                for row in rows {
                    push_row(row, &mut headers, &mut image_rows);
                }
            }
            if let Some(rows) = body.get(3).and_then(Value::as_array) {
                for row in rows {
                    push_row(row, &mut headers, &mut image_rows);
                }
            }
        }
    }
    // Foot rows.
    if let Some(foot_rows) = table["c"][5][1].as_array() {
        for row in foot_rows {
            push_row(row, &mut headers, &mut image_rows);
        }
    }
    (headers, image_rows)
}

/// Recursively walk a Pandoc AST node array collecting every Image
/// inline's `src` string in document order.  The Figure arm walks
/// `c[2]` block-by-block to preserve LaTeX layout intent (same block =
/// side-by-side, separate blocks = stacked); within each block this
/// helper finds all images regardless of further nesting (Plain →
/// Image, Plain → Subfigure → Image, etc.).
fn walk_for_images(nodes: &[Value], out: &mut Vec<String>) {
    for node in nodes {
        if node["t"].as_str() == Some("Image") {
            // c = [attr, alt_inlines, [src, title]]
            if let Some(src) = node["c"][2][0].as_str() {
                if !src.is_empty() {
                    out.push(src.to_string());
                    // Don't recurse into the matched Image's own children.
                    continue;
                }
            }
        }
        // Recurse into child arrays — Pandoc nests Image inside Plain,
        // Para, Div, etc.  The child structure varies per node type so
        // we walk every array-valued field.
        if let Some(arr) = node["c"].as_array() {
            walk_for_images(arr, out);
        }
        if let Some(arr) = node.as_array() {
            walk_for_images(arr, out);
        }
    }
}

#[cfg(test)]
mod table_image_walk_tests {
    use serde_json::json;

    /// Hand-build a minimal Pandoc Table value with the given rows.
    /// Each row is a list of cells; each cell is either Some("path")
    /// for an `\includegraphics`-bearing cell or None for a text label
    /// cell.  Only the fields `walk_table_rows_for_images` actually
    /// reads are populated — everything else uses empty defaults.
    fn make_table(rows: &[&[Option<&str>]]) -> serde_json::Value {
        let mk_cell = |src: Option<&str>| -> serde_json::Value {
            // Cell = [attr, align, rowspan, colspan, [blocks]].
            let blocks = match src {
                Some(p) => json!([
                    {
                        "t": "Plain",
                        "c": [
                            {"t": "Image", "c": [["", [], []], [], [p, ""]]}
                        ]
                    }
                ]),
                None => json!([{"t": "Plain", "c": [{"t": "Str", "c": "label"}]}]),
            };
            json!([["", [], []], {"t": "AlignDefault"}, 1, 1, blocks])
        };
        let mk_row = |cells: &[Option<&str>]| -> serde_json::Value {
            let cells_json: Vec<_> = cells.iter().map(|c| mk_cell(*c)).collect();
            // Row = [attr, [cells]]
            json!([["", [], []], cells_json])
        };
        let body_rows: Vec<_> = rows.iter().map(|r| mk_row(r)).collect();
        json!({
            "t": "Table",
            "c": [
                ["", [], []],                         // attr
                [serde_json::Value::Null, []],        // caption (short, [blocks])
                [],                                   // colspecs
                [["", [], []], []],                   // head: (attr, [row])
                [                                     // bodies: one body
                    [["", [], []], 0, [], body_rows], // (attr, row_head_cols, intermediate, body_rows)
                ],
                [["", [], []], []],                   // foot
            ]
        })
    }

    #[test]
    fn flat_three_row_grid_preserves_rows() {
        // 3 rows × 3 image cells — the figure-3 shape.
        let t = make_table(&[
            &[Some("a/0"), Some("a/1"), Some("a/2")],
            &[Some("b/0"), Some("b/1"), Some("b/2")],
            &[Some("c/0"), Some("c/1"), Some("c/2")],
        ]);
        let (headers, groups) = super::walk_table_rows_for_images(&t);
        assert!(headers.is_empty(), "no header rows expected");
        assert_eq!(groups.len(), 3, "want 3 groups, got {groups:?}");
        assert_eq!(groups[0], vec!["a/0", "a/1", "a/2"]);
        assert_eq!(groups[1], vec!["b/0", "b/1", "b/2"]);
        assert_eq!(groups[2], vec!["c/0", "c/1", "c/2"]);
    }

    #[test]
    fn label_rows_are_dropped() {
        // First two rows are text labels (figure 3 has `$N=4$` etc.
        // header rows above the image grid).  Only image rows go into
        // the image_rows return — the header rows are captured
        // separately in `headers` (left empty here because
        // make_table's `None` cells produce blank text and the row-
        // drop heuristic discards all-empty rows).
        let t = make_table(&[
            &[None, None, None],
            &[None, None, None],
            &[Some("img/0"), Some("img/1"), Some("img/2")],
        ]);
        let (_headers, groups) = super::walk_table_rows_for_images(&t);
        assert_eq!(groups.len(), 1, "want 1 image-bearing row, got {groups:?}");
        assert_eq!(groups[0], vec!["img/0", "img/1", "img/2"]);
    }

    #[test]
    fn empty_table_returns_empty() {
        let t = make_table(&[]);
        let (headers, groups) = super::walk_table_rows_for_images(&t);
        assert!(headers.is_empty());
        assert!(groups.is_empty());
    }
}
