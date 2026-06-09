//! Figure-preview side pane.
//!
//! Renders the right-hand pane that hosts one figure at a time when
//! `Reader::figure_preview_active` is true.  The pane has no box: a single
//! inset vertical divider (`draw_divider`) separates it from the reading
//! column, and the figure/reference label is a plain header line rather
//! than a border title.  The actual pixel transmission is done by
//! `lib::after_draw` post-frame; this function only draws the ratatui-side
//! scaffolding (divider, header, column labels, caption).

use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    widgets::{Clear, Paragraph, Wrap},
};
use ui_theme::Theme;

use crate::state::Reader;
use super::preview_image_area;

/// Rows of clear space left at the top and bottom of the pane before the
/// divider begins.  Keeps the rule floating clear of the screen edges so it
/// reads as a quiet seam rather than a full-height wall.
const PREVIEW_DIVIDER_GAP: u16 = 2;

/// Draw the single inset vertical divider down the pane's left edge.  This
/// replaces the old three-sided box: no top/bottom caps, just one thin rule
/// that stops `PREVIEW_DIVIDER_GAP` rows short of each screen edge.
fn draw_divider(frame: &mut Frame, area: Rect, t: &Theme) {
    let gap = PREVIEW_DIVIDER_GAP;
    // Need room for a gap at both ends plus at least one rule cell between.
    if area.height <= gap.saturating_mul(2) {
        return;
    }
    let start = area.y.saturating_add(gap);
    let end = area.y + area.height - gap; // exclusive
    let style = Style::default().fg(t.border_active);
    for y in start..end {
        frame.buffer_mut().set_string(area.x, y, "│", style);
    }
}

/// Plain header label at the very top of the pane, indented to sit above the
/// content column.  Replaces the border title.
fn draw_preview_title(frame: &mut Frame, area: Rect, title: &str, t: &Theme) {
    let x = preview_image_area(area).x;
    frame
        .buffer_mut()
        .set_string(x, area.y, title, Style::default().fg(t.text_dim));
}

pub(super) fn draw_preview_pane(frame: &mut Frame, reader: &Reader, area: Rect, t: &Theme) {
    // Context-first: when the cursor is on a citation, the pane shows that
    // reference instead of a figure (the figure is the fallback when the
    // cursor isn't on a cross-reference).
    if let Some((key, text)) = reader.cursor_citation() {
        draw_citation_pane(frame, &key, &text, area, t);
        return;
    }
    let title = reader
        .preview_figure_position()
        .map(|(idx, total)| format!("Figure {idx}/{total}"))
        // Figure-less papers: the pane is reference-oriented, not a figure
        // browser, so don't mislabel an empty pane as "Figure".
        .unwrap_or_else(|| {
            if reader.figure_count() == 0 {
                "Reference".to_string()
            } else {
                "Figure".to_string()
            }
        });
    // `Clear` writes default-styled spaces into every cell.  This gives
    // iTerm2 the cell anchors its Kitty-graphics placement needs — without
    // it the interior cells stay uninitialized, Crossterm's diff skips
    // them, and the image placement has no cell substrate to bind to,
    // leaving the pane empty even though `transmit_and_place` succeeded.
    //
    // No bg fill on the Clear — that would obscure the image.  Default
    // style on the spaces is transparent on the terminals tested (iTerm2,
    // Ghostty, Kitty).
    frame.render_widget(Clear, area);
    draw_divider(frame, area, t);
    draw_preview_title(frame, area, &title, t);

    // Column-label rows captured from the figure's tabular header.  Use
    // the SAME layout function the image tiler uses, so each label sits
    // exactly above the image columns it describes.
    //
    // We render via `frame.buffer_mut().set_string` with `Style::default()`
    // so the terminal's chosen fg/bg colours pass through unmodified —
    // headers inherit the user's terminal palette instead of forcing the
    // theme's bg, which would clash with whatever's behind the image
    // grid.
    if let Some(entry) = reader.preview_figure_entry() {
        let image_area = preview_image_area(area);
        let layout = entry.layout(image_area);
        for hrow in &layout.headers {
            for cell in &hrow.cells {
                if cell.text.is_empty() || cell.width == 0 {
                    continue;
                }
                let truncated: String = cell.text.chars().take(cell.width as usize).collect();
                let visible_width = truncated.chars().count() as u16;
                let pad = cell.width.saturating_sub(visible_width) / 2;
                let x = cell.abs_col.saturating_add(pad);
                frame.buffer_mut().set_string(
                    x,
                    hrow.y,
                    &truncated,
                    Style::default(),
                );
            }
        }
        if let Some(caption) = &layout.caption {
            // Pane interior is image_area; clip caption rows that fall
            // outside (tiny preview panes don't get truncated text
            // bleeding past the bottom).
            let bottom = image_area.y.saturating_add(image_area.height);
            for (i, line) in caption.lines.iter().enumerate() {
                let y = caption.y.saturating_add(i as u16);
                if y >= bottom {
                    break;
                }
                let truncated: String = line.chars().take(caption.width as usize).collect();
                frame.buffer_mut().set_string(
                    caption.x,
                    y,
                    &truncated,
                    Style::default(),
                );
            }
        }
    } else if reader.figure_count() == 0 {
        // Figure-less paper, cursor not on a citation: explain the pane.
        let hint = Paragraph::new("Move the cursor onto a citation to see its reference here.")
            .style(Style::default().fg(t.text_dim))
            .wrap(Wrap { trim: false });
        frame.render_widget(hint, preview_image_area(area));
    }
}

/// Render the cursor's citation as a text panel: `[key]` header + the
/// wrapped bibliography entry.  Same inset divider and header shape as the
/// figure pane so the two read as one surface; like the figure pane it
/// carries no background fill.
fn draw_citation_pane(frame: &mut Frame, key: &str, text: &str, area: Rect, t: &Theme) {
    frame.render_widget(Clear, area);
    draw_divider(frame, area, t);
    draw_preview_title(frame, area, &format!("[{key}]"), t);
    // Body shares the figure pane's content column so header and text align.
    let inner = preview_image_area(area);
    let para = Paragraph::new(text.to_string())
        .style(Style::default().fg(t.text))
        .wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
}
