mod wrap;

use wrap::{wrap_list_item, wrap_spans};

/// Internal jump target carried on an `InlineSpan` to make refs/citations
/// interactive in the reader.  The reader resolves these to line indices
/// at runtime via its `label_lines` / `bib_entries_lines` maps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// LaTeX label / Pandoc anchor id — e.g. `"sec:method"`, `"eq:elbo"`,
    /// `"tab:variations"`.  `Enter` jumps to (the line *before*) the
    /// labeled element.
    Internal(String),
    /// BibTeX cite-key — e.g. `"vaswani"`.  `Enter` jumps to the bib
    /// entry; `K` / `Shift+Enter` shows the entry in a popup.
    Citation(String),
}

/// Inline styled run within a paragraph line.
/// `color` uses raw RGB rather than a ratatui type — doc-model has no UI dependency.
#[derive(Debug, Clone, Default)]
pub struct InlineSpan {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub monospace: bool,
    pub color: Option<(u8, u8, u8)>,
    pub url: Option<String>,
    pub link_target: Option<LinkTarget>,
}

impl InlineSpan {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            monospace: false,
            color: None,
            url: None,
            link_target: None,
        }
    }

    /// Internal cross-reference span — `\ref{...}` / `\eqref{...}`.
    pub fn internal_link(text: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            link_target: Some(LinkTarget::Internal(label.into())),
            ..Self::plain(text)
        }
    }

    /// Citation span — `\cite{...}`.
    pub fn citation(text: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            link_target: Some(LinkTarget::Citation(key.into())),
            ..Self::plain(text)
        }
    }

    pub fn bold(text: impl Into<String>) -> Self {
        Self {
            bold: true,
            ..Self::plain(text)
        }
    }

    pub fn italic(text: impl Into<String>) -> Self {
        Self {
            italic: true,
            ..Self::plain(text)
        }
    }

    pub fn monospace(text: impl Into<String>) -> Self {
        Self {
            monospace: true,
            ..Self::plain(text)
        }
    }
}

/// Semantic block — the producer's view of the document.
#[derive(Debug, Clone)]
pub enum Block {
    /// A single line of prose, already word-wrapped by the producer.
    Line(String),
    /// A display math equation rendered as multiple Unicode lines, treated as one unit.
    /// `num` carries the equation number for numbered environments (equation, align, etc.).
    DisplayMath {
        lines: Vec<String>,
        num: Option<usize>,
    },
    /// A section header. level: 1=section, 2=subsection, 3=subsubsection/paragraph.
    Header { level: u8, text: String },
    /// A matrix rendered as a grid of cells (row-major).
    /// Each cell is `(text, col_span)` — `\multicolumn{N}` cells carry span > 1.
    /// `vertical_rules` lists raw column indices BEFORE which a `│` is drawn.
    /// Empty Vec = booktabs default (no vertical lines). 0 = left edge,
    /// raw_col_count = right edge. The renderer translates raw indices to
    /// active-column space (after blank-column collapse).
    Matrix {
        rows: Vec<Vec<(String, usize)>>,
        vertical_rules: Vec<usize>,
    },
    /// Explicit vertical space (blank line).
    Blank,
    /// A prose line carrying inline styling (bold, italic, monospace, etc.).
    /// The producer emits this when any span has a non-default style.
    /// build_visual_lines wraps it to terminal_width.
    StyledLine(Vec<InlineSpan>),
    /// A list item. depth=0 for top-level; marker is "• " or "1. " etc.
    ListItem {
        depth: u8,
        marker: String,
        content: Vec<InlineSpan>,
    },
    /// A verbatim / code-listing block. Lines are raw (no LaTeX processing).
    CodeBlock {
        lang: Option<String>,
        lines: Vec<String>,
    },
    /// A horizontal rule: \hline, \toprule, \midrule, \bottomrule.
    Rule,
    /// A block quote: \begin{quote}, \begin{quotation}, \begin{epigraph}.
    Quote(Vec<InlineSpan>),
    /// Invisible label marker.  Produced by parsers when they encounter a
    /// `\label{X}` (or Pandoc `attr.id`) so the reader can resolve
    /// `\ref{X}` jumps at runtime.  Emits no visual line; the reader
    /// associates the label with the *next* visible line's index.
    Anchor(String),
    /// One whole figure as the source intended it — possibly multi-row
    /// (stacked panels) and possibly multi-column-per-row (subfigures).
    ///
    /// `rows` is the 2D grid: outer index is the stack row, inner is the
    /// side-by-side items inside that row.  A standalone figure is just
    /// `rows = [[one_item]]`.  A 3-row table of 2 subfigures each is
    /// `rows = [[a, b], [c, d], [e, f]]`.
    ///
    /// `alt` is the figure caption (e.g. "Figure 3: …") attached once to
    /// the whole figure — not duplicated across rows.
    ///
    /// `figure_id` is the parser's per-document figure counter (1, 2, …).
    /// Carried so consumers that need a stable per-figure identifier can
    /// use it without rebuilding from block positions.
    ///
    /// `column_gaps_after` lists column indices (in the flat item space
    /// of each row) AFTER which a small horizontal gap should be drawn —
    /// recovered from the source tabular's `@{\hspace{...}}` separators.
    /// Empty when the source doesn't use column-group spacing.  Shared
    /// across all rows because tabular column groupings are uniform.
    ///
    /// `header_rows` carries column labels recovered from the figure's
    /// tabular header (e.g. "N=4", "Input", "Avat3r"…).  Each entry is
    /// a row of cells with text + col_span — the preview tiles them
    /// above the image grid, aligned to the columns they describe.
    /// Empty for figures without textual headers.
    Figure {
        rows: Vec<Vec<ImageItem>>,
        alt: String,
        figure_id: u32,
        column_gaps_after: Vec<usize>,
        header_rows: Vec<Vec<HeaderCell>>,
    },
}

/// One cell from a figure tabular's header row.  `text` is the flat
/// inline text (no formatting); `col_span` is how many image columns
/// the cell visually spans (1 for normal cells, N for `\multicolumn{N}`).
#[derive(Debug, Clone)]
pub struct HeaderCell {
    pub text: String,
    pub col_span: u16,
}

/// One sub-image inside a `Block::Figure` row.  Each carries its own
/// `kitty_id` so deletions and re-placements address them individually,
/// and its own pixel `dims` so the row's height is computed from the
/// most demanding sibling's aspect ratio.
#[derive(Debug, Clone)]
pub struct ImageItem {
    pub path: std::path::PathBuf,
    pub kitty_id: u32,
    pub dims: Option<(u32, u32)>,
}

/// A single screen row, fully expanded from a Block.
/// This is the flat table the reader indexes into — offset and cursor_y
/// are indices into Vec<VisualLine>, identical to how they used Vec<String>.
///
/// `block_byte_start` / `block_byte_end` describe the byte range within the
/// parent block's canonical text that this visual line covers.  Used by the
/// highlight system so that character-range highlights survive terminal
/// resize: highlights are stored at block-byte granularity and projected
/// onto the (potentially-rewrapped) visual line grid at render time.
///
/// For non-text blocks (Rule, Matrix, Blank), both fields are `0`.
#[derive(Debug, Clone)]
pub struct VisualLine {
    pub block_idx: usize,
    pub line_in_block: usize,
    pub text: String,
    pub kind: VisualLineKind,
    pub block_byte_start: usize,
    pub block_byte_end: usize,
}

#[derive(Debug, Clone)]
pub enum VisualLineKind {
    Prose,
    /// Part of a display math block. text is pre-centered with leading spaces.
    MathLine {
        block_width: usize,
        is_first: bool,
        is_last: bool,
    },
    Header(u8),
    MatrixLine {
        is_first: bool,
        is_last: bool,
        is_header: bool,
    },
    Blank,
    /// Prose with inline styling. text = plain concatenation (for search).
    /// Spans carry the styled runs for the renderer.
    StyledProse(Vec<InlineSpan>),
    /// A list item row. text already contains indent+marker prefix.
    ListItem {
        depth: u8,
        marker_len: u8,
        is_continuation: bool,
    },
    /// A line from a code/verbatim block.
    Code {
        is_first: bool,
        is_last: bool,
    },
    /// A horizontal rule; text = "─".repeat(terminal_width).
    Rule,
    /// A block quote; text = plain concatenation of spans.
    Quote {
        is_continuation: bool,
    },
    /// One row of an inline image figure.  `kitty_id` identifies the
    /// image to the Kitty graphics protocol.  `rows` × `cols` is the
    /// cell footprint chosen to preserve aspect ratio (computed in
    /// `build_visual_lines` from `Block::Image::dims` and the terminal
    /// width).  `is_first` flags the row where the renderer should emit
    /// the placement escape.
    Image {
        kitty_id: u32,
        cols: u16,
        rows: u16,
        is_first: bool,
    },
    /// One row of a multi-image figure (`Block::ImageRow`).  All sub-image
    /// kitty_ids share the same `rows`; each sibling renders at its own
    /// `cols` width side-by-side.  Per-image cols are stored as the
    /// `(kitty_id, cols)` pairs so siblings with different aspect ratios
    /// can take different horizontal slices.
    ImageRow {
        items: Vec<(u32, u16)>,
        rows: u16,
        is_first: bool,
    },
}

/// Expand a block list into the flat visual line table.
///
/// Hardcoded cell-pixel ratio for cell-footprint math.  Real terminals
/// vary (retina iTerm2 ≈ 7×14 px, classic xterm 8×16 px, Kitty 9×18,
/// etc.), but `(cell_w / cell_h) ≈ 0.5` is close enough to preserve
/// aspect ratios within a couple of percent.  Querying the terminal
/// for actual pixel-per-cell via `\e[14t` is on the v2 backlog.
const CELL_W_PX: u32 = 8;
const CELL_H_PX: u32 = 16;

/// Lower bound on rows reserved for an image — even tiny figures
/// (icons, small diagrams) get this much screen space so they're
/// recognisable rather than vestigial.
const MIN_IMAGE_ROWS: u16 = 6;

/// Estimated rows reserved for the caption beneath each figure.  Used
/// when budgeting how much vertical space a figure may consume so the
/// image plus caption together fit within the per-figure budget.
const CAPTION_ROWS_BUDGET: u16 = 4;

/// Compute the **whole-figure** vertical budget from terminal height.
/// A figure here means image(s) + caption together; this number is
/// the rows available for the IMAGE PORTION (caption is reserved
/// separately at `CAPTION_ROWS_BUDGET`).  Roughly 70% of the usable
/// height — leaves room for context above and below the figure when
/// reading.
///
/// For a standalone (single-image) figure, the whole budget goes to
/// that one image.  For a stacked figure of N panels, the budget is
/// split N ways minus separator rows.  See `panel_row_budget`.
fn figure_row_budget(content_height: usize) -> u16 {
    let h = content_height as u16;
    let usable = h.saturating_sub(CAPTION_ROWS_BUDGET);
    // 70% of usable height, with a floor.
    ((usable * 7) / 10).max(MIN_IMAGE_ROWS)
}

/// Per-panel row budget for one image in a stacked figure.  For
/// `stack_total == 1` this is the whole figure budget.  For N panels
/// we apply a **stack bonus**: a 2-panel figure is intrinsically more
/// content than a 1-panel figure and earns extra vertical space, so
/// a naive figure_budget/N split would shrink panels too aggressively.
/// Formula: `(figure_budget + (N-1) × STACK_BONUS_ROWS) / N`, minus
/// separator rows.  Tuned so a 2-stack lands at ~21 rows per panel
/// (matches the old half-screen sizing) and 3+ stacks degrade
/// gracefully without underflowing `MIN_IMAGE_ROWS`.
///
/// Cap result at `PANEL_ROW_CAP` so single-figure panels match the
/// stacked-panel size — uniform visual mass per panel across the
/// document instead of solo figures looking outsized vs. stacked ones.
fn panel_row_budget(figure_budget: u16, stack_total: u8) -> u16 {
    /// Extra row budget granted per additional stacked panel.  10 was
    /// picked so a 2-stack on a 50-row content area gets 21 rows per
    /// panel — same as the old half-screen budget — while a 3-stack
    /// gets ~17 each (small but readable).
    const STACK_BONUS_ROWS: u16 = 10;
    /// Hard cap on rows per panel.  Single figures and stacked panels
    /// both top out here so visual mass stays consistent across the
    /// document.  Below the cap, smaller terminals still respect their
    /// natural figure_budget.
    const PANEL_ROW_CAP: u16 = 21;
    let n = stack_total.max(1) as u16;
    let raw = if n == 1 {
        figure_budget
    } else {
        let bonus = (n - 1) * STACK_BONUS_ROWS;
        let separators = n - 1;
        figure_budget
            .saturating_add(bonus)
            .saturating_sub(separators)
            / n
    };
    raw.min(PANEL_ROW_CAP).max(MIN_IMAGE_ROWS)
}

/// Compute cell `(cols, rows)` for an image given its pixel `dims` and
/// the available content_width / max_rows budgets.  Preserves aspect
/// ratio; clamps rows by the budget and shrinks cols when the row
/// clamp kicks in.  Falls back to a sensible default when dims are
/// unknown.
pub fn compute_cell_footprint(
    dims: Option<(u32, u32)>,
    content_width: usize,
    max_rows: u16,
) -> (u16, u16) {
    let max_cols = (content_width as u16).max(1);
    let Some((w_px, h_px)) = dims else {
        return (max_cols, max_rows);
    };
    if w_px == 0 || h_px == 0 {
        return (max_cols, max_rows);
    }
    // Natural cell footprint: pixel dims divided by per-cell pixels.
    let nat_c = ((w_px / CELL_W_PX) as u16).max(1);
    // Start by fitting width to the column budget.
    let mut c = nat_c.min(max_cols);
    // Preserve aspect ratio: rows = cols × (h_px/w_px) × (cell_w/cell_h).
    // With CELL_W_PX=8, CELL_H_PX=16, the cell ratio is 0.5.
    let mut r = ((c as u32 * h_px * CELL_W_PX) / (w_px * CELL_H_PX)) as u16;
    r = r.max(MIN_IMAGE_ROWS);
    // If still too tall, clamp rows and shrink cols proportionally.
    if r > max_rows {
        r = max_rows;
        c = ((r as u32 * w_px * CELL_H_PX) / (h_px * CELL_W_PX)) as u16;
        c = c.min(max_cols).max(1);
    }
    (c, r)
}

/// Word-wrap a caption to fit `width` columns.  Greedy: accumulates
/// words into the current line until adding another would exceed
/// width, then breaks.  Used only for `Block::Image` / `Block::ImageRow`
/// captions — long figure descriptions otherwise get truncated at the
/// right edge of the screen.
fn wrap_caption(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if cur.is_empty() {
            cur.push_str(word);
        } else if cur.len() + 1 + word.len() <= width {
            cur.push(' ');
            cur.push_str(word);
        } else {
            lines.push(std::mem::take(&mut cur));
            cur.push_str(word);
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Emit one or more Prose VLs for a wrapped figure caption.  Caller
/// gives the figure's bottom row index; we lay the wrapped lines out
/// sequentially below.  Each wrapped line is **centered** within the
/// content width by prepending leading spaces — visually aligns the
/// caption beneath the (centered) image.
fn emit_caption(
    out: &mut Vec<VisualLine>,
    block_idx: usize,
    caption: &str,
    start_line: usize,
    width: usize,
) {
    let formatted = format!("[{}]", caption);
    let wrapped = wrap_caption(&formatted, width);
    for (i, line) in wrapped.into_iter().enumerate() {
        let line_width = line.chars().count();
        let pad = if width > line_width {
            (width - line_width) / 2
        } else {
            0
        };
        let centered = format!("{}{}", " ".repeat(pad), line);
        let len = centered.len();
        out.push(VisualLine {
            block_idx,
            line_in_block: start_line + i,
            text: centered,
            kind: VisualLineKind::Prose,
            block_byte_start: 0,
            block_byte_end: len,
        });
    }
}

/// Push N consecutive `VisualLineKind::Image` rows for one Block::Image.
fn emit_image_rows(
    out: &mut Vec<VisualLine>,
    block_idx: usize,
    kitty_id: u32,
    cols: u16,
    rows: u16,
) {
    for line_in_block in 0..rows {
        out.push(VisualLine {
            block_idx,
            line_in_block: line_in_block as usize,
            text: String::new(),
            kind: VisualLineKind::Image {
                kitty_id,
                cols,
                rows,
                is_first: line_in_block == 0,
            },
            block_byte_start: 0,
            block_byte_end: 0,
        });
    }
}

/// Push N consecutive `VisualLineKind::ImageRow` rows for one Block::ImageRow.
fn emit_image_row_rows(
    out: &mut Vec<VisualLine>,
    block_idx: usize,
    items: &[(u32, u16)],
    rows: u16,
) {
    for line_in_block in 0..rows {
        out.push(VisualLine {
            block_idx,
            line_in_block: line_in_block as usize,
            text: String::new(),
            kind: VisualLineKind::ImageRow {
                items: items.to_vec(),
                rows,
                is_first: line_in_block == 0,
            },
            block_byte_start: 0,
            block_byte_end: 0,
        });
    }
}

/// Called once at document load and again on terminal resize.  Both
/// `terminal_width` and `terminal_height` are needed so figures can be
/// scaled to fit (the height budget caps image rows so a single figure
/// + caption never overflows the visible viewport).
pub fn build_visual_lines(
    blocks: &[Block],
    terminal_width: usize,
    terminal_height: usize,
) -> Vec<VisualLine> {
    let figure_budget = figure_row_budget(terminal_height);
    let mut out = Vec::new();
    let mut i = 0;

    while i < blocks.len() {
        // Table groups: consecutive Rule and Matrix blocks are rendered together
        // so they can share column widths and proper box-drawing borders.
        if matches!(&blocks[i], Block::Rule | Block::Matrix { .. }) {
            let group_start = i;
            while i < blocks.len() && matches!(&blocks[i], Block::Rule | Block::Matrix { .. }) {
                i += 1;
            }
            let group = &blocks[group_start..i];
            let has_matrix = group.iter().any(|b| matches!(b, Block::Matrix { .. }));
            if has_matrix {
                render_table_group(group, group_start, terminal_width, &mut out);
            } else {
                for (k, _) in group.iter().enumerate() {
                    out.push(VisualLine {
                        block_idx: group_start + k,
                        line_in_block: 0,
                        text: "─".repeat(terminal_width),
                        kind: VisualLineKind::Rule,
                        block_byte_start: 0,
                        block_byte_end: 0,
                    });
                }
            }
            continue;
        }

        let block_idx = i;
        match &blocks[i] {
            Block::Line(s) => {
                let len = s.len();
                out.push(VisualLine {
                    block_idx,
                    line_in_block: 0,
                    text: s.clone(),
                    kind: VisualLineKind::Prose,
                    block_byte_start: 0,
                    block_byte_end: len,
                });
            }

            Block::Blank => {
                out.push(VisualLine {
                    block_idx,
                    line_in_block: 0,
                    text: String::new(),
                    kind: VisualLineKind::Blank,
                    block_byte_start: 0,
                    block_byte_end: 0,
                });
            }

            Block::Header { level, text } => {
                let len = text.len();
                out.push(VisualLine {
                    block_idx,
                    line_in_block: 0,
                    text: text.clone(),
                    kind: VisualLineKind::Header(*level),
                    block_byte_start: 0,
                    block_byte_end: len,
                });
            }

            Block::DisplayMath { lines, num } => {
                let block_width = lines.iter().map(|l| visual_width(l)).max().unwrap_or(0);
                let n = lines.len();
                for (li, line) in lines.iter().enumerate() {
                    let mut centered = center_line(line, block_width, terminal_width);
                    if li == n - 1 {
                        if let Some(eq_num) = num {
                            let tag = format!("({})", eq_num);
                            let used = visual_width(&centered);
                            let avail = terminal_width.saturating_sub(tag.len());
                            if used < avail {
                                centered.push_str(&" ".repeat(avail - used));
                            }
                            centered.push_str(&tag);
                        }
                    }
                    out.push(VisualLine {
                        block_idx,
                        line_in_block: li,
                        text: centered,
                        kind: VisualLineKind::MathLine {
                            block_width,
                            is_first: li == 0,
                            is_last: li == n - 1,
                        },
                        block_byte_start: 0,
                        block_byte_end: 0,
                    });
                }
            }

            Block::StyledLine(spans) => {
                // "Block text" for highlight purposes is the concatenation of the
                // wrapped lines joined by single spaces — this is the canonical
                // post-normalization form (wrap_spans collapses whitespace).
                let wrapped = wrap_spans(spans, terminal_width);
                let mut byte_cursor = 0usize;
                let n = wrapped.len();
                for (li, (line_spans, plain)) in wrapped.into_iter().enumerate() {
                    let start = byte_cursor;
                    let end = start + plain.len();
                    out.push(VisualLine {
                        block_idx,
                        line_in_block: li,
                        text: plain,
                        kind: VisualLineKind::StyledProse(line_spans),
                        block_byte_start: start,
                        block_byte_end: end,
                    });
                    byte_cursor = end;
                    if li + 1 < n {
                        byte_cursor += 1;
                    } // separator space between wrapped lines
                }
            }

            Block::ListItem {
                depth,
                marker,
                content,
            } => {
                let wrapped = wrap_list_item(*depth, marker, content, terminal_width);
                let mut byte_cursor = 0usize;
                let n = wrapped.len();
                for (li, (_line_spans, plain, is_continuation)) in wrapped.into_iter().enumerate() {
                    let start = byte_cursor;
                    let end = start + plain.len();
                    out.push(VisualLine {
                        block_idx,
                        line_in_block: li,
                        text: plain,
                        kind: VisualLineKind::ListItem {
                            depth: *depth,
                            marker_len: marker.len() as u8,
                            is_continuation,
                        },
                        block_byte_start: start,
                        block_byte_end: end,
                    });
                    byte_cursor = end;
                    if li + 1 < n {
                        byte_cursor += 1;
                    }
                }
            }

            Block::CodeBlock { lines, .. } => {
                // Block text is the lines joined by '\n' — preserves raw layout so
                // highlights on code track exact characters.
                let n = lines.len();
                let mut byte_cursor = 0usize;
                for (i, line) in lines.iter().enumerate() {
                    let start = byte_cursor;
                    let end = start + line.len();
                    out.push(VisualLine {
                        block_idx,
                        line_in_block: i,
                        text: line.clone(),
                        kind: VisualLineKind::Code {
                            is_first: i == 0,
                            is_last: i == n - 1,
                        },
                        block_byte_start: start,
                        block_byte_end: end,
                    });
                    byte_cursor = end + 1; // '\n' separator between lines
                }
            }

            Block::Quote(spans) => {
                let quote_width = terminal_width.saturating_sub(4).max(1);
                let wrapped = wrap_spans(spans, quote_width);
                let mut byte_cursor = 0usize;
                let n = wrapped.len();
                for (li, (_line_spans, plain)) in wrapped.into_iter().enumerate() {
                    let start = byte_cursor;
                    let end = start + plain.len();
                    out.push(VisualLine {
                        block_idx,
                        line_in_block: li,
                        text: plain,
                        kind: VisualLineKind::Quote {
                            is_continuation: li > 0,
                        },
                        block_byte_start: start,
                        block_byte_end: end,
                    });
                    byte_cursor = end;
                    if li + 1 < n {
                        byte_cursor += 1;
                    }
                }
            }

            // Rule and Matrix are handled above via the table-group path.
            Block::Rule | Block::Matrix { .. } => {}
            // Anchors are invisible — they tag the next visible block for
            // label-to-line resolution by the reader.
            Block::Anchor(_) => {}
            Block::Figure { rows, alt, .. } => {
                // One whole figure → N panel rows (stacked) sharing the
                // figure vertical budget.  Each panel row is either a
                // single image or a side-by-side strip of subfigures.
                let stack_total = rows.len() as u8;
                if stack_total == 0 {
                    continue;
                }
                let max_rows = panel_row_budget(figure_budget, stack_total);
                let last_row_idx = rows.len() - 1;
                for (row_idx, row) in rows.iter().enumerate() {
                    if row.is_empty() {
                        continue;
                    }
                    if row.len() == 1 {
                        // Single-image panel row.
                        let item = &row[0];
                        let (cols, vrows) =
                            compute_cell_footprint(item.dims, terminal_width, max_rows);
                        emit_image_rows(&mut out, block_idx, item.kitty_id, cols, vrows);
                    } else {
                        // Side-by-side panel row.  Per-item width is the
                        // row's share; height = the tallest sibling's
                        // natural aspect-correct height under that width
                        // (capped at the panel budget so a single huge
                        // sibling can't crowd out the stack).
                        let count = row.len() as u16;
                        let per_cols = (terminal_width as u16) / count;
                        let mut row_height: u16 = MIN_IMAGE_ROWS;
                        for item in row {
                            let (_c, r) =
                                compute_cell_footprint(item.dims, per_cols as usize, max_rows);
                            if r > row_height {
                                row_height = r;
                            }
                        }
                        let row_items: Vec<(u32, u16)> = row
                            .iter()
                            .map(|item| {
                                let cols = match item.dims {
                                    Some((w, h)) if h > 0 && w > 0 => {
                                        let derived = ((row_height as u32 * w * 2) / h) as u16;
                                        derived.min(per_cols).max(1)
                                    }
                                    _ => per_cols,
                                };
                                (item.kitty_id, cols)
                            })
                            .collect();
                        emit_image_row_rows(&mut out, block_idx, &row_items, row_height);
                    }
                    // Separator between stacked panels (not after the
                    // last one — caption follows there).
                    if row_idx < last_row_idx {
                        out.push(VisualLine {
                            block_idx,
                            line_in_block: 0,
                            text: String::new(),
                            kind: VisualLineKind::Blank,
                            block_byte_start: 0,
                            block_byte_end: 0,
                        });
                    }
                }
                if !alt.is_empty() {
                    emit_caption(&mut out, block_idx, alt, max_rows as usize, terminal_width);
                }
            }
        }

        i += 1;
    }

    out
}

// ── Table rendering helpers ───────────────────────────────────────────────────

// Column separator width — booktabs style by default (no vertical lines).
// When a table has vertical rules, gaps widen to RULED_COL_SEP so " │ " fits.
const COL_SEP: usize = 2;
const RULED_COL_SEP: usize = 3;

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

/// Render a table group — a run of consecutive `Block::Rule` and `Block::Matrix` blocks.
/// Booktabs style by default (horizontal rules only).  When the table's
/// `vertical_rules` is non-empty, also draws `│` between marked columns and
/// `┬`/`┼`/`┴` at the corresponding intersections in horizontal rules.
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
                            let per = (extra + n - 1) / n;
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
    // Matrix found after the Rule) prevents looking past the sub-header row to ByteNet.
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
    let mut seen_matrix = false; // used to suppress cmidrules (Rules after first Matrix in header zone)
    let mut top_done = false;

    for (k, block) in group.iter().enumerate() {
        let block_idx = base_block_idx + k;
        match block {
            Block::Rule => {
                // Suppress cmidrule-style rules: Rules inside the header zone, between
                // two Matrix blocks of the header. A Rule with no Matrix following it
                // is the bottom rule, never a cmidrule — don't suppress.
                let has_matrix_after = group[k + 1..]
                    .iter()
                    .any(|b| matches!(b, Block::Matrix { .. }));
                let is_cmidrule = k < header_end && seen_matrix && has_matrix_after;
                if !is_cmidrule {
                    // Choose intersection style: bottom if this Rule comes after the
                    // last Matrix, mid otherwise.
                    let is_bottom = last_matrix_k.map_or(false, |lk| k > lk);
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
                // Emit the top rule (toprule equivalent) before the very first row.
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
/// OR has a blank first column.  Data rows have non-blank first columns and all span=1.
fn rows_look_like_header(rows: &[Vec<(String, usize)>]) -> bool {
    rows.iter().all(|row| {
        row.iter().any(|(_, span)| *span > 1)
            || row.first().map_or(true, |(cell, _)| cell.trim().is_empty())
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

    // Leading-edge rule (raw position 0 from a `|c...` spec).
    if active_rules.contains(&0) {
        result.push_str("│ ");
    }

    let mut col_pos = 0usize;
    let mut first_part = true;

    for (cell_text, span) in row.iter() {
        let cell_end = col_pos + span;
        // Which active-column slots does this cell cover?
        let ai_start = active_cols.partition_point(|&j| j < col_pos);
        let ai_end = active_cols.partition_point(|&j| j < cell_end);

        if ai_end > ai_start && ai_start < n_active {
            let n_covered = ai_end - ai_start;
            // Display width = sum of individual slot widths + inner gaps.
            let display_width: usize =
                col_widths[ai_start..ai_end].iter().sum::<usize>() + (n_covered - 1) * gap_width;

            if !first_part {
                // Gap immediately before this cell sits at active boundary ai_start.
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
                // Center spanning header over combined width.
                if tw > display_width && display_width > 0 {
                    let t: String = text.chars().take(display_width.saturating_sub(1)).collect();
                    format!("{t}…")
                } else {
                    let pad = display_width - tw;
                    let pl = pad / 2;
                    let pr = pad - pl;
                    format!("{}{}{}", " ".repeat(pl), text, " ".repeat(pr))
                }
            } else {
                // Left-align with truncation if needed.
                if tw > display_width && display_width > 0 {
                    let t: String = text.chars().take(display_width.saturating_sub(1)).collect();
                    format!("{t}…")
                } else {
                    format!("{:<width$}", text, width = display_width)
                }
            };
            result.push_str(&content);
        }
        col_pos += span;
    }

    // Trailing-edge rule (raw position n_raw from a `...c|` spec).
    if active_rules.contains(&n_active) {
        result.push_str(" │");
    }

    result
}

/// Center `line` (of visual width `block_width`) within `terminal_width`.
fn center_line(line: &str, block_width: usize, terminal_width: usize) -> String {
    if terminal_width <= block_width {
        return line.to_string();
    }
    let pad = (terminal_width - block_width) / 2;
    format!("{}{}", " ".repeat(pad), line)
}

/// Approximate visual column width of a string (ASCII chars = 1, others = 1 for now).
/// A full Unicode-aware implementation can replace this without API changes.
fn visual_width(s: &str) -> usize {
    s.chars().count()
}

