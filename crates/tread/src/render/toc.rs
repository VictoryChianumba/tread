//! Table-of-contents side pane.
//!
//! Draws the left-hand section list when `reader.toc_visible` is true.
//! The current section auto-scrolls into the panel's vertical middle,
//! and the current entry is highlighted with the theme's accent colour.

use std::collections::HashSet;

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

/// Indices of the current section's ancestor path (parent, grandparent,
/// …) so the sidebar can brighten the breadcrumb — on a deep subsection
/// you still see which top-level section you're reading.
fn ancestor_path(sections: &[(usize, u8, String)], current: Option<usize>) -> HashSet<usize> {
    let mut set = HashSet::new();
    let Some(cur) = current else {
        return set;
    };
    // Walk to the current section maintaining a level stack; what remains
    // (minus the current section itself) is the path to it.
    let mut stack: Vec<(usize, u8)> = Vec::new();
    for (i, (_, level, _)) in sections.iter().enumerate().take(cur + 1) {
        while stack.last().is_some_and(|&(_, l)| l >= *level) {
            stack.pop();
        }
        stack.push((i, *level));
    }
    stack.pop(); // drop the current section; keep only ancestors
    set.extend(stack.into_iter().map(|(i, _)| i));
    set
}

pub(super) fn draw_toc(frame: &mut Frame, reader: &Reader, area: Rect, t: &Theme) {
    let panel_h = area.height as usize;
    // 1 right border + 1 leading space + 2 marker cols = 4 chars overhead.
    let inner_w = area.width.saturating_sub(2) as usize;
    let cur_sec = reader.current_section_idx();
    let ancestors = ancestor_path(reader.sections(), cur_sec);

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
            let is_current = cur_sec == Some(sec_idx);
            // A reserved 2-col marker keeps every row's indent aligned;
            // only the current section fills it (so it reads as "you are
            // here" without shifting the others).
            let marker = if is_current { "▸ " } else { "  " };
            let avail = inner_w.saturating_sub(indent + 2);
            let label = format!(" {marker}{}{}", " ".repeat(indent), toc_trunc(text, avail));
            let style = if is_current {
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
            } else if ancestors.contains(&sec_idx) {
                // Breadcrumb: the path to the current section, brighter
                // than unrelated entries but not the accent of "here".
                Style::default().fg(t.text)
            } else {
                Style::default().fg(t.toc_dim)
            };
            Line::styled(label, style)
        })
        .collect();

    let widget = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::RIGHT)
            .border_style(Style::default().fg(t.text_dim)),
    );
    frame.render_widget(widget, area);
}

#[cfg(test)]
mod tests {
    use super::ancestor_path;

    fn secs() -> Vec<(usize, u8, String)> {
        vec![
            (0, 1, "1".into()),     // 0
            (1, 2, "1.1".into()),   // 1
            (2, 1, "2".into()),     // 2
            (3, 2, "2.1".into()),   // 3
            (4, 3, "2.1.1".into()), // 4
        ]
    }

    #[test]
    fn ancestor_path_of_deep_section_is_its_parents() {
        // Current = "2.1.1" (idx 4) → ancestors "2" (2) and "2.1" (3).
        let got = ancestor_path(&secs(), Some(4));
        assert_eq!(got, [2usize, 3].into_iter().collect());
    }

    #[test]
    fn ancestor_path_of_top_level_or_none_is_empty() {
        assert!(ancestor_path(&secs(), Some(2)).is_empty());
        assert!(ancestor_path(&secs(), None).is_empty());
    }
}
