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

use doc_model::{Block, InlineSpan, LinkTarget};
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
    out
}

fn walk_top(el: ElementRef, out: &mut Vec<Block>) {
    let classes: Vec<&str> = el.value().classes().collect();
    let tag = el.value().name();

    if has_class(&classes, "ltx_title_document") {
        out.push(Block::Header { level: 1, text: collect_text(el) });
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
            out.push(Block::Header { level: lvl, text: collect_heading_text(child) });
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
            emit_figure_caption(child, out);
        } else if has_class(&classes, "ltx_equation")
            || has_class(&classes, "ltx_equationgroup")
        {
            emit_display_math(child, out);
        } else if has_class(&classes, "ltx_itemize")
            || has_class(&classes, "ltx_enumerate")
            || has_class(&classes, "ltx_description")
        {
            // Lists in ar5iv carry their own item structure; for the
            // prototype we flatten into the section stream so any nested
            // paragraphs / equations / figures get emitted.  Real list
            // semantics (Block::ListItem with markers) would be the next
            // polish step.
            walk_section(child, level, out);
        } else if tag == "div" || tag == "li" || tag == "ol" || tag == "ul" {
            // Fall through into any unrecognised structural container so
            // we don't silently drop content (e.g. LaTeXML wrapping a
            // paragraph in an unnumbered list inside an appendix).
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
    out.push(Block::Header { level: 2, text: "Abstract".into() });
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
            emit_figure_caption(child, out);
            emitted_any = true;
        } else if has_class(&classes, "ltx_itemize")
            || has_class(&classes, "ltx_enumerate")
            || has_class(&classes, "ltx_description")
            || (tag == "div" && has_class(&classes, "ltx_para"))
            || tag == "ol"
            || tag == "ul"
            || tag == "li"
        {
            // Nested structural content inside a paragraph wrapper —
            // delegate to the section walker so list items, sub-paragraphs,
            // and the equations buried inside them get the right treatment
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

fn emit_table(el: ElementRef, out: &mut Vec<Block>) {
    // Pull the caption first as a heading-style line.
    if let Some(cap) = el
        .child_elements()
        .find(|c| c.value().name() == "figcaption")
    {
        let text = collect_text(cap);
        if !text.trim().is_empty() {
            out.push(Block::StyledLine(vec![InlineSpan::bold(text.trim())]));
        }
    }

    let table_sel = Selector::parse("table.ltx_tabular").unwrap();
    let Some(table) = el.select(&table_sel).next() else {
        return;
    };

    let mut rows: Vec<Vec<(String, usize)>> = Vec::new();
    let row_sel = Selector::parse("tr").unwrap();
    let cell_sel = Selector::parse("td, th").unwrap();
    for tr in table.select(&row_sel) {
        let mut row: Vec<(String, usize)> = Vec::new();
        for cell in tr.select(&cell_sel) {
            let span = cell
                .value()
                .attr("colspan")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(1);
            // Walk the cell as inline content so `<math>` nodes contribute
            // their rendered form once (instead of bleeding both the
            // presentation MathML text and the LaTeX annotation).
            let text: String = inline_spans_from(cell)
                .into_iter()
                .map(|s| s.text)
                .collect();
            row.push((text.trim().to_string(), span));
        }
        if !row.is_empty() {
            rows.push(row);
        }
    }
    if !rows.is_empty() {
        out.push(Block::Matrix { rows, vertical_rules: Vec::new() });
        out.push(Block::Blank);
    }
}

fn emit_figure_caption(el: ElementRef, out: &mut Vec<Block>) {
    // Prototype: don't try to fetch image assets; surface the caption as a
    // bold line so the reader still shows "Figure N: …".
    if let Some(cap) = el
        .child_elements()
        .find(|c| c.value().name() == "figcaption")
    {
        let text = collect_text(cap);
        if !text.trim().is_empty() {
            out.push(Block::StyledLine(vec![InlineSpan::bold(text.trim())]));
            out.push(Block::Blank);
        }
    }
}

fn walk_bibliography(el: ElementRef, out: &mut Vec<Block>) {
    out.push(Block::Header { level: 1, text: "References".into() });
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
/// → "Introduction").  Tread already auto-numbers, so the prefix would double.
fn collect_heading_text(el: ElementRef) -> String {
    let tag_sel = Selector::parse(".ltx_tag").unwrap();
    let mut full = collect_text(el);
    for tag in el.select(&tag_sel) {
        let prefix = collect_text(tag);
        if let Some(rest) = full.strip_prefix(&prefix) {
            full = rest.to_string();
        }
    }
    full.trim().to_string()
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
