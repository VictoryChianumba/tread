//! LaTeX `tabular` column-spec parser.
//!
//! Recovers two pieces of information from a `\begin{tabular}{spec}`
//! header that Pandoc discards:
//! - which raw column positions carry vertical rules (`|`)
//! - per-column horizontal-rule positions inside the table body
//!
//! Output is consumed by:
//! - `pandoc_parse::table::parse_table` to attach vertical-rule info
//!   to `Block::Matrix`
//! - `pandoc_parse::figure::*` for figure-tabular header layout
//!
//! Pure byte-level scanning; no Pandoc dependency.

use super::{match_brace, match_delim, parse_three_brace_args, skip_ascii_ws};

#[cfg(test)]
use super::preprocess_latex_source;

// ── Tabular column-spec extraction (vertical & horizontal rules) ─────────────

/// Per-table layout info recovered from the LaTeX source.  Pandoc discards
/// both `|` characters (vertical rules) and mid-body `\hline`/`\specialrule`
/// directives (horizontal rules between data groups), so we extract both
/// directly from the source.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TableSpec {
    pub col_count: usize,
    /// Raw column indices BEFORE which a vertical rule appears (0 = left
    /// edge, col_count = right edge).
    pub vertical_rules: Vec<usize>,
    /// Source row indices BEFORE which a horizontal rule appears (0 = top of
    /// table, total_rows = bottom).  Top/bottom positions are deduplicated
    /// against the renderer's auto-emitted top/bottom rules; positions in
    /// between split the body into groups.
    pub horizontal_rules: Vec<usize>,
    /// Column indices AFTER which a non-empty `@{...}` separator (e.g.
    /// `\hspace{1mm}`) appears in the source spec.  Figure-rendering uses
    /// this to insert a 1-cell horizontal gap between column groups so
    /// the visual structure of a `ccc@{\hspace{1mm}}ccc` table survives
    /// from the source into the preview pane.
    pub column_gaps_after: Vec<usize>,
}

/// Walk a LaTeX source string and pull out the column spec and horizontal
/// rule positions of every `\begin{tabular}{...}` and `\begin{tabular*}{...}{...}`
/// (and the long* variants).  Specs we can't parse (e.g. those using the
/// `*{N}{...}` repeat operator) are skipped silently — the affected table
/// will render with no vertical/horizontal rules instead of risking misalignment.
pub(crate) fn extract_tabular_specs(src: &str) -> Vec<TableSpec> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        // Skip `%...\n` line comments — Pandoc ignores them, so we must too.
        // Without this, commented-out `\begin{tabular}{...}` lines (common in
        // arXiv sources) would pollute the spec queue and attach rules to
        // unrelated tables. `\%` is a literal percent — preserve it.
        if bytes[i] == b'%' && !preceded_by_odd_backslashes(bytes, i) {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // Match openers as byte slices — avoids string-slicing at arbitrary
        // positions (which would panic if `i` happened to land mid-codepoint).
        let opener = if bytes[i..].starts_with(b"\\begin{tabular*}") {
            Some((b"\\begin{tabular*}".len(), true))
        } else if bytes[i..].starts_with(b"\\begin{tabular}") {
            Some((b"\\begin{tabular}".len(), false))
        } else if bytes[i..].starts_with(b"\\begin{longtable*}") {
            Some((b"\\begin{longtable*}".len(), true))
        } else if bytes[i..].starts_with(b"\\begin{longtable}") {
            Some((b"\\begin{longtable}".len(), false))
        } else {
            None
        };
        if let Some((skip, has_width_arg)) = opener {
            let mut p = i + skip;
            p = skip_ascii_ws(bytes, p);
            if p < bytes.len() && bytes[p] == b'[' {
                if let Some(close) = match_delim(bytes, p, b'[', b']') {
                    p = skip_ascii_ws(bytes, close + 1);
                } else {
                    i += utf8_char_width(bytes[i]);
                    continue;
                }
            }
            if has_width_arg {
                if p >= bytes.len() || bytes[p] != b'{' {
                    i += utf8_char_width(bytes[i]);
                    continue;
                }
                let close = match match_brace(bytes, p) {
                    Some(c) => c,
                    None => {
                        i += utf8_char_width(bytes[i]);
                        continue;
                    }
                };
                p = skip_ascii_ws(bytes, close + 1);
            }
            if p >= bytes.len() || bytes[p] != b'{' {
                i += utf8_char_width(bytes[i]);
                continue;
            }
            let close = match match_brace(bytes, p) {
                Some(c) => c,
                None => {
                    i += utf8_char_width(bytes[i]);
                    continue;
                }
            };
            // p+1 is past `{` (ASCII), close is the position of `}` (ASCII)
            // — both are guaranteed char boundaries, so this slice is safe.
            let spec_str = &src[p + 1..close];
            if let Some(parsed) = parse_column_spec_full(spec_str) {
                let content_start = close + 1;
                let (horizontal_rules, content_end) =
                    scan_table_horizontal_rules(bytes, content_start);
                out.push(TableSpec {
                    col_count: parsed.col_count,
                    vertical_rules: parsed.vertical_rules,
                    horizontal_rules,
                    column_gaps_after: parsed.column_gaps_after,
                });
                i = content_end;
                continue;
            }
            i = close + 1;
            continue;
        }
        i += utf8_char_width(bytes[i]);
    }
    out
}

/// Walk a tabular env's content (starting just after the column spec's `}`)
/// up to its `\end{tabular...}` and return the row indices BEFORE which a
/// horizontal rule appears, plus the byte position just past the `\end`.
///
/// Recognised rule directives: `\hline`, `\midrule`, `\toprule`, `\bottomrule`,
/// and `\specialrule{...}{...}{...}`.  Skipped: `\rule{...}{...}` (a vertical
/// strut, not a rule), `\cmidrule(...){n-m}` and `\cline{n-m}` (partial rules
/// with no terminal equivalent).
fn scan_table_horizontal_rules(bytes: &[u8], start: usize) -> (Vec<usize>, usize) {
    let mut rules: Vec<usize> = Vec::new();
    let mut row_idx = 0usize;
    let mut i = start;
    while i < bytes.len() {
        // Stop at any \end{tabular...} or \end{longtable...}.
        for end_tag in [
            b"\\end{tabular}".as_slice(),
            b"\\end{tabular*}".as_slice(),
            b"\\end{longtable}".as_slice(),
            b"\\end{longtable*}".as_slice(),
        ] {
            if bytes[i..].starts_with(end_tag) {
                return (rules, i + end_tag.len());
            }
        }
        // Comments — Pandoc ignores, we must too.
        if bytes[i] == b'%' && !preceded_by_odd_backslashes(bytes, i) {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // Row terminator: `\\` followed by optional `[<dim>]` and optional `*`.
        if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
            i += 2;
            i = skip_ascii_ws(bytes, i);
            if i < bytes.len() && bytes[i] == b'[' {
                if let Some(close) = match_delim(bytes, i, b'[', b']') {
                    i = close + 1;
                }
            }
            if i < bytes.len() && bytes[i] == b'*' {
                i += 1;
            }
            row_idx += 1;
            continue;
        }
        // Rule directives.  Each requires a non-alphanumeric boundary so we
        // don't match `\hlinexxx` etc.
        let no_arg_directive = if bytes[i..].starts_with(b"\\hline") {
            Some(b"\\hline".len())
        } else if bytes[i..].starts_with(b"\\midrule") {
            Some(b"\\midrule".len())
        } else if bytes[i..].starts_with(b"\\toprule") {
            Some(b"\\toprule".len())
        } else if bytes[i..].starts_with(b"\\bottomrule") {
            Some(b"\\bottomrule".len())
        } else {
            None
        };
        if let Some(skip) = no_arg_directive {
            let after = i + skip;
            if after >= bytes.len() || !bytes[after].is_ascii_alphanumeric() {
                if rules.last() != Some(&row_idx) {
                    rules.push(row_idx);
                }
                i = after;
                continue;
            }
        }
        // \specialrule{}{}{}
        if bytes[i..].starts_with(b"\\specialrule") {
            let after = i + b"\\specialrule".len();
            if after >= bytes.len() || !bytes[after].is_ascii_alphanumeric() {
                if let Some((_, _, end)) = parse_three_brace_args(bytes, after) {
                    if rules.last() != Some(&row_idx) {
                        rules.push(row_idx);
                    }
                    i = end;
                    continue;
                }
            }
        }
        i += utf8_char_width(bytes[i]);
    }
    (rules, i)
}

/// Number of bytes in the UTF-8 codepoint that starts at this byte.  Defaults
/// to 1 for ill-formed leading bytes so the scanner makes progress instead of
/// looping forever.
pub(super) fn utf8_char_width(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b < 0xC2 {
        1
    } else if b < 0xE0 {
        2
    } else if b < 0xF0 {
        3
    } else {
        4
    }
}

/// True iff `bytes[pos]` is preceded by an odd number of consecutive
/// backslashes — meaning the character at `pos` is escaped (`\%`, `\\\%`).
/// `\\%` and `\\\\%` are NOT escaped (the `\` themselves are escaped pairs).
fn preceded_by_odd_backslashes(bytes: &[u8], pos: usize) -> bool {
    let mut count = 0usize;
    let mut p = pos;
    while p > 0 && bytes[p - 1] == b'\\' {
        count += 1;
        p -= 1;
    }
    count % 2 == 1
}

/// Parsed column spec: col_count, vertical-rule positions, and
/// column-group break positions.
///
/// `column_gaps_after` lists column indices AFTER which a non-trivial
/// `@{...}` separator (e.g. `\hspace{1mm}`) appears in the spec — these
/// are the visible inter-column gaps that LaTeX renders to break a wide
/// table into logical groups (typical for figure tabulars that arrange
/// several N-column "panels" with a small space between them).
///
/// Empty `@{}` separators are ignored (they only suppress the default
/// `\tabcolsep` between columns, not add visible gaps).
pub(crate) struct ParsedColumnSpec {
    pub col_count: usize,
    pub vertical_rules: Vec<usize>,
    pub column_gaps_after: Vec<usize>,
}

/// Parse a LaTeX `tabular` column spec.
///
/// Recognised primitives:
/// - `c`, `l`, `r`             — a column (count += 1)
/// - `p{...}`, `m{...}`, `b{...}` — a column with width arg (count += 1, skip arg)
/// - `|`                       — vertical rule before next column
/// - `>{...}`, `<{...}`, `!{...}` — column-modifier macros (skip brace group)
/// - `@{...}`                  — column separator; non-empty content
///                                between two columns marks a group break
/// - whitespace                — ignored
///
/// Returns `None` if the spec uses unsupported syntax (e.g. `*{N}{spec}` or
/// any character we don't recognise) — graceful degradation: the table will
/// render with no vertical rules and no column-group gaps.
#[allow(dead_code)]
pub(crate) fn parse_column_spec(spec: &str) -> Option<(usize, Vec<usize>)> {
    parse_column_spec_full(spec).map(|p| (p.col_count, p.vertical_rules))
}

pub(crate) fn parse_column_spec_full(spec: &str) -> Option<ParsedColumnSpec> {
    let bytes = spec.as_bytes();
    let mut col_count: usize = 0;
    let mut rules: Vec<usize> = Vec::new();
    let mut gaps: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b' ' | b'\t' | b'\n' | b'\r' => {
                i += 1;
            }
            b'|' => {
                rules.push(col_count);
                i += 1;
            }
            b'c' | b'l' | b'r' => {
                col_count += 1;
                i += 1;
            }
            b'p' | b'm' | b'b' => {
                // Width-arg column.  Must be followed (after optional ws) by `{...}`.
                let mut p = i + 1;
                p = skip_ascii_ws(bytes, p);
                if p < bytes.len() && bytes[p] == b'{' {
                    let close = match_brace(bytes, p)?;
                    col_count += 1;
                    i = close + 1;
                } else {
                    return None;
                }
            }
            b'>' | b'<' | b'!' => {
                let mut p = i + 1;
                p = skip_ascii_ws(bytes, p);
                if p < bytes.len() && bytes[p] == b'{' {
                    let close = match_brace(bytes, p)?;
                    i = close + 1;
                } else {
                    return None;
                }
            }
            b'@' => {
                // Column separator.  Non-empty content between columns
                // (after at least one column letter and before the next)
                // signals a visible group break — record it.  The
                // edge-of-spec `@{}` (before col 0 or after the last
                // col) is just a margin suppressor, not a group break.
                let mut p = i + 1;
                p = skip_ascii_ws(bytes, p);
                if p < bytes.len() && bytes[p] == b'{' {
                    let close = match_brace(bytes, p)?;
                    let body = &bytes[p + 1..close];
                    let has_content = body.iter().any(|c| !c.is_ascii_whitespace());
                    if has_content && col_count > 0 {
                        // Only record if there's at least one more column
                        // after this separator — otherwise it's the trailing
                        // edge marker (`@{}` at end of spec).
                        gaps.push(col_count);
                    }
                    i = close + 1;
                } else {
                    return None;
                }
            }
            _ => {
                return None;
            }
        }
    }
    // Filter out gaps that turned out to be trailing (no column after
    // them).  Easy check: gap position == col_count means it lands past
    // the last column.
    gaps.retain(|g| *g < col_count);
    Some(ParsedColumnSpec {
        col_count,
        vertical_rules: rules,
        column_gaps_after: gaps,
    })
}

#[cfg(test)]
mod spec_parser_tests {
    use super::{parse_column_spec, preprocess_latex_source};

    #[test]
    fn simple_no_rules() {
        assert_eq!(parse_column_spec("lccccc"), Some((6, vec![])));
    }

    #[test]
    fn two_rules() {
        // c|cccccc|ccc → 10 cols, rules before col 1 and col 7
        assert_eq!(parse_column_spec("c|cccccc|ccc"), Some((10, vec![1, 7])));
    }

    #[test]
    fn edge_rules() {
        // |l|c|r| → 3 cols, rules at 0,1,2,3
        assert_eq!(parse_column_spec("|l|c|r|"), Some((3, vec![0, 1, 2, 3])));
    }

    #[test]
    fn p_column_with_width() {
        assert_eq!(parse_column_spec("p{2cm}cc"), Some((3, vec![])));
    }

    #[test]
    fn whitespace_tolerant() {
        assert_eq!(parse_column_spec("c | c | c"), Some((3, vec![1, 2])));
    }

    #[test]
    fn star_repeat_unsupported() {
        // *{3}{c} repeat operator → bail out, return None
        assert!(parse_column_spec("*{3}{c}").is_none());
    }

    #[test]
    fn modifier_macro_skipped() {
        // >{\bfseries}c >{\itshape}c → 2 cols, no rules
        assert_eq!(
            parse_column_spec(">{\\bfseries}c>{\\itshape}c"),
            Some((2, vec![]))
        );
    }

    #[test]
    fn column_group_gaps_recovered() {
        // The Fig 3 tabular: `@{}` margin-killers at the ends, plus
        // `@{\hspace{1mm}}` between groups → gaps after cols 3, 6, 8.
        // The trailing `@{}` should NOT be recorded (no column follows).
        let p = super::parse_column_spec_full(
            "@{}ccc@{\\hspace{1mm}}ccc@{\\hspace{1mm}}cc@{\\hspace{1mm}}c@{}",
        )
        .unwrap();
        assert_eq!(p.col_count, 9);
        assert_eq!(p.column_gaps_after, vec![3, 6, 8]);
    }

    #[test]
    fn empty_at_separators_make_no_gap() {
        // `@{}` (empty) between cols is just \tabcolsep suppression,
        // not a visible gap — must not be recorded.
        let p = super::parse_column_spec_full("c@{}c@{}c").unwrap();
        assert_eq!(p.col_count, 3);
        assert!(p.column_gaps_after.is_empty());
    }

    /// Regression: extract_tabular_specs must not panic on sources containing
    /// multi-byte UTF-8 characters (e.g. "ä" in `\documentclass` comments).
    /// The previous byte-only `i += 1` advance landed mid-codepoint and the
    /// `let rest = &src[i..]` slice at the top of the loop panicked.
    #[test]
    fn utf8_safe_scanning() {
        let src = "% ä é ö\n\\begin{tabular}{c|c}\nfoo\n\\end{tabular}\n% ß";
        let specs = super::extract_tabular_specs(src);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].col_count, 2);
        assert_eq!(specs[0].vertical_rules, vec![1]);
    }

    /// Regression: commented-out `\begin{tabular}` must not produce a spec.
    /// In the Attention paper, `background.tex:40` has `%\begin{tabular}{l|c|c|c}`
    /// which falsely matched Table 1's column count and attached spurious
    /// vertical rules.
    #[test]
    fn line_comments_skipped() {
        let src = "%\\begin{tabular}{l|c|c|c}\n\\begin{tabular}{lc}\nfoo\n\\end{tabular}";
        let specs = super::extract_tabular_specs(src);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].col_count, 2);
        assert_eq!(specs[0].vertical_rules, Vec::<usize>::new());
    }

    /// `\%` is a LITERAL percent — must not trigger comment skipping.
    #[test]
    fn escaped_percent_not_a_comment() {
        let src = "100\\% \\begin{tabular}{cc}\nfoo\n\\end{tabular}";
        let specs = super::extract_tabular_specs(src);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].col_count, 2);
    }

    #[test]
    fn preprocess_unwraps_resizebox_body() {
        let src = "\\resizebox{\\linewidth}{!}{\\begin{tabular}{cc}a & b\\\\c & d\\end{tabular}}";
        let out = preprocess_latex_source(src);
        assert!(out.contains("\\begin{tabular}{cc}a & b\\\\c & d\\end{tabular}"));
        assert!(!out.contains("\\resizebox"));
    }

    #[test]
    fn preprocess_unwraps_starred_resizebox_body() {
        let src = "\\resizebox*{0.9\\linewidth}{!}{\\includegraphics{foo}}";
        let out = preprocess_latex_source(src);
        assert_eq!(out, "\\includegraphics{foo}");
    }

    #[test]
    fn preprocess_unwraps_adjustbox_body() {
        let src = "\\adjustbox{width=\\linewidth}{\\begin{tabular}{cc}a & b\\end{tabular}}";
        let out = preprocess_latex_source(src);
        assert_eq!(out, "\\begin{tabular}{cc}a & b\\end{tabular}");
    }

    #[test]
    fn preprocess_unwraps_scalebox_body() {
        let src = "\\scalebox{0.9}{\\includegraphics{foo}}";
        let out = preprocess_latex_source(src);
        assert_eq!(out, "\\includegraphics{foo}");
    }

    /// Horizontal rule scanning: \hline between data rows produces an entry
    /// in horizontal_rules at the row index immediately AFTER the row break.
    #[test]
    fn horizontal_rules_between_rows() {
        let src = "\\begin{tabular}{cc}\n\
                   \\toprule\n\
                   a & b \\\\\n\
                   \\midrule\n\
                   1 & 2 \\\\\n\
                   3 & 4 \\\\\n\
                   \\hline\n\
                   5 & 6 \\\\\n\
                   \\bottomrule\n\
                   \\end{tabular}";
        let specs = super::extract_tabular_specs(src);
        assert_eq!(specs.len(), 1);
        // Rules at: row 0 (toprule), row 1 (midrule between header and data),
        // row 3 (hline before "5 & 6"), row 4 (bottomrule after last row).
        assert_eq!(specs[0].horizontal_rules, vec![0, 1, 3, 4]);
    }

    /// `\rule{0pt}{2.0ex}` (vertical strut) must NOT be counted as a rule.
    #[test]
    fn rule_strut_not_counted() {
        let src = "\\begin{tabular}{cc}\\hline\\rule{0pt}{2.0ex}\na & b \\\\\\end{tabular}";
        let specs = super::extract_tabular_specs(src);
        assert_eq!(specs.len(), 1);
        // Only the \hline counts; the \rule strut is silently passed over.
        assert_eq!(specs[0].horizontal_rules, vec![0]);
    }

    /// Smoke test against a snippet representative of the Attention paper's
    /// background.tex, which has a commented-out 4-col tabular followed by no
    /// real tabular.  The commented-out one previously poisoned the queue.
    #[test]
    fn attention_background_no_phantom_specs() {
        let src = r"
% Some text
%\begin{tabular}{l|c|c|c}
% commented out content
\hline
%\end{tabular}
% More text
";
        assert!(super::extract_tabular_specs(src).is_empty());
    }

    #[test]
    fn match_brace_with_unicode_escape() {
        // `\é` — backslash followed by a 2-byte char should not panic.
        // Brace structure: `{ ä é hello }` → close at the trailing `}`.
        let src = "{ ä é hello }";
        let bytes = src.as_bytes();
        let close = super::match_brace(bytes, 0).expect("must find closing brace");
        assert_eq!(bytes[close], b'}');
        assert_eq!(close, bytes.len() - 1);
    }
}

