//! Table-of-contents side pane.
//!
//! Draws the left-hand section list when `reader.toc_visible` is true.
//! The current section auto-scrolls into the panel's vertical middle,
//! and the current entry is highlighted with the theme's accent colour.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
};
use ui_theme::Theme;

use crate::state::Reader;
use super::toc_trunc;

pub(super) fn draw_toc(frame: &mut Frame, reader: &Reader, area: Rect, t: &Theme) {
    let panel_h = area.height as usize;
    // 1 char right border + 1 char leading space = 2 chars overhead
    let inner_w = area.width.saturating_sub(2) as usize;
    let cur_sec = reader.current_section_idx();

    // Scroll to keep current section vertically centered in the panel.
    let toc_scroll = cur_sec
        .map(|idx| idx.saturating_sub(panel_h / 2))
        .unwrap_or(0);

    let total = reader.sections().len();

    let lines: Vec<Line> = (0..panel_h)
        .map(|row| {
            let sec_idx = toc_scroll + row;
            if sec_idx >= total {
                return Line::raw("");
            }
            let (_, level, text) = &reader.sections()[sec_idx];
            let indent = match level {
                1 => 0usize,
                2 => 2usize,
                _ => 4usize,
            };
            let avail = inner_w.saturating_sub(indent);
            let label = format!(" {}{}", " ".repeat(indent), toc_trunc(text, avail));
            let is_current = cur_sec == Some(sec_idx);
            if is_current {
                Line::styled(
                    label,
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                )
            } else {
                Line::styled(label, Style::default().fg(t.toc_dim))
            }
        })
        .collect();

    let widget = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::RIGHT)
            .border_style(Style::default().fg(t.text_dim)),
    );
    frame.render_widget(widget, area);
}
