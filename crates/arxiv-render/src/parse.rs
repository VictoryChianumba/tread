use doc_model::{Block, InlineSpan};
use math_render::{MathInput, render};
use std::collections::HashMap;

const WRAP_WIDTH: usize = 80;

#[derive(Clone)]
enum ListKind {
  Itemize,
  Enumerate(usize),
  Description,
}

// ── Inline builder ────────────────────────────────────────────────────────────

/// Accumulates plain text and styled spans for the current paragraph line.
/// When flushed, emits Block::StyledLine if any styled span was added,
/// or Block::Line if everything is plain text.
struct InlineBuilder {
  plain_buf: String,
  spans: Vec<InlineSpan>,
  has_style: bool,
}

impl InlineBuilder {
  fn new() -> Self {
    Self { plain_buf: String::new(), spans: Vec::new(), has_style: false }
  }

  fn push_plain(&mut self, s: &str) { self.plain_buf.push_str(s); }
  fn push_char(&mut self, c: char) { self.plain_buf.push(c); }

  fn is_empty(&self) -> bool { self.plain_buf.is_empty() && self.spans.is_empty() }

  /// Flush accumulated plain text as a plain span into the spans vec.
  fn flush_plain(&mut self) {
    if !self.plain_buf.is_empty() {
      self.spans.push(InlineSpan::plain(std::mem::take(&mut self.plain_buf)));
    }
  }

  /// Add a styled span. The closure sets style flags on an InlineSpan.
  fn push_styled(&mut self, text: String, style: impl Fn(InlineSpan) -> InlineSpan) {
    if text.is_empty() { return; }
    self.flush_plain();
    self.spans.push(style(InlineSpan::plain(text)));
    self.has_style = true;
  }

  fn push_url(&mut self, text: &str, url: String) {
    if text.is_empty() { return; }
    self.flush_plain();
    self.spans.push(InlineSpan { underline: true, url: Some(url), ..InlineSpan::plain(text) });
    self.has_style = true;
  }

  /// Consume and reset. Returns Some(spans) if styled, None if plain-only.
  fn finish(&mut self) -> Option<Vec<InlineSpan>> {
    if !self.has_style {
      self.spans.clear();
      return None;
    }
    self.flush_plain();
    self.has_style = false;
    Some(std::mem::take(&mut self.spans))
  }

  fn take_plain(&mut self) -> String {
    self.spans.clear();
    self.has_style = false;
    std::mem::take(&mut self.plain_buf)
  }
}

const THEOREM_ENVS: &[&str] = &[
  "theorem", "lemma", "proposition", "corollary", "definition",
  "remark", "example", "proof", "claim", "conjecture",
];

const FULL_SKIP_ENVS: &[&str] = &[
  "tikzpicture", "minipage", "pgfpicture",
];

const CODE_ENVS: &[&str] = &["verbatim", "lstlisting", "Verbatim", "minted"];

const ALGO_ENVS: &[&str] = &["algorithm", "algorithm2e", "algorithmic", "algorithmicx"];

const CAPTION_ENVS: &[&str] = &[
  "figure", "figure*", "wrapfigure", "subfigure",
];

const TABULAR_ENVS: &[&str] = &[
  "tabular", "tabular*", "longtable", "tabularx", "tabulary", "array",
];

/// Pre-scan sources for `\bibitem{key}…` entries, preserving source order.
/// Used to assign sequential `[N]` citation numbers and to synthesize a
/// numbered bibliography that replaces Pandoc's structureless rendering.
pub fn extract_bibitems_ordered(sources: &[(String, String)]) -> Vec<(String, String)> {
  let mut out: Vec<(String, String)> = Vec::new();
  for (_, content) in sources {
    let bytes = content.as_bytes();
    let mut i = 0;
    while i + 8 < bytes.len() {
      if &bytes[i..i + 8] != b"\\bibitem" {
        i += 1;
        continue;
      }
      let mut j = i + 8;
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
        Err(_) => { i = key_end + 1; continue; }
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
        // Skip duplicates — same key in multiple files keeps first occurrence.
        if !out.iter().any(|(k, _)| k == &key) {
          out.push((key, cleaned));
        }
      }
      i = entry_end;
    }
  }
  out
}

/// Pre-scan all source files for `\bibitem{key}…` entries.  Pandoc's
/// AST surfaces bibliography paragraphs but doesn't carry cite-keys
/// down to them, so we scrape them ourselves.  Used to populate the
/// reader's bib-entry popups.
pub fn extract_bibitems(sources: &[(String, String)]) -> HashMap<String, String> {
  let mut out = HashMap::new();
  for (_, content) in sources {
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
        Err(_) => { i = key_end + 1; continue; }
      };
      let entry_start = key_end + 1;
      // Entry runs until next \bibitem, \end{thebibliography}, or EOF.
      let entry_end = (entry_start..bytes.len())
        .find(|&k| {
          bytes[k..].starts_with(b"\\bibitem")
            || bytes[k..].starts_with(b"\\end{thebibliography}")
        })
        .unwrap_or(bytes.len());
      let raw = &content[entry_start..entry_end];
      let cleaned = clean_bibitem_text(raw);
      if !key.is_empty() && !cleaned.is_empty() {
        out.insert(key, cleaned);
      }
      i = entry_end;
    }
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
        // Skip a command name and any immediate brace group.
        while let Some(&next) = chars.peek() {
          if next.is_ascii_alphabetic() || next == '*' { chars.next(); } else { break; }
        }
        // Skip a single brace group following the command (e.g. \em{...}).
        if let Some(&'{') = chars.peek() {
          chars.next();
          let mut depth = 1;
          for inner in chars.by_ref() {
            match inner {
              '{' => depth += 1,
              '}' => { depth -= 1; if depth == 0 { break; } }
              _ => out.push(inner),
            }
          }
        }
      }
      '{' | '}' | '~' => { out.push(' '); }
      '\n' => { out.push(' '); }
      _ => out.push(c),
    }
  }
  // Collapse whitespace runs.
  out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Convert a set of `.tex` source files into a semantic block document.
pub fn to_blocks(sources: Vec<(String, String)>) -> Vec<Block> {
  // Try Pandoc first — better LaTeX coverage, handles multi-file \input{}, etc.
  // Falls back to hand-parser if Pandoc is not installed or fails.
  if let Ok(blocks) = crate::pandoc_parse::try_pandoc(&sources) {
    return blocks;
  }

  let file_map: HashMap<String, String> = sources.into_iter().collect();
  let root = find_root(&file_map);
  let expanded = expand_inputs(&root, &file_map, 0);

  let macros = extract_macros(&expanded);

  let title = extract_command_arg(&expanded, "title").map(clean_inline);
  let authors = extract_command_arg(&expanded, "author").map(clean_authors);

  let body_raw = extract_body(&expanded);
  // Strip comments before float reordering so we don't match %\begin{table} in comments.
  let body_stripped = strip_tex_comments(&body_raw);
  let body_floated = float_tables_into_sections(&body_stripped);
  let body = reorder_t_floats(&body_floated);
  let mut footnotes: Vec<String> = Vec::new();
  let body_blocks = process(&body, &macros, &mut footnotes);

  let mut out: Vec<Block> = Vec::new();
  out.push(Block::Blank);
  if let Some(t) = title {
    if !t.is_empty() {
      out.push(Block::Header { level: 1, text: t });
    }
  }
  if let Some(a) = authors {
    if !a.is_empty() {
      out.push(Block::Line(a));
    }
  }
  out.push(Block::Blank);
  out.extend(body_blocks);

  if !footnotes.is_empty() {
    out.push(Block::Blank);
    out.push(Block::Header { level: 2, text: "Notes".to_string() });
    for (i, note) in footnotes.iter().enumerate() {
      out.push(Block::Line(format!("[{}] {}", i + 1, note)));
    }
  }

  out
}

// ── Root selection ────────────────────────────────────────────────────────────

fn find_root(files: &HashMap<String, String>) -> String {
  for content in files.values() {
    if content.contains(r"\begin{document}") {
      return content.clone();
    }
  }
  files.values().max_by_key(|c| c.len()).cloned().unwrap_or_default()
}

// ── \input{} resolution ───────────────────────────────────────────────────────

fn expand_inputs(content: &str, files: &HashMap<String, String>, depth: usize) -> String {
  if depth > 10 {
    return content.to_string();
  }
  let mut out = String::with_capacity(content.len());
  let mut rest = content;
  while let Some(pos) = rest.find(r"\input{") {
    out.push_str(&rest[..pos]);
    rest = &rest[pos + 7..];
    if let Some(end) = rest.find('}') {
      let filename = rest[..end].trim();
      rest = &rest[end + 1..];
      if let Some(included) = resolve_input(filename, files) {
        out.push_str(&expand_inputs(&included, files, depth + 1));
      }
    }
  }
  out.push_str(rest);
  out
}

fn resolve_input(name: &str, files: &HashMap<String, String>) -> Option<String> {
  let candidates = [
    name.to_string(),
    format!("{name}.tex"),
    std::path::Path::new(name)
      .file_name()
      .map(|n| n.to_string_lossy().to_string())
      .unwrap_or_default(),
    format!(
      "{}.tex",
      std::path::Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
    ),
  ];
  for c in &candidates {
    if let Some(content) = files.get(c.as_str()) {
      return Some(content.clone());
    }
  }
  None
}

// ── Document body extraction ──────────────────────────────────────────────────

fn extract_body(content: &str) -> String {
  let start = content
    .find(r"\begin{document}")
    .map(|p| p + r"\begin{document}".len())
    .unwrap_or(0);
  let end = content.rfind(r"\end{document}").unwrap_or(content.len());
  content[start..end].to_string()
}

// ── Float reordering ─────────────────────────────────────────────────────────

/// Move `\begin{table}...\end{table}` blocks that appear immediately before a
/// `\section` or `\subsection` to just after that heading.
///
/// This corrects the common LaTeX pattern of defining a float at the very start
/// of an `\input{}` file (before the section command), trusting the LaTeX float
/// algorithm to position it on the right page. Example: BERT's glue_official_tab.tex.
fn float_tables_into_sections(body: &str) -> String {
  const SECTION_CMDS: &[&str] = &[r"\section{", r"\subsection{", r"\section*{", r"\subsection*{"];

  let mut result = body.to_string();
  let mut changed = true;
  while changed {
    changed = false;
    for (begin_tag, end_tag) in &[(r"\begin{table}", r"\end{table}"), (r"\begin{table*}", r"\end{table*}")] {
      if let Some(reordered) = try_move_table_before_section(&result, begin_tag, end_tag, SECTION_CMDS) {
        result = reordered;
        changed = true;
        break;
      }
    }
  }
  result
}

fn try_move_table_before_section(
  text: &str,
  begin_tag: &str,
  end_tag: &str,
  section_cmds: &[&str],
) -> Option<String> {
  let t_start = text.find(begin_tag)?;
  // Find the matching \end{table}.
  let after_begin = t_start + begin_tag.len();
  let end_rel = text[after_begin..].find(end_tag)?;
  let t_end = after_begin + end_rel + end_tag.len();

  // Skip any \footnotetext{...} commands that follow directly.
  let mut cursor = t_end;
  loop {
    let after_ws = cursor + (text[cursor..].len() - text[cursor..].trim_start().len());
    if text[after_ws..].starts_with(r"\footnotetext{") {
      let brace_start = after_ws + r"\footnotetext".len();
      if text[brace_start..].starts_with('{') {
        let chars: Vec<char> = text[brace_start..].chars().collect();
        let (_, adv) = read_braced_arg(&chars, 0);
        cursor = brace_start + adv;
      } else {
        break;
      }
    } else {
      break;
    }
  }
  let after_table = cursor; // byte position right after table + footnotetext

  // Skip whitespace to first significant content.
  let rest = text[after_table..].trim_start();
  let ws_len = text[after_table..].len() - rest.len();
  let sec_pos = after_table + ws_len;

  // Check if that content is a section/subsection command.
  let _sec_cmd = section_cmds.iter().find(|&&sc| rest.starts_with(sc))?;

  // Find the end of the section command (up to and including the closing brace).
  let brace_offset = rest.find('{')?;
  let chars: Vec<char> = rest[brace_offset..].chars().collect();
  let (_, adv) = read_braced_arg(&chars, 0);
  let sec_end = sec_pos + brace_offset + adv;

  // Build reordered text: [before table][section cmd]\n[table block][rest after section]
  let before = &text[..t_start];
  let table_block = &text[t_start..after_table];
  let section_text = &text[sec_pos..sec_end];
  let after_section = &text[sec_end..];
  Some(format!("{before}{section_text}\n{table_block}{after_section}"))
}

// ── Top-float reordering ─────────────────────────────────────────────────────

/// Moves \begin{table}[t] / \begin{table*}[t] floats to a more natural terminal
/// reading position:
///
/// Case A – prose separates the enclosing section heading from the table →
///   move the table to immediately after the heading (section top).
///
/// Case B – no prose between a \subsection and the table, AND the subsection is
///   itself the very first content of its parent \section (no prose between
///   \section and \subsection) → move the table to after the parent \section.
///
/// All other [t] tables are left in place.
fn reorder_t_floats(body: &str) -> String {
  const VARIANTS: &[(&str, &str)] = &[
    (r"\begin{table}", r"\end{table}"),
    (r"\begin{table*}", r"\end{table*}"),
  ];
  const ALL_HEADS: &[&str] = &[r"\section{", r"\subsection{", r"\section*{", r"\subsection*{"];
  const SEC_ONLY: &[&str]  = &[r"\section{", r"\section*{"];

  let mut text = body.to_string();
  let mut scan_from = 0usize;

  loop {
    let Some((t_start, t_end)) = next_t_float(&text, scan_from, VARIANTS) else { break };

    let before_table = &text[..t_start];
    let Some((h_start, h_end, is_sub)) = prev_section(before_table, ALL_HEADS) else {
      scan_from = t_end;
      continue;
    };

    let gap = &text[h_end..t_start];

    if gap_has_prose(gap) {
      // Case A: prose before the table — move table to top of enclosing section.
      text = splice_before(&text, h_end, t_start, t_end);
      scan_from = 0;
      continue;
    }

    if is_sub {
      // Case B: subsection is table's nearest heading with no prose.
      // If that subsection is itself the first content of its parent \section,
      // move the table to after the parent \section.
      let before_sub = &text[..h_start];
      if let Some((_, p_end, _)) = prev_section(before_sub, SEC_ONLY) {
        if !gap_has_prose(&text[p_end..h_start]) {
          text = splice_before(&text, p_end, t_start, t_end);
          scan_from = 0;
          continue;
        }
      }
    }

    scan_from = t_end;
  }

  text
}

/// Return the start and end of the next `\begin{table[*]}[...t...]` from `from`.
fn next_t_float(text: &str, from: usize, variants: &[(&str, &str)]) -> Option<(usize, usize)> {
  let mut pos = from;
  loop {
    // Pick the earliest begin tag among all variants.
    let (t_start, begin_tag, end_tag) = variants
      .iter()
      .filter_map(|&(bt, et)| text[pos..].find(bt).map(|r| (pos + r, bt, et)))
      .min_by_key(|&(s, _, _)| s)?;

    let after_begin = t_start + begin_tag.len();

    // Require a placement specifier containing 't'.
    let has_t = text[after_begin..].starts_with('[') && {
      let close = text[after_begin..].find(']').unwrap_or(0);
      text[after_begin..after_begin + close + 1].contains('t')
    };

    if !has_t {
      pos = t_start + 1;
      continue;
    }

    let t_end = after_begin + text[after_begin..].find(end_tag)? + end_tag.len();
    return Some((t_start, t_end));
  }
}

/// Find the last section/subsection command in `text`.
/// Returns (cmd_start, cmd_end_after_closing_brace, is_subsection).
fn prev_section(text: &str, cmds: &[&str]) -> Option<(usize, usize, bool)> {
  cmds.iter()
    .filter_map(|&cmd| {
      let s = text.rfind(cmd)?;
      // cmd ends with '{'; the opening brace is at s + cmd.len() - 1.
      let brace_pos = s + cmd.len() - 1;
      let chars: Vec<char> = text[brace_pos..].chars().collect();
      let (_, adv) = read_braced_arg(&chars, 0);
      Some((s, brace_pos + adv, cmd.contains("subsection")))
    })
    .max_by_key(|&(s, _, _)| s)
}

/// True if `gap` contains at least 4 prose words (tokens ≥4 alphabetic chars,
/// not starting with `\` or `{`).  This distinguishes body text from LaTeX
/// structural commands like `\label{}`, `\vspace{}`, blank lines, etc.
fn gap_has_prose(gap: &str) -> bool {
  gap.split_whitespace()
    .filter(|t| {
      !t.starts_with('\\') && !t.starts_with('{') && !t.starts_with('%')
        && t.chars().filter(|c| c.is_alphabetic()).count() >= 4
    })
    .count() >= 4
}

/// Move `text[t_start..t_end]` to immediately after `insert_after`,
/// preserving all other text in order.
fn splice_before(text: &str, insert_after: usize, t_start: usize, t_end: usize) -> String {
  format!(
    "{}\n{}{}{}",
    &text[..insert_after],
    &text[t_start..t_end],
    &text[insert_after..t_start],
    &text[t_end..]
  )
}

// ── Label map (two-pass cross-reference resolution) ──────────────────────────

struct LabelMap {
  /// label key → display string: "1.2", "Theorem 3", "(4)"
  labels: HashMap<String, String>,
  /// bibitem cite-key → citation number [1], [2], …
  bibitems: HashMap<String, usize>,
  /// ordered cite-keys for bibliography rendering
  bibitem_order: Vec<String>,
}

/// Lightweight first-pass scanner that walks the document body collecting
/// `\label`, `\bibitem`, and section/theorem/equation counter state so that
/// `process_body` can resolve `\ref` and `\cite` to real numbers.
fn collect_labels(body: &str) -> LabelMap {
  let mut labels: HashMap<String, String> = HashMap::new();
  let mut bibitems: HashMap<String, usize> = HashMap::new();
  let mut bibitem_order: Vec<String> = Vec::new();

  let chars: Vec<char> = body.chars().collect();
  let len = chars.len();
  let mut i = 0;

  let mut sec: [u8; 3] = [0, 0, 0];
  let mut thm_counters: HashMap<String, usize> = HashMap::new();
  let mut eq_counter: usize = 0;
  let mut env_stack: Vec<String> = Vec::new();
  let mut bibitem_counter: usize = 0;

  let display_math_envs = [
    "equation", "equation*", "align", "align*", "aligned",
    "gather", "gather*", "multline", "multline*", "eqnarray", "eqnarray*",
  ];

  while i < len {
    if chars[i] == '%' && (i == 0 || chars[i - 1] != '\\') {
      while i < len && chars[i] != '\n' { i += 1; }
      continue;
    }
    if chars[i] != '\\' { i += 1; continue; }
    if i + 1 >= len { i += 1; continue; }

    let (cmd, consumed) = read_command(&chars, i + 1);
    i += 1 + consumed;

    match cmd.as_str() {
      "begin" => {
        let (env, skip) = read_braced_arg(&chars, i);
        i += skip;
        let env = env.trim().to_string();
        if display_math_envs.contains(&env.as_str()) && !env.ends_with('*') {
          eq_counter += 1;
        }
        if THEOREM_ENVS.contains(&env.as_str()) && env != "proof" {
          let n = thm_counters.entry(env.clone()).or_insert(0);
          *n += 1;
        }
        env_stack.push(env);
      }
      "end" => {
        let (env, skip) = read_braced_arg(&chars, i);
        i += skip;
        env_stack.retain(|e| e != env.trim());
      }
      "section" => {
        let (_, skip) = read_braced_arg(&chars, i); i += skip;
        sec[0] += 1; sec[1] = 0; sec[2] = 0;
      }
      "subsection" => {
        let (_, skip) = read_braced_arg(&chars, i); i += skip;
        sec[1] += 1; sec[2] = 0;
      }
      "subsubsection" => {
        let (_, skip) = read_braced_arg(&chars, i); i += skip;
        sec[2] += 1;
      }
      "label" => {
        let (key, skip) = read_braced_arg(&chars, i);
        i += skip;
        let key = key.trim().to_string();
        let value = if let Some(env) = env_stack.last() {
          if display_math_envs.contains(&env.as_str()) {
            format!("({})", eq_counter)
          } else if THEOREM_ENVS.contains(&env.as_str()) {
            let n = thm_counters.get(env.as_str()).copied().unwrap_or(0);
            format!("{} {}", capitalize(env), n)
          } else {
            // Default to section number.
            match env_stack.iter().rev().find(|e| {
              matches!(e.as_str(), "section" | "subsection" | "subsubsection")
            }) {
              _ => {
                if sec[2] > 0 { format!("{}.{}.{}", sec[0], sec[1], sec[2]) }
                else if sec[1] > 0 { format!("{}.{}", sec[0], sec[1]) }
                else { format!("{}", sec[0]) }
              }
            }
          }
        } else {
          if sec[2] > 0 { format!("{}.{}.{}", sec[0], sec[1], sec[2]) }
          else if sec[1] > 0 { format!("{}.{}", sec[0], sec[1]) }
          else { format!("{}", sec[0]) }
        };
        labels.insert(key, value);
      }
      "bibitem" => {
        // Skip optional [label].
        if i < len && chars[i] == '[' {
          while i < len && chars[i] != ']' { i += 1; }
          if i < len { i += 1; }
        }
        let (key, skip) = read_braced_arg(&chars, i);
        i += skip;
        let key = key.trim().to_string();
        if !key.is_empty() && !bibitems.contains_key(&key) {
          bibitem_counter += 1;
          bibitems.insert(key.clone(), bibitem_counter);
          bibitem_order.push(key);
        }
      }
      _ => {
        // Consume any braced argument for commands we don't track.
        if i < len && chars[i] == '{' {
          let (_, skip) = read_braced_arg(&chars, i);
          i += skip;
        }
      }
    }
  }

  LabelMap { labels, bibitems, bibitem_order }
}

// ── Main processor ────────────────────────────────────────────────────────────

fn process(
  body: &str,
  macros: &HashMap<String, (usize, String)>,
  footnotes: &mut Vec<String>,
) -> Vec<Block> {
  let label_map = collect_labels(body);
  let mut out: Vec<Block> = Vec::new();

  if let Some(abs) = extract_env(body, "abstract") {
    out.push(Block::Blank);
    out.push(Block::Header { level: 2, text: "Abstract".to_string() });
    for line in process_prose(&abs, macros) {
      out.push(Block::Line(line));
    }
    out.push(Block::Blank);
  }

  let mut list_stack: Vec<ListKind> = Vec::new();
  out.extend(process_body(body, macros, footnotes, &mut list_stack, &label_map));
  out
}

// ── Body state machine ────────────────────────────────────────────────────────

fn process_body(
  body: &str,
  macros: &HashMap<String, (usize, String)>,
  footnotes: &mut Vec<String>,
  list_stack: &mut Vec<ListKind>,
  label_map: &LabelMap,
) -> Vec<Block> {
  let mut out: Vec<Block> = Vec::new();
  let mut builder = InlineBuilder::new();
  // When Some, the next flush emits Block::ListItem instead of Block::Line/StyledLine.
  let mut list_item_pending: Option<(u8, String)> = None;
  // Section counters: [section, subsection, subsubsection]
  let mut sec: [u8; 3] = [0, 0, 0];
  // Per-kind theorem counters (shared across all theorem environments).
  let mut thm_counters: HashMap<String, usize> = HashMap::new();
  // Equation counter — incremented for non-starred display math environments.
  let mut eq_counter: usize = 0;

  let display_math_envs = [
    "equation", "equation*", "align", "align*", "aligned",
    "gather", "gather*", "multline", "multline*", "eqnarray", "eqnarray*",
  ];

  let mut i = 0usize;
  let text: Vec<char> = body.chars().collect();
  let len = text.len();

  while i < len {
    let c = text[i];

    // LaTeX comment: skip to end of line.
    if c == '%' && (i == 0 || text[i - 1] != '\\') {
      while i < len && text[i] != '\n' { i += 1; }
      continue;
    }

    // Backslash command.
    if c == '\\' && i + 1 < len {
      let (cmd, consumed) = read_command(&text, i + 1);
      i += 1 + consumed;

      match cmd.as_str() {
        "end" => {
          let (env, skip) = read_braced_arg(&text, i);
          i += skip;
          match env.trim() {
            "itemize" | "enumerate" | "description" => { list_stack.pop(); }
            "proof" => {
              flush_builder(&mut builder, &mut list_item_pending, &mut out);
              out.push(Block::Line("∎".to_string()));
            }
            _ => {}
          }
          continue;
        }
        "begin" => {
          let (env, skip) = read_braced_arg(&text, i);
          i += skip;
          let env = env.trim().to_string();

          if display_math_envs.iter().any(|e| *e == env.as_str()) {
            flush_builder(&mut builder, &mut list_item_pending, &mut out);
            let (math, adv) = read_until_end(&text, i, &env);
            i += adv;
            let numbered = !env.ends_with('*');
            let eq_num = if numbered { eq_counter += 1; Some(eq_counter) } else { None };
            let rendered = render_math(&math, macros);
            let lines: Vec<String> = rendered.lines().map(|l| l.to_string()).collect();
            if !lines.is_empty() { out.push(Block::DisplayMath { lines, num: eq_num }); }
            continue;
          }

          if env == "abstract" {
            let (_abs, adv) = read_until_end(&text, i, "abstract");
            i += adv;
            continue;
          }

          if THEOREM_ENVS.contains(&env.as_str()) {
            flush_builder(&mut builder, &mut list_item_pending, &mut out);
            out.push(Block::Blank);
            let label = if env == "proof" {
              "Proof".to_string()
            } else {
              let n = thm_counters.entry(env.clone()).or_insert(0);
              *n += 1;
              format!("{} {}", capitalize(&env), n)
            };
            out.push(Block::Header { level: 3, text: label });
            continue;
          }

          if env == "itemize" || env == "description" {
            list_stack.push(ListKind::Itemize);
            continue;
          }
          if env == "enumerate" {
            list_stack.push(ListKind::Enumerate(0));
            continue;
          }

          if matches!(env.as_str(), "quote" | "quotation" | "epigraph") {
            let (body_text, adv) = read_until_end(&text, i, &env);
            i += adv;
            flush_builder(&mut builder, &mut list_item_pending, &mut out);
            let cleaned = clean_inline(body_text.replace('\n', " "));
            if !cleaned.trim().is_empty() {
              out.push(Block::Quote(vec![InlineSpan::italic(cleaned.trim().to_string())]));
            }
            continue;
          }

          if matches!(env.as_str(), "table" | "table*") {
            let (body_text, adv) = read_until_end(&text, i, &env);
            i += adv;
            flush_builder(&mut builder, &mut list_item_pending, &mut out);
            // Caption goes ABOVE the table (academic convention for tables;
            // figures place caption below, handled in the CAPTION_ENVS arm).
            if let Some(cap) = extract_caption(&body_text) {
              out.push(Block::Line(format!("[Table: {}]", cap)));
            }
            for tab_env in TABULAR_ENVS {
              if let Some(tab_body) = extract_env(&body_text, tab_env) {
                // Pre-expand zero-arg custom macros so that e.g. \dmodel → d_{\text{model}}
                // before cell splitting and math rendering.
                let expanded_body = expand_zero_arg_macros(&tab_body, macros);
                out.extend(parse_tabular(&expanded_body));
                break;
              }
            }
            out.push(Block::Blank);
            continue;
          }

          if CAPTION_ENVS.contains(&env.as_str()) {
            let (body_text, adv) = read_until_end(&text, i, &env);
            i += adv;
            flush_builder(&mut builder, &mut list_item_pending, &mut out);
            if let Some(cap) = extract_caption(&body_text) {
              out.push(Block::Line(format!("[Figure: {}]", cap)));
              out.push(Block::Blank);
            }
            continue;
          }

          if TABULAR_ENVS.contains(&env.as_str()) {
            let (body_text, adv) = read_until_end(&text, i, &env);
            i += adv;
            flush_builder(&mut builder, &mut list_item_pending, &mut out);
            let expanded_body = expand_zero_arg_macros(&body_text, macros);
            out.extend(parse_tabular(&expanded_body));
            continue;
          }

          if CODE_ENVS.contains(&env.as_str()) {
            let (body_text, adv) = read_until_end(&text, i, &env);
            i += adv;
            flush_builder(&mut builder, &mut list_item_pending, &mut out);
            let lang = if env.starts_with("lstlisting") || env == "minted" {
              extract_lstlisting_lang(&body_text)
            } else { None };
            let skip_opts = body_text.trim_start().starts_with('[');
            let lines: Vec<String> = body_text.lines()
              .skip(if skip_opts { 1 } else { 0 })
              .map(|l| l.to_string())
              .collect();
            if !lines.is_empty() { out.push(Block::CodeBlock { lang, lines }); }
            continue;
          }

          if ALGO_ENVS.contains(&env.as_str()) {
            let (body_text, adv) = read_until_end(&text, i, &env);
            i += adv;
            flush_builder(&mut builder, &mut list_item_pending, &mut out);
            // Outer \begin{algorithm} wrapper may contain a caption and an inner
            // \begin{algorithmic} — extract the inner body if present.
            let inner = extract_env(&body_text, "algorithmic")
              .or_else(|| extract_env(&body_text, "algorithmicx"))
              .unwrap_or_else(|| body_text.clone());
            if let Some(cap) = extract_caption(&body_text) {
              out.push(Block::Line(format!("[Algorithm: {}]", cap)));
            }
            let lines = parse_algorithmic_body(&inner);
            if !lines.is_empty() {
              out.push(Block::CodeBlock { lang: Some("algorithm".to_string()), lines });
            }
            continue;
          }

          if env == "thebibliography" {
            // Skip the required {widestlabel} arg that follows \begin{thebibliography}.
            if i < len && text[i] == '{' {
              let (_, skip) = read_braced_arg(&text, i);
              i += skip;
            }
            let (body_text, adv) = read_until_end(&text, i, &env);
            i += adv;
            flush_builder(&mut builder, &mut list_item_pending, &mut out);
            out.push(Block::Blank);
            out.push(Block::Header { level: 1, text: "References".to_string() });
            out.push(Block::Blank);
            for block in parse_bibliography(&body_text, label_map) {
              out.push(block);
            }
            continue;
          }

          if FULL_SKIP_ENVS.contains(&env.as_str()) {
            let (_body_text, adv) = read_until_end(&text, i, &env);
            i += adv;
            continue;
          }

          continue;
        }

        "section" | "section*" => {
          let (title, skip) = read_braced_arg(&text, i);
          i += skip;
          let title = clean_inline(title.trim().to_string());
          flush_builder(&mut builder, &mut list_item_pending, &mut out);
          let numbered = if cmd == "section" {
            sec[0] += 1; sec[1] = 0; sec[2] = 0;
            format!("{}  {}", sec[0], title)
          } else {
            title
          };
          out.push(Block::Blank);
          out.push(Block::Header { level: 1, text: numbered });
          out.push(Block::Blank);
        }
        "subsection" | "subsection*" => {
          let (title, skip) = read_braced_arg(&text, i);
          i += skip;
          let title = clean_inline(title.trim().to_string());
          flush_builder(&mut builder, &mut list_item_pending, &mut out);
          let numbered = if cmd == "subsection" {
            sec[1] += 1; sec[2] = 0;
            format!("{}.{}  {}", sec[0], sec[1], title)
          } else {
            title
          };
          out.push(Block::Blank);
          out.push(Block::Header { level: 2, text: numbered });
          out.push(Block::Blank);
        }
        "subsubsection" | "subsubsection*" | "paragraph" => {
          let (title, skip) = read_braced_arg(&text, i);
          i += skip;
          let title = clean_inline(title.trim().to_string());
          flush_builder(&mut builder, &mut list_item_pending, &mut out);
          let numbered = if cmd == "subsubsection" {
            sec[2] += 1;
            format!("{}.{}.{}  {}", sec[0], sec[1], sec[2], title)
          } else {
            title
          };
          out.push(Block::Header { level: 3, text: numbered });
        }

        // Inline styling — each variant sets the appropriate style flag.
        "textbf" | "mathbf" => {
          let (content, skip) = read_braced_arg(&text, i);
          i += skip;
          builder.push_styled(content, |s| InlineSpan { bold: true, ..s });
        }
        "textit" | "emph" | "mathit" => {
          let (content, skip) = read_braced_arg(&text, i);
          i += skip;
          builder.push_styled(content, |s| InlineSpan { italic: true, ..s });
        }
        "texttt" | "mathtt" | "textnormal" => {
          let (content, skip) = read_braced_arg(&text, i);
          i += skip;
          builder.push_styled(content, |s| InlineSpan { monospace: true, ..s });
        }
        "underline" | "uline" => {
          let (content, skip) = read_braced_arg(&text, i);
          i += skip;
          builder.push_styled(content, |s| InlineSpan { underline: true, ..s });
        }
        "sout" => {
          let (content, skip) = read_braced_arg(&text, i);
          i += skip;
          builder.push_styled(content, |s| InlineSpan { strikethrough: true, ..s });
        }
        // Commands that carry content but no renderable style difference:
        "text" | "mathrm" | "mathcal" | "mathbb" | "overline"
        | "textsubscript" | "textsuperscript"
        | "textsf" | "textsc" | "textsl" | "textrm" | "textmd" | "textup" => {
          let (content, skip) = read_braced_arg(&text, i);
          i += skip;
          builder.push_plain(&content);
        }

        // Text-mode symbol commands with no braced argument.
        "textbackslash" => builder.push_char('\\'),
        "textless"      => builder.push_char('<'),
        "textgreater"   => builder.push_char('>'),
        "textbar"       => builder.push_char('|'),
        "textasciitilde"    => builder.push_char('~'),
        "textasciicircum"   => builder.push_char('^'),

        "textcolor" | "colorbox" | "fbox" | "mbox" | "makebox" => {
          let (_opt, skip1) = read_braced_arg(&text, i);
          i += skip1;
          let (content, skip2) = read_braced_arg(&text, i);
          i += skip2;
          builder.push_plain(&content);
        }

        // Inline verbatim: \verb|...|
        "verb" => {
          if i < len {
            let delim = text[i];
            i += 1;
            let start = i;
            while i < len && text[i] != delim { i += 1; }
            let verbatim: String = text[start..i].iter().collect();
            if i < len { i += 1; }
            builder.push_styled(verbatim, |s| InlineSpan { monospace: true, ..s });
          }
        }

        // Ellipsis commands → Unicode.
        "ldots" | "cdots" | "dots" | "dotsc" | "dotsb" | "dotsi" => {
          builder.push_char('…');
        }

        // Special letter commands → Unicode.
        "ss" => builder.push_char('ß'),
        "ae" => builder.push_char('æ'),
        "AE" => builder.push_char('Æ'),
        "oe" => builder.push_char('œ'),
        "OE" => builder.push_char('Œ'),
        "aa" => builder.push_char('å'),
        "AA" => builder.push_char('Å'),
        "o"  => builder.push_char('ø'),
        "O"  => builder.push_char('Ø'),
        "l"  => builder.push_char('ł'),
        "L"  => builder.push_char('Ł'),
        "i"  => builder.push_char('ı'),

        // Non-alphabetic accent commands: \' \" \` \^ \~ \. \=
        "'" | "`" | "\"" | "^" | "~" | "." | "=" => {
          let (base, skip) = read_accent_arg(&text, i);
          i += skip;
          match accent_char(&cmd, base.trim()) {
            Some(ch) => builder.push_char(ch),
            None => builder.push_plain(base.trim()),
          }
        }
        // Alphabetic accent commands: \c \H \v \k \u \r
        "c" | "H" | "v" | "k" | "u" | "r" => {
          if i < len && (text[i] == '{' || text[i].is_alphabetic()) {
            let (base, skip) = read_accent_arg(&text, i);
            i += skip;
            match accent_char(&cmd, base.trim()) {
              Some(ch) => builder.push_char(ch),
              None => builder.push_plain(base.trim()),
            }
          }
        }

        // Backslash-space → literal space.
        " " => builder.push_char(' '),

        // LaTeX special characters — \% \$ \& \# \_ \{ \} → literal character.
        "%" => builder.push_char('%'),
        "$" => builder.push_char('$'),
        "&" => builder.push_char('&'),
        "#" => builder.push_char('#'),
        "_" => builder.push_char('_'),
        "{" => builder.push_char('{'),
        "}" => builder.push_char('}'),

        "color" | "bibliography" | "bibliographystyle" | "maketitle"
        | "tableofcontents" | "newcommand" | "renewcommand" | "providecommand"
        | "setlength" | "addtolength" | "setcounter" | "addtocounter"
        | "usepackage" | "RequirePackage" | "PassOptionsToPackage"
        | "geometry" | "vspace*" | "hspace" | "hspace*" | "rule"
        | "includegraphics" | "captionsetup" | "caption" | "subcaption"
        | "pagestyle" | "thispagestyle" | "pagenumbering"
        | "definecolor" | "colorlet" | "DeclareMathOperator"
        | "theoremstyle" | "newtheorem" | "newenvironment" | "renewenvironment"
        | "crefname" | "Crefname" | "hypersetup" | "setcitestyle"
        | "IEEEauthorblockN" | "IEEEauthorblockA" | "institute"
        | "affil" | "address" | "email" | "date" => {
          while i < len && (text[i] == '{' || text[i] == '[') {
            if text[i] == '[' {
              while i < len && text[i] != ']' { i += 1; }
              if i < len { i += 1; }
            } else {
              let (_, skip) = read_braced_arg(&text, i);
              i += skip;
            }
          }
        }

        "cite" | "citep" | "citet" | "citealt" | "citealp" | "citeauthor"
        | "citeyear" | "nocite" => {
          if i < len && text[i] == '[' {
            while i < len && text[i] != ']' { i += 1; }
            if i < len { i += 1; }
          }
          let (key, skip) = read_braced_arg(&text, i);
          i += skip;
          if cmd != "nocite" {
            let nums: Vec<String> = key.split(',').map(|k| {
              label_map.bibitems.get(k.trim())
                .map(|n| n.to_string())
                .unwrap_or_else(|| "?".to_string())
            }).collect();
            builder.push_plain(&format!("[{}]", nums.join(", ")));
          }
        }
        "ref" | "cref" | "Cref" | "autoref" | "vref" | "nameref" | "pageref" => {
          let (key, skip) = read_braced_arg(&text, i);
          i += skip;
          let resolved = label_map.labels.get(key.trim())
            .cloned().unwrap_or_else(|| "?".to_string());
          builder.push_plain(&resolved);
        }
        "eqref" => {
          let (key, skip) = read_braced_arg(&text, i);
          i += skip;
          let resolved = label_map.labels.get(key.trim())
            .cloned().unwrap_or_else(|| "(?)".to_string());
          builder.push_plain(&resolved);
        }
        "label" => {
          let (_key, skip) = read_braced_arg(&text, i);
          i += skip;
        }
        "footnote" | "footnotetext" => {
          if i < len && text[i] == '[' {
            while i < len && text[i] != ']' { i += 1; }
            if i < len { i += 1; }
          }
          let (note, skip) = read_braced_arg(&text, i);
          i += skip;
          let n = footnotes.len() + 1;
          footnotes.push(render_text_with_math(&note, macros));
          builder.push_plain(&format!("[{}]", n));
        }
        "hyperref" => {
          if i < len && text[i] == '[' {
            while i < len && text[i] != ']' { i += 1; }
            if i < len { i += 1; }
          }
          let (content, skip) = read_braced_arg(&text, i);
          i += skip;
          builder.push_plain(&content);
        }
        "url" | "href" => {
          let (url, skip) = read_braced_arg(&text, i);
          i += skip;
          if cmd == "href" {
            let (display, skip2) = read_braced_arg(&text, i);
            i += skip2;
            builder.push_url(&display, url);
          } else {
            let u = url.trim().to_string();
            builder.push_url(&u.clone(), u);
          }
        }
        "item" => {
          if i < len && text[i] == '[' {
            while i < len && text[i] != ']' { i += 1; }
            if i < len { i += 1; }
          }
          flush_builder(&mut builder, &mut list_item_pending, &mut out);
          let marker = match list_stack.last_mut() {
            Some(ListKind::Enumerate(n)) => { *n += 1; format!("{}. ", n) }
            _ => "• ".to_string(),
          };
          let depth = list_stack.len().saturating_sub(1) as u8;
          list_item_pending = Some((depth, marker));
        }

        // Inline math \( ... \)
        "(" => {
          let (math, adv) = read_until_str(&text, i, r"\)");
          i += adv;
          let rendered = render_math(&math, macros);
          let flat = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
          builder.push_plain(&flat);
        }
        // Display math \[ ... \]
        "[" => {
          let (math, adv) = read_until_str(&text, i, r"\]");
          i += adv;
          flush_builder(&mut builder, &mut list_item_pending, &mut out);
          let rendered = render_math(&math, macros);
          let lines: Vec<String> = rendered.lines().map(|l| l.to_string()).collect();
          if !lines.is_empty() { out.push(Block::DisplayMath { lines, num: None }); }
        }

        "\\" | "newline" => flush_builder(&mut builder, &mut list_item_pending, &mut out),

        // Table rules → horizontal separator block.
        "hline" | "toprule" | "midrule" | "bottomrule" | "cline" => {
          flush_builder(&mut builder, &mut list_item_pending, &mut out);
          out.push(Block::Rule);
        }

        "par" | "medskip" | "bigskip" | "smallskip" | "vspace" | "vskip" => {
          let _ = if cmd == "vspace" || cmd == "vskip" {
            let (_arg, skip) = read_braced_arg(&text, i);
            i += skip;
          };
          flush_builder(&mut builder, &mut list_item_pending, &mut out);
          out.push(Block::Blank);
        }

        _ => {
          if let Some((arity, def)) = macros.get(cmd.as_str()) {
            let def = def.clone();
            match arity {
              0 => {
                let expanded = if def.contains('\\') {
                  render_math(&def, macros)
                } else {
                  def
                };
                if expanded.contains('\n') {
                  flush_builder(&mut builder, &mut list_item_pending, &mut out);
                  let lines: Vec<String> =
                    expanded.lines().map(|l| l.to_string()).collect();
                  out.push(Block::DisplayMath { lines, num: None });
                } else {
                  builder.push_plain(&expanded);
                }
              }
              1 => {
                if i < len && text[i] == '{' {
                  let (arg, skip) = read_braced_arg(&text, i);
                  i += skip;
                  let substituted = def.replace("#1", &arg);
                  let expanded = if substituted.contains('\\') {
                    render_math(&substituted, macros)
                  } else {
                    substituted
                  };
                  if expanded.contains('\n') {
                    flush_builder(&mut builder, &mut list_item_pending, &mut out);
                    let lines: Vec<String> =
                      expanded.lines().map(|l| l.to_string()).collect();
                    out.push(Block::DisplayMath { lines, num: None });
                  } else {
                    builder.push_plain(&expanded);
                  }
                }
              }
              _ => {}
            }
          } else if i < len && text[i] == '{' {
            let (content, skip) = read_braced_arg(&text, i);
            i += skip;
            builder.push_plain(&content);
          }
        }
      }
      continue;
    }

    // Inline math $...$ (single dollar sign, not $$).
    if c == '$' {
      if i + 1 < len && text[i + 1] == '$' {
        i += 2;
        let (math, adv) = read_until_double_dollar(&text, i);
        i += adv;
        flush_builder(&mut builder, &mut list_item_pending, &mut out);
        let rendered = render_math(&math, macros);
        let lines: Vec<String> = rendered.lines().map(|l| l.to_string()).collect();
        if !lines.is_empty() { out.push(Block::DisplayMath { lines, num: None }); }
      } else {
        i += 1;
        let (math, adv) = read_until_single_dollar(&text, i);
        i += adv;
        let rendered = render_math(&math, macros);
        let flat = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
        builder.push_plain(&flat);
      }
      continue;
    }

    // Non-breaking space → regular space.
    if c == '~' {
      builder.push_char(' ');
      i += 1;
      continue;
    }

    // Dash ligatures: --- → em dash, -- → en dash.
    if c == '-' {
      if i + 2 < len && text[i + 1] == '-' && text[i + 2] == '-' {
        builder.push_char('—');
        i += 3;
        continue;
      } else if i + 1 < len && text[i + 1] == '-' {
        builder.push_char('–');
        i += 2;
        continue;
      }
    }

    // Strip bare grouping braces — content is handled when the command is read.
    if c == '{' || c == '}' {
      i += 1;
      continue;
    }

    if c == '\n' {
      if i + 1 < len && text[i + 1] == '\n' {
        flush_builder(&mut builder, &mut list_item_pending, &mut out);
        out.push(Block::Blank);
        while i < len && text[i] == '\n' { i += 1; }
        continue;
      } else {
        builder.push_char(' ');
      }
    } else {
      builder.push_char(c);
    }
    i += 1;
  }

  flush_builder(&mut builder, &mut list_item_pending, &mut out);
  wrap_blocks(out)
}

// ── Math rendering ────────────────────────────────────────────────────────────

/// Expand user-defined macros in a math string, then render to Unicode.
fn render_math(math: &str, macros: &HashMap<String, (usize, String)>) -> String {
  let expanded = expand_math_macros(math.trim(), macros, 0);
  // Normalize: \_ → _ (escaped underscore valid in text, redundant in math).
  let cleaned = expanded.replace(r"\_", "_");
  render(MathInput::Latex(cleaned.trim()))
}

/// Recursively expand user-defined macros inside a LaTeX math string.
fn expand_math_macros(
  math: &str,
  macros: &HashMap<String, (usize, String)>,
  depth: usize,
) -> String {
  if depth > 8 || macros.is_empty() {
    return math.to_string();
  }
  let chars: Vec<char> = math.chars().collect();
  let len = chars.len();
  let mut out = String::new();
  let mut i = 0;
  while i < len {
    if chars[i] != '\\' || i + 1 >= len {
      out.push(chars[i]);
      i += 1;
      continue;
    }
    let (cmd, consumed) = read_command(&chars, i + 1);
    if let Some((arity, def)) = macros.get(cmd.as_str()) {
      i += 1 + consumed;
      let def = def.clone();
      match arity {
        0 => out.push_str(&expand_math_macros(&def, macros, depth + 1)),
        1 => {
          if i < len && chars[i] == '{' {
            let (arg, skip) = read_braced_arg(&chars, i);
            i += skip;
            let substituted = def.replace("#1", &arg);
            out.push_str(&expand_math_macros(&substituted, macros, depth + 1));
          } else {
            out.push('\\');
            out.push_str(&cmd);
          }
        }
        _ => {
          out.push('\\');
          out.push_str(&cmd);
        }
      }
    } else {
      out.push('\\');
      out.push_str(&cmd);
      i += 1 + consumed;
    }
  }
  out
}

/// Render a prose string that may contain inline `$...$` math.
/// Used for footnotes so that math in notes is rendered, not left as raw LaTeX.
fn render_text_with_math(s: &str, macros: &HashMap<String, (usize, String)>) -> String {
  let chars: Vec<char> = s.chars().collect();
  let mut out = String::new();
  let mut i = 0;
  while i < chars.len() {
    if chars[i] == '%' && (i == 0 || chars[i - 1] != '\\') {
      while i < chars.len() && chars[i] != '\n' { i += 1; }
      continue;
    }
    if chars[i] == '$' {

      i += 1;
      let (math, adv) = read_until_single_dollar(&chars, i);
      i += adv;
      let rendered = render_math(math.trim(), macros);
      // Collapse multi-line math to a single line for inline footnote context.
      let flat = rendered
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
      out.push_str(&flat);
      continue;
    }
    if chars[i] == '~' {
      out.push(' ');
      i += 1;
      continue;
    }
    if chars[i] == '\\' && i + 1 < chars.len() {
      let (cmd, consumed) = read_command(&chars, i + 1);
      i += 1 + consumed;
      match cmd.as_str() {
        "thanks" | "footnote" | "footnotemark" => {
          if i < chars.len() && chars[i] == '{' {
            let (_, skip) = read_braced_arg(&chars, i);
            i += skip;
          }
        }
        "%" | "$" | "&" | "#" | "_" | "{" | "}" => {
          out.push(cmd.chars().next().unwrap());
        }
        _ => {
          if i < chars.len() && chars[i] == '{' {
            let (content, skip) = read_braced_arg(&chars, i);
            i += skip;
            out.push_str(&content);
          }
        }
      }
      continue;
    }
    out.push(chars[i]);
    i += 1;
  }
  out.trim().to_string()
}

// ── Accent helpers ────────────────────────────────────────────────────────────

/// Read an accent argument: either `{letter}` or a bare letter.
fn read_accent_arg(text: &[char], start: usize) -> (String, usize) {
  if start >= text.len() {
    return (String::new(), 0);
  }
  if text[start] == '{' {
    read_braced_arg(text, start)
  } else {
    (text[start].to_string(), 1)
  }
}

/// Map a LaTeX accent command + base letter to a Unicode character.
fn accent_char(accent: &str, base: &str) -> Option<char> {
  match (accent, base) {
    // Acute \'
    ("'","a")=>Some('á'),("'","e")=>Some('é'),("'","i")=>Some('í'),
    ("'","o")=>Some('ó'),("'","u")=>Some('ú'),("'","y")=>Some('ý'),
    ("'","A")=>Some('Á'),("'","E")=>Some('É'),("'","I")=>Some('Í'),
    ("'","O")=>Some('Ó'),("'","U")=>Some('Ú'),("'","Y")=>Some('Ý'),
    ("'","n")=>Some('ń'),("'","c")=>Some('ć'),("'","s")=>Some('ś'),
    ("'","z")=>Some('ź'),("'","l")=>Some('ĺ'),
    // Grave \`
    ("`","a")=>Some('à'),("`","e")=>Some('è'),("`","i")=>Some('ì'),
    ("`","o")=>Some('ò'),("`","u")=>Some('ù'),
    ("`","A")=>Some('À'),("`","E")=>Some('È'),("`","I")=>Some('Ì'),
    ("`","O")=>Some('Ò'),("`","U")=>Some('Ù'),
    // Umlaut \"
    ("\"","a")=>Some('ä'),("\"","e")=>Some('ë'),("\"","i")=>Some('ï'),
    ("\"","o")=>Some('ö'),("\"","u")=>Some('ü'),("\"","y")=>Some('ÿ'),
    ("\"","A")=>Some('Ä'),("\"","E")=>Some('Ë'),("\"","I")=>Some('Ï'),
    ("\"","O")=>Some('Ö'),("\"","U")=>Some('Ü'),
    // Circumflex \^
    ("^","a")=>Some('â'),("^","e")=>Some('ê'),("^","i")=>Some('î'),
    ("^","o")=>Some('ô'),("^","u")=>Some('û'),
    ("^","A")=>Some('Â'),("^","E")=>Some('Ê'),("^","I")=>Some('Î'),
    ("^","O")=>Some('Ô'),("^","U")=>Some('Û'),
    // Tilde \~
    ("~","a")=>Some('ã'),("~","n")=>Some('ñ'),("~","o")=>Some('õ'),
    ("~","A")=>Some('Ã'),("~","N")=>Some('Ñ'),("~","O")=>Some('Õ'),
    // Cedilla \c
    ("c","c")=>Some('ç'),("c","C")=>Some('Ç'),
    ("c","s")=>Some('ş'),("c","S")=>Some('Ş'),
    // Double acute \H
    ("H","o")=>Some('ő'),("H","O")=>Some('Ő'),
    ("H","u")=>Some('ű'),("H","U")=>Some('Ű'),
    // Caron \v
    ("v","s")=>Some('š'),("v","S")=>Some('Š'),
    ("v","c")=>Some('č'),("v","C")=>Some('Č'),
    ("v","z")=>Some('ž'),("v","Z")=>Some('Ž'),
    ("v","r")=>Some('ř'),("v","R")=>Some('Ř'),
    ("v","n")=>Some('ň'),("v","N")=>Some('Ň'),
    ("v","e")=>Some('ě'),("v","E")=>Some('Ě'),
    // Ogonek \k
    ("k","a")=>Some('ą'),("k","A")=>Some('Ą'),
    ("k","e")=>Some('ę'),("k","E")=>Some('Ę'),
    // Ring \r
    ("r","a")=>Some('å'),("r","A")=>Some('Å'),
    // Breve \u
    ("u","a")=>Some('ă'),("u","A")=>Some('Ă'),
    ("u","e")=>Some('ĕ'),("u","o")=>Some('ŏ'),("u","u")=>Some('ŭ'),
    // Macron \=
    ("=","a")=>Some('ā'),("=","e")=>Some('ē'),("=","i")=>Some('ī'),
    ("=","o")=>Some('ō'),("=","u")=>Some('ū'),
    ("=","A")=>Some('Ā'),("=","E")=>Some('Ē'),("=","I")=>Some('Ī'),
    ("=","O")=>Some('Ō'),("=","U")=>Some('Ū'),
    _ => None,
  }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn flush_builder(
  builder: &mut InlineBuilder,
  list_item: &mut Option<(u8, String)>,
  out: &mut Vec<Block>,
) {
  if builder.is_empty() && list_item.is_none() {
    return;
  }
  match (builder.finish(), list_item.take()) {
    (Some(spans), Some((depth, marker))) => {
      out.push(Block::ListItem { depth, marker, content: spans });
    }
    (Some(spans), None) => {
      out.push(Block::StyledLine(spans));
    }
    (None, Some((depth, marker))) => {
      let text = builder.take_plain();
      let trimmed = text.trim().to_string();
      if !trimmed.is_empty() {
        out.push(Block::ListItem { depth, marker, content: vec![InlineSpan::plain(trimmed)] });
      }
    }
    (None, None) => {
      let text = builder.take_plain();
      let trimmed = text.trim().to_string();
      if !trimmed.is_empty() {
        out.push(Block::Line(trimmed));
      }
    }
  }
}

/// Parse the optional `[language=xxx]` argument on the first line of a lstlisting body.
fn extract_lstlisting_lang(body: &str) -> Option<String> {
  let first = body.trim_start().lines().next().unwrap_or("");
  if first.starts_with('[') {
    if let Some(pos) = first.find("language=") {
      let rest = &first[pos + 9..];
      let lang: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '+' || *c == '#')
        .collect();
      if !lang.is_empty() {
        return Some(lang);
      }
    }
  }
  None
}

/// Parse an algorithmic/algorithmicx/algorithm2e body into plain-text pseudocode lines.
/// Structural commands (`\If`, `\For`, `\While`, `\Function`, `\Procedure`) open indented
/// blocks; their matching `\End*` commands close them. Leaf commands (`\State`, `\Return`,
/// `\Require`, `\Ensure`, `\Comment`) emit a single line at the current indent level.
fn parse_algorithmic_body(body: &str) -> Vec<String> {
  let mut lines: Vec<String> = Vec::new();
  let mut indent: usize = 0;
  let chars: Vec<char> = body.chars().collect();
  let len = chars.len();
  let mut i = 0;

  let pad = |n: usize| "  ".repeat(n);

  while i < len {
    // Skip whitespace between commands.
    while i < len && (chars[i] == ' ' || chars[i] == '\t' || chars[i] == '\n') { i += 1; }
    if i >= len { break; }

    // Comments: % …
    if chars[i] == '%' {
      while i < len && chars[i] != '\n' { i += 1; }
      continue;
    }

    if chars[i] != '\\' {
      i += 1;
      continue;
    }

    let (cmd, consumed) = read_command(&chars, i + 1);
    i += 1 + consumed;

    // Peek at optional [label] or {arg} immediately following.
    let read_arg = |chars: &[char], i: &mut usize| -> String {
      if *i >= chars.len() { return String::new(); }
      if chars[*i] == '{' {
        let (s, skip) = read_braced_arg(chars, *i);
        *i += skip;
        s
      } else if chars[*i] == '[' {
        let start = *i + 1;
        *i += 1;
        while *i < chars.len() && chars[*i] != ']' { *i += 1; }
        let s: String = chars[start..*i].iter().collect();
        if *i < chars.len() { *i += 1; }
        s
      } else {
        String::new()
      }
    };

    match cmd.to_ascii_lowercase().as_str() {
      // Block-opening: structural commands with condition/name args.
      "if" | "elsif" | "elseif" | "elif" => {
        let cond = read_arg(&chars, &mut i);
        lines.push(format!("{}if {}:", pad(indent), cond.trim()));
        indent += 1;
      }
      "else" => {
        indent = indent.saturating_sub(1);
        lines.push(format!("{}else:", pad(indent)));
        indent += 1;
      }
      "endif" | "endifx" => { indent = indent.saturating_sub(1); }

      "for" | "foreach" | "forall" | "loop" => {
        let var = read_arg(&chars, &mut i);
        lines.push(format!("{}for {}:", pad(indent), var.trim()));
        indent += 1;
      }
      "endfor" | "endforeach" | "endloop" => { indent = indent.saturating_sub(1); }

      "while" => {
        let cond = read_arg(&chars, &mut i);
        lines.push(format!("{}while {}:", pad(indent), cond.trim()));
        indent += 1;
      }
      "endwhile" => { indent = indent.saturating_sub(1); }

      "repeat" => {
        lines.push(format!("{}repeat:", pad(indent)));
        indent += 1;
      }
      "until" => {
        indent = indent.saturating_sub(1);
        let cond = read_arg(&chars, &mut i);
        lines.push(format!("{}until {}", pad(indent), cond.trim()));
      }

      "function" | "procedure" => {
        let name = read_arg(&chars, &mut i);
        let params = read_arg(&chars, &mut i);
        let kw = capitalize(&cmd);
        lines.push(format!("{}{}({}):", pad(indent), name.trim(), params.trim()));
        let _ = kw;
        indent += 1;
      }
      "endfunction" | "endprocedure" => { indent = indent.saturating_sub(1); }

      // Leaf: emit a single line.
      "state" | "statex" => {
        let content = read_arg(&chars, &mut i);
        if !content.trim().is_empty() {
          lines.push(format!("{}{}", pad(indent), content.trim()));
        }
      }
      "return" => {
        let content = read_arg(&chars, &mut i);
        lines.push(format!("{}return {}", pad(indent), content.trim()));
      }
      "require" | "input" => {
        let content = read_arg(&chars, &mut i);
        lines.push(format!("{}Input: {}", pad(indent), content.trim()));
      }
      "ensure" | "output" => {
        let content = read_arg(&chars, &mut i);
        lines.push(format!("{}Output: {}", pad(indent), content.trim()));
      }
      "comment" | "linecomment" => {
        let content = read_arg(&chars, &mut i);
        if let Some(last) = lines.last_mut() {
          last.push_str(&format!("  // {}", content.trim()));
        } else {
          lines.push(format!("{}// {}", pad(indent), content.trim()));
        }
      }
      "print" | "printline" => {
        let content = read_arg(&chars, &mut i);
        lines.push(format!("{}print {}", pad(indent), content.trim()));
      }
      // Ignore structural/formatting commands that don't affect content.
      "algorithmic" | "algorithmicx" | "begin" | "end"
      | "algrenewcommand" | "algnewcommand" | "algblock"
      | "algrequire" | "algensure" | "algsetblock"
      | "caption" | "label" | "vspace" | "hspace" => {}
      _ => {}
    }
  }

  lines
}

/// Render a `\begin{thebibliography}` body into numbered `Block::Line` entries.
/// Each `\bibitem{key}` starts a new entry; its number comes from `label_map.bibitems`.
fn parse_bibliography(body: &str, label_map: &LabelMap) -> Vec<Block> {
  let mut out: Vec<Block> = Vec::new();
  // Split on \bibitem occurrences.
  let mut rest = body.trim();
  while let Some(pos) = rest.find(r"\bibitem") {
    rest = &rest[pos + 8..];
    // Skip optional [label].
    if rest.starts_with('[') {
      if let Some(end) = rest.find(']') {
        rest = &rest[end + 1..];
      }
    }
    // Read {key}.
    if !rest.starts_with('{') { continue; }
    if let Some(end) = rest.find('}') {
      let key = rest[1..end].trim().to_string();
      rest = &rest[end + 1..];
      let num = label_map.bibitems.get(key.as_str()).copied().unwrap_or(0);
      // Content runs until the next \bibitem or end of body.
      let content_end = rest.find(r"\bibitem").unwrap_or(rest.len());
      let raw = rest[..content_end].trim();
      // Strip LaTeX commands: remove \ sequences and braces for clean plain text.
      let clean = clean_bib_entry(raw);
      if !clean.is_empty() {
        out.push(Block::Line(format!("[{}] {}", num, clean)));
      }
    }
  }
  out
}

/// Strip common LaTeX markup from a bibliography entry for plain-text display.
fn clean_bib_entry(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  let chars: Vec<char> = s.chars().collect();
  let len = chars.len();
  let mut i = 0;
  while i < len {
    if chars[i] == '%' {
      while i < len && chars[i] != '\n' { i += 1; }
      continue;
    }
    if chars[i] == '\\' && i + 1 < len {
      let (cmd, consumed) = read_command(&chars, i + 1);
      i += 1 + consumed;
      // For known decorators that wrap text, emit the text.
      if i < len && chars[i] == '{' {
        let (content, skip) = read_braced_arg(&chars, i);
        i += skip;
        // Recurse so that commands like \em inside {\em journal} are stripped.
        out.push_str(&clean_bib_entry(&content));
      }
      continue;
    }
    if chars[i] == '{' || chars[i] == '}' { i += 1; continue; }
    out.push(chars[i]);
    i += 1;
  }
  // Collapse whitespace.
  out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Move table groups (Rule/Matrix blocks + optional "[Table:...]" caption) that
/// appear directly before a section heading to just after that heading.
/// This corrects for LaTeX float placement: authors often write \begin{table}
/// in the source just before the section that discusses it, expecting the float
/// algorithm to position it at the top of that section's page.
fn wrap_blocks(blocks: Vec<Block>) -> Vec<Block> {
  let mut out = Vec::new();
  let mut last_was_blank = false;
  for block in blocks {
    match block {
      Block::Blank => {
        if !last_was_blank {
          out.push(Block::Blank);
          last_was_blank = true;
        }
      }
      Block::Line(s) => {
        last_was_blank = false;
        for wrapped in textwrap::wrap(&s, WRAP_WIDTH) {
          out.push(Block::Line(wrapped.to_string()));
        }
      }
      other => {
        last_was_blank = false;
        out.push(other);
      }
    }
  }
  out
}

fn capitalize(s: &str) -> String {
  let mut chars = s.chars();
  match chars.next() {
    None => String::new(),
    Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
  }
}

/// Extract `\newcommand` / `\renewcommand` / `\providecommand` definitions.
fn extract_macros(content: &str) -> HashMap<String, (usize, String)> {
  let mut map = HashMap::new();
  let chars: Vec<char> = content.chars().collect();
  let len = chars.len();
  let mut i = 0;
  while i < len {
    if chars[i] != '\\' { i += 1; continue; }
    let (cmd, consumed) = read_command(&chars, i + 1);
    i += 1 + consumed;
    if !matches!(cmd.as_str(), "newcommand" | "renewcommand" | "providecommand") {
      continue;
    }
    if i < len && chars[i] == '*' { i += 1; }
    while i < len && chars[i] == ' ' { i += 1; }

    let name = if i < len && chars[i] == '{' {
      let (raw, skip) = read_braced_arg(&chars, i);
      i += skip;
      raw.trim_start_matches('\\').trim().to_string()
    } else if i < len && chars[i] == '\\' {
      let (n, c2) = read_command(&chars, i + 1);
      i += 1 + c2;
      n
    } else {
      continue;
    };
    if name.is_empty() { continue; }

    while i < len && chars[i] == ' ' { i += 1; }

    let arity = if i < len && chars[i] == '[' {
      i += 1;
      let mut n_str = String::new();
      while i < len && chars[i] != ']' { n_str.push(chars[i]); i += 1; }
      if i < len { i += 1; }
      n_str.trim().parse::<usize>().unwrap_or(0)
    } else {
      0
    };

    while i < len && chars[i] == ' ' { i += 1; }

    // Skip optional default value [default] for 1-arg commands.
    if i < len && chars[i] == '[' {
      while i < len && chars[i] != ']' { i += 1; }
      if i < len { i += 1; }
    }

    if i < len && chars[i] == '{' {
      let (def, skip) = read_braced_arg(&chars, i);
      i += skip;
      map.insert(name, (arity, def));
    }
  }
  map
}

/// Extract the `\caption{...}` text from a raw environment body.
fn extract_caption(body: &str) -> Option<String> {
  extract_command_arg(body, "caption").map(clean_inline)
}

/// Strip LaTeX `%`-to-end-of-line comments from a string.
fn strip_tex_comments(s: &str) -> String {
  let mut out = String::new();
  for line in s.lines() {
    let trimmed = if let Some(pos) = line.find('%') {
      if pos == 0 || !line[..pos].ends_with('\\') {
        &line[..pos]
      } else {
        line
      }
    } else {
      line
    };
    out.push_str(trimmed);
    out.push('\n');
  }
  out
}

/// Strip any leading horizontal-rule commands from a tabular row chunk.
/// Returns `(had_rule, remaining_text)`.
fn peel_row_prefix(mut text: &str) -> (bool, &str) {
  const RULE_PREFIXES: &[&str] = &[
    r"\hline", r"\toprule", r"\midrule", r"\bottomrule",
    r"\cmidrule", r"\specialrule",
  ];
  let mut had_rule = false;
  loop {
    let t = text.trim_start();
    let matched = RULE_PREFIXES.iter().find(|&&r| t.starts_with(r));
    match matched {
      None => break,
      Some(&r) => {
        had_rule = true;
        let after = &t[r.len()..];
        // Skip optional paren group first: \cmidrule(lr){2-3}
        let after = skip_paren_group(after);
        // Then skip zero or more braced args: \cmidrule{2-3}, \specialrule{w}{a}{b}
        let after = skip_braced_args(after);
        text = after;
      }
    }
  }
  (had_rule, text)
}

/// Skip one optional leading `(...)` group (for `\cmidrule(lr)`, `\cmidrule(r)`, etc.).
fn skip_paren_group(s: &str) -> &str {
  let t = s.trim_start();
  if !t.starts_with('(') { return t; }
  t.find(')').map_or(t, |end| &t[end + 1..])
}

/// Skip zero or more leading `{...}` groups (used after rule commands).
fn skip_braced_args(mut s: &str) -> &str {
  loop {
    let t = s.trim_start();
    if !t.starts_with('{') { return t; }
    let mut depth = 0usize;
    let mut end = 0;
    for (i, c) in t.char_indices() {
      match c {
        '{' => depth += 1,
        '}' => {
          depth -= 1;
          if depth == 0 { end = i + 1; break; }
        }
        _ => {}
      }
    }
    if end == 0 { return t; }
    s = &t[end..];
  }
}

/// Split a tabular row into `(content, col_span)` pairs.
/// `\multicolumn{N}{align}{content}` → `(content, N)`; ordinary cells → `(cell, 1)`.
/// `\&` is preserved as a literal `&` within a cell (not a column separator).
fn split_with_spans(row: &str) -> Vec<(String, usize)> {
  split_tabular_cells(row)
    .into_iter()
    .map(|cell| {
      let t = cell.trim();
      if t.starts_with(r"\multicolumn") {
        parse_multicolumn_cell(t).unwrap_or_else(|| (t.to_string(), 1))
      } else {
        (t.to_string(), 1)
      }
    })
    .collect()
}

/// Parse `\multicolumn{N}{align}{content}` → `(content, N)`.
fn parse_multicolumn_cell(s: &str) -> Option<(String, usize)> {
  let rest = s.trim_start_matches(r"\multicolumn");
  let chars: Vec<char> = rest.chars().collect();
  let mut i = 0;
  while i < chars.len() && chars[i] == ' ' { i += 1; }
  let (n_str, s1) = read_braced_arg(&chars, i); i += s1;
  let (_align, s2) = read_braced_arg(&chars, i); i += s2;
  let (content, _) = read_braced_arg(&chars, i);
  let n = n_str.trim().parse::<usize>().ok()?.max(1);
  Some((content, n))
}

/// Split a tabular row on `&` while respecting `\&` (escaped ampersand = literal `&` in content).
fn split_tabular_cells(row: &str) -> Vec<String> {
  let mut cells = Vec::new();
  let mut current = String::new();
  let chars: Vec<char> = row.chars().collect();
  let mut i = 0;
  while i < chars.len() {
    if chars[i] == '\\' && i + 1 < chars.len() {
      current.push(chars[i]);
      current.push(chars[i + 1]);
      i += 2;
    } else if chars[i] == '&' {
      cells.push(current.clone());
      current.clear();
      i += 1;
    } else {
      current.push(chars[i]);
      i += 1;
    }
  }
  cells.push(current);
  cells
}

/// Parse a raw tabular body into a `Block::Matrix`.
fn parse_tabular(body: &str) -> Vec<Block> {
  let body = body.trim_start();
  let body = if body.starts_with('{') {
    match body.find('}') {
      Some(p) => &body[p + 1..],
      None => body,
    }
  } else {
    body
  };

  let cleaned = strip_tex_comments(body);
  let mut blocks: Vec<Block> = Vec::new();
  let mut current_rows: Vec<Vec<(String, usize)>> = Vec::new();

  for raw_row in cleaned.split(r"\\") {
    let (had_rule, data_text) = peel_row_prefix(raw_row);
    if had_rule && !current_rows.is_empty() {
      blocks.push(Block::Matrix { rows: std::mem::take(&mut current_rows), vertical_rules: Vec::new() });
    }
    if had_rule {
      blocks.push(Block::Rule);
    }
    let trimmed_data = data_text.trim();
    if trimmed_data.is_empty() {
      continue;
    }
    let cells: Vec<(String, usize)> = split_with_spans(trimmed_data)
      .into_iter()
      .map(|(cell, span)| (clean_inline(cell), span))
      .collect();
    let has_content = cells.iter().any(|(c, _)| !c.is_empty());
    if has_content {
      current_rows.push(cells);
    }
  }
  if !current_rows.is_empty() {
    blocks.push(Block::Matrix { rows: current_rows, vertical_rules: Vec::new() });
  }
  blocks
}

// ── Low-level parsers ─────────────────────────────────────────────────────────

fn read_command(text: &[char], start: usize) -> (String, usize) {
  let mut cmd = String::new();
  let mut i = start;
  if i < text.len() && !text[i].is_alphabetic() {
    return (text[i].to_string(), 1);
  }
  while i < text.len() && text[i].is_alphabetic() {
    cmd.push(text[i]);
    i += 1;
  }
  while i < text.len() && text[i] == ' ' {
    i += 1;
  }
  (cmd, i - start)
}

fn read_braced_arg(text: &[char], start: usize) -> (String, usize) {
  if start >= text.len() || text[start] != '{' {
    return (String::new(), 0);
  }
  let mut depth = 0usize;
  let mut content = String::new();
  let mut i = start;
  while i < text.len() {
    match text[i] {
      '{' => { depth += 1; if depth > 1 { content.push('{'); } }
      '}' => {
        depth -= 1;
        if depth == 0 { i += 1; break; }
        content.push('}');
      }
      c => content.push(c),
    }
    i += 1;
  }
  (content, i - start)
}

fn read_until_end(text: &[char], start: usize, env: &str) -> (String, usize) {
  let end_marker: Vec<char> = format!(r"\end{{{env}}}").chars().collect();
  let mut content = String::new();
  let mut i = start;
  while i < text.len() {
    if text[i..].starts_with(&end_marker) {
      i += end_marker.len();
      break;
    }
    content.push(text[i]);
    i += 1;
  }
  (content, i - start)
}

fn read_until_str(text: &[char], start: usize, marker: &str) -> (String, usize) {
  let marker_chars: Vec<char> = marker.chars().collect();
  let mut content = String::new();
  let mut i = start;
  while i < text.len() {
    if text[i..].starts_with(&marker_chars) {
      i += marker_chars.len();
      break;
    }
    content.push(text[i]);
    i += 1;
  }
  (content, i - start)
}

fn read_until_single_dollar(text: &[char], start: usize) -> (String, usize) {
  let mut content = String::new();
  let mut i = start;
  while i < text.len() {
    if text[i] == '$' && (i == 0 || text[i - 1] != '\\') {
      i += 1;
      break;
    }
    content.push(text[i]);
    i += 1;
  }
  (content, i - start)
}

fn read_until_double_dollar(text: &[char], start: usize) -> (String, usize) {
  let mut content = String::new();
  let mut i = start;
  while i + 1 < text.len() {
    if text[i] == '$' && text[i + 1] == '$' {
      i += 2;
      break;
    }
    content.push(text[i]);
    i += 1;
  }
  (content, i - start)
}

/// Expand zero-argument custom macros in `text` using the document macro map.
/// This handles macros like `\dmodel` → `d_{\text{model}}` defined via `\newcommand`.
/// One-arg or multi-arg macros are left untouched (they need argument context).
fn expand_zero_arg_macros(text: &str, macros: &HashMap<String, (usize, String)>) -> String {
  let chars: Vec<char> = text.chars().collect();
  let mut out = String::new();
  let mut i = 0;
  while i < chars.len() {
    if chars[i] == '\\' && i + 1 < chars.len() {
      let (cmd, consumed) = read_command(&chars, i + 1);
      if let Some((0, template)) = macros.get(&cmd) {
        out.push_str(template);
        i += 1 + consumed;
      } else {
        out.push('\\');
        i += 1;
      }
    } else {
      out.push(chars[i]);
      i += 1;
    }
  }
  out
}

fn extract_env(body: &str, env: &str) -> Option<String> {
  let begin = format!(r"\begin{{{env}}}");
  let end = format!(r"\end{{{env}}}");
  let start = body.find(&begin)? + begin.len();
  let finish = body.find(&end)?;
  if start < finish { Some(body[start..finish].to_string()) } else { None }
}

fn extract_command_arg(text: &str, cmd: &str) -> Option<String> {
  let pattern = format!(r"\{cmd}");
  let pos = text.find(&pattern)?;
  let after = text[pos + pattern.len()..].trim_start();
  if !after.starts_with('{') { return None; }
  let chars: Vec<char> = after.chars().collect();
  let (content, _) = read_braced_arg(&chars, 0);
  Some(content)
}

fn clean_inline(s: String) -> String {
  let chars: Vec<char> = s.chars().collect();
  let mut out = String::new();
  let mut i = 0;
  while i < chars.len() {
    if chars[i] == '%' && (i == 0 || chars[i - 1] != '\\') {
      while i < chars.len() && chars[i] != '\n' { i += 1; }
      continue;
    }
    if chars[i] == '\\' && i + 1 < chars.len() {
      let (cmd, consumed) = read_command(&chars, i + 1);
      i += 1 + consumed;
      match cmd.as_str() {
        "thanks" | "footnote" | "footnotemark" => {
          if i < chars.len() && chars[i] == '{' {
            let (_, skip) = read_braced_arg(&chars, i);
            i += skip;
          }
        }
        // citation commands: discard the key arg(s), emit nothing
        "cite" | "citet" | "citep" | "citealp" | "citealt"
        | "citeauthor" | "citeyear" | "citenum" | "citeyearpar"
        | "shortcite" | "fullcite" | "autocite" | "parencite"
        | "textcite" | "footcite" => {
          // Skip optional [note] arg then the required {keys} arg
          let t = &chars[i..];
          if !t.is_empty() && t[0] == '[' {
            let end = t.iter().position(|&c| c == ']').unwrap_or(0);
            i += end + 1;
          }
          if i < chars.len() && chars[i] == '{' {
            let (_, skip) = read_braced_arg(&chars, i);
            i += skip;
          }
        }
        // \multirow{N}{align}{content}, \multicolumn{N}{align}{content}: skip 2, use 3rd
        "multirow" | "multicolumn" => {
          for _ in 0..2 {
            if i < chars.len() && chars[i] == '{' {
              let (_, skip) = read_braced_arg(&chars, i);
              i += skip;
            }
          }
          if i < chars.len() && chars[i] == '{' {
            let (content, skip) = read_braced_arg(&chars, i);
            i += skip;
            out.push_str(&clean_inline(content));
          }
        }
        // \rule{width}{height}: spacing-only, discard both args
        "rule" => {
          for _ in 0..2 {
            if i < chars.len() && chars[i] == '{' {
              let (_, skip) = read_braced_arg(&chars, i);
              i += skip;
            }
          }
        }
        // spacing/layout commands: discard the arg
        "vspace" | "hspace" | "vspace*" | "hspace*" | "vskip" | "hskip"
        | "medskip" | "smallskip" | "bigskip" | "noindent" | "centering"
        | "raggedright" | "raggedleft" | "phantom" | "vphantom" | "hphantom" => {
          if i < chars.len() && chars[i] == '{' {
            let (_, skip) = read_braced_arg(&chars, i);
            i += skip;
          }
        }
        "%" | "$" | "&" | "#" | "_" | "{" | "}" => {
          out.push(cmd.chars().next().unwrap());
        }
        _ => {
          if i < chars.len() && chars[i] == '{' {
            let (content, skip) = read_braced_arg(&chars, i);
            i += skip;
            out.push_str(&content);
          }
        }
      }
    } else if chars[i] == '~' {
      out.push(' ');
      i += 1;
    } else if chars[i] == '$' {
      // Inline math: render to Unicode via the math pipeline.
      i += 1;
      let mut math = String::new();
      while i < chars.len() && chars[i] != '$' {
        math.push(chars[i]);
        i += 1;
      }
      if i < chars.len() { i += 1; } // consume closing $
      if !math.is_empty() {
        let rendered = render(MathInput::Latex(math.trim()));
        // Collapse any multi-line math output to a single line for inline contexts.
        let flat = rendered.lines()
          .map(|l| l.trim())
          .filter(|l| !l.is_empty())
          .collect::<Vec<_>>()
          .join(" ");
        if flat.is_empty() {
          out.push_str(math.trim());
        } else {
          out.push_str(&flat);
        }
      }
    } else if chars[i] == '{' {
      // bare braced group: strip braces, recurse on content
      let (content, skip) = read_braced_arg(&chars, i);
      i += skip;
      out.push_str(&clean_inline(content));
    } else if chars[i] == '}' {
      // orphaned closing brace: skip
      i += 1;
    } else {
      out.push(chars[i]);
      i += 1;
    }
  }
  out.trim().to_string()
}

fn clean_authors(s: String) -> String {
  let s = s.replace(r"\and", ",").replace(r"\\", ",").replace(r"\AND", ",");
  let cleaned = clean_inline(s);
  let mut out = String::new();
  for part in cleaned.split(',') {
    let part = part.trim();
    if part.is_empty() { continue; }
    if !out.is_empty() { out.push_str(", "); }
    out.push_str(part);
  }
  out
}

fn process_prose(text: &str, macros: &HashMap<String, (usize, String)>) -> Vec<String> {
  let mut out = String::new();
  let chars: Vec<char> = text.chars().collect();
  let mut i = 0;
  while i < chars.len() {
    if chars[i] == '%' && (i == 0 || chars[i - 1] != '\\') {
      while i < chars.len() && chars[i] != '\n' { i += 1; }
      continue;
    }
    if chars[i] == '\\' && i + 1 < chars.len() {
      let (cmd, consumed) = read_command(&chars, i + 1);
      i += 1 + consumed;
      match cmd.as_str() {
        "emph" | "textbf" | "textit" | "texttt" | "textsf" | "textsc"
        | "textsl" | "textrm" | "textmd" | "textup" | "textnormal" => {
          let (content, skip) = read_braced_arg(&chars, i);
          i += skip;
          out.push_str(&content);
        }
        "%" | "$" | "&" | "#" | "_" | "{" | "}" => {
          out.push(cmd.chars().next().unwrap());
        }
        "textbackslash" => out.push('\\'),
        "textless"      => out.push('<'),
        "textgreater"   => out.push('>'),
        "textbar"       => out.push('|'),
        "textasciitilde"  => out.push('~'),
        "textasciicircum" => out.push('^'),
        _ => {
          if i < chars.len() && chars[i] == '{' {
            let (content, skip) = read_braced_arg(&chars, i);
            i += skip;
            out.push_str(&content);
          }
        }
      }
      continue;
    }
    if chars[i] == '$' {
      i += 1;
      let (math, adv) = read_until_single_dollar(&chars, i);
      i += adv;
      out.push_str(&render_math(math.trim(), macros));
      continue;
    }
    if chars[i] == '~' {
      out.push(' ');
      i += 1;
      continue;
    }
    out.push(chars[i]);
    i += 1;
  }
  textwrap::wrap(out.trim(), WRAP_WIDTH)
    .into_iter()
    .map(|l| l.to_string())
    .collect()
}
