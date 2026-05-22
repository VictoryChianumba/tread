//! Figure-preview side pane.
//!
//! Renders the right-hand pane that hosts one figure at a time when
//! `Reader::figure_preview_active` is true.  Owns the border + title +
//! Clear-fill that gives iTerm2 the cell substrate its Kitty graphics
//! placement needs.  The actual pixel transmission is done by
//! `lib::after_draw` post-frame; this function only draws the
//! ratatui-side scaffolding (border, headers, caption).

use ratatui::{
    Frame,
    layout::{Margin, Rect},
    style::Style,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use ui_theme::Theme;

use crate::state::Reader;
use super::preview_image_area;

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
        .map(|(idx, total)| format!(" Figure {idx}/{total} "))
        .unwrap_or_else(|| " Figure ".to_string());
    // First: `Clear` writes default-styled spaces into every cell.  This
    // gives iTerm2 the cell anchors its Kitty-graphics placement needs —
    // a bare `Block` without `.style()` only paints its border cells, so
    // the interior cells stay uninitialized in the buffer.  Crossterm's
    // diff then doesn't write them to the terminal, and iTerm2's image
    // placement has no cell substrate to bind to, leaving the pane
    // empty even though `transmit_and_place` succeeded.  The inline
    // path doesn't hit this because Image VL rows go through a
    // Paragraph which always writes cells.
    //
    // No bg fill on the Clear — that would obscure the image (see prior
    // bg_panel removal).  Default style on the spaces is transparent on
    // the terminals tested (iTerm2, Ghostty, Kitty).
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(title)
        .borders(Borders::LEFT | Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(t.border_active));
    frame.render_widget(block, area);

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
            // bleeding into the border).
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
    }
}

/// Render the cursor's citation as a text panel: `[key]` title + the
/// wrapped bibliography entry.  Same border shape as the figure pane so
/// the two read as one surface; unlike the figure pane it carries a
/// background since there's no image to keep transparent.
fn draw_citation_pane(frame: &mut Frame, key: &str, text: &str, area: Rect, t: &Theme) {
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(format!(" [{key}] "))
        .borders(Borders::LEFT | Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(t.border_active))
        .style(Style::default().bg(t.bg_popup));
    let inner = block
        .inner(area)
        .inner(Margin { horizontal: 1, vertical: 0 });
    frame.render_widget(block, area);
    let para = Paragraph::new(text.to_string())
        .style(Style::default().fg(t.text).bg(t.bg_popup))
        .wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
}
