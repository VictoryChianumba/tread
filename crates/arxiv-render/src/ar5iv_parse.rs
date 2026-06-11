//! Parse ar5iv (LaTeXML) HTML into our `Block` model.
//!
//! ar5iv (https://ar5iv.labs.arxiv.org) serves arXiv papers as LaTeXML-
//! generated HTML5.  The DOM is class-based and predictable: `ltx_section`,
//! `ltx_para`, `ltx_table`, `ltx_Math` etc.  Math nodes carry the original
//! LaTeX in an `alttext` attribute, which we hand straight to `math_render`
//! — no MathML parsing needed.
//!
//! This is the bench-D prototype: walks 1706.03762 cleanly; handles the
//! ~80 % of structural shapes that show up in modern ML/math papers.
//! Anything unrecognised is preserved as plain text rather than dropped.
//
// Block emission rules (kept consistent with `pandoc_parse.rs`):
// - Top-level title → `Block::Header { level: 1, .. }` + a `Block::Blank`.
// - Authors      → one or more `Block::StyledLine` (italic).
// - Sections     → `Block::Header` at appropriate level, then body.
// - Paragraphs   → `Block::StyledLine` carrying inline spans, with `\n`-split
//                  display math interleaved as `Block::DisplayMath`.
// - Tables       → `Block::Matrix` with row spans flattened.
// - Bibliography → a `Block::Header { level: 1, "References" }` then
//                  one `Block::StyledLine` per bibitem, each prefixed with
//                  an `Anchor` carrying the cite-key.

use doc_model::{Alignment, Block, HeaderCell, ImageItem, InlineSpan, LinkTarget};
use scraper::{ElementRef, Html, Node, Selector};
use std::collections::HashMap;

/// Recover a `cite-key → entry-text` map from a block stream produced
/// by `to_blocks`.  ar5iv emits each bibliography item as an
/// `Anchor("bib.bibN")` immediately followed by a `StyledLine` carrying
/// the rendered citation text; we pair them up so the reader's
/// citation-popup machinery sees the same shape it gets from
/// `parse::extract_bibitems` on a LaTeX tarball.
pub fn extract_bibitems(blocks: &[Block]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut iter = blocks.iter().peekable();
    while let Some(b) = iter.next() {
        if let Block::Anchor(id) = b
            && id.starts_with("bib.")
            && let Some(Block::StyledLine(spans)) = iter.peek() {
                let text: String = spans.iter().map(|s| s.text.as_str()).collect();
                let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if !cleaned.is_empty() {
                    out.insert(id.clone(), cleaned);
                }
            }
    }
    out
}

/// Walk an ar5iv HTML document and emit our `Block` stream.
pub fn to_blocks(html: &str) -> Vec<Block> {
    let doc = Html::parse_document(html);
    let article_sel = Selector::parse("article.ltx_document").unwrap();
    let Some(article) = doc.select(&article_sel).next() else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for child in article.child_elements() {
        walk_top(child, &mut out);
    }
    number_figures(&mut out);
    out
}

/// Assign sequential `figure_id` (one per `Block::Figure`) and `kitty_id`
/// (one per image) in document order.  The walk emits placeholder zeros
/// because the free-function walkers don't thread counter state through;
/// numbering in a single post-pass keeps every walk signature untouched
/// and matches the Pandoc path's document-order numbering.
fn number_figures(blocks: &mut [Block]) {
    let mut fig = 0u32;
    let mut kitty = 0u32;
    for b in blocks.iter_mut() {
        if let Block::Figure { rows, figure_id, .. } = b {
            fig += 1;
            *figure_id = fig;
            for row in rows.iter_mut() {
                for item in row.iter_mut() {
                    kitty += 1;
                    item.kitty_id = kitty;
                }
            }
        }
    }
}

fn walk_top(el: ElementRef, out: &mut Vec<Block>) {
    let classes: Vec<&str> = el.value().classes().collect();
    let tag = el.value().name();

    if has_class(&classes, "ltx_title_document") {
        out.push(Block::Header {
            level: 1,
            text: collect_text(el),
            number: None,
        });
        out.push(Block::Blank);
    } else if has_class(&classes, "ltx_authors") {
        emit_authors(el, out);
    } else if has_class(&classes, "ltx_abstract") {
        emit_abstract(el, out);
    } else if has_class(&classes, "ltx_section") {
        walk_section(el, 1, out);
    } else if has_class(&classes, "ltx_bibliography") {
        walk_bibliography(el, out);
    } else if has_class(&classes, "ltx_appendix") {
        // Appendices follow the same shape as sections; LaTeXML labels the
        // outer container `ltx_appendix` but the inner `ltx_title_section`
        // still drives the heading level.
        walk_section(el, 1, out);
    } else if tag == "div" && has_class(&classes, "ltx_para") {
        emit_para(el, out);
    }
    // Anything else — page-level color-attribution divs, scripts, etc. —
    // is silently skipped.  Better to omit than mis-render.
}

fn walk_section(el: ElementRef, level: u8, out: &mut Vec<Block>) {
    // Each `<section id="...">` opens with an Anchor so the reader can resolve
    // `\ref{...}` jumps.  LaTeXML uses the LaTeX label as the section id, or
    // a synthetic `Sn`/`S1.SS1` when no label was given.
    if let Some(id) = el.value().attr("id") {
        out.push(Block::Anchor(id.to_string()));
    }
    for child in el.child_elements() {
        let classes: Vec<&str> = child.value().classes().collect();
        let tag = child.value().name();

        if has_class(&classes, "ltx_title_section")
            || has_class(&classes, "ltx_title_subsection")
            || has_class(&classes, "ltx_title_subsubsection")
            || has_class(&classes, "ltx_title_paragraph")
            || has_class(&classes, "ltx_title_appendix")
            || has_class(&classes, "ltx_title_bibliography")
        {
            let lvl = heading_level(&classes, level);
            out.push(Block::Header {
                level: lvl,
                text: collect_heading_text(child),
                number: extract_heading_number(child),
            });
            out.push(Block::Blank);
        } else if has_class(&classes, "ltx_subsection") {
            walk_section(child, level + 1, out);
        } else if has_class(&classes, "ltx_subsubsection")
            || has_class(&classes, "ltx_paragraph")
        {
            walk_section(child, level + 2, out);
        } else if has_class(&classes, "ltx_appendix") {
            walk_section(child, level, out);
        } else if tag == "div" && has_class(&classes, "ltx_para") {
            emit_para(child, out);
        } else if has_class(&classes, "ltx_table") {
            emit_table(child, out);
        } else if has_class(&classes, "ltx_figure") {
            emit_figure(child, out);
        } else if has_class(&classes, "ltx_equation")
            || has_class(&classes, "ltx_equationgroup")
        {
            emit_display_math(child, out);
        } else if is_list_el(child) {
            emit_list(child, 0, out);
        } else if tag == "div" || tag == "li" {
            // Fall through into any unrecognised structural container so
            // we don't silently drop content (e.g. LaTeXML wrapping a
            // paragraph in an unnumbered div inside an appendix).
            walk_section(child, level, out);
        }
    }
}

fn emit_authors(el: ElementRef, out: &mut Vec<Block>) {
    // Walk each personname as inline content so `inline_spans_inner` strips
    // LaTeXML noise (ltx_ERROR `\AND`, ltx_note footnote markers).  Then
    // condense the residual whitespace and any leading `&` (the literal
    // author-separator LaTeXML preserves from `\And`).
    let person_sel = Selector::parse(".ltx_personname").unwrap();
    for person in el.select(&person_sel) {
        let spans = inline_spans_from(person);
        let raw: String = spans.into_iter().map(|s| s.text).collect();
        let cleaned: String = raw
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .trim_start_matches('&')
            .trim()
            .to_string();
        if !cleaned.is_empty() {
            out.push(Block::StyledLine(vec![InlineSpan::italic(cleaned)]));
        }
    }
    out.push(Block::Blank);
}

fn emit_abstract(el: ElementRef, out: &mut Vec<Block>) {
    out.push(Block::Header {
        level: 2,
        text: "Abstract".into(),
        number: None,
    });
    out.push(Block::Blank);
    for child in el.child_elements() {
        let classes: Vec<&str> = child.value().classes().collect();
        let tag = child.value().name();
        if tag == "div" && has_class(&classes, "ltx_para") {
            emit_para(child, out);
        } else if tag == "p" {
            // Some ar5iv abstracts use a bare `<p class="ltx_p">`.
            let spans = inline_spans_from(child);
            if !spans.is_empty() {
                out.push(Block::StyledLine(spans));
                out.push(Block::Blank);
            }
        }
    }
}

/// An ar5iv `<div class="ltx_para">` may hold any mix of prose
/// (`<p class="ltx_p">`) and display equations (`<table class="ltx_equation*">`).
/// We walk the para's element children in source order, emitting prose
/// blocks and `Block::DisplayMath` interleaved.
fn emit_para(el: ElementRef, out: &mut Vec<Block>) {
    let mut emitted_any = false;
    for child in el.child_elements() {
        let classes: Vec<&str> = child.value().classes().collect();
        let tag = child.value().name();
        if tag == "p" && has_class(&classes, "ltx_p") {
            emit_prose_p(child, out);
            emitted_any = true;
        } else if has_class(&classes, "ltx_equation")
            || has_class(&classes, "ltx_equationgroup")
        {
            emit_display_math(child, out);
            emitted_any = true;
        } else if has_class(&classes, "ltx_table") {
            emit_table(child, out);
            emitted_any = true;
        } else if has_class(&classes, "ltx_figure") {
            emit_figure(child, out);
            emitted_any = true;
        } else if is_list_el(child) {
            emit_list(child, 0, out);
            emitted_any = true;
        } else if (tag == "div" && has_class(&classes, "ltx_para")) || tag == "li" {
            // Nested structural content inside a paragraph wrapper —
            // delegate to the section walker so sub-paragraphs and the
            // equations buried inside them get the right treatment
            // (cf. GPT-3 paper, Appendix A.1 enumerate-with-equation).
            walk_section(child, 1, out);
            emitted_any = true;
        }
    }
    // Some ar5iv paragraphs carry inline content directly on the div (no
    // wrapping `<p>`); fall back to prose treatment of the div itself.
    if !emitted_any {
        emit_prose_p(el, out);
    }
    out.push(Block::Blank);
}

/// Emit one prose paragraph: inline children walked, inline `\(…\)` math
/// rendered into the span text, inline display math (rare here — usually
/// hoisted to its own table) flushed as a `DisplayMath` block.
fn emit_prose_p(p: ElementRef, out: &mut Vec<Block>) {
    let mut spans: Vec<InlineSpan> = Vec::new();
    for node in p.children() {
        match node.value() {
            Node::Text(t) => push_text(&mut spans, t.trim_matches('\n')),
            Node::Element(elem) => {
                let child_ref = ElementRef::wrap(node).unwrap();
                let classes: Vec<&str> = elem.classes().collect();
                if elem.name() == "math" && has_class(&classes, "ltx_Math") {
                    let display = elem.attr("display").is_some_and(|s| s == "block");
                    let latex = elem.attr("alttext").unwrap_or_default();
                    if display {
                        flush_styled(&mut spans, out);
                        push_display_math(latex, out);
                    } else {
                        spans.push(InlineSpan::plain(math_render::render_inline(latex)));
                    }
                } else {
                    spans.extend(inline_spans_from(child_ref));
                }
            }
            _ => {}
        }
    }
    flush_styled(&mut spans, out);
}

/// Build inline spans from a non-paragraph element (`<span>`, `<a>`, etc.),
/// inheriting any style hints from class names.
fn inline_spans_from(el: ElementRef) -> Vec<InlineSpan> {
    let mut out = Vec::new();
    inline_spans_inner(el, Style::default(), &mut out);
    out
}

#[derive(Clone, Copy, Default)]
struct Style {
    bold: bool,
    italic: bool,
    monospace: bool,
}

fn inline_spans_inner(el: ElementRef, mut style: Style, out: &mut Vec<InlineSpan>) {
    let classes: Vec<&str> = el.value().classes().collect();
    let tag = el.value().name();

    // LaTeXML inserts ltx_ERROR spans for undefined macros (`\AND`, custom
    // commands the engine couldn't expand).  Skipping their text avoids
    // bleeding raw command names into the rendered output.
    if has_class(&classes, "ltx_ERROR") {
        return;
    }
    // Footnote markers carry both the "1" superscript and a hidden body
    // copy of the footnote text.  Surfacing either ruins author lines and
    // body prose alike — drop them for the prototype.
    if has_class(&classes, "ltx_note") || has_class(&classes, "ltx_note_mark") {
        return;
    }
    // The math `<annotation>` element is the raw-LaTeX twin of the rendered
    // MathML; iterating both would double-print.  We already extract LaTeX
    // from the `<math alttext=…>` attribute above, so skip the annotation.
    if tag == "annotation" {
        return;
    }
    // Convert <br> within inline content to a space so author lines join.
    if tag == "br" {
        if !out.last().is_some_and(|s| s.text.ends_with(' ')) {
            out.push(InlineSpan::plain(" "));
        }
        return;
    }

    if has_class(&classes, "ltx_font_italic") || tag == "em" || tag == "i" {
        style.italic = true;
    }
    if has_class(&classes, "ltx_font_bold") || tag == "strong" || tag == "b" {
        style.bold = true;
    }
    if has_class(&classes, "ltx_font_typewriter") || tag == "code" {
        style.monospace = true;
    }

    // Citations and cross-references hand back interactive spans even when
    // their inner text is short — that's what makes them clickable.
    if tag == "a"
        && let Some(span) = link_span(el, style) {
            out.push(span);
            return;
        }
    if has_class(&classes, "ltx_cite") {
        for child in el.child_elements() {
            if child.value().name() == "a"
                && let Some(span) = link_span(child, style) {
                    out.push(span);
                }
        }
        return;
    }
    if tag == "math" && has_class(&classes, "ltx_Math") {
        let latex = el.value().attr("alttext").unwrap_or_default();
        out.push(styled(math_render::render_inline(latex), style));
        return;
    }

    for child in el.children() {
        match child.value() {
            Node::Text(t) => {
                let text: String = t.to_string();
                if !text.is_empty() {
                    out.push(styled(text, style));
                }
            }
            Node::Element(_) => {
                let child_ref = ElementRef::wrap(child).unwrap();
                inline_spans_inner(child_ref, style, out);
            }
            _ => {}
        }
    }
}

fn link_span(a: ElementRef, style: Style) -> Option<InlineSpan> {
    let href = a.value().attr("href").unwrap_or_default();
    let text = collect_text(a);
    if text.trim().is_empty() {
        return None;
    }
    if let Some(target) = href.strip_prefix('#') {
        // Heuristic: bibliography anchors look like `bib.bibX` in ar5iv;
        // everything else is an internal cross-reference (section, eqn, fig).
        if target.starts_with("bib.") || a.value().has_class("ltx_cite", scraper::CaseSensitivity::AsciiCaseInsensitive) {
            return Some(InlineSpan {
                link_target: Some(LinkTarget::Citation(target.to_string())),
                ..styled(text.trim(), style)
            });
        }
        return Some(InlineSpan {
            link_target: Some(LinkTarget::Internal(target.to_string())),
            ..styled(text.trim(), style)
        });
    }
    // External URL.
    Some(InlineSpan {
        url: Some(href.to_string()),
        ..styled(text.trim(), style)
    })
}

fn styled(text: impl Into<String>, style: Style) -> InlineSpan {
    InlineSpan {
        bold: style.bold,
        italic: style.italic,
        monospace: style.monospace,
        ..InlineSpan::plain(text)
    }
}

fn push_text(spans: &mut Vec<InlineSpan>, text: &str) {
    if !text.is_empty() {
        spans.push(InlineSpan::plain(text));
    }
}

fn flush_styled(spans: &mut Vec<InlineSpan>, out: &mut Vec<Block>) {
    if !spans.is_empty() {
        out.push(Block::StyledLine(std::mem::take(spans)));
    }
}

fn push_display_math(latex: &str, out: &mut Vec<Block>) {
    let rendered = math_render::render(math_render::MathInput::Latex(latex.trim()));
    let lines: Vec<String> = rendered.lines().map(|s| s.to_string()).collect();
    out.push(Block::DisplayMath { lines, num: None });
    out.push(Block::Blank);
}

fn emit_display_math(el: ElementRef, out: &mut Vec<Block>) {
    // Equation containers come in two shapes:
    //   - `<table class="ltx_equation">`      — a single equation
    //   - `<table class="ltx_equationgroup">` — `align`/`eqnarray` with one
    //                                            row per aligned line
    //
    // For a single equation, emit one `Block::DisplayMath`.  For a group,
    // emit one block per row — each row's `<math alttext=…>` reflects the
    // joined LaTeX for that line, which is what we want.  Equation cells
    // sometimes also carry an equation-number `<td class="ltx_eqn_num">`;
    // we capture it as `num` for parity with pandoc_parse.
    let row_sel = Selector::parse("tr.ltx_equation").unwrap();
    let math_sel = Selector::parse("math.ltx_Math").unwrap();
    let num_sel = Selector::parse("td.ltx_eqn_num").unwrap();
    let mut rows: Vec<ElementRef> = el.select(&row_sel).collect();
    if rows.is_empty() {
        rows.push(el);
    }
    for row in rows {
        // Concatenate alttext from every `<math>` in this row so a split
        // `α = β` (left/right of `=`) becomes one line, not two.
        let latex: String = row
            .select(&math_sel)
            .filter_map(|m| m.value().attr("alttext"))
            .collect::<Vec<_>>()
            .join(" ");
        let num: Option<usize> = row
            .select(&num_sel)
            .next()
            .and_then(|n| {
                collect_text(n)
                    .trim_matches(|c: char| !c.is_ascii_digit())
                    .parse::<usize>()
                    .ok()
            });
        if latex.trim().is_empty() {
            continue;
        }
        let rendered = math_render::render(math_render::MathInput::Latex(latex.trim()));
        let lines: Vec<String> = rendered.lines().map(|s| s.to_string()).collect();
        out.push(Block::DisplayMath { lines, num });
    }
    out.push(Block::Blank);
}

/// Column alignment from a LaTeXML cell's `ltx_align_*` class, or `None`
/// when the cell carries no explicit alignment (caller defaults to Left).
fn ar5iv_cell_alignment(cell: ElementRef) -> Option<Alignment> {
    let classes: Vec<&str> = cell.value().classes().collect();
    if classes.contains(&"ltx_align_right") {
        Some(Alignment::Right)
    } else if classes.contains(&"ltx_align_center") {
        Some(Alignment::Center)
    } else if classes.contains(&"ltx_align_left") {
        Some(Alignment::Left)
    } else {
        None
    }
}

fn emit_table(el: ElementRef, out: &mut Vec<Block>) {
    // Caption first (table convention: caption above the table).  Emit it the
    // same shape the Pandoc path does — `Block::Line("[Table N: …]")` — so
    // `placement::identify_groups` captures it into the table group; without
    // this it was a bare bold line stranded at the parse site when the table
    // was lifted to its PDF-anchored position.  The LaTeXML figcaption
    // already carries the "Table N: " tag, so wrapping it in brackets yields
    // the same "[Table N: …]" text.  `inline_spans_from` renders caption math
    // once (vs `collect_text`, which would triple the MathML forms).
    if let Some(cap) = el
        .child_elements()
        .find(|c| c.value().name() == "figcaption")
    {
        let text: String = inline_spans_from(cap).into_iter().map(|s| s.text).collect();
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if !text.is_empty() {
            out.push(Block::Line(format!("[{text}]")));
        }
    }

    let table_sel = Selector::parse("table.ltx_tabular").unwrap();
    let Some(table) = el.select(&table_sel).next() else {
        return;
    };

    // Per-row capture.  LaTeXML encodes a `tabular`'s rules as cell border
    // classes, not separate rule rows: `\toprule`/`\midrule`/`\bottomrule`
    // become `ltx_border_t{t}` / `ltx_border_b{b}` (horizontal) on the
    // adjacent cells, and a column spec `|` becomes `ltx_border_l`/`_r`
    // (vertical).  We recover both so the renderer can draw a real
    // booktabs-style table instead of a bare grid.
    struct RowInfo {
        cells: Vec<(String, usize)>,
        top_border: bool,
        bottom_border: bool,
    }
    let mut row_infos: Vec<RowInfo> = Vec::new();
    // Per-raw-column alignment, captured from single-span DATA cells — first
    // explicit `ltx_align_*` class per column wins.  Header cells (tagged
    // `ltx_th`) are skipped because LaTeXML centres them regardless of the
    // column spec, which would mislabel the column's true alignment.
    let mut alignments: Vec<Alignment> = Vec::new();
    // Raw vertical-rule positions (before blank-column collapse); the
    // renderer maps them through `translate_rules_to_active`.  `p` means a
    // `│` immediately before raw column `p` (so `0` = left edge, `ncols` =
    // right edge).
    let mut vrules: Vec<usize> = Vec::new();
    // Per-raw-column count of how many further rows are still covered by a
    // `rowspan` cell from above.  LaTeXML uses `rowspan` for multi-line
    // column headers and `\multirow` (e.g. Table 2's "Model", Table 3's
    // single-line headers); without honouring it the cells in the row below
    // a spanning header slide left into its column and the sub-labels
    // misalign.
    let mut row_cover: Vec<u32> = Vec::new();
    let row_sel = Selector::parse("tr").unwrap();
    let cell_sel = Selector::parse("td, th").unwrap();
    for tr in table.select(&row_sel) {
        let mut cells: Vec<(String, usize)> = Vec::new();
        let mut top_border = false;
        let mut bottom_border = false;
        let mut col = 0usize;
        for cell in tr.select(&cell_sel) {
            // Step over columns a rowspan from a previous row still covers,
            // inserting one empty placeholder so this cell lands in its true
            // column.
            let skip_start = col;
            while col < row_cover.len() && row_cover[col] > 0 {
                col += 1;
            }
            if col > skip_start {
                cells.push((String::new(), col - skip_start));
            }
            let span = cell
                .value()
                .attr("colspan")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(1);
            let rowspan = cell
                .value()
                .attr("rowspan")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(1);
            let classes: Vec<&str> = cell.value().classes().collect();
            // Horizontal rules: a top/bottom border on any cell puts a rule
            // above/below this whole row.
            if classes.contains(&"ltx_border_t") || classes.contains(&"ltx_border_tt") {
                top_border = true;
            }
            if classes.contains(&"ltx_border_b") || classes.contains(&"ltx_border_bb") {
                bottom_border = true;
            }
            // Vertical rules: a left border sits before this cell's first
            // column; a right border after its last spanned column.
            if classes.contains(&"ltx_border_l") {
                vrules.push(col);
            }
            if classes.contains(&"ltx_border_r") {
                vrules.push(col + span);
            }
            if span == 1 && !classes.contains(&"ltx_th") {
                if col >= alignments.len() {
                    alignments.resize(col + 1, Alignment::Left);
                }
                if alignments[col] == Alignment::Left
                    && let Some(a) = ar5iv_cell_alignment(cell)
                {
                    alignments[col] = a;
                }
            }
            // Walk the cell as inline content so `<math>` nodes contribute
            // their rendered form once (instead of bleeding both the
            // presentation MathML text and the LaTeX annotation).
            let text: String = inline_spans_from(cell)
                .into_iter()
                .map(|s| s.text)
                .collect();
            cells.push((text.trim().to_string(), span));
            // Record rowspan coverage for the columns this cell occupies; the
            // full count is stored, then aged by one at the end of every row
            // (including this one), so a `rowspan="2"` covers exactly the one
            // row below.
            if rowspan > 1 {
                if row_cover.len() < col + span {
                    row_cover.resize(col + span, 0);
                }
                for c in row_cover[col..col + span].iter_mut() {
                    *c = rowspan;
                }
            }
            col += span;
        }
        // Age the rowspan coverage by one row.
        for c in row_cover.iter_mut() {
            *c = c.saturating_sub(1);
        }
        if !cells.is_empty() {
            row_infos.push(RowInfo {
                cells,
                top_border,
                bottom_border,
            });
        }
    }
    if row_infos.is_empty() {
        return;
    }
    vrules.sort_unstable();
    vrules.dedup();

    // Split the rows into Matrix segments wherever a horizontal rule falls
    // between two rows — this row's bottom border OR the next row's top
    // border (LaTeXML often sets both for one `\midrule`) — emitting a
    // `Block::Rule` at each split, plus a trailing rule after the last row
    // when it carries a bottom border (`\bottomrule`).  The renderer always
    // draws the top rule itself, so row 0's top border (`\toprule`) needs no
    // Block here.
    let n = row_infos.len();
    let mut seg_start = 0usize;
    for i in 0..n {
        let rule_after = if i + 1 < n {
            row_infos[i].bottom_border || row_infos[i + 1].top_border
        } else {
            row_infos[i].bottom_border
        };
        if rule_after {
            let rows: Vec<Vec<(String, usize)>> = row_infos[seg_start..=i]
                .iter()
                .map(|r| r.cells.clone())
                .collect();
            out.push(Block::Matrix {
                rows,
                vertical_rules: vrules.clone(),
                alignments: alignments.clone(),
            });
            out.push(Block::Rule);
            seg_start = i + 1;
        }
    }
    // Rows after the final rule (a table with no `\bottomrule`) still need a
    // Matrix so they render.
    if seg_start < n {
        let rows: Vec<Vec<(String, usize)>> = row_infos[seg_start..]
            .iter()
            .map(|r| r.cells.clone())
            .collect();
        out.push(Block::Matrix {
            rows,
            vertical_rules: vrules,
            alignments,
        });
    }
    out.push(Block::Blank);
}

/// Convert one `<img class="ltx_graphics">` into an `ImageItem` (tarball-
/// relative path; `kitty_id` filled in later by `number_figures`).  `None`
/// when the img carries no `src`.
fn ar5iv_image_item(img: ElementRef) -> Option<ImageItem> {
    img.value().attr("src").map(|src| ImageItem {
        path: std::path::PathBuf::from(strip_ar5iv_asset_prefix(src)),
        kitty_id: 0,
        dims: None,
    })
}

/// A figure's recovered panel layout: the 2D image grid plus any
/// column-label header rows (from a labelled subfigure table).
struct FigureGrid {
    rows: Vec<Vec<ImageItem>>,
    header_rows: Vec<Vec<HeaderCell>>,
}

/// Recover a figure's 2D image grid (`rows[stack][side-by-side]`) — and any
/// column-label header rows — from the layout shapes LaTeXML emits for
/// multi-panel figures, falling back to a single side-by-side row otherwise:
///
/// 1. **Flexbox** (`<div class="ltx_flex_figure">`): panels are
///    `ltx_flex_cell`s split into stack rows by `ltx_flex_break` divs —
///    LaTeXML's rendering of the source `\\` row break.  Attention's
///    Figures 4/5 use this.  We walk in document order, accumulating images
///    into the current row and breaking on each separator.  (No headers.)
/// 2. **Table grid** (`<table>` with `<tr>`/`<td>`): image-bearing `<tr>`s
///    are stack rows; text-only `<tr>`s are column-label header rows
///    (e.g. Ava-256's "250 / 500 / 1K …" over a grid of renders).  See
///    [`figure_table_grid`].
/// 3. **Otherwise**: every image in one side-by-side row (single-image
///    figures, side-by-side subfigures with no break).
fn figure_image_rows(el: ElementRef) -> FigureGrid {
    let img_sel = Selector::parse("img.ltx_graphics").unwrap();

    // (1) Flexbox layout — split on `ltx_flex_break`.
    let break_sel = Selector::parse("div.ltx_flex_break").unwrap();
    if el.select(&break_sel).next().is_some() {
        let mut rows: Vec<Vec<ImageItem>> = vec![Vec::new()];
        for node in el.descendants() {
            let Some(e) = ElementRef::wrap(node) else {
                continue;
            };
            let classes: Vec<&str> = e.value().classes().collect();
            if e.value().name() == "img" && classes.contains(&"ltx_graphics") {
                if let Some(item) = ar5iv_image_item(e) {
                    rows.last_mut().unwrap().push(item);
                }
            } else if classes.contains(&"ltx_flex_break") && !rows.last().unwrap().is_empty() {
                rows.push(Vec::new());
            }
        }
        rows.retain(|r| !r.is_empty());
        if !rows.is_empty() {
            return FigureGrid {
                rows,
                header_rows: Vec::new(),
            };
        }
    }

    // (2) Table grid (with optional column labels).
    if let Some(grid) = figure_table_grid(el, &img_sel) {
        return grid;
    }

    // (3) Flat: a single side-by-side row of all images (empty when none).
    let flat: Vec<ImageItem> = el.select(&img_sel).filter_map(ar5iv_image_item).collect();
    FigureGrid {
        rows: if flat.is_empty() { Vec::new() } else { vec![flat] },
        header_rows: Vec::new(),
    }
}

/// Read a figure's `<table>` layout into image rows + column-label header
/// rows, or `None` when there's no usable grid (the caller then flattens).
///
/// A `<tr>` with images is a panel row; a text-only `<tr>` is a header row.
/// Leading label columns — cells before the first image, e.g. an empty
/// corner or a row-label column — are trimmed from the headers so each
/// label aligns with the image column beneath it (the preview maps header
/// cell *i* straight onto image column *i*).  Mirrors the Pandoc path's
/// `walk_table_rows_for_images`, the same shape `Block::Figure.header_rows`
/// expects.
fn figure_table_grid(el: ElementRef, img_sel: &Selector) -> Option<FigureGrid> {
    let row_sel = Selector::parse("tr").unwrap();
    let cell_sel = Selector::parse("td, th").unwrap();

    let mut rows: Vec<Vec<ImageItem>> = Vec::new();
    // Text rows held as `(raw_col, col_span, text)` until we know where the
    // images start and can trim the leading label columns.
    let mut text_rows: Vec<Vec<(usize, u16, String)>> = Vec::new();
    let mut lead = usize::MAX;

    for tr in el.select(&row_sel) {
        let mut col = 0usize;
        let mut imgs: Vec<ImageItem> = Vec::new();
        let mut texts: Vec<(usize, u16, String)> = Vec::new();
        for cell in tr.select(&cell_sel) {
            let span = cell
                .value()
                .attr("colspan")
                .and_then(|s| s.parse::<u16>().ok())
                .unwrap_or(1)
                .max(1);
            if let Some(item) = cell.select(img_sel).next().and_then(ar5iv_image_item) {
                if imgs.is_empty() {
                    lead = lead.min(col);
                }
                imgs.push(item);
            } else {
                let text: String = inline_spans_from(cell).into_iter().map(|s| s.text).collect();
                texts.push((col, span, text.trim().to_string()));
            }
            col += span as usize;
        }
        if !imgs.is_empty() {
            rows.push(imgs);
        } else if texts.iter().any(|(_, _, t)| !t.is_empty()) {
            text_rows.push(texts);
        }
    }

    // Only use the grid when it captures every image and is worth gridding —
    // a lone image row with no headers is identical to the flat case, so let
    // the caller produce that.
    let total = el.select(img_sel).filter_map(ar5iv_image_item).count();
    let gridded: usize = rows.iter().map(|r| r.len()).sum();
    if rows.is_empty() || gridded != total || (rows.len() < 2 && text_rows.is_empty()) {
        return None;
    }

    let lead = if lead == usize::MAX { 0 } else { lead };
    let header_rows: Vec<Vec<HeaderCell>> = text_rows
        .into_iter()
        .map(|cells| {
            cells
                .into_iter()
                .filter(|(c, _, _)| *c >= lead)
                .map(|(_, span, text)| HeaderCell {
                    text,
                    col_span: span,
                })
                .collect::<Vec<_>>()
        })
        .filter(|r: &Vec<HeaderCell>| r.iter().any(|c| !c.text.is_empty()))
        .collect();

    Some(FigureGrid { rows, header_rows })
}

/// Emit a figure as a `Block::Figure` carrying its image grid plus the
/// caption, falling back to a caption-only `StyledLine` when the figure
/// has no raster image (e.g. a TikZ/tabular-only float).
///
/// ar5iv hosts each image at a root-relative `/html/<id>/assets/xN.png`
/// URL.  We strip the `/html/<id>/` prefix so the emitted
/// `ImageItem.path` is tarball-relative (`assets/xN.png`) — the same
/// shape the Pandoc path produces — which lets the shared
/// `absolutize_image_paths` resolve it against the downloaded asset dir
/// (and read its pixel dims), and the non-kitty
/// `degrade_images_to_captions` turn it back into a `[caption]` line.
///
/// The 2D stack/side-by-side grid is recovered by [`figure_image_rows`].
/// `figure_id` / `kitty_id` are placeholders; `number_figures` assigns them
/// in document order after the walk completes.
fn emit_figure(el: ElementRef, out: &mut Vec<Block>) {
    let caption = el
        .child_elements()
        .find(|c| c.value().name() == "figcaption")
        .map(collect_text)
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());

    let FigureGrid { rows, header_rows } = figure_image_rows(el);

    if rows.is_empty() {
        // No raster image (TikZ-rendered, or LaTeXML couldn't emit one) —
        // keep the legacy caption-only line so the reader still has a
        // navigable "Figure N: …" target for `]f` / `[f`.
        if let Some(cap) = caption {
            out.push(Block::StyledLine(vec![InlineSpan::bold(cap)]));
            out.push(Block::Blank);
        }
        return;
    }

    // Anchor the figure on its LaTeXML id (the `\ref{fig:…}` target) so
    // the reader can resolve figure cross-references — parity with the
    // section anchors above and the Pandoc path.
    if let Some(id) = el.value().attr("id") {
        out.push(Block::Anchor(id.to_string()));
    }
    out.push(Block::Figure {
        rows,
        alt: caption.unwrap_or_default(),
        figure_id: 0,
        // `@{\hspace}` column-group gaps aren't reliably recoverable from
        // LaTeXML HTML; left empty (still Pandoc-only).
        column_gaps_after: Vec::new(),
        header_rows,
    });
    out.push(Block::Blank);
}

/// Convert an ar5iv image `src` to a tarball-relative asset path.  ar5iv
/// serves images at `/html/<id>/assets/x1.png`; downstream resolution
/// joins the path against the per-paper asset dir, so we drop the
/// `/html/<id>/` prefix and keep `assets/x1.png`.  Anything not matching
/// that shape passes through with only a leading slash trimmed.
fn strip_ar5iv_asset_prefix(src: &str) -> String {
    if let Some(rest) = src.strip_prefix("/html/")
        && let Some(slash) = rest.find('/')
    {
        return rest[slash + 1..].to_string();
    }
    src.trim_start_matches('/').to_string()
}

/// Walk an ar5iv list container and emit one `Block::ListItem` per
/// `<li class="ltx_item">`, recursing into nested lists at `depth + 1`.
///
/// LaTeXML hands us the rendered marker in each item's
/// `<span class="ltx_tag ltx_tag_item">` — "•" for itemize, the actual
/// "1."/"a."/… label for enumerate — so we reuse it verbatim (custom
/// enumerate labels come through for free) instead of synthesising
/// counters the way the Pandoc path does.
fn emit_list(list_el: ElementRef, depth: u8, out: &mut Vec<Block>) {
    for li in list_el.child_elements() {
        let classes: Vec<&str> = li.value().classes().collect();
        if has_class(&classes, "ltx_item") {
            emit_list_item(li, depth, out);
        }
    }
}

fn emit_list_item(li: ElementRef, depth: u8, out: &mut Vec<Block>) {
    let marker = li
        .select(&Selector::parse(".ltx_tag_item").unwrap())
        .next()
        .map(collect_text)
        .map(|t| normalize_marker(&t))
        .unwrap_or_else(|| "• ".to_string());

    // The item's own text is its first `<p class="ltx_p">` that isn't
    // buried inside a nested list — inline math / citations / styling are
    // handled by `inline_spans_from`.
    let content = first_item_paragraph(li)
        .map(inline_spans_from)
        .unwrap_or_default();
    if !content.is_empty() {
        out.push(Block::ListItem { depth, marker, content });
    }

    // LaTeXML nests sub-lists inside the item's para div (as a sibling of
    // the `<p>`); recurse into each one level deeper.
    let mut sublists = Vec::new();
    collect_immediate_sublists(li, &mut sublists);
    for sub in sublists {
        emit_list(sub, depth + 1, out);
    }
}

/// First `<p class="ltx_p">` reachable from `el` without descending into a
/// nested list or `<li>` — i.e. the item's *own* text, not a child
/// item's.  Document order alone isn't enough when a parent item has no
/// text of its own but a child does.
fn first_item_paragraph(el: ElementRef) -> Option<ElementRef> {
    for child in el.child_elements() {
        let classes: Vec<&str> = child.value().classes().collect();
        if child.value().name() == "p" && has_class(&classes, "ltx_p") {
            return Some(child);
        }
        if is_list_el(child) || child.value().name() == "li" {
            continue;
        }
        if let Some(found) = first_item_paragraph(child) {
            return Some(found);
        }
    }
    None
}

/// Collect the list containers nested directly under `el` — descending
/// through wrapper elements (e.g. `<div class="ltx_para">`) but stopping
/// at the first list on each branch (its items are handled by the
/// recursive `emit_list` call) and never crossing into a nested `<li>`.
fn collect_immediate_sublists<'a>(el: ElementRef<'a>, acc: &mut Vec<ElementRef<'a>>) {
    for child in el.child_elements() {
        if is_list_el(child) {
            acc.push(child);
        } else if child.value().name() != "li" {
            collect_immediate_sublists(child, acc);
        }
    }
}

fn is_list_el(el: ElementRef) -> bool {
    let classes: Vec<&str> = el.value().classes().collect();
    let tag = el.value().name();
    has_class(&classes, "ltx_itemize")
        || has_class(&classes, "ltx_enumerate")
        || has_class(&classes, "ltx_description")
        || tag == "ul"
        || tag == "ol"
}

/// Normalise a LaTeXML item tag into a list marker with a single trailing
/// space, matching the doc-model convention ("• ", "1. ").  Empty tags
/// (LaTeXML occasionally renders none) fall back to a bullet.
fn normalize_marker(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        "• ".to_string()
    } else {
        format!("{t} ")
    }
}

fn walk_bibliography(el: ElementRef, out: &mut Vec<Block>) {
    out.push(Block::Header {
        level: 1,
        text: "References".into(),
        number: None,
    });
    out.push(Block::Blank);
    let item_sel = Selector::parse("li.ltx_bibitem").unwrap();
    for item in el.select(&item_sel) {
        if let Some(id) = item.value().attr("id") {
            out.push(Block::Anchor(id.to_string()));
        }
        let text = collect_text(item);
        let trimmed = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if !trimmed.is_empty() {
            out.push(Block::StyledLine(vec![InlineSpan::plain(trimmed)]));
            out.push(Block::Blank);
        }
    }
}

fn collect_text(el: ElementRef) -> String {
    el.text().collect::<Vec<_>>().join("")
}

/// Like `collect_text` but strips the leading `<span class="ltx_tag">…</span>`
/// LaTeXML inserts for the numeric prefix on section headings ("1 Introduction"
/// → "Introduction").  The number is captured separately by
/// [`extract_heading_number`] and carried on `Block::Header.number`, so the
/// reader (not the parser) decides how to present it.
fn collect_heading_text(el: ElementRef) -> String {
    let full = collect_text(el);
    // Strip the leading section number (captured separately on
    // `Block::Header.number`) so the title is clean.  The old prefix-match
    // against the raw `.ltx_tag` text was whitespace-fragile and silently
    // failed on ar5iv's markup, leaving "1 Introduction" in the title; we
    // now strip the exact extracted number, then drop any "." / whitespace
    // separator that followed it.
    let stripped = extract_heading_number(el)
        .and_then(|num| full.trim_start().strip_prefix(&num).map(str::to_string));
    stripped
        .unwrap_or(full)
        .trim_start_matches(|c: char| c == '.' || c.is_whitespace())
        .trim_end()
        .to_string()
}

/// The section number LaTeXML renders in `<span class="ltx_tag">` ("2",
/// "2.1", "A").  `None` when the heading is unnumbered (no tag, or an
/// empty one).  This is the ground-truth numbering from LaTeXML, so
/// appendix letters and unnumbered front/back matter come out right.
fn extract_heading_number(el: ElementRef) -> Option<String> {
    let tag_sel = Selector::parse(".ltx_tag").unwrap();
    let tag = el.select(&tag_sel).next()?;
    let num = collect_text(tag).trim().to_string();
    if num.is_empty() { None } else { Some(num) }
}

fn has_class(classes: &[&str], target: &str) -> bool {
    classes.contains(&target)
}

fn heading_level(classes: &[&str], _section_level: u8) -> u8 {
    if has_class(classes, "ltx_title_section")
        || has_class(classes, "ltx_title_appendix")
        || has_class(classes, "ltx_title_bibliography")
    {
        1
    } else if has_class(classes, "ltx_title_subsection") {
        2
    } else if has_class(classes, "ltx_title_subsubsection") {
        3
    } else if has_class(classes, "ltx_title_paragraph") {
        4
    } else {
        2
    }
}

#[cfg(test)]
mod figure_tests {
    use super::{strip_ar5iv_asset_prefix, to_blocks};
    use doc_model::Block;

    fn doc(body: &str) -> String {
        format!(r#"<html><body><article class="ltx_document">{body}</article></body></html>"#)
    }

    fn figures(blocks: &[Block]) -> Vec<&Block> {
        blocks
            .iter()
            .filter(|b| matches!(b, Block::Figure { .. }))
            .collect()
    }

    #[test]
    fn strips_html_id_prefix() {
        assert_eq!(strip_ar5iv_asset_prefix("/html/1512.03385/assets/x1.png"), "assets/x1.png");
        assert_eq!(
            strip_ar5iv_asset_prefix("/html/1706.03762/assets/Figures/ModalNet-21.png"),
            "assets/Figures/ModalNet-21.png"
        );
        // Non-ar5iv shapes only lose a leading slash.
        assert_eq!(strip_ar5iv_asset_prefix("assets/x1.png"), "assets/x1.png");
    }

    #[test]
    fn single_image_figure_emits_block_figure() {
        let html = doc(
            r#"<section class="ltx_section" id="S1">
                 <figure id="S1.F1" class="ltx_figure">
                   <img src="/html/9/assets/x1.png" class="ltx_graphics" width="461" height="152">
                   <figcaption>Figure 1: A caption.</figcaption>
                 </figure>
               </section>"#,
        );
        let blocks = to_blocks(&html);
        let figs = figures(&blocks);
        assert_eq!(figs.len(), 1);
        let Block::Figure { rows, alt, figure_id, .. } = figs[0] else { unreachable!() };
        assert_eq!(*figure_id, 1);
        assert_eq!(alt, "Figure 1: A caption.");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), 1);
        assert_eq!(rows[0][0].path.to_str().unwrap(), "assets/x1.png");
        assert_eq!(rows[0][0].kitty_id, 1);
    }

    #[test]
    fn subfigures_flatten_into_one_row_with_sequential_ids() {
        let html = doc(
            r#"<section class="ltx_section" id="S1">
                 <figure id="S1.F1" class="ltx_figure">
                   <figure class="ltx_figure"><img src="/html/9/assets/a.png" class="ltx_graphics"></figure>
                   <figure class="ltx_figure"><img src="/html/9/assets/b.png" class="ltx_graphics"></figure>
                   <figcaption>Figure 1: Two panels.</figcaption>
                 </figure>
                 <figure id="S1.F2" class="ltx_figure">
                   <img src="/html/9/assets/c.png" class="ltx_graphics">
                   <figcaption>Figure 2: Solo.</figcaption>
                 </figure>
               </section>"#,
        );
        let blocks = to_blocks(&html);
        let figs = figures(&blocks);
        assert_eq!(figs.len(), 2);
        let Block::Figure { rows, figure_id, .. } = figs[0] else { unreachable!() };
        assert_eq!(*figure_id, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), 2);
        // kitty_ids are document-global and sequential across figures.
        assert_eq!(rows[0][0].kitty_id, 1);
        assert_eq!(rows[0][1].kitty_id, 2);
        let Block::Figure { rows, figure_id, .. } = figs[1] else { unreachable!() };
        assert_eq!(*figure_id, 2);
        assert_eq!(rows[0][0].kitty_id, 3);
    }

    /// LaTeXML's flexbox figure layout: `ltx_flex_cell` panels split into
    /// stack rows by `ltx_flex_break` (the `\\` row break).  This is how
    /// Attention's Figures 4/5 stack; emit_figure must honour the break.
    #[test]
    fn flex_break_figure_stacks_panels() {
        let html = doc(
            r#"<section class="ltx_section" id="S1">
                 <figure id="S1.F4" class="ltx_figure">
                   <div class="ltx_flex_figure">
                     <div class="ltx_flex_cell ltx_flex_size_1">
                       <img src="/html/9/assets/x2.png" class="ltx_graphics ltx_figure_panel">
                     </div>
                     <div class="ltx_flex_break"></div>
                     <div class="ltx_flex_cell ltx_flex_size_1">
                       <img src="/html/9/assets/x3.png" class="ltx_graphics ltx_figure_panel">
                     </div>
                   </div>
                   <figcaption>Figure 4: Two attention heads.</figcaption>
                 </figure>
               </section>"#,
        );
        let blocks = to_blocks(&html);
        let figs = figures(&blocks);
        let Block::Figure { rows, .. } = figs[0] else { unreachable!() };
        // Break between the two cells → two stack rows of one panel each.
        assert_eq!(rows.len(), 2, "rows: {rows:?}");
        assert_eq!(rows[0].len(), 1);
        assert_eq!(rows[1].len(), 1);
        assert_eq!(rows[0][0].path.to_str().unwrap(), "assets/x2.png");
        assert_eq!(rows[1][0].path.to_str().unwrap(), "assets/x3.png");
    }

    /// A flexbox figure with no `ltx_flex_break` (Figure 2: left/right
    /// panels) stays a single side-by-side row.
    #[test]
    fn flex_without_break_stays_side_by_side() {
        let html = doc(
            r#"<section class="ltx_section" id="S1">
                 <figure id="S1.F2" class="ltx_figure">
                   <div class="ltx_flex_figure">
                     <div class="ltx_flex_cell">
                       <img src="/html/9/assets/x4.png" class="ltx_graphics">
                     </div>
                     <div class="ltx_flex_cell">
                       <img src="/html/9/assets/x5.png" class="ltx_graphics">
                     </div>
                   </div>
                   <figcaption>Figure 2: left and right.</figcaption>
                 </figure>
               </section>"#,
        );
        let blocks = to_blocks(&html);
        let figs = figures(&blocks);
        let Block::Figure { rows, .. } = figs[0] else { unreachable!() };
        assert_eq!(rows.len(), 1, "rows: {rows:?}");
        assert_eq!(rows[0].len(), 2);
    }

    /// A labelled subfigure grid (Ava-256 style): a text-only `<tr>` of
    /// column labels above image rows, with a leading empty row-label
    /// column.  emit_figure must capture the labels as `header_rows` and
    /// trim the leading column so each label sits over its image column.
    #[test]
    fn labeled_subfigure_grid_recovers_header_row() {
        let html = doc(
            r#"<section class="ltx_section" id="S1">
                 <figure id="S1.F5" class="ltx_figure">
                   <table class="ltx_tabular">
                     <tr class="ltx_tr">
                       <td class="ltx_td"></td>
                       <td class="ltx_td ltx_align_center">250</td>
                       <td class="ltx_td ltx_align_center">500</td>
                     </tr>
                     <tr class="ltx_tr">
                       <td class="ltx_td"></td>
                       <td class="ltx_td"><img src="/html/9/assets/a.png" class="ltx_graphics"></td>
                       <td class="ltx_td"><img src="/html/9/assets/b.png" class="ltx_graphics"></td>
                     </tr>
                     <tr class="ltx_tr">
                       <td class="ltx_td"></td>
                       <td class="ltx_td"><img src="/html/9/assets/c.png" class="ltx_graphics"></td>
                       <td class="ltx_td"><img src="/html/9/assets/d.png" class="ltx_graphics"></td>
                     </tr>
                   </table>
                   <figcaption>Figure 5: Training data scaling.</figcaption>
                 </figure>
               </section>"#,
        );
        let blocks = to_blocks(&html);
        let figs = figures(&blocks);
        let Block::Figure { rows, header_rows, .. } = figs[0] else { unreachable!() };
        assert_eq!(rows.len(), 2, "rows: {rows:?}");
        assert_eq!(rows[0].len(), 2);
        assert_eq!(header_rows.len(), 1, "headers: {header_rows:?}");
        let labels: Vec<&str> = header_rows[0].iter().map(|c| c.text.as_str()).collect();
        assert_eq!(labels, vec!["250", "500"]);
        assert!(header_rows[0].iter().all(|c| c.col_span == 1));
    }

    /// LaTeXML lays a multi-panel figure out as a `<table>` grid: each `<tr>`
    /// is a stack row, each image-bearing cell a side-by-side sibling.
    /// emit_figure must recover that 2D structure instead of flattening it.
    #[test]
    fn table_grid_figure_recovers_stack_rows() {
        let html = doc(
            r#"<section class="ltx_section" id="S1">
                 <figure id="S1.F1" class="ltx_figure">
                   <table class="ltx_tabular">
                     <tr class="ltx_tr">
                       <td class="ltx_td"><img src="/html/9/assets/a.png" class="ltx_graphics"></td>
                       <td class="ltx_td"><img src="/html/9/assets/b.png" class="ltx_graphics"></td>
                     </tr>
                     <tr class="ltx_tr">
                       <td class="ltx_td"><img src="/html/9/assets/c.png" class="ltx_graphics"></td>
                       <td class="ltx_td"><img src="/html/9/assets/d.png" class="ltx_graphics"></td>
                     </tr>
                   </table>
                   <figcaption>Figure 1: A 2x2 grid.</figcaption>
                 </figure>
               </section>"#,
        );
        let blocks = to_blocks(&html);
        let figs = figures(&blocks);
        assert_eq!(figs.len(), 1);
        let Block::Figure { rows, .. } = figs[0] else { unreachable!() };
        // Two stack rows of two side-by-side panels each.
        assert_eq!(rows.len(), 2, "rows: {rows:?}");
        assert_eq!(rows[0].len(), 2);
        assert_eq!(rows[1].len(), 2);
        // Document-order paths and kitty ids across the grid.
        let paths: Vec<&str> = rows
            .iter()
            .flatten()
            .map(|i| i.path.to_str().unwrap())
            .collect();
        assert_eq!(paths, vec!["assets/a.png", "assets/b.png", "assets/c.png", "assets/d.png"]);
        assert_eq!(rows[1][1].kitty_id, 4);
    }

    fn list_items(blocks: &[Block]) -> Vec<(&u8, &str, String)> {
        blocks
            .iter()
            .filter_map(|b| match b {
                Block::ListItem { depth, marker, content } => Some((
                    depth,
                    marker.as_str(),
                    content.iter().map(|s| s.text.as_str()).collect::<String>(),
                )),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn itemize_emits_bullet_list_items() {
        let html = doc(
            r#"<section class="ltx_section" id="S1">
                 <ul class="ltx_itemize">
                   <li class="ltx_item"><span class="ltx_tag ltx_tag_item">•</span>
                     <div class="ltx_para"><p class="ltx_p">First point.</p></div></li>
                   <li class="ltx_item"><span class="ltx_tag ltx_tag_item">•</span>
                     <div class="ltx_para"><p class="ltx_p">Second point.</p></div></li>
                 </ul>
               </section>"#,
        );
        let blocks = to_blocks(&html);
        let items = list_items(&blocks);
        assert_eq!(items.len(), 2);
        assert_eq!((*items[0].0, items[0].1, items[0].2.as_str()), (0, "• ", "First point."));
        assert_eq!((*items[1].0, items[1].1, items[1].2.as_str()), (0, "• ", "Second point."));
    }

    #[test]
    fn enumerate_preserves_latexml_numeric_markers() {
        let html = doc(
            r#"<section class="ltx_section" id="S1">
                 <ol class="ltx_enumerate">
                   <li class="ltx_item"><span class="ltx_tag ltx_tag_item">1.</span>
                     <div class="ltx_para"><p class="ltx_p">One.</p></div></li>
                   <li class="ltx_item"><span class="ltx_tag ltx_tag_item">2.</span>
                     <div class="ltx_para"><p class="ltx_p">Two.</p></div></li>
                 </ol>
               </section>"#,
        );
        let blocks = to_blocks(&html);
        let items = list_items(&blocks);
        assert_eq!(items.iter().map(|i| i.1).collect::<Vec<_>>(), vec!["1. ", "2. "]);
    }

    #[test]
    fn nested_list_recurses_at_increasing_depth() {
        // Mirrors the LaTeXML shape: the sub-list is a sibling of the
        // parent item's `<p>`, inside the same `ltx_para` div.
        let html = doc(
            r#"<section class="ltx_section" id="S1">
                 <ol class="ltx_enumerate">
                   <li class="ltx_item"><span class="ltx_tag ltx_tag_item">1.</span>
                     <div class="ltx_para">
                       <p class="ltx_p">Parent.</p>
                       <ol class="ltx_enumerate">
                         <li class="ltx_item"><span class="ltx_tag ltx_tag_item">(a)</span>
                           <div class="ltx_para"><p class="ltx_p">Child.</p></div></li>
                       </ol>
                     </div></li>
                 </ol>
               </section>"#,
        );
        let blocks = to_blocks(&html);
        let items = list_items(&blocks);
        assert_eq!(items.len(), 2);
        assert_eq!((*items[0].0, items[0].1, items[0].2.as_str()), (0, "1. ", "Parent."));
        assert_eq!((*items[1].0, items[1].1, items[1].2.as_str()), (1, "(a) ", "Child."));
    }

    #[test]
    fn imageless_figure_falls_back_to_caption_line() {
        let html = doc(
            r#"<section class="ltx_section" id="S1">
                 <figure id="S1.F1" class="ltx_figure">
                   <figcaption>Figure 1: TikZ-only float.</figcaption>
                 </figure>
               </section>"#,
        );
        let blocks = to_blocks(&html);
        assert!(figures(&blocks).is_empty(), "no raster image → no Block::Figure");
        assert!(
            blocks.iter().any(|b| matches!(b, Block::StyledLine(spans)
                if spans.iter().any(|s| s.text.contains("TikZ-only float")))),
            "caption should survive as a StyledLine"
        );
    }
}

#[cfg(test)]
mod table_tests {
    use super::to_blocks;
    use doc_model::Block;


    fn doc(body: &str) -> String {
        format!(r#"<html><body><article class="ltx_document">{body}</article></body></html>"#)
    }

    /// LaTeXML encodes booktabs rules as cell border classes:
    /// `\toprule`/`\midrule`/`\bottomrule` → `ltx_border_tt`/`_t`/`_bb`, and a
    /// `|` column spec → `ltx_border_r`.  emit_table must recover these as a
    /// `Matrix + Rule + Matrix + Rule` sequence (the renderer draws the top
    /// rule itself) with `vertical_rules` populated — not the old single
    /// rule-less Matrix.
    #[test]
    fn booktabs_table_recovers_mid_bottom_and_vertical_rules() {
        let html = doc(
            r#"<section class="ltx_section" id="S1">
                 <figure class="ltx_table" id="S1.T1">
                   <table class="ltx_tabular">
                     <thead class="ltx_thead">
                       <tr>
                         <td class="ltx_td ltx_th ltx_th_column ltx_border_tt ltx_border_r">H1</td>
                         <td class="ltx_td ltx_th ltx_th_column ltx_border_tt">H2</td>
                       </tr>
                     </thead>
                     <tbody class="ltx_tbody">
                       <tr>
                         <td class="ltx_td ltx_align_left ltx_border_t ltx_border_r">a</td>
                         <td class="ltx_td ltx_align_right ltx_border_t">1</td>
                       </tr>
                       <tr>
                         <td class="ltx_td ltx_align_left ltx_border_bb ltx_border_r">b</td>
                         <td class="ltx_td ltx_align_right ltx_border_bb">2</td>
                       </tr>
                     </tbody>
                   </table>
                 </figure>
               </section>"#,
        );
        let blocks = to_blocks(&html);

        // Sequence (ignoring the trailing Blank): Matrix(header) + Rule +
        // Matrix(data) + Rule.
        let kinds: Vec<&'static str> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Matrix { .. } => Some("M"),
                Block::Rule => Some("R"),
                _ => None,
            })
            .collect();
        assert_eq!(kinds, vec!["M", "R", "M", "R"], "blocks: {blocks:?}");

        let matrices: Vec<&Block> = blocks
            .iter()
            .filter(|b| matches!(b, Block::Matrix { .. }))
            .collect();
        let Block::Matrix { rows: hrows, vertical_rules, alignments } = matrices[0] else {
            unreachable!()
        };
        // Header segment holds the single header row; vertical rule sits after
        // raw column 0 (the `ltx_border_r` on the first column).
        assert_eq!(hrows.len(), 1);
        assert_eq!(vertical_rules, &vec![1]);
        // Data-cell alignment is recovered (col 1 is right-aligned); header
        // cells are skipped so they don't mislabel it.
        assert_eq!(alignments.get(1).copied(), Some(doc_model::Alignment::Right));

        let Block::Matrix { rows: drows, .. } = matrices[1] else { unreachable!() };
        assert_eq!(drows.len(), 2, "two data rows in the body segment");
    }

    /// A table whose cells carry no border classes (no booktabs rules) must
    /// still render: one Matrix, no interior/bottom Rule blocks.  The renderer
    /// supplies the top rule regardless.
    #[test]
    fn borderless_table_emits_single_matrix_no_rules() {
        let html = doc(
            r#"<section class="ltx_section" id="S1">
                 <figure class="ltx_table" id="S1.T1">
                   <table class="ltx_tabular">
                     <tbody class="ltx_tbody">
                       <tr><td class="ltx_td">a</td><td class="ltx_td">b</td></tr>
                       <tr><td class="ltx_td">c</td><td class="ltx_td">d</td></tr>
                     </tbody>
                   </table>
                 </figure>
               </section>"#,
        );
        let blocks = to_blocks(&html);
        let matrices = blocks.iter().filter(|b| matches!(b, Block::Matrix { .. })).count();
        let rules = blocks.iter().filter(|b| matches!(b, Block::Rule)).count();
        assert_eq!((matrices, rules), (1, 0), "blocks: {blocks:?}");
    }

    /// The table caption must be a `Block::Line("[Table N: …]")` immediately
    /// before the Matrix — that exact shape is what `placement::identify_groups`
    /// captures into the table group, so the caption travels with the table
    /// when it's lifted to its PDF-anchored position.  A bare bold
    /// `StyledLine` (the old form) was left stranded at the parse site and
    /// vanished from above the moved table.
    #[test]
    fn table_caption_emits_as_placement_capturable_line() {
        let html = doc(
            r#"<section class="ltx_section" id="S1">
                 <figure class="ltx_table" id="S4.T1">
                   <figcaption class="ltx_caption">
                     <span class="ltx_tag ltx_tag_table">Table 1: </span>Maximum path lengths.
                   </figcaption>
                   <table class="ltx_tabular">
                     <tbody class="ltx_tbody">
                       <tr><td class="ltx_td">a</td><td class="ltx_td">b</td></tr>
                     </tbody>
                   </table>
                 </figure>
               </section>"#,
        );
        let blocks = to_blocks(&html);
        // The caption is a Block::Line that placement keys on (`[Table`), and
        // it sits immediately before the first Matrix.
        let cap_idx = blocks
            .iter()
            .position(|b| matches!(b, Block::Line(s) if s.starts_with("[Table")))
            .expect("a [Table …] caption line");
        let Block::Line(text) = &blocks[cap_idx] else { unreachable!() };
        assert_eq!(text, "[Table 1: Maximum path lengths.]");
        assert!(
            matches!(blocks.get(cap_idx + 1), Some(Block::Matrix { .. })),
            "caption must sit directly before the Matrix: {blocks:?}"
        );
    }

    /// A `rowspan` header cell (e.g. Table 2's "Model") covers its column in
    /// the row below, so the second header row must be offset by an empty
    /// placeholder — otherwise its sub-labels slide left under the wrong
    /// columns.
    #[test]
    fn rowspan_header_offsets_the_row_below() {
        let html = doc(
            r#"<section class="ltx_section" id="S1">
                 <figure class="ltx_table" id="S1.T1">
                   <table class="ltx_tabular">
                     <thead class="ltx_thead">
                       <tr>
                         <th rowspan="2" class="ltx_td ltx_th ltx_border_tt">Model</th>
                         <td colspan="2" class="ltx_td ltx_th ltx_border_tt">BLEU</td>
                       </tr>
                       <tr>
                         <td class="ltx_td ltx_th">EN-DE</td>
                         <td class="ltx_td ltx_th">EN-FR</td>
                       </tr>
                     </thead>
                     <tbody class="ltx_tbody">
                       <tr>
                         <td class="ltx_td ltx_border_t ltx_border_bb">ByteNet</td>
                         <td class="ltx_td ltx_border_t ltx_border_bb">23.7</td>
                         <td class="ltx_td ltx_border_t ltx_border_bb">39.2</td>
                       </tr>
                     </tbody>
                   </table>
                 </figure>
               </section>"#,
        );
        let blocks = to_blocks(&html);
        let first_matrix = blocks
            .iter()
            .find(|b| matches!(b, Block::Matrix { .. }))
            .expect("a header Matrix");
        let Block::Matrix { rows, .. } = first_matrix else { unreachable!() };
        // Both header rows live in the first segment (no rule between them).
        assert_eq!(rows.len(), 2, "rows: {rows:?}");
        // Row 0: Model (1 col) + BLEU (spans 2).
        assert_eq!(rows[0], vec![("Model".to_string(), 1), ("BLEU".to_string(), 2)]);
        // Row 1: an empty placeholder under "Model", then the two sub-labels —
        // so EN-DE/EN-FR sit under BLEU, not under Model.
        assert_eq!(
            rows[1],
            vec![
                (String::new(), 1),
                ("EN-DE".to_string(), 1),
                ("EN-FR".to_string(), 1),
            ],
            "second header row must be offset past the rowspan column",
        );
    }
}
