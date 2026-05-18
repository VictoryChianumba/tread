//! Minimal BibTeX `.bib` entry scanner.
//!
//! arXiv tarballs that ship a `.bib` instead of a precompiled `.bbl`
//! (2605.04035 is one such) yielded zero bibitems before this module:
//! the existing scanner only knows about `\bibitem{key}` macros, which
//! `.bib` files don't contain.  This parser walks `@<type>{key, …}`
//! entries and produces the same `(key, plain_text)` shape as
//! `clean_bibitem_text` so downstream consumers (citation popup,
//! `:references`) don't have to care which source the data came from.
//!
//! Scope: covers the BibTeX features that appear in real arXiv `.bib`
//! files.  Author / title / year / journal / booktitle / volume /
//! number / pages get pretty-printed in a stable order; everything else
//! is dropped from the popup string (still keyed correctly).  `@String`
//! macros are resolved when a bareword field value matches a known
//! macro name (`journal = TOG` → "ACM Transactions on Graphics" if
//! `@String{TOG = {ACM Transactions on Graphics}}` appears earlier in
//! the same file).  `@comment` and `@preamble` are skipped.
//!
//! Not handled (would surface if a paper needed it):
//! - Cross-references between entries (`crossref` field)
//! - Numeric-only field values without quotes/braces (e.g. `year = 1968`)
//!   — accepted as bareword; if it doesn't match a macro we keep it raw.
//! - Concatenation via `#` (e.g. `TOG # " 42"`); the parser sees the
//!   left operand and drops the rest.

use std::collections::HashMap;

/// Walk `content` and emit every `@<type>{key, …}` entry as a
/// `(key, plain_text)` pair, with `@String` macros resolved inside
/// field values.  Order is preserved (source order in the file), so a
/// caller that turns the result into a numbered bibliography sees the
/// same ordering as the .bib file itself.
pub fn extract_bibtex_entries(content: &str) -> Vec<(String, String)> {
    let bytes = content.as_bytes();
    let mut out = Vec::new();
    let mut macros: HashMap<String, String> = HashMap::new();
    let mut i = 0;

    while i < bytes.len() {
        // Scan for `@`.
        if bytes[i] != b'@' {
            i += 1;
            continue;
        }

        // Entry type: letters after `@`.
        let type_start = i + 1;
        let type_end = (type_start..bytes.len())
            .find(|&k| !bytes[k].is_ascii_alphabetic())
            .unwrap_or(bytes.len());
        if type_end == type_start {
            i += 1;
            continue;
        }
        let entry_type = content[type_start..type_end].to_ascii_lowercase();
        i = type_end;

        // Whitespace, then `{` or `(`.
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let close = match bytes.get(i).copied() {
            Some(b'{') => b'}',
            Some(b'(') => b')',
            _ => continue,
        };
        i += 1;
        let body_start = i;

        // Read body to the matching outer close character, tracking
        // brace depth (depth ignores parentheses by design — they're
        // only the outer delimiter, never nested inside field values).
        let mut depth: i32 = 1;
        while i < bytes.len() && depth > 0 {
            match bytes[i] {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                c if c == close && depth == 1 && close == b')' => depth -= 1,
                _ => {}
            }
            i += 1;
        }
        let body_end = if depth == 0 { i - 1 } else { i };
        let body = &content[body_start..body_end];

        match entry_type.as_str() {
            "comment" | "preamble" => continue,
            "string" => {
                if let Some((k, v)) = parse_string_macro(body) {
                    macros.insert(k, v);
                }
                continue;
            }
            _ => {}
        }

        // Regular entry: key, then comma-separated fields.
        let Some((key, fields_str)) = body.split_once(',') else {
            continue;
        };
        let key = key.trim().to_string();
        if key.is_empty() {
            continue;
        }

        let fields = parse_fields(fields_str, &macros);
        let display = format_citation(&fields);
        if !display.is_empty() {
            out.push((key, display));
        }
    }

    out
}

/// Parse `KEY = "value"` or `KEY = {value}` inside an `@string{...}`
/// body.  BibTeX allows `@string` macros to chain via `#`, but no real
/// arXiv .bib uses that — we'd see the left operand only and skip the
/// rest, which is fine for our use case (macro is informative, not
/// load-bearing).
fn parse_string_macro(body: &str) -> Option<(String, String)> {
    let (name, value_part) = body.split_once('=')?;
    let name = name.trim().to_ascii_lowercase();
    if name.is_empty() {
        return None;
    }
    let value = read_field_value(value_part.trim_start(), &HashMap::new())?;
    Some((name, value))
}

/// Parse the field list inside a `@<type>{key, …}` body — everything
/// after the first comma.  Walks `<name> = <value>` pairs separated by
/// top-level commas, returning a name → value map (names lowercased,
/// values with surrounding `{}` / `""` stripped and `@String` macros
/// resolved).
fn parse_fields(body: &str, macros: &HashMap<String, String>) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    let bytes = body.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        // Find next field name: skip whitespace and commas.
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }

        // Field name = letters/digits until whitespace or `=`.
        let name_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
        {
            i += 1;
        }
        let name = body[name_start..i].trim().to_ascii_lowercase();
        if name.is_empty() {
            break;
        }

        // Skip whitespace + `=`.
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'=') {
            i += 1;
        }

        // Field value: `{...}`, `"..."`, or bareword (potentially a macro).
        let Some(value) = read_field_value(&body[i..], macros) else {
            break;
        };
        // Advance past the consumed value.  Re-scan the same prefix to
        // find where it ended — simpler than threading offsets through
        // read_field_value.
        let consumed = field_value_byte_length(&body[i..]);
        i += consumed;

        // Field name kept lowercased so format_citation can do case-insensitive lookup.
        fields.insert(name, value);
    }

    fields
}

/// Read a single field value at `start_of_value`, returning the
/// unwrapped string.  Handles `{nested {braces}}`, `"quoted"`, and
/// bareword values (looked up in `macros`, else used as-is).
fn read_field_value(s: &str, macros: &HashMap<String, String>) -> Option<String> {
    let bytes = s.as_bytes();
    let first = *bytes.first()?;
    match first {
        b'{' => {
            // Find matching close brace, tracking depth.
            let mut depth = 1;
            let mut i = 1;
            while i < bytes.len() && depth > 0 {
                match bytes[i] {
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    _ => {}
                }
                i += 1;
            }
            let end = if depth == 0 { i - 1 } else { i };
            Some(strip_latex(&s[1..end]))
        }
        b'"' => {
            // Quoted string; quotes don't nest in BibTeX.
            let mut i = 1;
            while i < bytes.len() && bytes[i] != b'"' {
                i += 1;
            }
            Some(strip_latex(&s[1..i]))
        }
        _ => {
            // Bareword: letters/digits/underscore, then resolve against macros.
            let mut i = 0;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
            {
                i += 1;
            }
            if i == 0 {
                return None;
            }
            let bareword = s[..i].to_ascii_lowercase();
            Some(macros.get(&bareword).cloned().unwrap_or(s[..i].to_string()))
        }
    }
}

/// Mirror of `read_field_value` that returns how many bytes the value
/// occupied (including delimiters).  Kept in lockstep so the caller in
/// `parse_fields` knows where to resume.
fn field_value_byte_length(s: &str) -> usize {
    let bytes = s.as_bytes();
    let Some(&first) = bytes.first() else {
        return 0;
    };
    match first {
        b'{' => {
            let mut depth = 1;
            let mut i = 1;
            while i < bytes.len() && depth > 0 {
                match bytes[i] {
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    _ => {}
                }
                i += 1;
            }
            i
        }
        b'"' => {
            let mut i = 1;
            while i < bytes.len() && bytes[i] != b'"' {
                i += 1;
            }
            (i + 1).min(bytes.len())
        }
        _ => {
            let mut i = 0;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
            {
                i += 1;
            }
            i
        }
    }
}

/// Strip LaTeX commands and protective braces from a BibTeX field
/// value, leaving readable plain text.  Mirrors `clean_bibitem_text`
/// (used by the .bbl path) but operates on a single field rather than
/// a multi-line bibitem body.
fn strip_latex(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                while let Some(&next) = chars.peek() {
                    if next.is_ascii_alphabetic() || next == '*' {
                        chars.next();
                    } else {
                        break;
                    }
                }
                // Skip one brace group after a command (e.g. `\emph{...}`),
                // emitting its inner text.
                if chars.peek() == Some(&'{') {
                    chars.next();
                    let mut depth = 1;
                    for inner in chars.by_ref() {
                        match inner {
                            '{' => depth += 1,
                            '}' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            _ => out.push(inner),
                        }
                    }
                }
            }
            '{' | '}' | '~' => out.push(' '),
            '\n' => out.push(' '),
            _ => out.push(c),
        }
    }
    // Collapse runs of whitespace into single spaces.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Assemble a readable one-paragraph citation from the parsed fields.
/// Format: `<authors>. (<year>). <title>. <venue>[, <volume>][, <pages>].`
/// Missing pieces are skipped quietly; if neither author nor title is
/// present we return empty so the caller can drop the entry rather than
/// shipping a misleading "(2020)." popup.
fn format_citation(fields: &HashMap<String, String>) -> String {
    let author = fields.get("author").map(|s| condense_authors(s));
    let title = fields.get("title").map(|s| s.trim().trim_end_matches('.').to_string());
    let year = fields.get("year").map(|s| s.trim().to_string());
    let venue = fields
        .get("journal")
        .or_else(|| fields.get("booktitle"))
        .or_else(|| fields.get("publisher"))
        .map(|s| s.trim().to_string());
    let volume = fields.get("volume").map(|s| s.trim().to_string());
    let pages = fields.get("pages").map(|s| s.trim().to_string());

    if author.is_none() && title.is_none() {
        return String::new();
    }

    let mut parts: Vec<String> = Vec::new();
    if let Some(a) = author {
        parts.push(a);
    }
    if let Some(y) = year {
        parts.push(format!("({y})"));
    }
    if let Some(t) = title {
        parts.push(t);
    }
    if let Some(v) = venue {
        let with_volume = match volume {
            Some(vol) if !vol.is_empty() => format!("{v} {vol}"),
            _ => v,
        };
        parts.push(with_volume);
    }
    if let Some(p) = pages {
        parts.push(format!("pp. {p}"));
    }
    parts.join(". ")
}

/// Trim a BibTeX author list ("Smith, John and Doe, Jane and Roe, Richard")
/// to the leading author followed by "et al." once we go past two names.
/// Two authors keep both ("Smith and Doe").  Surnames-only when the
/// "Last, First" form is used; otherwise the raw token is kept.
fn condense_authors(raw: &str) -> String {
    let names: Vec<&str> = raw
        .split(" and ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let surname = |full: &str| -> String {
        if let Some((last, _)) = full.split_once(',') {
            last.trim().to_string()
        } else {
            full.split_whitespace()
                .last()
                .unwrap_or(full)
                .to_string()
        }
    };
    match names.len() {
        0 => String::new(),
        1 => surname(names[0]),
        2 => format!("{} and {}", surname(names[0]), surname(names[1])),
        _ => format!("{} et al.", surname(names[0])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_simple_article() {
        let content = r#"
@article{einstein1905,
  author = {Albert Einstein},
  title  = {On the Electrodynamics of Moving Bodies},
  journal = {Annalen der Physik},
  volume = {322},
  year   = {1905},
}
"#;
        let entries = extract_bibtex_entries(content);
        assert_eq!(entries.len(), 1);
        let (key, body) = &entries[0];
        assert_eq!(key, "einstein1905");
        assert!(body.contains("Einstein"));
        assert!(body.contains("(1905)"));
        assert!(body.contains("On the Electrodynamics of Moving Bodies"));
        assert!(body.contains("Annalen der Physik 322"));
    }

    #[test]
    fn resolves_string_macros_for_bareword_values() {
        let content = r#"
@string{TOG = {ACM Transactions on Graphics}}
@article{kerbl2023,
  author = {Kerbl, Bernhard and Kopanas, Georgios and Leimk\"uhler, Thomas and Drettakis, George},
  title  = {{3D} Gaussian Splatting for Real-Time Radiance Field Rendering},
  journal = TOG,
  volume = {42},
  year   = {2023},
}
"#;
        let entries = extract_bibtex_entries(content);
        assert_eq!(entries.len(), 1);
        let (key, body) = &entries[0];
        assert_eq!(key, "kerbl2023");
        assert!(body.contains("Kerbl et al."));
        assert!(body.contains("ACM Transactions on Graphics 42"));
        assert!(!body.contains("{")); // braces stripped
    }

    #[test]
    fn skips_comment_and_preamble() {
        let content = r#"
@comment{This is a comment block; should not become an entry.}
@preamble{ "\newcommand{\noop}{}" }
@article{real, author = {A}, title = {T}}
"#;
        let entries = extract_bibtex_entries(content);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "real");
    }

    #[test]
    fn handles_quoted_and_bareword_year() {
        let content = r#"
@article{a, author = {A}, title = "T", year = "2020"}
@article{b, author = {B}, title = {T2}, year = 2021}
"#;
        let entries = extract_bibtex_entries(content);
        assert_eq!(entries.len(), 2);
        assert!(entries[0].1.contains("(2020)"));
        assert!(entries[1].1.contains("(2021)"));
    }

    #[test]
    fn drops_entries_without_author_or_title() {
        let content = "@misc{empty, year = {2020}}";
        let entries = extract_bibtex_entries(content);
        assert_eq!(entries.len(), 0);
    }

    #[test]
    fn condense_authors_picks_surnames() {
        // "Last, First" form
        assert_eq!(condense_authors("Smith, John"), "Smith");
        // Two authors keep both
        assert_eq!(
            condense_authors("Smith, John and Doe, Jane"),
            "Smith and Doe"
        );
        // Three+ collapse with et al.
        assert_eq!(
            condense_authors("Smith, John and Doe, Jane and Roe, Richard"),
            "Smith et al."
        );
        // No comma: take last whitespace-separated token
        assert_eq!(condense_authors("Albert Einstein"), "Einstein");
    }
}
