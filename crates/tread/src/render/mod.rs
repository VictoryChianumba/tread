mod chrome;
mod content;
mod overlays;
mod preview;
mod toc;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Margin, Rect},
};
use ui_theme::Theme;

use crate::state::{Mode, Reader, TOC_WIDTH};

pub fn draw(frame: &mut Frame, area: Rect, reader: &Reader, t: &Theme) {
    let (header_area, toc_area, content_area, status_area, search_area) =
        split_layout(area, reader);

    if let Some(ha) = header_area {
        let _s = crate::bench::Span::new("draw_header");
        chrome::draw_header(frame, reader, ha, t);
    }
    if let Some(ta) = toc_area {
        let _s = crate::bench::Span::new("draw_toc");
        toc::draw_toc(frame, reader, ta, t);
    }
    // When the figure-preview pane is active, the right 40% of the
    // content area is reserved for the figure (drawn post-draw by
    // `images::place_one_figure`).  The reader content fills the
    // left 60%.  When inactive, content fills the whole area.
    let (reader_area, preview_area) = split_content_for_preview(content_area, reader);
    {
        let _s = crate::bench::Span::new("draw_content");
        content::draw_content(frame, reader, reader_area, t);
    }
    if let Some(pa) = preview_area {
        let _s = crate::bench::Span::new("draw_preview_pane");
        preview::draw_preview_pane(frame, reader, pa, t);
    }
    {
        let _s = crate::bench::Span::new("draw_status");
        chrome::draw_status(frame, reader, status_area, t);
    }
    if *reader.mode() == Mode::Search {
        overlays::draw_search_bar(frame, reader, search_area.unwrap(), t);
    }
    if *reader.mode() == Mode::Command {
        overlays::draw_command_bar(frame, reader, search_area.unwrap(), t);
    }
    if let Some(popup) = reader.popup() {
        overlays::draw_text_popup(frame, area, t, popup);
    }
    if reader.help_visible {
        overlays::draw_help_overlay(frame, area, t);
    }
}

/// Carve out the figure-preview pane from the reader's content area.
///
/// Returns `(reader_area, preview_area)`.  When the preview is off
/// or the document has no figures, `preview_area` is `None` and
/// `reader_area` matches the input — the legacy single-pane layout.
/// When on, splits the area horizontally 60/40 (text : figure).
///
/// Used by both `render::draw` (to size the reader content area) and
/// `lib::after_draw` (to find where `place_one_figure` should paint).
/// Keeping the two callers in sync is critical or the image will land
/// over the reader text.
pub fn split_content_for_preview(area: Rect, reader: &Reader) -> (Rect, Option<Rect>) {
    if !reader.figure_preview_visible() {
        return (area, None);
    }
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);
    (cols[0], Some(cols[1]))
}

pub fn preview_image_area(area: Rect) -> Rect {
    if area.width <= 2 || area.height <= 2 {
        area
    } else {
        area.inner(Margin {
            horizontal: 1,
            vertical: 1,
        })
    }
}

pub fn split_layout(
    area: Rect,
    reader: &Reader,
) -> (Option<Rect>, Option<Rect>, Rect, Rect, Option<Rect>) {
    // Optional 1-row header at the very top.
    let (header_area, below_header) = if reader.meta.is_some() {
        let v = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(area);
        (Some(v[0]), v[1])
    } else {
        (None, area)
    };

    // Optional TOC panel on the left.
    let (toc_area, right) = if reader.toc_visible {
        let h = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(TOC_WIDTH as u16), Constraint::Min(1)])
            .split(below_header);
        (Some(h[0]), h[1])
    } else {
        (None, below_header)
    };

    let (content_area, status_area, search_area) = match reader.mode() {
        Mode::Normal
        | Mode::Visual { .. }
        | Mode::AwaitingChar { .. }
        | Mode::AwaitingMarkName { .. }
        | Mode::AwaitingG
        | Mode::AwaitingBracket { .. }
        | Mode::AwaitingOperator { .. }
        | Mode::AwaitingTextObject { .. } => {
            let v = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .split(right);
            (v[0], v[1], None)
        }
        Mode::Search | Mode::Command => {
            // Command mode reuses the search-bar slot — same shape, different prefix.
            let v = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(1),
                    Constraint::Length(1),
                    Constraint::Length(1),
                ])
                .split(right);
            (v[0], v[1], Some(v[2]))
        }
    };

    (
        header_area,
        toc_area,
        content_area,
        status_area,
        search_area,
    )
}

pub(super) fn toc_trunc(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max {
        s.to_string()
    } else if max > 1 {
        let end = s
            .char_indices()
            .nth(max - 1)
            .map(|(i, _)| i)
            .unwrap_or(s.len());
        format!("{}…", &s[..end])
    } else {
        s.chars().take(max).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doc_model::Block;

    fn reader_with_preview() -> Reader {
        let mut reader = Reader::new(
            vec![Block::Figure {
                rows: vec![vec![doc_model::ImageItem {
                    path: std::path::PathBuf::from("figure.png"),
                    kitty_id: 1,
                    dims: Some((100, 100)),
                }]],
                alt: "figure".to_string(),
                figure_id: 1,
                column_gaps_after: Vec::new(),
                header_rows: Vec::new(),
            }],
            100,
            24,
        );
        reader.set_figure_preview_active(true);
        reader
    }

    #[test]
    fn split_content_for_preview_reserves_side_pane() {
        let reader = reader_with_preview();
        let (reader_area, preview_area) =
            split_content_for_preview(Rect::new(0, 0, 100, 20), &reader);

        assert_eq!(reader_area.width, 60);
        assert_eq!(preview_area.map(|area| area.width), Some(40));
    }

    #[test]
    fn preview_image_area_leaves_room_for_frame() {
        let area = preview_image_area(Rect::new(60, 2, 40, 20));

        assert_eq!(area, Rect::new(61, 3, 38, 18));
    }

}
