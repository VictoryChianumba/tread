//! Pre-Pandoc LaTeX preprocessing.
//!
//! Pandoc trips over a handful of LaTeX commands that don't have JSON
//! AST equivalents — `\resizebox`, `\adjustbox`, `\scalebox` (wrap
//! tabulars without changing layout), `\multirow` (gets dropped
//! silently with information loss), and `\cmidrule[trim]{i-j}` (the
//! optional bracket arg trips the LaTeX reader).  This module rewrites
//! the source in-memory before handing it to Pandoc so the parser
//! sees only commands it understands.
//!
//! Also hosts the byte-level brace/delim primitives that the spec
//! parser and the strip-* rewriters share.

use super::spec::utf8_char_width;

// ── Pre-Pandoc LaTeX preprocessing ────────────────────────────────────────────

/// Rewrite LaTeX constructs that Pandoc mishandles before sending to the parser.
///
/// Currently handles:
/// - `\resizebox{W}{H}{BODY}` → `BODY`. Pandoc drops the contents of some
///   figure bodies when `\includegraphics` lives inside a `\resizebox{...}`
///   wrapper around `tabular` / `minipage` content, so we unwrap the visual
///   scaling macro before parsing.
/// - `\adjustbox{OPTS}{BODY}` → `BODY` and `\scalebox{X}{BODY}` → `BODY`.
///   Same family of visual wrappers as `\resizebox`; we only care about
///   preserving the inner figure content for Pandoc's AST.
/// - `\multirow{N}{W}{TEXT}` → `TEXT`. Pandoc silently drops the content when
///   the multirow declaration appears on its own line before a row, so row
///   labels like `(A)`, `(B)` vanish from the output.
/// - `\cmidrule[W](T){N-M}` → ``. Pandoc drops the command name but keeps the
///   brace content as plain text (e.g. "2-3"), which leaks into the next cell.
pub(super) fn preprocess_latex_source(src: &str) -> String {
    let after_resizebox = strip_resizebox(src);
    let after_adjustbox = strip_adjustbox(&after_resizebox);
    let after_scalebox = strip_scalebox(&after_adjustbox);
    let after_multirow = strip_multirow(&after_scalebox);
    strip_cmidrule(&after_multirow)
}

fn strip_resizebox(src: &str) -> String {
    let bytes = src.as_bytes();
    let cmd = b"\\resizebox";
    let mut out = String::with_capacity(src.len());
    let mut copy_from = 0usize;
    let mut i = 0;
    while i + cmd.len() <= bytes.len() {
        if &bytes[i..i + cmd.len()] == cmd {
            let mut after = i + cmd.len();
            if after < bytes.len() && bytes[after] == b'*' {
                after += 1;
            }
            let bnd_ok = after >= bytes.len() || !bytes[after].is_ascii_alphanumeric();
            if bnd_ok {
                if let Some((arg3_start, arg3_end, end)) = parse_three_brace_args(bytes, after) {
                    out.push_str(&src[copy_from..i]);
                    out.push_str(&src[arg3_start..arg3_end]);
                    i = end;
                    copy_from = end;
                    continue;
                }
            }
        }
        i += 1;
    }
    out.push_str(&src[copy_from..]);
    out
}

fn strip_adjustbox(src: &str) -> String {
    let bytes = src.as_bytes();
    let cmd = b"\\adjustbox";
    let mut out = String::with_capacity(src.len());
    let mut copy_from = 0usize;
    let mut i = 0;
    while i + cmd.len() <= bytes.len() {
        if &bytes[i..i + cmd.len()] == cmd {
            let after = i + cmd.len();
            let bnd_ok = after >= bytes.len() || !bytes[after].is_ascii_alphanumeric();
            if bnd_ok {
                if let Some((arg2_start, arg2_end, end)) = parse_two_brace_args(bytes, after) {
                    out.push_str(&src[copy_from..i]);
                    out.push_str(&src[arg2_start..arg2_end]);
                    i = end;
                    copy_from = end;
                    continue;
                }
            }
        }
        i += 1;
    }
    out.push_str(&src[copy_from..]);
    out
}

fn strip_scalebox(src: &str) -> String {
    let bytes = src.as_bytes();
    let cmd = b"\\scalebox";
    let mut out = String::with_capacity(src.len());
    let mut copy_from = 0usize;
    let mut i = 0;
    while i + cmd.len() <= bytes.len() {
        if &bytes[i..i + cmd.len()] == cmd {
            let after = i + cmd.len();
            let bnd_ok = after >= bytes.len() || !bytes[after].is_ascii_alphanumeric();
            if bnd_ok {
                if let Some((arg2_start, arg2_end, end)) = parse_two_brace_args(bytes, after) {
                    out.push_str(&src[copy_from..i]);
                    out.push_str(&src[arg2_start..arg2_end]);
                    i = end;
                    copy_from = end;
                    continue;
                }
            }
        }
        i += 1;
    }
    out.push_str(&src[copy_from..]);
    out
}

fn strip_multirow(src: &str) -> String {
    let bytes = src.as_bytes();
    let cmd = b"\\multirow";
    let mut out = String::with_capacity(src.len());
    let mut copy_from = 0usize;
    let mut i = 0;
    while i + cmd.len() <= bytes.len() {
        if &bytes[i..i + cmd.len()] == cmd {
            let after = i + cmd.len();
            let bnd_ok = after >= bytes.len() || !bytes[after].is_ascii_alphanumeric();
            if bnd_ok {
                if let Some((arg3_start, arg3_end, end)) = parse_three_brace_args(bytes, after) {
                    out.push_str(&src[copy_from..i]);
                    out.push_str(&src[arg3_start..arg3_end]);
                    i = end;
                    copy_from = end;
                    continue;
                }
            }
        }
        i += 1;
    }
    out.push_str(&src[copy_from..]);
    out
}

fn strip_cmidrule(src: &str) -> String {
    let bytes = src.as_bytes();
    let cmd = b"\\cmidrule";
    let mut out = String::with_capacity(src.len());
    let mut copy_from = 0usize;
    let mut i = 0;
    while i + cmd.len() <= bytes.len() {
        if &bytes[i..i + cmd.len()] == cmd {
            let after = i + cmd.len();
            let bnd_ok = after >= bytes.len() || !bytes[after].is_ascii_alphanumeric();
            if bnd_ok {
                if let Some(end) = parse_cmidrule_args(bytes, after) {
                    out.push_str(&src[copy_from..i]);
                    // Drop the command entirely — surrounding whitespace
                    // already separates it from neighbouring tokens.
                    i = end;
                    copy_from = end;
                    continue;
                }
            }
        }
        i += 1;
    }
    out.push_str(&src[copy_from..]);
    out
}

/// Parse `{...}{...}{...}` (skipping ASCII whitespace between groups) starting
/// at byte position `pos`. Returns `(arg3_content_start, arg3_content_end, end)`
/// where `end` is the byte position just past the closing brace of arg 3.
pub(super) fn parse_two_brace_args(bytes: &[u8], mut pos: usize) -> Option<(usize, usize, usize)> {
    pos = skip_ascii_ws(bytes, pos);
    let close1 = match_brace(bytes, pos)?;
    pos = skip_ascii_ws(bytes, close1 + 1);
    if pos >= bytes.len() || bytes[pos] != b'{' {
        return None;
    }
    let close2 = match_brace(bytes, pos)?;
    Some((pos + 1, close2, close2 + 1))
}

/// Parse `{...}{...}{...}` (skipping ASCII whitespace between groups) starting
/// at byte position `pos`. Returns `(arg3_content_start, arg3_content_end, end)`
/// where `end` is the byte position just past the closing brace of arg 3.
pub(super) fn parse_three_brace_args(bytes: &[u8], mut pos: usize) -> Option<(usize, usize, usize)> {
    pos = skip_ascii_ws(bytes, pos);
    let close1 = match_brace(bytes, pos)?;
    pos = skip_ascii_ws(bytes, close1 + 1);
    let close2 = match_brace(bytes, pos)?;
    pos = skip_ascii_ws(bytes, close2 + 1);
    if pos >= bytes.len() || bytes[pos] != b'{' {
        return None;
    }
    let close3 = match_brace(bytes, pos)?;
    Some((pos + 1, close3, close3 + 1))
}

/// Parse `\cmidrule` arguments: optional `[width]`, optional `(trim)`, required
/// `{N-M}`. Returns the byte position just past the final `}`.
pub(super) fn parse_cmidrule_args(bytes: &[u8], mut pos: usize) -> Option<usize> {
    pos = skip_ascii_ws(bytes, pos);
    if pos < bytes.len() && bytes[pos] == b'[' {
        pos = match_delim(bytes, pos, b'[', b']')? + 1;
        pos = skip_ascii_ws(bytes, pos);
    }
    if pos < bytes.len() && bytes[pos] == b'(' {
        pos = match_delim(bytes, pos, b'(', b')')? + 1;
        pos = skip_ascii_ws(bytes, pos);
    }
    if pos >= bytes.len() || bytes[pos] != b'{' {
        return None;
    }
    let close = match_brace(bytes, pos)?;
    Some(close + 1)
}

/// Find the matching close delimiter for an opening one.  Honours `\\` escapes
/// and supports nested delimiters of the same kind.
pub(super) fn match_delim(bytes: &[u8], open: usize, open_ch: u8, close_ch: u8) -> Option<usize> {
    if open >= bytes.len() || bytes[open] != open_ch {
        return None;
    }
    let mut depth = 1;
    let mut i = open + 1;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' {
            i += 1;
            if i < bytes.len() {
                i += utf8_char_width(bytes[i]);
            }
            continue;
        }
        if b == open_ch {
            depth += 1;
        } else if b == close_ch {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += utf8_char_width(bytes[i]);
    }
    None
}

pub(super) fn skip_ascii_ws(bytes: &[u8], mut p: usize) -> usize {
    while p < bytes.len() && bytes[p].is_ascii_whitespace() {
        p += 1;
    }
    p
}

/// Given that `bytes[open]` is `b'{'`, return the byte position of the matching
/// `}`. Honours `\\` escapes so `\\}` doesn't end the group.
pub(super) fn match_brace(bytes: &[u8], open: usize) -> Option<usize> {
    if open >= bytes.len() || bytes[open] != b'{' {
        return None;
    }
    let mut depth = 1;
    let mut i = open + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                // Skip the backslash and the following codepoint (one Unicode
                // char, not necessarily one byte — `\é` etc. are valid LaTeX).
                i += 1;
                if i < bytes.len() {
                    i += utf8_char_width(bytes[i]);
                }
                continue;
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += utf8_char_width(bytes[i]);
    }
    None
}

