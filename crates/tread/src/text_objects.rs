//! Vim text objects for the read-only reader.
//!
//! Each function takes a `&Reader` and returns the substring to yank.
//! Returns `None` when the cursor isn't inside a valid object (e.g.
//! `yi"` outside any quotes is a deliberate no-op — vim's behaviour).
//!
//! Single-line objects (word, quote, bracket pair) operate on the
//! current visual line's `text`.  Multi-line objects (paragraph,
//! sentence) walk the visual-line list joining with `\n`.

use crate::state::Reader;

// ── Word ─────────────────────────────────────────────────────────────────────

/// `iw` / `aw` (small word) and `iW` / `aW` (BIG word).
/// Inner = the word chars only.  Around = inner + trailing whitespace
/// (or leading if there's no trailing).
pub fn word(reader: &Reader, big: bool, around: bool) -> Option<String> {
    let vl = reader.visual_lines.get(reader.current_line())?;
    let text = vl.text.as_str();
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let x = reader.cursor_x().min(bytes.len() - 1);

    let is_word_byte = |b: u8| -> bool {
        if big {
            !b.is_ascii_whitespace()
        } else {
            b.is_ascii_alphanumeric() || b == b'_'
        }
    };

    // If cursor is on whitespace, expand to the next word (vim behaviour).
    let mut start = x;
    let mut end = x;
    if !is_word_byte(bytes[x]) {
        // Find next word start.
        let next = (x..bytes.len()).find(|&i| is_word_byte(bytes[i]))?;
        start = next;
        end = next;
    }
    // Walk start backward, end forward through the word run.
    while start > 0 && is_word_byte(bytes[start - 1]) {
        start -= 1;
    }
    while end + 1 < bytes.len() && is_word_byte(bytes[end + 1]) {
        end += 1;
    }
    let inner_end_excl = end + 1;

    if !around {
        return text.get(start..inner_end_excl).map(|s| s.to_string());
    }

    // Around: include trailing whitespace if present, else leading.
    let mut a_end = inner_end_excl;
    while a_end < bytes.len() && bytes[a_end].is_ascii_whitespace() {
        a_end += 1;
    }
    let mut a_start = start;
    if a_end == inner_end_excl {
        // No trailing ws — eat leading.
        while a_start > 0 && bytes[a_start - 1].is_ascii_whitespace() {
            a_start -= 1;
        }
    }
    text.get(a_start..a_end).map(|s| s.to_string())
}

// ── Quote ────────────────────────────────────────────────────────────────────

/// `i"` / `a"` and friends.  Single-line only (matches vim).  Returns
/// `None` if the cursor isn't between two `ch` instances on the line.
pub fn quote(reader: &Reader, ch: char, around: bool) -> Option<String> {
    if !ch.is_ascii() {
        return None;
    }
    let vl = reader.visual_lines.get(reader.current_line())?;
    let text = vl.text.as_str();
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let target = ch as u8;
    let x = reader.cursor_x().min(bytes.len() - 1);

    // Find the previous quote at-or-before x, and the next quote after the
    // previous one.  Skip backslash-escaped instances (LaTeX commonly has
    // \"...\").  When the cursor itself is on a quote char, we treat that
    // as the *opening* of the next pair (match vim).
    let prev = find_quote_before(bytes, target, x);
    let opener = if bytes.get(x) == Some(&target) {
        // Cursor is on a quote — pair it forward with the next.
        Some(x)
    } else {
        prev
    };
    let opener = opener?;
    let closer = find_quote_after(bytes, target, opener + 1)?;

    if around {
        text.get(opener..=closer).map(|s| s.to_string())
    } else {
        text.get(opener + 1..closer).map(|s| s.to_string())
    }
}

fn find_quote_before(bytes: &[u8], target: u8, max_inclusive: usize) -> Option<usize> {
    let cap = max_inclusive.min(bytes.len().saturating_sub(1));
    (0..=cap)
        .rev()
        .find(|&i| bytes[i] == target && !preceded_by_backslash(bytes, i))
}

fn find_quote_after(bytes: &[u8], target: u8, from: usize) -> Option<usize> {
    (from..bytes.len()).find(|&i| bytes[i] == target && !preceded_by_backslash(bytes, i))
}

fn preceded_by_backslash(bytes: &[u8], i: usize) -> bool {
    // Odd number of immediately-preceding backslashes means the char is escaped.
    let mut count = 0;
    let mut j = i;
    while j > 0 && bytes[j - 1] == b'\\' {
        count += 1;
        j -= 1;
    }
    count % 2 == 1
}

// ── Pair ─────────────────────────────────────────────────────────────────────

/// `i(` / `a(`, `i[` / `a[`, `i{` / `a{`.  Walks the line with a depth
/// counter to handle nested pairs — `yi(` inside `(a (b) c)` from
/// position of `b` yanks `b`; from position of `a` yanks `a (b) c`.
/// Single-line only for v1.
pub fn pair(reader: &Reader, open: char, close: char, around: bool) -> Option<String> {
    if !open.is_ascii() || !close.is_ascii() {
        return None;
    }
    let vl = reader.visual_lines.get(reader.current_line())?;
    let text = vl.text.as_str();
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let o = open as u8;
    let c = close as u8;
    let x = reader.cursor_x().min(bytes.len() - 1);

    // Walk back to find the opening that contains the cursor.
    let mut depth: i32 = 0;
    let mut opener: Option<usize> = None;
    for i in (0..=x).rev() {
        if bytes[i] == c {
            depth += 1;
        } else if bytes[i] == o {
            if depth == 0 {
                opener = Some(i);
                break;
            }
            depth -= 1;
        }
    }
    let opener = opener?;

    // Walk forward to find the matching closer.
    let mut depth: i32 = 0;
    let mut closer: Option<usize> = None;
    for i in (opener + 1)..bytes.len() {
        if bytes[i] == o {
            depth += 1;
        } else if bytes[i] == c {
            if depth == 0 {
                closer = Some(i);
                break;
            }
            depth -= 1;
        }
    }
    let closer = closer?;

    if around {
        text.get(opener..=closer).map(|s| s.to_string())
    } else {
        text.get(opener + 1..closer).map(|s| s.to_string())
    }
}

// ── Paragraph ────────────────────────────────────────────────────────────────

/// `ip` / `ap`.  A paragraph is a contiguous run of non-blank visual
/// lines.  Inner = the lines themselves joined with `\n`.  Around =
/// inner + one trailing blank line if present (vim convention).
pub fn paragraph(reader: &Reader, around: bool) -> Option<String> {
    let total = reader.total_lines();
    if total == 0 {
        return None;
    }
    let cur = reader.current_line();

    // If the cursor is on a blank line, vim's `ip` treats the *blank run*
    // as the paragraph.  We simplify: from a blank line, just yank the
    // run of blank lines (works for `ip`/`ap` consistently enough).
    let is_blank = |i: usize| {
        reader
            .visual_lines
            .get(i)
            .map(|vl| vl.text.trim().is_empty())
            .unwrap_or(true)
    };

    if is_blank(cur) {
        let mut start = cur;
        while start > 0 && is_blank(start - 1) {
            start -= 1;
        }
        let mut end = cur;
        while end + 1 < total && is_blank(end + 1) {
            end += 1;
        }
        return Some(join_lines(reader, start, end));
    }

    // Walk back to the first non-blank in this paragraph.
    let mut start = cur;
    while start > 0 && !is_blank(start - 1) {
        start -= 1;
    }
    // Walk forward to the last non-blank.
    let mut end = cur;
    while end + 1 < total && !is_blank(end + 1) {
        end += 1;
    }
    if around {
        // Include one trailing blank line if available.
        if end + 1 < total && is_blank(end + 1) {
            end += 1;
        }
    }
    Some(join_lines(reader, start, end))
}

/// Like `paragraph` but also returns the visual-line range so callers
/// (voice playback) can track which rows correspond to the text being
/// read aloud.  Returns `(text, vl_start, vl_end)` where both
/// indices are inclusive.  Skips trailing blank lines from the range
/// even when `around=true` so the read-aloud range matches the
/// "actually-prose" content.
pub fn paragraph_with_range(reader: &Reader, _around: bool) -> Option<(String, usize, usize)> {
    let total = reader.total_lines();
    if total == 0 {
        return None;
    }
    let cur = reader.current_line();

    let is_blank = |i: usize| {
        reader
            .visual_lines
            .get(i)
            .map(|vl| vl.text.trim().is_empty())
            .unwrap_or(true)
    };

    if is_blank(cur) {
        let mut start = cur;
        while start > 0 && is_blank(start - 1) {
            start -= 1;
        }
        let mut end = cur;
        while end + 1 < total && is_blank(end + 1) {
            end += 1;
        }
        return Some((join_lines(reader, start, end), start, end));
    }

    let mut start = cur;
    while start > 0 && !is_blank(start - 1) {
        start -= 1;
    }
    let mut end = cur;
    while end + 1 < total && !is_blank(end + 1) {
        end += 1;
    }
    let inner_end = end;
    // Voice tracking always wants the prose range, not trailing blanks.
    Some((join_lines(reader, start, inner_end), start, inner_end))
}

/// Returns the text from the cursor to the end of the current paragraph
/// (exclusive of trailing blank lines), plus the VL range.  Used by
/// capital `R` ("read from cursor").
pub fn cursor_to_paragraph_end(reader: &Reader) -> Option<(String, usize, usize)> {
    let total = reader.total_lines();
    if total == 0 {
        return None;
    }
    let cur = reader.current_line();
    let is_blank = |i: usize| {
        reader
            .visual_lines
            .get(i)
            .map(|vl| vl.text.trim().is_empty())
            .unwrap_or(true)
    };
    if is_blank(cur) {
        return None;
    }
    let mut end = cur;
    while end + 1 < total && !is_blank(end + 1) {
        end += 1;
    }
    // Slice the cursor's line at cursor_x; subsequent lines verbatim.
    let mut out = String::new();
    if let Some(vl) = reader.visual_lines.get(cur) {
        let cut = reader.cursor_x().min(vl.text.len());
        out.push_str(&vl.text[cut..]);
    }
    for i in (cur + 1)..=end {
        if let Some(vl) = reader.visual_lines.get(i) {
            out.push('\n');
            out.push_str(&vl.text);
        }
    }
    if out.trim().is_empty() {
        return None;
    }
    Some((out, cur, end))
}

fn join_lines(reader: &Reader, start: usize, end: usize) -> String {
    let mut out = String::new();
    for i in start..=end {
        if let Some(vl) = reader.visual_lines.get(i) {
            if i > start {
                out.push('\n');
            }
            out.push_str(&vl.text);
        }
    }
    out
}

// ── Sentence ─────────────────────────────────────────────────────────────────

/// `is` / `as`.  A sentence runs from the previous sentence end (or
/// document start) to the next sentence end.  End markers are `.`,
/// `!`, `?` followed by whitespace (allowing closer punctuation
/// `)`/`]`/`"`/`'`).  Around = inner + trailing whitespace.
pub fn sentence(reader: &Reader, around: bool) -> Option<String> {
    let total = reader.total_lines();
    if total == 0 {
        return None;
    }
    let cur_line = reader.current_line();
    let cur_col = reader.cursor_x();

    // Find sentence start: walk back from cursor looking for a terminator
    // followed by whitespace.  The first non-whitespace byte after that
    // is the sentence start.  If none found, start at (0, 0).
    let mut start = (0usize, 0usize);
    {
        let mut saw_terminator = false;
        let mut saw_ws_after = true; // start-of-doc sentinel
        'outer: for line in 0..total {
            let Some(text) = reader.visual_lines.get(line).map(|vl| vl.text.as_str()) else {
                continue;
            };
            let bytes = text.as_bytes();
            for col in 0..bytes.len() {
                if line == cur_line && col > cur_col {
                    break 'outer;
                }
                let b = bytes[col];
                if saw_ws_after && !b.is_ascii_whitespace() {
                    start = (line, col);
                    saw_terminator = false;
                    saw_ws_after = false;
                }
                if matches!(b, b'.' | b'!' | b'?') {
                    saw_terminator = true;
                } else if saw_terminator && matches!(b, b')' | b']' | b'"' | b'\'') {
                    // closer — keep terminator state
                } else if saw_terminator && b.is_ascii_whitespace() {
                    saw_ws_after = true;
                } else if !b.is_ascii_whitespace() {
                    saw_terminator = false;
                }
            }
            if saw_terminator {
                saw_ws_after = true;
            }
        }
    }

    // Find sentence end (inner): walk forward from cursor for the next
    // terminator + closer + (whitespace or EOL).  End-byte is the
    // terminator + closers, exclusive of the trailing whitespace.
    let mut inner_end = (cur_line, cur_col);
    let mut found_end = false;
    {
        let mut line = start.0;
        let mut col = start.1;
        while line < total {
            let Some(text) = reader.visual_lines.get(line).map(|vl| vl.text.as_str()) else {
                break;
            };
            let bytes = text.as_bytes();
            while col < bytes.len() {
                let b = bytes[col];
                if matches!(b, b'.' | b'!' | b'?') {
                    // Walk past closers.
                    let mut after = col + 1;
                    while after < bytes.len() && matches!(bytes[after], b')' | b']' | b'"' | b'\'')
                    {
                        after += 1;
                    }
                    // Need whitespace or EOL after.
                    if after >= bytes.len() || bytes[after].is_ascii_whitespace() {
                        inner_end = (line, after.saturating_sub(1));
                        found_end = true;
                        break;
                    }
                }
                col += 1;
            }
            if found_end {
                break;
            }
            // EOL counts as whitespace; if the line ended after a terminator
            // we treat that as the sentence end.
            line += 1;
            col = 0;
        }
    }
    if !found_end {
        // Sentence runs to end of document.
        let last = total.saturating_sub(1);
        let last_len = reader
            .visual_lines
            .get(last)
            .map(|vl| vl.text.len())
            .unwrap_or(0);
        inner_end = (last, last_len.saturating_sub(1));
    }

    // Build inner text.
    let inner = join_range(reader, start, inner_end)?;
    if !around {
        return Some(inner);
    }
    // Around: add trailing whitespace until the next non-ws byte.
    let mut a_end = inner_end;
    // Walk forward through whitespace.
    loop {
        let Some(text) = reader.visual_lines.get(a_end.0).map(|vl| vl.text.as_str()) else {
            break;
        };
        let bytes = text.as_bytes();
        if a_end.1 + 1 < bytes.len() && bytes[a_end.1 + 1].is_ascii_whitespace() {
            a_end.1 += 1;
        } else if a_end.1 + 1 >= bytes.len() && a_end.0 + 1 < total {
            a_end = (a_end.0 + 1, 0);
            // Skip leading whitespace on the new line.
            let Some(next_text) = reader.visual_lines.get(a_end.0).map(|vl| vl.text.as_str())
            else {
                break;
            };
            let next_bytes = next_text.as_bytes();
            while a_end.1 < next_bytes.len() && next_bytes[a_end.1].is_ascii_whitespace() {
                a_end.1 += 1;
            }
            if a_end.1 == 0 {
                // Empty line — include it but don't go further.
            } else {
                a_end.1 = a_end.1.saturating_sub(1);
                break;
            }
        } else {
            break;
        }
    }
    join_range(reader, start, a_end)
}

/// Concatenate the substring from `(start_line, start_col)` to
/// `(end_line, end_col_inclusive)` joining lines with `\n`.
fn join_range(reader: &Reader, start: (usize, usize), end: (usize, usize)) -> Option<String> {
    if start.0 > end.0 || (start.0 == end.0 && start.1 > end.1) {
        return None;
    }
    let mut out = String::new();
    for line in start.0..=end.0 {
        let text = reader.visual_lines.get(line).map(|vl| vl.text.as_str())?;
        let lo = if line == start.0 { start.1 } else { 0 };
        let hi_excl = if line == end.0 {
            end.1.saturating_add(1).min(text.len())
        } else {
            text.len()
        };
        if line > start.0 {
            out.push('\n');
        }
        if let Some(slice) = text.get(lo..hi_excl) {
            out.push_str(slice);
        }
    }
    Some(out)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use doc_model::Block;

    fn reader_with(lines: &[&str]) -> Reader {
        let blocks: Vec<Block> = lines
            .iter()
            .map(|l| {
                if l.is_empty() {
                    Block::Blank
                } else {
                    Block::Line(l.to_string())
                }
            })
            .collect();
        Reader::new(blocks, 80, 24)
    }

    // --- word ---

    #[test]
    fn iw_on_word_yields_word() {
        let mut r = reader_with(&["hello world"]);
        r.cursor_set_for_test(0, 2); // inside "hello"
        assert_eq!(word(&r, false, false).as_deref(), Some("hello"));
    }

    #[test]
    fn iw_on_whitespace_yields_next_word() {
        let mut r = reader_with(&["hello world"]);
        r.cursor_set_for_test(0, 5); // the space
        assert_eq!(word(&r, false, false).as_deref(), Some("world"));
    }

    #[test]
    fn aw_includes_trailing_whitespace() {
        let mut r = reader_with(&["hello world"]);
        r.cursor_set_for_test(0, 0);
        assert_eq!(word(&r, false, true).as_deref(), Some("hello "));
    }

    #[test]
    fn aw_falls_back_to_leading_when_no_trailing() {
        let mut r = reader_with(&["hello world"]);
        r.cursor_set_for_test(0, 7); // inside "world"
        assert_eq!(word(&r, false, true).as_deref(), Some(" world"));
    }

    #[test]
    fn iw_punct_separation() {
        let mut r = reader_with(&["foo,bar"]);
        r.cursor_set_for_test(0, 1); // inside "foo"
        assert_eq!(word(&r, false, false).as_deref(), Some("foo"));
    }

    #[test]
    fn iw_big_treats_punct_as_word() {
        let mut r = reader_with(&["foo,bar baz"]);
        r.cursor_set_for_test(0, 1);
        // BIG word stops at whitespace only.
        assert_eq!(word(&r, true, false).as_deref(), Some("foo,bar"));
    }

    // --- quote ---

    #[test]
    fn iq_inside_quotes() {
        let mut r = reader_with(&[r#"the "quick" brown fox"#]);
        r.cursor_set_for_test(0, 6); // inside "quick"
        assert_eq!(quote(&r, '"', false).as_deref(), Some("quick"));
    }

    #[test]
    fn aq_includes_quote_chars() {
        let mut r = reader_with(&[r#"the "quick" brown fox"#]);
        r.cursor_set_for_test(0, 6);
        assert_eq!(quote(&r, '"', true).as_deref(), Some(r#""quick""#));
    }

    #[test]
    fn quote_outside_returns_none() {
        let mut r = reader_with(&["no quotes here"]);
        r.cursor_set_for_test(0, 5);
        assert!(quote(&r, '"', false).is_none());
    }

    #[test]
    fn quote_skips_escaped() {
        let mut r = reader_with(&[r#"a \"escaped\" "real" b"#]);
        r.cursor_set_for_test(0, 16); // inside "real" (the unescaped pair)
        assert_eq!(quote(&r, '"', false).as_deref(), Some("real"));
    }

    // --- pair ---

    #[test]
    fn pair_inner() {
        let mut r = reader_with(&["foo (bar baz) qux"]);
        r.cursor_set_for_test(0, 6); // inside the parens
        assert_eq!(pair(&r, '(', ')', false).as_deref(), Some("bar baz"));
    }

    #[test]
    fn pair_around() {
        let mut r = reader_with(&["foo (bar baz) qux"]);
        r.cursor_set_for_test(0, 6);
        assert_eq!(pair(&r, '(', ')', true).as_deref(), Some("(bar baz)"));
    }

    #[test]
    fn pair_nested_inner() {
        let mut r = reader_with(&["(a (b) c)"]);
        r.cursor_set_for_test(0, 4); // inside the inner (b)
        assert_eq!(pair(&r, '(', ')', false).as_deref(), Some("b"));
    }

    #[test]
    fn pair_nested_outer_from_outside_inner() {
        let mut r = reader_with(&["(a (b) c)"]);
        r.cursor_set_for_test(0, 1); // on 'a', between outer parens but outside inner
        assert_eq!(pair(&r, '(', ')', false).as_deref(), Some("a (b) c"));
    }

    #[test]
    fn pair_outside_returns_none() {
        let mut r = reader_with(&["no parens here"]);
        r.cursor_set_for_test(0, 5);
        assert!(pair(&r, '(', ')', false).is_none());
    }

    // --- paragraph ---

    #[test]
    fn ip_yields_current_block() {
        let mut r = reader_with(&["line one", "line two", "line three", "", "next para"]);
        r.cursor_set_for_test(1, 0); // on "line two"
        assert_eq!(
            paragraph(&r, false).as_deref(),
            Some("line one\nline two\nline three")
        );
    }

    #[test]
    fn ap_includes_trailing_blank() {
        let mut r = reader_with(&["aaa", "bbb", "", "next"]);
        r.cursor_set_for_test(0, 0); 
        assert_eq!(paragraph(&r, true).as_deref(), Some("aaa\nbbb\n"));
    }

    // --- paragraph_with_range ---

    #[test]
    fn paragraph_with_range_yields_indices() {
        let mut r = reader_with(&["line one", "line two", "", "next para"]);
        r.cursor_set_for_test(1, 0); // on "line two"
        let (text, start, end) = paragraph_with_range(&r, false).expect("paragraph_with_range");
        assert_eq!(text, "line one\nline two");
        assert_eq!(start, 0);
        assert_eq!(end, 1);
    }

    #[test]
    fn paragraph_with_range_at_eof() {
        let mut r = reader_with(&["only paragraph"]);
        r.cursor_set_for_test(0, 0); 
        let (text, start, end) = paragraph_with_range(&r, false).expect("paragraph_with_range");
        assert_eq!(text, "only paragraph");
        assert_eq!(start, 0);
        assert_eq!(end, 0);
    }

    #[test]
    fn paragraph_with_range_skips_trailing_blank() {
        // `around=true` would normally include the trailing blank in the
        // string output of `paragraph()`, but for voice we want only the
        // prose range so the dim-overlay matches what's being read.
        let mut r = reader_with(&["aaa", "bbb", "", "next"]);
        r.cursor_set_for_test(0, 0); 
        let (text, start, end) = paragraph_with_range(&r, true).expect("paragraph_with_range");
        assert_eq!(text, "aaa\nbbb");
        assert_eq!((start, end), (0, 1));
    }

    // --- cursor_to_paragraph_end ---

    #[test]
    fn cursor_to_paragraph_end_starts_at_cursor() {
        let mut r = reader_with(&["aaa bbb ccc", "ddd eee", "", "next"]);
        r.cursor_set_for_test(0, 4); // start of "bbb"
        let (text, start, end) = cursor_to_paragraph_end(&r).expect("cursor_to_paragraph_end");
        assert_eq!(text, "bbb ccc\nddd eee");
        assert_eq!((start, end), (0, 1));
    }

    // --- sentence ---

    #[test]
    fn is_yields_current_sentence() {
        let mut r = reader_with(&["First sentence. Second sentence here. Third."]);
        r.cursor_set_for_test(0, 20); // somewhere in "Second sentence here"
        let out = sentence(&r, false).expect("sentence");
        assert!(out.contains("Second sentence here"));
    }
}
