//! LaTeX-source bibitem extraction.
//!
//! Pandoc's AST surfaces bibliography paragraphs but doesn't carry
//! cite-keys down to them, so we scrape `\bibitem{key}` and `.bib`
//! entries directly from the source files we already have in memory.
//! Used to populate the reader's citation popups and to seed
//! `pandoc_parse`'s cite-numbering map.
//!
//! Lifted out of the (now removed) hand-rolled `parse.rs` walker —
//! the rest of that module was the legacy regex-based LaTeX parser,
//! superseded by `pandoc_parse` and `ar5iv_parse`.

use std::collections::HashMap;

/// Pre-scan all source files for `\bibitem{key}…` entries.  Returns a
/// `key → entry-text` map used by citation-popup lookup.
pub fn extract_bibitems(sources: &[(String, String)]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (path, content) in sources {
        if path.ends_with(".bib") {
            for (key, body) in crate::bibtex::extract_bibtex_entries(content) {
                out.insert(key, body);
            }
            continue;
        }
        for (key, cleaned) in scan_thebibliography(content) {
            out.insert(key, cleaned);
        }
    }
    out
}

/// Same scan in source order, with duplicates dropped.  Pandoc's
/// citation walker needs the original ordering to assign `[N]` indices
/// that match the bibliography it eventually renders.
pub fn extract_bibitems_ordered(sources: &[(String, String)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (path, content) in sources {
        if path.ends_with(".bib") {
            for (key, body) in crate::bibtex::extract_bibtex_entries(content) {
                if !out.iter().any(|(k, _)| k == &key) {
                    out.push((key, body));
                }
            }
            continue;
        }
        for (key, cleaned) in scan_thebibliography(content) {
            if !out.iter().any(|(k, _)| k == &key) {
                out.push((key, cleaned));
            }
        }
    }
    out
}

/// Walk a `.tex` source and yield `(cite_key, cleaned_entry_text)` for
/// every `\bibitem{…}` it contains.  Stops an entry at the next
/// `\bibitem`, the closing `\end{thebibliography}`, or EOF.
fn scan_thebibliography(content: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = content.as_bytes();
    let mut i = 0;
    while i + 8 < bytes.len() {
        if &bytes[i..i + 8] != b"\\bibitem" {
            i += 1;
            continue;
        }
        let mut j = i + 8;
        // Optional [label] argument.
        if j < bytes.len() && bytes[j] == b'[' {
            if let Some(close) = (j..bytes.len()).find(|&k| bytes[k] == b']') {
                j = close + 1;
            }
        }
        if j >= bytes.len() || bytes[j] != b'{' {
            i = j;
            continue;
        }
        let key_start = j + 1;
        let key_end = match (key_start..bytes.len()).find(|&k| bytes[k] == b'}') {
            Some(p) => p,
            None => break,
        };
        let key = match std::str::from_utf8(&bytes[key_start..key_end]) {
            Ok(s) => s.trim().to_string(),
            Err(_) => {
                i = key_end + 1;
                continue;
            }
        };
        let entry_start = key_end + 1;
        let entry_end = (entry_start..bytes.len())
            .find(|&k| {
                bytes[k..].starts_with(b"\\bibitem")
                    || bytes[k..].starts_with(b"\\end{thebibliography}")
            })
            .unwrap_or(bytes.len());
        let raw = &content[entry_start..entry_end];
        let cleaned = clean_bibitem_text(raw);
        if !key.is_empty() && !cleaned.is_empty() {
            out.push((key, cleaned));
        }
        i = entry_end;
    }
    out
}

/// Strip LaTeX commands and collapse whitespace from a bibitem entry.
fn clean_bibitem_text(raw: &str) -> String {
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
                if let Some(&'{') = chars.peek() {
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
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}
