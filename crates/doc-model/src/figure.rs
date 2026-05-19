//! Figure sizing and visual-line emission for `Block::Figure`.
//!
//! Owns the cell-footprint math (`compute_cell_footprint`, re-exported
//! from the crate root), the per-panel row-budget formula, and the
//! `emit_figure_lines` entry point that the layout orchestrator calls
//! for each `Block::Figure` block.

use crate::{ImageItem, VisualLine, VisualLineKind};

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
pub(crate) fn figure_row_budget(content_height: usize) -> u16 {
    let h = content_height as u16;
    let usable = h.saturating_sub(CAPTION_ROWS_BUDGET);
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
    let nat_c = ((w_px / CELL_W_PX) as u16).max(1);
    let mut c = nat_c.min(max_cols);
    let mut r = ((c as u32 * h_px * CELL_W_PX) / (w_px * CELL_H_PX)) as u16;
    r = r.max(MIN_IMAGE_ROWS);
    if r > max_rows {
        r = max_rows;
        c = ((r as u32 * w_px * CELL_H_PX) / (h_px * CELL_W_PX)) as u16;
        c = c.min(max_cols).max(1);
    }
    (c, r)
}

/// Word-wrap a caption to fit `width` columns.  Greedy: accumulates
/// words into the current line until adding another would exceed
/// width, then breaks.  Used only for figure captions — long figure
/// descriptions otherwise get truncated at the right edge of the screen.
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

/// Push N consecutive `VisualLineKind::Image` rows for one single-image panel.
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

/// Push N consecutive `VisualLineKind::ImageRow` rows for one
/// side-by-side panel row.
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

/// Emit the visual lines for one `Block::Figure`.  One whole figure
/// becomes N panel rows (stacked) sharing the figure vertical budget,
/// followed by an optional centered caption.  Each panel row is either
/// a single image or a side-by-side strip of subfigures.
///
/// `figure_budget` is the whole-figure vertical budget (rows available
/// for the image portion, computed via `figure_row_budget` once per
/// document at the top of layout dispatch).
pub(crate) fn emit_figure_lines(
    out: &mut Vec<VisualLine>,
    block_idx: usize,
    rows: &[Vec<ImageItem>],
    alt: &str,
    terminal_width: usize,
    figure_budget: u16,
) {
    let stack_total = rows.len() as u8;
    if stack_total == 0 {
        return;
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
            let (cols, vrows) = compute_cell_footprint(item.dims, terminal_width, max_rows);
            emit_image_rows(out, block_idx, item.kitty_id, cols, vrows);
        } else {
            // Side-by-side panel row.  Per-item width is the row's
            // share; height = the tallest sibling's natural
            // aspect-correct height under that width (capped at the
            // panel budget so a single huge sibling can't crowd out
            // the stack).
            let count = row.len() as u16;
            let per_cols = (terminal_width as u16) / count;
            let mut row_height: u16 = MIN_IMAGE_ROWS;
            for item in row {
                let (_c, r) = compute_cell_footprint(item.dims, per_cols as usize, max_rows);
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
            emit_image_row_rows(out, block_idx, &row_items, row_height);
        }
        // Separator between stacked panels (not after the last one —
        // caption follows there).
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
        emit_caption(out, block_idx, alt, max_rows as usize, terminal_width);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// When the image's natural aspect-correct rows exceed the row
    /// budget, the function must clamp rows AND shrink cols to keep
    /// the aspect ratio intact (lines 369-373 of the original code).
    #[test]
    fn compute_cell_footprint_preserves_aspect_when_height_clamped() {
        // Tall image: 80×400 pixels.  At CELL_W=8 / CELL_H=16, natural cell
        // size is 10 cols × 25 rows.  Clamping max_rows to 10 should also
        // shrink cols proportionally so the image isn't squished.
        let (cols, rows) = compute_cell_footprint(Some((80, 400)), 100, 10);
        assert_eq!(rows, 10, "rows must be clamped to max_rows");
        // Aspect-preserved cols: rows * w_px * CELL_H_PX / (h_px * CELL_W_PX)
        //                      = 10 * 80 * 16 / (400 * 8) = 4
        assert_eq!(cols, 4, "cols must shrink proportionally, got {}", cols);
    }

    /// The PANEL_ROW_CAP (21) is load-bearing — it ensures single
    /// figures and stacked panels look uniform.  A regression that
    /// lifts the cap would make solo figures balloon on tall
    /// terminals.
    #[test]
    fn panel_row_budget_stack_bonus_caps_at_panel_row_cap() {
        // Solo figure with huge budget — must be capped at 21.
        assert_eq!(panel_row_budget(100, 1), 21);

        // 2-stack with huge budget — must also be capped at 21.
        // Raw formula: (100 + 10 - 1) / 2 = 54, then min(21) = 21.
        assert_eq!(panel_row_budget(100, 2), 21);

        // 3-stack with budget=30: (30 + 20 - 2) / 3 = 16, then min(21) = 16.
        // Below the cap, the natural budget applies.
        assert_eq!(panel_row_budget(30, 3), 16);

        // Tiny budget on a solo figure must respect the MIN_IMAGE_ROWS
        // floor (6) — the stack-bonus branch only kicks in for n >= 2,
        // so n=1 is the path that can actually underflow.
        assert_eq!(panel_row_budget(2, 1), MIN_IMAGE_ROWS);
    }
}
