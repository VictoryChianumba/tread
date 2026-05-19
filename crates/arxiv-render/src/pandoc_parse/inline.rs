//! Inline-span walkers.
//!
//! Convert Pandoc inline AST into `Vec<InlineSpan>` (for styled prose
//! blocks) or `String` (for plain-text contexts like captions and
//! table cells).  Also hosts `synthesize_bibliography`, which builds
//! the References section from the thread-local bibitem list when
//! Pandoc's own bibliography Div didn't fire.

use doc_model::{Block, InlineSpan, LinkTarget};
use serde_json::Value;

use super::{
    BIBITEMS_ORDERED, CITE_NUMBERS, render_inline_math, render_math,
};

pub(super) fn synthesize_bibliography() -> Vec<Block> {
    BIBITEMS_ORDERED.with(|b| {
        let bibs = b.borrow();
        let mut out: Vec<Block> = Vec::new();
        if bibs.is_empty() {
            return out;
        }
        for (i, (key, entry)) in bibs.iter().enumerate() {
            let n = i + 1;
            // Anchor first so reader's label_lines maps `ref-<key>` to
            // the entry's first VL.
            out.push(Block::Anchor(format!("ref-{key}")));
            // Render as `[N]  <entry text>`.  StyledLine wraps to width.
            let line_text = format!("[{n}]  {entry}");
            out.push(Block::StyledLine(vec![InlineSpan::plain(line_text)]));
            out.push(Block::Blank);
        }
        out
    })
}

/// LaTeX prefix words that typically introduce a `\ref{...}` and should
/// share its link styling — so "Table 3", "Figure 4", "Section 6.1"
/// render as cohesive linked phrases instead of "(prose) (linked-number)".
/// Case-insensitive match; trailing dot on abbreviations matched.
const REF_PREFIX_WORDS: &[&str] = &[
    "table",
    "tab.",
    "tab",
    "figure",
    "fig.",
    "fig",
    "section",
    "sec.",
    "sec",
    "equation",
    "eq.",
    "eq",
    "algorithm",
    "alg.",
    "alg",
    "theorem",
    "thm.",
    "thm",
    "lemma",
    "lem.",
    "chapter",
    "chap.",
    "appendix",
    "app.",
    "definition",
    "def.",
    "proposition",
    "prop.",
    "corollary",
    "cor.",
];

/// Walk backward through `out` from the tail.  If the most recent prose
/// is whitespace followed by a span whose stripped text exactly matches
/// a `REF_PREFIX_WORDS` entry (case-insensitive), set `link_target` on
/// both that span and the whitespace span so a phrase like "Table 3"
/// gets uniform link styling.
fn extend_link_back_to_prefix(out: &mut [InlineSpan], target: &LinkTarget) {
    if out.is_empty() {
        return;
    }
    let mut idx = out.len();
    // Skip an optional trailing whitespace span.
    if idx > 0 && out[idx - 1].text.trim().is_empty() {
        idx -= 1;
    }
    if idx == 0 {
        return;
    }
    let candidate = &out[idx - 1];
    let trimmed = candidate.text.trim().to_ascii_lowercase();
    if !REF_PREFIX_WORDS.contains(&trimmed.as_str()) {
        return;
    }
    // Both the candidate and any whitespace span between it and the
    // forthcoming link get the link target.
    if out[idx - 1].link_target.is_none() {
        out[idx - 1].link_target = Some(target.clone());
    }
    if idx < out.len() && out[idx].link_target.is_none() {
        out[idx].link_target = Some(target.clone());
    }
}

pub(super) fn walk_inlines_text(inlines: &[Value]) -> String {
    walk_inlines_spans(inlines)
        .into_iter()
        .map(|s| s.text)
        .collect()
}

pub(super) fn walk_inlines_spans(inlines: &[Value]) -> Vec<InlineSpan> {
    let mut out = Vec::new();

    for node in inlines {
        let t = node["t"].as_str().unwrap_or("");
        let c = &node["c"];

        match t {
            "Str" => {
                if let Some(s) = c.as_str() {
                    // U+00A0 (non-breaking space from LaTeX ~) → regular space.
                    // Do NOT trim — trailing spaces from ~ before \ref{} are load-bearing;
                    // trimming them causes adjacent tokens to merge ("Figure1", "consumemodel").
                    let s = s.replace('\u{00A0}', " ");
                    if !s.is_empty() {
                        out.push(InlineSpan::plain(&s));
                    }
                }
            }

            "Space" | "SoftBreak" => out.push(InlineSpan::plain(" ")),
            "LineBreak" => out.push(InlineSpan::plain("\n")),

            "Emph" => {
                if let Some(inner) = c.as_array() {
                    for mut span in walk_inlines_spans(inner) {
                        span.italic = true;
                        out.push(span);
                    }
                }
            }

            "Strong" => {
                if let Some(inner) = c.as_array() {
                    for mut span in walk_inlines_spans(inner) {
                        span.bold = true;
                        out.push(span);
                    }
                }
            }

            "Underline" => {
                if let Some(inner) = c.as_array() {
                    for mut span in walk_inlines_spans(inner) {
                        span.underline = true;
                        out.push(span);
                    }
                }
            }

            "Strikeout" => {
                if let Some(inner) = c.as_array() {
                    for mut span in walk_inlines_spans(inner) {
                        span.strikethrough = true;
                        out.push(span);
                    }
                }
            }

            "SmallCaps" => {
                if let Some(inner) = c.as_array() {
                    out.extend(walk_inlines_spans(inner));
                }
            }

            // Wrap super/subscript in brackets so "d_k" renders as "d[k]" not "dk".
            "Superscript" => {
                if let Some(inner) = c.as_array() {
                    out.push(InlineSpan::plain("^("));
                    out.extend(walk_inlines_spans(inner));
                    out.push(InlineSpan::plain(")"));
                }
            }

            "Subscript" => {
                if let Some(inner) = c.as_array() {
                    out.push(InlineSpan::plain("_("));
                    out.extend(walk_inlines_spans(inner));
                    out.push(InlineSpan::plain(")"));
                }
            }

            "Code" => {
                // c = [attr, text]
                if let Some(text) = c[1].as_str() {
                    out.push(InlineSpan {
                        monospace: true,
                        ..InlineSpan::plain(text)
                    });
                }
            }

            "Math" => {
                let kind = c[0]["t"].as_str().unwrap_or("InlineMath");
                let latex = c[1].as_str().unwrap_or("");
                if kind == "DisplayMath" {
                    // Normally caught in para_to_block; guard the in-line fall-through.
                    let rendered = render_math(latex);
                    out.push(InlineSpan::plain(format!("  {rendered}  ")));
                } else {
                    // Inline math: use strip_latex path so the result is always a
                    // single compact token — tui_math's vertical output breaks wrapping.
                    out.push(InlineSpan::plain(render_inline_math(latex)));
                }
            }

            "Link" => {
                // c = [attr, inlines, [url, title]]
                // External URLs surface as OSC 8 hyperlinks via `span.url`.
                // Internal `#anchor` refs become `LinkTarget::Internal(anchor)`
                // for jump-to-line by `Enter` in the reader.
                let raw_url = c[2][0].as_str().unwrap_or("");
                let url = if raw_url.starts_with("http://") || raw_url.starts_with("https://") {
                    Some(raw_url.to_string())
                } else {
                    None
                };
                let internal: Option<LinkTarget> = raw_url
                    .strip_prefix('#')
                    .filter(|a| !a.is_empty())
                    .map(|a| LinkTarget::Internal(a.to_string()));

                // For internal refs, papers typically write "Table~\ref{X}",
                // and Pandoc emits ["Table", " ", Link("3")].  Without
                // back-propagation, only "3" is styled — "Table 3" looks
                // half-linked.  Walk backward through whitespace and into
                // the previous prose span; if it ends with a known ref
                // prefix word, extend the link styling to cover it.
                if let Some(target) = &internal {
                    extend_link_back_to_prefix(&mut out, target);
                }

                if let Some(inner) = c[1].as_array() {
                    for mut span in walk_inlines_spans(inner) {
                        if span.url.is_none() {
                            span.url = url.clone();
                        }
                        if span.link_target.is_none() {
                            span.link_target = internal.clone();
                        }
                        out.push(span);
                    }
                }
            }

            "Quoted" => {
                // c = [quote_type, inlines]
                let (open, close) = match c[0]["t"].as_str() {
                    Some("DoubleQuote") => ("\u{201C}", "\u{201D}"),
                    _ => ("\u{2018}", "\u{2019}"),
                };
                out.push(InlineSpan::plain(open));
                if let Some(inner) = c[1].as_array() {
                    out.extend(walk_inlines_spans(inner));
                }
                out.push(InlineSpan::plain(close));
            }

            "Cite" => {
                // c = [citations, fallback_inlines]
                // Render as numeric `[N]` (or `[N, M, ...]` for multi-cite)
                // using the bibitem source-order map established in
                // `try_pandoc`.  If a key isn't in the map (cite to a
                // missing bibitem), we fall back to the cite-key in
                // brackets so the citation is still visible.
                let keys: Vec<String> = c[0]
                    .as_array()
                    .map(|cs| {
                        cs.iter()
                            .filter_map(|cit| cit["citationId"].as_str().map(|s| s.to_string()))
                            .filter(|s| !s.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                if keys.is_empty() {
                    if let Some(inner) = c[1].as_array() {
                        out.extend(walk_inlines_spans(inner));
                    }
                } else {
                    let primary = keys[0].clone();
                    let rendered = CITE_NUMBERS.with(|cn| {
                        let map = cn.borrow();
                        let parts: Vec<String> = keys
                            .iter()
                            .map(|k| match map.get(k) {
                                Some(n) => n.to_string(),
                                None => k.clone(),
                            })
                            .collect();
                        format!("[{}]", parts.join(", "))
                    });
                    out.push(InlineSpan::citation(rendered, primary));
                }
            }

            "Span" => {
                // c = [attr, inlines]  where attr = [id, [classes], [kvs]]
                // Skip footnote markers — they leak superscript letters/numbers into prose.
                let is_footnote = c[0][1].as_array().map_or(false, |cls| {
                    cls.iter().any(|cl| {
                        matches!(
                            cl.as_str(),
                            Some("footnote-ref") | Some("footnote-mark") | Some("footnote")
                        )
                    })
                });
                if !is_footnote {
                    if let Some(inner) = c[1].as_array() {
                        out.extend(walk_inlines_spans(inner));
                    }
                }
            }

            "Image" => {
                // c = [attr, alt_inlines, [url, title]]
                if let Some(alt) = c[1].as_array() {
                    let text = walk_inlines_text(alt);
                    if !text.is_empty() {
                        out.push(InlineSpan::plain(format!("[Image: {text}]")));
                    }
                }
            }

            // RawInline, Note — skip
            _ => {}
        }
    }

    // Collapse consecutive all-space spans into one, avoiding double-space gaps
    // that arise when "Figure\u{00A0}" + Space node both produce " ".
    dedup_spaces(out)
}

fn dedup_spaces(spans: Vec<InlineSpan>) -> Vec<InlineSpan> {
    let mut out = Vec::with_capacity(spans.len());
    let mut prev_is_space = false;
    for span in spans {
        let is_space = span.text.chars().all(|c| c == ' ');
        if is_space && prev_is_space {
            continue;
        }
        prev_is_space = is_space;
        out.push(span);
    }
    out
}

