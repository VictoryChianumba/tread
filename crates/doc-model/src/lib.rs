/// Inline styled run within a paragraph line.
/// `color` uses raw RGB rather than a ratatui type — doc-model has no UI dependency.
#[derive(Debug, Clone)]
pub struct InlineSpan {
  pub text: String,
  pub bold: bool,
  pub italic: bool,
  pub underline: bool,
  pub strikethrough: bool,
  pub monospace: bool,
  pub color: Option<(u8, u8, u8)>,
  pub url: Option<String>,
}

impl InlineSpan {
  pub fn plain(text: impl Into<String>) -> Self {
    Self {
      text: text.into(),
      bold: false,
      italic: false,
      underline: false,
      strikethrough: false,
      monospace: false,
      color: None,
      url: None,
    }
  }

  pub fn bold(text: impl Into<String>) -> Self {
    Self { bold: true, ..Self::plain(text) }
  }

  pub fn italic(text: impl Into<String>) -> Self {
    Self { italic: true, ..Self::plain(text) }
  }

  pub fn monospace(text: impl Into<String>) -> Self {
    Self { monospace: true, ..Self::plain(text) }
  }
}

/// Semantic block — the producer's view of the document.
#[derive(Debug, Clone)]
pub enum Block {
  /// A single line of prose, already word-wrapped by the producer.
  Line(String),
  /// A display math equation rendered as multiple Unicode lines, treated as one unit.
  /// `num` carries the equation number for numbered environments (equation, align, etc.).
  DisplayMath { lines: Vec<String>, num: Option<usize> },
  /// A section header. level: 1=section, 2=subsection, 3=subsubsection/paragraph.
  Header { level: u8, text: String },
  /// A matrix rendered as a grid of cells (row-major).
  /// Each cell is `(text, col_span)` — `\multicolumn{N}` cells carry span > 1.
  Matrix { rows: Vec<Vec<(String, usize)>> },
  /// Explicit vertical space (blank line).
  Blank,
  /// A prose line carrying inline styling (bold, italic, monospace, etc.).
  /// The producer emits this when any span has a non-default style.
  /// build_visual_lines wraps it to terminal_width.
  StyledLine(Vec<InlineSpan>),
  /// A list item. depth=0 for top-level; marker is "• " or "1. " etc.
  ListItem { depth: u8, marker: String, content: Vec<InlineSpan> },
  /// A verbatim / code-listing block. Lines are raw (no LaTeX processing).
  CodeBlock { lang: Option<String>, lines: Vec<String> },
  /// A horizontal rule: \hline, \toprule, \midrule, \bottomrule.
  Rule,
  /// A block quote: \begin{quote}, \begin{quotation}, \begin{epigraph}.
  Quote(Vec<InlineSpan>),
}

/// A single screen row, fully expanded from a Block.
/// This is the flat table the reader indexes into — offset and cursor_y
/// are indices into Vec<VisualLine>, identical to how they used Vec<String>.
#[derive(Debug, Clone)]
pub struct VisualLine {
  pub block_idx: usize,
  pub line_in_block: usize,
  pub text: String,
  pub kind: VisualLineKind,
}

#[derive(Debug, Clone)]
pub enum VisualLineKind {
  Prose,
  /// Part of a display math block. text is pre-centered with leading spaces.
  MathLine { block_width: usize, is_first: bool, is_last: bool },
  Header(u8),
  MatrixLine { is_first: bool, is_last: bool, is_header: bool },
  Blank,
  /// Prose with inline styling. text = plain concatenation (for search).
  /// Spans carry the styled runs for the renderer.
  StyledProse(Vec<InlineSpan>),
  /// A list item row. text already contains indent+marker prefix.
  ListItem { depth: u8, marker_len: u8, is_continuation: bool },
  /// A line from a code/verbatim block.
  Code { is_first: bool, is_last: bool },
  /// A horizontal rule; text = "─".repeat(terminal_width).
  Rule,
  /// A block quote; text = plain concatenation of spans.
  Quote { is_continuation: bool },
}

/// Expand a block list into the flat visual line table.
///
/// Called once at document load and again on terminal resize (only the
/// centering offset of MathLine entries changes on resize).
pub fn build_visual_lines(blocks: &[Block], terminal_width: usize) -> Vec<VisualLine> {
  let mut out = Vec::new();
  let mut i = 0;

  while i < blocks.len() {
    // Table groups: consecutive Rule and Matrix blocks are rendered together
    // so they can share column widths and proper box-drawing borders.
    if matches!(&blocks[i], Block::Rule | Block::Matrix { .. }) {
      let group_start = i;
      while i < blocks.len() && matches!(&blocks[i], Block::Rule | Block::Matrix { .. }) {
        i += 1;
      }
      let group = &blocks[group_start..i];
      let has_matrix = group.iter().any(|b| matches!(b, Block::Matrix { .. }));
      if has_matrix {
        render_table_group(group, group_start, &mut out);
      } else {
        for (k, _) in group.iter().enumerate() {
          out.push(VisualLine {
            block_idx: group_start + k,
            line_in_block: 0,
            text: "─".repeat(terminal_width),
            kind: VisualLineKind::Rule,
          });
        }
      }
      continue;
    }

    let block_idx = i;
    match &blocks[i] {
      Block::Line(s) => {
        out.push(VisualLine {
          block_idx,
          line_in_block: 0,
          text: s.clone(),
          kind: VisualLineKind::Prose,
        });
      }

      Block::Blank => {
        out.push(VisualLine {
          block_idx,
          line_in_block: 0,
          text: String::new(),
          kind: VisualLineKind::Blank,
        });
      }

      Block::Header { level, text } => {
        out.push(VisualLine {
          block_idx,
          line_in_block: 0,
          text: text.clone(),
          kind: VisualLineKind::Header(*level),
        });
      }

      Block::DisplayMath { lines, num } => {
        let block_width = lines.iter().map(|l| visual_width(l)).max().unwrap_or(0);
        let n = lines.len();
        for (li, line) in lines.iter().enumerate() {
          let mut centered = center_line(line, block_width, terminal_width);
          if li == n - 1 {
            if let Some(eq_num) = num {
              let tag = format!("({})", eq_num);
              let used = visual_width(&centered);
              let avail = terminal_width.saturating_sub(tag.len());
              if used < avail {
                centered.push_str(&" ".repeat(avail - used));
              }
              centered.push_str(&tag);
            }
          }
          out.push(VisualLine {
            block_idx,
            line_in_block: li,
            text: centered,
            kind: VisualLineKind::MathLine {
              block_width,
              is_first: li == 0,
              is_last: li == n - 1,
            },
          });
        }
      }

      Block::StyledLine(spans) => {
        let wrapped = wrap_spans(spans, terminal_width);
        for (li, (line_spans, plain)) in wrapped.into_iter().enumerate() {
          out.push(VisualLine {
            block_idx,
            line_in_block: li,
            text: plain,
            kind: VisualLineKind::StyledProse(line_spans),
          });
        }
      }

      Block::ListItem { depth, marker, content } => {
        let wrapped = wrap_list_item(*depth, marker, content, terminal_width);
        for (li, (_line_spans, plain, is_continuation)) in wrapped.into_iter().enumerate() {
          out.push(VisualLine {
            block_idx,
            line_in_block: li,
            text: plain,
            kind: VisualLineKind::ListItem {
              depth: *depth,
              marker_len: marker.len() as u8,
              is_continuation,
            },
          });
        }
      }

      Block::CodeBlock { lines, .. } => {
        let n = lines.len();
        for (i, line) in lines.iter().enumerate() {
          out.push(VisualLine {
            block_idx,
            line_in_block: i,
            text: line.clone(),
            kind: VisualLineKind::Code {
              is_first: i == 0,
              is_last: i == n - 1,
            },
          });
        }
      }

      Block::Quote(spans) => {
        let quote_width = terminal_width.saturating_sub(4).max(1);
        let wrapped = wrap_spans(spans, quote_width);
        for (li, (_line_spans, plain)) in wrapped.into_iter().enumerate() {
          out.push(VisualLine {
            block_idx,
            line_in_block: li,
            text: plain,
            kind: VisualLineKind::Quote { is_continuation: li > 0 },
          });
        }
      }

      // Rule and Matrix are handled above via the table-group path.
      Block::Rule | Block::Matrix { .. } => {}
    }

    i += 1;
  }

  out
}

// ── Table rendering helpers ───────────────────────────────────────────────────

const COL_SEP: usize = 2; // spaces between columns in borderless tables

/// Render a table group — a run of consecutive `Block::Rule` and `Block::Matrix` blocks.
/// Uses academic-style rendering: plain horizontal rules, space-separated columns,
/// no box-drawing dividers.  Always-blank columns (LaTeX spacers) are collapsed.
/// Spanning cells (`\multicolumn{N}`) are centered over their combined column width.
fn render_table_group(group: &[Block], base_block_idx: usize, out: &mut Vec<VisualLine>) {
  type Row = Vec<(String, usize)>;
  let all_matrix_rows: Vec<&Vec<Row>> = group.iter()
    .filter_map(|b| if let Block::Matrix { rows } = b { Some(rows) } else { None })
    .collect();

  // ncols = max total column positions (summing all spans in a row).
  let ncols = all_matrix_rows.iter()
    .flat_map(|rows| rows.iter())
    .map(|row| row.iter().map(|(_, span)| span).sum::<usize>())
    .max()
    .unwrap_or(0);

  if ncols == 0 { return; }

  // Collapse always-blank columns.  A column j is active if any cell covers it
  // (via its span) with non-empty content.
  let mut col_ever_nonempty = vec![false; ncols];
  for rows in &all_matrix_rows {
    for row in rows.iter() {
      let mut pos = 0usize;
      for (cell, span) in row.iter() {
        if !cell.trim().is_empty() {
          for j in pos..(pos + span).min(ncols) {
            col_ever_nonempty[j] = true;
          }
        }
        pos += span;
      }
    }
  }
  let active_cols: Vec<usize> = (0..ncols).filter(|&j| col_ever_nonempty[j]).collect();
  let n_active = active_cols.len();
  if n_active == 0 { return; }

  // Collect widths from span=1 cells only (spanning cells don't dictate individual column widths).
  let mut col_all_widths: Vec<Vec<usize>> = vec![vec![]; n_active];
  for rows in &all_matrix_rows {
    for row in rows.iter() {
      let mut pos = 0usize;
      for (cell, span) in row.iter() {
        if *span == 1 {
          let ai = active_cols.partition_point(|&j| j < pos);
          if ai < n_active && active_cols[ai] == pos {
            let w = visual_width(cell);
            if w > 0 { col_all_widths[ai].push(w); }
          }
        }
        pos += span;
      }
    }
  }

  // Cap outlier columns: if max > 2× second-max AND > 20 chars, constrain.
  let mut col_widths: Vec<usize> = (0..n_active).map(|ai| {
    let ws = &col_all_widths[ai];
    if ws.is_empty() { return 3; }
    let max_w = *ws.iter().max().unwrap();
    if ws.len() < 2 { return max_w; }
    let second = ws.iter().filter(|&&w| w < max_w).max().copied().unwrap_or(max_w);
    if max_w > second * 2 && max_w > 20 { (second + 5).max(15) } else { max_w }
  }).collect();

  // Expand column widths so spanning headers are never truncated.
  for rows in &all_matrix_rows {
    for row in rows.iter() {
      let mut pos = 0usize;
      for (cell, span) in row.iter() {
        if *span > 1 {
          let tw = visual_width(cell.trim());
          let ai_start = active_cols.partition_point(|&j| j < pos);
          let ai_end   = active_cols.partition_point(|&j| j < pos + span);
          if ai_end > ai_start {
            let n = ai_end - ai_start;
            let cur: usize = col_widths[ai_start..ai_end].iter().sum::<usize>()
              + (n - 1) * COL_SEP;
            if tw > cur {
              let extra = tw - cur;
              let per = (extra + n - 1) / n;
              for ai in ai_start..ai_end { col_widths[ai] += per; }
            }
          }
        }
        pos += span;
      }
    }
  }

  // Header zone: all Matrix blocks before the first Rule whose IMMEDIATE next Matrix
  // block is data-like (non-blank first col, all span=1).  Using "immediate" (first
  // Matrix found after the Rule) prevents looking past the sub-header row to ByteNet.
  let header_end = (0..group.len()).find(|&k| {
    if !matches!(group[k], Block::Rule) { return false; }
    match group[k + 1..].iter().find(|b| matches!(b, Block::Matrix { .. })) {
      Some(Block::Matrix { rows }) => !rows_look_like_header(rows),
      _ => false,
    }
  }).unwrap_or(group.len());

  let total_width = col_widths.iter().sum::<usize>() + (n_active.saturating_sub(1)) * COL_SEP;
  let rule_text = "─".repeat(total_width);

  let mut seq = 0usize;
  let mut seen_matrix = false; // used to suppress cmidrules (Rules after first Matrix in header zone)

  for (k, block) in group.iter().enumerate() {
    let block_idx = base_block_idx + k;
    match block {
      Block::Rule => {
        // Suppress cmidrule-style rules: Rules inside the header zone after the first Matrix.
        let is_cmidrule = k < header_end && seen_matrix;
        if !is_cmidrule {
          out.push(VisualLine {
            block_idx,
            line_in_block: seq,
            text: rule_text.clone(),
            kind: VisualLineKind::Rule,
          });
          seq += 1;
        }
      }
      Block::Matrix { rows } => {
        let is_header = k < header_end;
        seen_matrix = true;
        let n_rows = rows.len();
        for (ri, row) in rows.iter().enumerate() {
          let text = render_row_with_spans(row, &active_cols, &col_widths);
          out.push(VisualLine {
            block_idx,
            line_in_block: seq,
            text,
            kind: VisualLineKind::MatrixLine { is_first: ri == 0, is_last: ri == n_rows - 1, is_header },
          });
          seq += 1;
        }
      }
      _ => {}
    }
  }
}

/// A Matrix block is "header-like" if every row either has a spanning cell
/// OR has a blank first column.  Data rows have non-blank first columns and all span=1.
fn rows_look_like_header(rows: &[Vec<(String, usize)>]) -> bool {
  rows.iter().all(|row| {
    row.iter().any(|(_, span)| *span > 1)
      || row.first().map_or(true, |(cell, _)| cell.trim().is_empty())
  })
}

/// Build the display string for one table row, respecting column spans.
/// Spanning cells are centered over their combined column width;
/// single cells are left-aligned.  Always-blank columns are skipped.
fn render_row_with_spans(
  row: &[(String, usize)],
  active_cols: &[usize],
  col_widths: &[usize],
) -> String {
  let n_active = active_cols.len();
  let mut result = String::new();
  let mut col_pos = 0usize;
  let mut first_part = true;

  for (cell_text, span) in row.iter() {
    let cell_end = col_pos + span;
    // Which active-column slots does this cell cover?
    let ai_start = active_cols.partition_point(|&j| j < col_pos);
    let ai_end   = active_cols.partition_point(|&j| j < cell_end);

    if ai_end > ai_start && ai_start < n_active {
      let n_covered = ai_end - ai_start;
      // Display width = sum of individual slot widths + inner COL_SEPs
      let display_width: usize = col_widths[ai_start..ai_end].iter().sum::<usize>()
        + (n_covered - 1) * COL_SEP;

      if !first_part { result.push_str(&" ".repeat(COL_SEP)); }
      first_part = false;

      let text = cell_text.trim();
      let tw = visual_width(text);
      let content = if *span > 1 && n_covered > 1 {
        // Center spanning header over combined width.
        if tw > display_width && display_width > 0 {
          let t: String = text.chars().take(display_width.saturating_sub(1)).collect();
          format!("{t}…")
        } else {
          let pad = display_width - tw;
          let pl = pad / 2;
          let pr = pad - pl;
          format!("{}{}{}", " ".repeat(pl), text, " ".repeat(pr))
        }
      } else {
        // Left-align with truncation if needed.
        if tw > display_width && display_width > 0 {
          let t: String = text.chars().take(display_width.saturating_sub(1)).collect();
          format!("{t}…")
        } else {
          format!("{:<width$}", text, width = display_width)
        }
      };
      result.push_str(&content);
    }
    col_pos += span;
  }
  result
}

/// Center `line` (of visual width `block_width`) within `terminal_width`.
fn center_line(line: &str, block_width: usize, terminal_width: usize) -> String {
  if terminal_width <= block_width {
    return line.to_string();
  }
  let pad = (terminal_width - block_width) / 2;
  format!("{}{}", " ".repeat(pad), line)
}

/// Approximate visual column width of a string (ASCII chars = 1, others = 1 for now).
/// A full Unicode-aware implementation can replace this without API changes.
fn visual_width(s: &str) -> usize {
  s.chars().count()
}

/// Word-wrap a sequence of styled spans to `width` columns.
/// Returns a vec of (line_spans, plain_text) pairs — one entry per visual line.
/// Adjacent words with identical style are coalesced into a single span.
fn wrap_spans(spans: &[InlineSpan], width: usize) -> Vec<(Vec<InlineSpan>, String)> {
  // Collect all words with their per-span style metadata.
  struct Word {
    text: String,
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
    monospace: bool,
    color: Option<(u8, u8, u8)>,
    url: Option<String>,
  }

  let mut words: Vec<Word> = Vec::new();
  for span in spans {
    for word in span.text.split_whitespace() {
      if !word.is_empty() {
        words.push(Word {
          text: word.to_string(),
          bold: span.bold,
          italic: span.italic,
          underline: span.underline,
          strikethrough: span.strikethrough,
          monospace: span.monospace,
          color: span.color,
          url: span.url.clone(),
        });
      }
    }
  }

  if words.is_empty() {
    return vec![(vec![], String::new())];
  }

  let effective_width = width.max(1);
  let mut result: Vec<(Vec<InlineSpan>, String)> = Vec::new();
  let mut line_spans: Vec<InlineSpan> = Vec::new();
  let mut line_plain = String::new();
  let mut line_width = 0usize;

  for word in &words {
    let wlen = word.text.chars().count();
    let needed = if line_width == 0 { wlen } else { line_width + 1 + wlen };

    if line_width > 0 && needed > effective_width {
      result.push((std::mem::take(&mut line_spans), std::mem::take(&mut line_plain)));
      line_width = 0;
    }

    let prefix = if line_width > 0 { " " } else { "" };
    let token = format!("{}{}", prefix, word.text);
    line_plain.push_str(&token);
    line_width += token.chars().count();

    // Coalesce with previous span when style is identical.
    let coalesce = line_spans.last().map_or(false, |last| {
      last.bold == word.bold
        && last.italic == word.italic
        && last.underline == word.underline
        && last.strikethrough == word.strikethrough
        && last.monospace == word.monospace
        && last.color == word.color
        && last.url == word.url
    });

    if coalesce {
      line_spans.last_mut().unwrap().text.push_str(&token);
    } else {
      line_spans.push(InlineSpan {
        text: token,
        bold: word.bold,
        italic: word.italic,
        underline: word.underline,
        strikethrough: word.strikethrough,
        monospace: word.monospace,
        color: word.color,
        url: word.url.clone(),
      });
    }
  }

  if !line_plain.is_empty() {
    result.push((line_spans, line_plain));
  }

  result
}

/// Wrap a list item's content to `width`, prepending the indent+marker prefix.
/// Returns (line_spans, plain_text, is_continuation) per visual line.
fn wrap_list_item(
  depth: u8,
  marker: &str,
  content: &[InlineSpan],
  width: usize,
) -> Vec<(Vec<InlineSpan>, String, bool)> {
  let indent_len = depth as usize * 2;
  let prefix_len = indent_len + marker.len();
  let content_width = width.saturating_sub(prefix_len).max(1);

  let wrapped = wrap_spans(content, content_width);

  wrapped
    .into_iter()
    .enumerate()
    .map(|(i, (spans, plain))| {
      let is_continuation = i > 0;
      let prefix = if is_continuation {
        format!("{}{}", "  ".repeat(depth as usize), " ".repeat(marker.len()))
      } else {
        format!("{}{}", "  ".repeat(depth as usize), marker)
      };
      let plain_with_prefix = format!("{}{}", prefix, plain);
      let mut all_spans = vec![InlineSpan::plain(prefix)];
      all_spans.extend(spans);
      (all_spans, plain_with_prefix, is_continuation)
    })
    .collect()
}

/// Convert a flat Vec<String> into Vec<Block> with no behavioral change.
/// Empty strings become Block::Blank; all others become Block::Line.
pub fn from_lines(lines: Vec<String>) -> Vec<Block> {
  lines
    .into_iter()
    .map(|s| if s.is_empty() { Block::Blank } else { Block::Line(s) })
    .collect()
}
