//! Header bar (top) and status line (bottom).
//!
//! These two rows frame the reader content area.  The header bar shows
//! paper title + authors when `Reader::meta` is set; the status bar
//! shows current mode, search/figure/voice/count details, and the
//! `cur/total · pct%` position summary.  `:`-command errors override
//! the entire status row until the next keystroke clears them.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use ui_theme::Theme;

use crate::state::{FindKind, Mode, Operator, Reader};
use super::toc_trunc;

pub(super) fn draw_header(frame: &mut Frame, reader: &Reader, area: Rect, t: &Theme) {
    let Some(meta) = &reader.meta else { return };
    let w = area.width as usize;
    let title = format!(" {}", meta.title);
    let authors = if meta.authors.is_empty() {
        String::new()
    } else {
        format!("  {}", meta.authors)
    };
    let title_width = title.chars().count();
    let authors_width = authors.chars().count();
    let mut spans = Vec::new();
    if authors.is_empty() || title_width + authors_width >= w {
        spans.push(Span::styled(
            toc_trunc(&title, w),
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ));
    } else {
        spans.push(Span::styled(
            title,
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(
            " ".repeat(w.saturating_sub(title_width + authors_width)),
        ));
        spans.push(Span::styled(authors, Style::default().fg(t.text_dim)));
    }
    let header = Paragraph::new(Line::from(spans)).style(Style::default().bg(t.bg_panel));
    frame.render_widget(header, area);
}

pub(super) fn draw_status(frame: &mut Frame, reader: &Reader, area: Rect, t: &Theme) {
    let cur = reader.current_line() + 1;
    let tot = reader.total_lines();
    let pct = if tot == 0 { 0 } else { cur * 100 / tot };
    // Errors from the most recent `:` command override the rest of the status
    // line so they're hard to miss.  Cleared on the next keystroke.
    if let Some(err) = &reader.cmd_error {
        let status =
            Paragraph::new(format!(" {err}")).style(Style::default().bg(t.bg_input).fg(t.error));
        frame.render_widget(status, area);
        return;
    }

    let mode = mode_label(reader);
    let mut details: Vec<String> = Vec::new();
    if !reader.search_matches().is_empty() {
        details.push(format!(
            "match {}/{}",
            reader.search_idx + 1,
            reader.search_matches().len()
        ));
    }
    if let Some((idx, total)) = reader
        .current_figure_position()
        .filter(|_| reader.figure_preview_visible())
    {
        details.push(format!("fig {idx}/{total}"));
    }
    if let Some(pending) = pending_input_label(reader) {
        details.push(pending);
    }
    if !reader.count_buf.is_empty() {
        details.push(format!("count {}_", reader.count_buf));
    }
    if let Some(voice) = crate::state::voice_control::voice_status_label(reader) {
        details.push(voice);
    }

    let mode_text = format!(" {mode} ");
    let right = format!("{cur}/{tot}  {pct}% ");
    let detail_text = if details.is_empty() {
        String::new()
    } else {
        format!(" {}", details.join("  "))
    };
    let available = area.width as usize;
    let used = mode_text.chars().count() + right.chars().count();
    let detail = toc_trunc(&detail_text, available.saturating_sub(used));
    let spacer_width = available
        .saturating_sub(mode_text.chars().count() + detail.chars().count() + right.chars().count());

    let line = Line::from(vec![
        Span::styled(
            mode_text,
            Style::default()
                .bg(t.accent)
                .fg(t.text_on_accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(detail, Style::default().fg(t.text_dim)),
        Span::raw(" ".repeat(spacer_width)),
        Span::styled(right, Style::default().fg(t.text_dim)),
    ]);
    let status = Paragraph::new(line).style(Style::default().bg(t.bg_input));
    frame.render_widget(status, area);
}

pub(super) fn mode_label(reader: &Reader) -> &'static str {
    match reader.mode() {
        Mode::Normal => "READ",
        Mode::Search => "SEARCH",
        Mode::Visual { line_mode: true } => "VISUAL LINE",
        Mode::Visual { line_mode: false } => "VISUAL",
        Mode::AwaitingChar { .. }
        | Mode::AwaitingMarkName { .. }
        | Mode::AwaitingG
        | Mode::AwaitingBracket { .. }
        | Mode::AwaitingOperator { .. }
        | Mode::AwaitingTextObject { .. } => "PENDING",
        Mode::Command => "COMMAND",
    }
}

pub(super) fn pending_input_label(reader: &Reader) -> Option<String> {
    match reader.mode() {
        Mode::AwaitingChar { kind } => {
            let prefix = match kind {
                FindKind::F => "f",
                FindKind::ShiftF => "F",
                FindKind::T => "t",
                FindKind::ShiftT => "T",
            };
            Some(format!("{prefix}_"))
        }
        Mode::AwaitingMarkName { for_set } => {
            if *for_set {
                Some("m_".to_string())
            } else {
                Some("'_".to_string())
            }
        }
        Mode::AwaitingG => Some("g_".to_string()),
        Mode::AwaitingBracket { forward } => {
            if *forward {
                Some("]_".to_string())
            } else {
                Some("[_".to_string())
            }
        }
        Mode::AwaitingOperator { op } => match op {
            Operator::Yank => Some("y_".to_string()),
        },
        Mode::AwaitingTextObject { op, around } => {
            let prefix = match op {
                Operator::Yank => "y",
            };
            let mid = if *around { "a" } else { "i" };
            Some(format!("{prefix}{mid}_"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doc_model::Block;

    /// Pin the mode → label mapping.  These labels show up in the
    /// status bar and are part of the visible UX; a regression here
    /// would silently swap "VISUAL LINE" with "VISUAL", etc.
    #[test]
    fn mode_label_covers_every_mode() {
        let mut reader = Reader::new(vec![Block::Line("x".into())], 80, 24);

        // Normal default.
        assert_eq!(mode_label(&reader), "READ");

        reader.enter_visual_mode(false);
        assert_eq!(mode_label(&reader), "VISUAL");

        reader.return_to_normal();
        reader.enter_visual_mode(true);
        assert_eq!(mode_label(&reader), "VISUAL LINE");

        reader.return_to_normal();
        reader.enter_command_mode();
        assert_eq!(mode_label(&reader), "COMMAND");

        reader.return_to_normal();
        reader.enter_search();
        assert_eq!(mode_label(&reader), "SEARCH");

        reader.return_to_normal();
        reader.enter_awaiting_g();
        assert_eq!(mode_label(&reader), "PENDING");
    }
}
