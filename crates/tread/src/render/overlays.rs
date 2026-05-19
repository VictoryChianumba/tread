//! Modal overlays and input bars.
//!
//! Two categories live here:
//! - **Input bars** (`draw_search_bar`, `draw_command_bar`): one-row
//!   prompts shown at the bottom of the screen while in `Mode::Search`
//!   or `Mode::Command`.  Both share the same shape via `prompt_line`.
//! - **Floating overlays** (`draw_text_popup`, `draw_help_overlay`):
//!   centered modal popups that paint over the reader content.
//!   Dismissed by any keystroke (handled in the event loop).

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use ui_theme::Theme;

use crate::state::{PopupContent, Reader};
use super::toc_trunc;

pub(super) fn draw_search_bar(frame: &mut Frame, reader: &Reader, area: Rect, t: &Theme) {
    let suffix = if reader.search_matches.is_empty() {
        String::new()
    } else {
        format!(" {}/{}", reader.search_idx + 1, reader.search_matches.len())
    };
    let line = prompt_line("/", &reader.search_query, &suffix, area.width as usize, t);
    let bar = Paragraph::new(line).style(Style::default().bg(t.bg_input));
    frame.render_widget(bar, area);
}

pub(super) fn draw_command_bar(frame: &mut Frame, reader: &Reader, area: Rect, t: &Theme) {
    let line = prompt_line(
        ":",
        &format!("{}_", reader.cmd_buf),
        "",
        area.width as usize,
        t,
    );
    let bar = Paragraph::new(line).style(Style::default().bg(t.bg_input));
    frame.render_widget(bar, area);
}

fn prompt_line<'a>(
    prefix: &'static str,
    body: &str,
    suffix: &str,
    width: usize,
    t: &Theme,
) -> Line<'a> {
    let prefix_width = prefix.chars().count() + 1;
    let suffix_width = suffix.chars().count();
    let body_width = width.saturating_sub(prefix_width + suffix_width);
    let body = toc_trunc(body, body_width);
    let spacer_width = width.saturating_sub(prefix_width + body.chars().count() + suffix_width);
    Line::from(vec![
        Span::styled(
            format!(" {prefix}"),
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(body, Style::default().fg(t.text)),
        Span::raw(" ".repeat(spacer_width)),
        Span::styled(suffix.to_string(), Style::default().fg(t.text_dim)),
    ])
}

/// Render a generic text popup (used by `:marks`, `:highlights`, `:about`,
/// `:placement`).  Sized to the longest line + padding, centered in `area`.
/// Dismissed by any keystroke (handled in the event loop).
pub(super) fn draw_text_popup(frame: &mut Frame, area: Rect, t: &Theme, popup: &PopupContent) {
    let max_line = popup
        .lines
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0)
        .max(popup.title.chars().count() + 4);
    let w = (max_line as u16 + 4)
        .min(area.width.saturating_sub(4))
        .max(20);
    let h = (popup.lines.len() as u16 + 4)
        .min(area.height.saturating_sub(4))
        .max(6);
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let popup_area = Rect {
        x,
        y,
        width: w,
        height: h,
    };

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::styled(
        format!("  {}", popup.title),
        Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
    ));
    lines.push(Line::raw(""));
    for l in &popup.lines {
        lines.push(Line::styled(l.clone(), Style::default().fg(t.text)));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "  Press any key to dismiss",
        Style::default().fg(t.text_dim),
    ));

    frame.render_widget(Clear, popup_area);
    let para = Paragraph::new(lines)
        .style(Style::default().bg(t.bg_popup))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(t.text_dim)),
        );
    frame.render_widget(para, popup_area);
}

pub(super) fn draw_help_overlay(frame: &mut Frame, area: Rect, t: &Theme) {
    const W: u16 = 94;
    const H: u16 = 32;
    let x = area.x + area.width.saturating_sub(W) / 2;
    let y = area.y + area.height.saturating_sub(H) / 2;
    let popup = Rect {
        x,
        y,
        width: W.min(area.width),
        height: H.min(area.height),
    };

    let help_bg = t.bg_popup;
    let key_fg = t.accent;
    let dim_fg = t.text_dim;
    let sec_fg = t.header;

    frame.render_widget(Clear, popup);
    let widget = Block::default()
        .title(Span::styled(
            " Help ",
            Style::default().fg(sec_fg).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Span::styled(
            " Press any key to dismiss ",
            Style::default().fg(dim_fg),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(t.text_dim))
        .style(Style::default().bg(help_bg));
    frame.render_widget(widget, popup);

    let inner = popup.inner(Margin {
        vertical: 1,
        horizontal: 2,
    });
    let rows = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).split(inner);
    let header = Paragraph::new(vec![
        Line::styled(
            "Reader keymap",
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            "Navigation, figures, links, voice, and command actions",
            Style::default().fg(dim_fg),
        ),
    ])
    .style(Style::default().bg(help_bg));
    frame.render_widget(header, rows[0]);

    let cols =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(rows[1]);
    let left = Rect {
        x: cols[0].x,
        y: cols[0].y,
        width: cols[0].width.saturating_sub(1),
        height: cols[0].height,
    };
    let right = Rect {
        x: cols[1].x.saturating_add(1),
        y: cols[1].y,
        width: cols[1].width.saturating_sub(1),
        height: cols[1].height,
    };

    let left_lines = help_section(
        "Movement",
        &[
            ("j / k", "scroll down / up"),
            ("PageDn / Up", "full page scroll"),
            ("Ctrl+d / Ctrl+u", "half-page down / up"),
            ("gg / G", "top / bottom"),
            ("H / M / L", "top / middle / bottom of screen"),
            ("z", "center cursor"),
            ("} / {", "next / previous paragraph"),
            (") / (", "next / previous sentence"),
            ("w / W", "word forward (small / BIG)"),
            ("b / B", "word back (small / BIG)"),
            ("e / E", "word end (small / BIG)"),
            ("ge / gE", "back to word end"),
            ("0  ^  $", "line start / first text / line end"),
            ("5j  10G", "count prefix"),
            ("]] / [[", "next / previous section"),
        ],
        key_fg,
        sec_fg,
        dim_fg,
    );

    let right_lines = [
        help_section(
            "Search + links",
            &[
                ("/  n  N", "search / next / previous"),
                ("*", "search word under cursor"),
                ("f / F", "find char forward / backward"),
                ("t / T", "till char forward / backward"),
                ("%", "matching brace"),
                ("m{a}  '{a}", "set / jump named mark"),
                ("Enter", "follow link or citation"),
                ("K", "show citation entry"),
                ("Ctrl+O", "go back"),
            ],
            key_fg,
            sec_fg,
            dim_fg,
        ),
        vec![Line::raw("")],
        help_section(
            "Figures + selection",
            &[
                ("i", "toggle figure preview"),
                ("]f / [f", "next / previous figure"),
                ("\\", "toggle TOC"),
                ("v / V", "enter char / line visual mode"),
                ("y", "yank selection"),
                ("yy", "yank current line"),
                ("yi / ya <obj>", "yank inner / around object"),
                ("H", "highlight selection"),
                ("X", "remove highlight"),
            ],
            key_fg,
            sec_fg,
            dim_fg,
        ),
        vec![Line::raw("")],
        help_section(
            "Voice + commands",
            &[
                ("r / R", "read paragraph / to paragraph end"),
                ("Ctrl+P", "continuous reading"),
                ("Space", "pause / resume playback"),
                ("c", "recenter while reading"),
                ("Esc", "stop reading or back out"),
                (":<N>", "go to line"),
                (":goto <N|text>", "jump to section"),
                (":abstract", "jump to abstract"),
                (":references", "jump to references"),
                (":set theme=<id>", "change theme"),
                (":marks  :highlights", "inspect marks / highlights"),
                (":about  :url  :cite", "metadata / copy URL / copy BibTeX"),
                (":open  :reload  :q", "open / reload / quit"),
                ("?", "toggle this help"),
            ],
            key_fg,
            sec_fg,
            dim_fg,
        ),
    ]
    .concat();

    frame.render_widget(
        Paragraph::new(left_lines)
            .style(Style::default().bg(help_bg))
            .wrap(Wrap { trim: false }),
        left,
    );
    frame.render_widget(
        Paragraph::new(right_lines)
            .style(Style::default().bg(help_bg))
            .wrap(Wrap { trim: false }),
        right,
    );
}

fn help_section(
    title: &str,
    rows: &[(&str, &str)],
    key_fg: Color,
    sec_fg: Color,
    dim_fg: Color,
) -> Vec<Line<'static>> {
    let mut out = vec![Line::styled(
        format!(" {title}"),
        Style::default().fg(sec_fg).add_modifier(Modifier::BOLD),
    )];
    for (key, desc) in rows {
        out.push(Line::from(vec![
            Span::styled(format!("  {:<18}", key), Style::default().fg(key_fg)),
            Span::styled(desc.to_string(), Style::default().fg(dim_fg)),
        ]));
    }
    out
}
