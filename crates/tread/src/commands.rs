//! Ex-command parser and dispatcher for tread's `:` mode.
//!
//! `execute(reader, ctx, line)` is the single public entry point.  The
//! event loop calls it when the user presses Enter in `Mode::Command`,
//! and reacts to the returned `CommandResult`.
//!
//! ## Command surface
//!
//! Built on top of one bare integer form (`:42` → goto line N) plus a
//! table of named commands with optional aliases.  Args after the
//! command name are split on whitespace.  See the `command_table`
//! function for the v1 list.

use ui_theme::{Theme, ThemeId};

use crate::config;
use crate::state::{PopupContent, Reader};

/// Per-session context that commands consult.  Held outside `Reader` so
/// it doesn't have to be threaded through every render path.
#[derive(Debug, Clone, Default)]
pub struct CmdCtx {
  pub arxiv_id: Option<String>,
  pub kitty_supported: bool,
}

/// Outcome of executing a command line.  The event loop reacts:
/// - `Continue`: stay in the loop, no other change
/// - `Quit`: break out, save state, exit
/// - `ChangeTheme`: swap the loop's local `theme`
/// - `OpenHelp`: set `reader.help_visible = true`
/// - `Error(msg)`: stash on `reader.cmd_error` for the status line
/// - `Reload`: paper data was just replaced; event loop must clear
///   image-cache state since old kitty_ids no longer match the new blocks
pub enum CommandResult {
  Continue,
  Quit,
  ChangeTheme(Theme),
  OpenHelp,
  Error(String),
  Reload,
}

/// Top-level dispatch.  Splits the command line into name + args, then
/// matches against the bare-integer form, then the command table.
pub fn execute(reader: &mut Reader, ctx: &CmdCtx, line: &str) -> CommandResult {
  let trimmed = line.trim();
  if trimmed.is_empty() {
    return CommandResult::Continue;
  }

  // Bare integer: `:42` → go to line N.
  if let Ok(n) = trimmed.parse::<usize>() {
    return goto_line(reader, n);
  }

  // Split on whitespace; the first token is the command name (case-insensitive).
  let mut parts = trimmed.split_whitespace();
  let name = parts.next().unwrap_or("").to_ascii_lowercase();
  let args: Vec<&str> = parts.collect();

  let n = name.as_str();
  for (canonical, aliases, handler) in command_table() {
    if n == *canonical || aliases.iter().any(|a| *a == n) {
      return handler(reader, ctx, &args);
    }
  }

  CommandResult::Error(format!("unknown command: {name}"))
}

type Handler = fn(&mut Reader, &CmdCtx, &[&str]) -> CommandResult;

fn command_table() -> &'static [(&'static str, &'static [&'static str], Handler)] {
  &[
    ("quit",       &["q", "exit"],     cmd_quit),
    ("help",       &["h"],             cmd_help),
    ("toc",        &["tree"],          cmd_toc),
    ("back",       &["bk"],            cmd_back),
    ("set",        &[],                cmd_set),
    ("goto",       &["g"],             cmd_goto),
    ("abstract",   &[],                cmd_abstract),
    ("references", &["bib", "r"],      cmd_references),
    ("marks",      &[],                cmd_marks),
    ("delmarks",   &["dm"],            cmd_delmarks),
    ("highlights", &["hl"],            cmd_highlights),
    ("about",      &[],                cmd_about),
    ("url",        &["link"],          cmd_url),
    ("cite",       &["bibtex"],        cmd_cite),
    ("open",       &[],                cmd_open),
    ("placement",  &[],                cmd_placement),
    ("reload",     &["e"],             cmd_reload),
  ]
}

/// `:reload` — re-fetch source from arXiv and rebuild the paper in
/// place.  Preserves cursor, scroll position, bookmarks, highlights.
/// Synchronous — the UI freezes for ~2s while the network round-trip
/// runs.  No-op when no paper is loaded (e.g. when running the reader
/// against a local file in the future).
fn cmd_reload(reader: &mut Reader, ctx: &CmdCtx, _args: &[&str]) -> CommandResult {
  let Some(id) = &ctx.arxiv_id else {
    return CommandResult::Error("no paper loaded — :reload requires an arxiv id".to_string());
  };
  match crate::fetch_paper(id, ctx.kitty_supported) {
    Ok(data) => {
      reader.reload_with(data.blocks, data.bibitems);
      CommandResult::Reload
    }
    Err(e) => CommandResult::Error(format!("reload: {e}")),
  }
}

// ── Quit / aliases ───────────────────────────────────────────────────────────

fn cmd_quit(_: &mut Reader, _: &CmdCtx, _: &[&str]) -> CommandResult {
  CommandResult::Quit
}

fn cmd_help(_: &mut Reader, _: &CmdCtx, _: &[&str]) -> CommandResult {
  CommandResult::OpenHelp
}

fn cmd_toc(reader: &mut Reader, _: &CmdCtx, _: &[&str]) -> CommandResult {
  reader.toggle_toc();
  CommandResult::Continue
}

fn cmd_back(reader: &mut Reader, _: &CmdCtx, _: &[&str]) -> CommandResult {
  reader.nav_back();
  CommandResult::Continue
}

// ── Goto / sections ──────────────────────────────────────────────────────────

fn goto_line(reader: &mut Reader, n_one_indexed: usize) -> CommandResult {
  let total = reader.total_lines();
  if total == 0 {
    return CommandResult::Error("empty document".to_string());
  }
  let target = n_one_indexed.saturating_sub(1).min(total - 1);
  reader.push_nav_mark();
  let ch = reader.content_height();
  if target < reader.offset || target >= reader.offset + ch {
    let max_offset = total.saturating_sub(ch);
    reader.offset = (target + 1).saturating_sub(ch).min(max_offset);
  }
  reader.cursor_y = target.saturating_sub(reader.offset);
  reader.cursor_x = 0;
  reader.desired_column = 0;
  CommandResult::Continue
}

fn cmd_goto(reader: &mut Reader, _: &CmdCtx, args: &[&str]) -> CommandResult {
  if args.is_empty() {
    return CommandResult::Error("goto: missing argument".to_string());
  }
  let arg = args.join(" ");
  if reader.sections.is_empty() {
    return CommandResult::Error("no sections in this document".to_string());
  }
  // First try numeric form ("3", "3.2", "3.2.1").
  let target = if arg.chars().next().map_or(false, |c| c.is_ascii_digit()) {
    reader.sections.iter().find(|s| section_starts_with(&s.2, &arg)).map(|s| s.0)
  } else {
    let needle = arg.to_ascii_lowercase();
    reader.sections.iter().find(|s| s.2.to_ascii_lowercase().contains(&needle)).map(|s| s.0)
  };
  match target {
    Some(line) => {
      reader.push_nav_mark();
      jump_to_line(reader, line);
      CommandResult::Continue
    }
    None => CommandResult::Error(format!("no section matching: {arg}")),
  }
}

fn section_starts_with(header: &str, prefix: &str) -> bool {
  // Section header looks like "3.2  Background".  Split on whitespace and
  // compare just the leading number token.
  let token = header.split_whitespace().next().unwrap_or("");
  token == prefix
}

fn cmd_abstract(reader: &mut Reader, _: &CmdCtx, _: &[&str]) -> CommandResult {
  jump_to_section_named(reader, &["abstract"])
}

fn cmd_references(reader: &mut Reader, _: &CmdCtx, _: &[&str]) -> CommandResult {
  jump_to_section_named(reader, &["references", "bibliography"])
}

fn jump_to_section_named(reader: &mut Reader, candidates: &[&str]) -> CommandResult {
  let target = reader.sections.iter().find(|s| {
    let lower = s.2.to_ascii_lowercase();
    candidates.iter().any(|c| lower.contains(c))
  }).map(|s| s.0);
  match target {
    Some(line) => {
      reader.push_nav_mark();
      jump_to_line(reader, line);
      CommandResult::Continue
    }
    None => CommandResult::Error(format!("no section: {}", candidates.join(" / "))),
  }
}

fn jump_to_line(reader: &mut Reader, line: usize) {
  let total = reader.total_lines();
  if total == 0 { return; }
  let line = line.min(total - 1);
  let ch = reader.content_height();
  if line >= reader.offset && line < reader.offset + ch {
    reader.cursor_y = line - reader.offset;
  } else {
    reader.offset = line;
    reader.cursor_y = 0;
  }
  reader.cursor_x = 0;
  reader.desired_column = 0;
}

// ── :set ─────────────────────────────────────────────────────────────────────

fn cmd_set(_reader: &mut Reader, _: &CmdCtx, args: &[&str]) -> CommandResult {
  if args.is_empty() {
    return CommandResult::Error("set: missing argument (e.g. theme=light)".to_string());
  }
  let pair = args.join(" ");
  let Some((key, value)) = pair.split_once('=') else {
    return CommandResult::Error("set: expected key=value".to_string());
  };
  let key = key.trim();
  let value = value.trim();
  match key {
    "theme" => set_theme(value),
    other => CommandResult::Error(format!("set: unknown option: {other}")),
  }
}

fn set_theme(value: &str) -> CommandResult {
  if value == "trench" {
    let mut cfg = config::load();
    cfg.theme_override = None;
    config::save(&cfg);
    return CommandResult::ChangeTheme(config::resolve_theme());
  }
  match ThemeId::from_id(value) {
    Some(tid) => {
      let mut cfg = config::load();
      cfg.theme_override = Some(value.to_string());
      config::save(&cfg);
      CommandResult::ChangeTheme(tid.theme())
    }
    None => {
      let names: Vec<&str> = ThemeId::all().iter().map(|t| t.label()).collect();
      CommandResult::Error(format!(
        "unknown theme: {value}.  Try `trench` or one of: {}",
        names.join(", ")
      ))
    }
  }
}

// ── Marks / highlights / metadata popups ─────────────────────────────────────

fn cmd_marks(reader: &mut Reader, _: &CmdCtx, _: &[&str]) -> CommandResult {
  let mut entries: Vec<(char, usize)> = reader.bookmarks.iter().map(|(&c, &l)| (c, l)).collect();
  entries.sort_by_key(|e| e.0);
  let lines: Vec<String> = if entries.is_empty() {
    vec!["(no marks set — use m{a} to set one)".to_string()]
  } else {
    entries.iter().map(|(letter, line)| {
      let snippet = reader.visual_lines.get(*line)
        .map(|vl| vl.text.clone())
        .unwrap_or_default();
      let snippet: String = snippet.chars().take(48).collect();
      format!("  {letter}    line {line:>5}    {snippet}")
    }).collect()
  };
  reader.popup = Some(PopupContent { title: "Marks".to_string(), lines });
  CommandResult::Continue
}

fn cmd_delmarks(reader: &mut Reader, _: &CmdCtx, args: &[&str]) -> CommandResult {
  if args.is_empty() {
    return CommandResult::Error("delmarks: missing letter".to_string());
  }
  let mut removed = 0;
  for token in args {
    for ch in token.chars() {
      if ch.is_ascii_alphabetic() && reader.bookmarks.remove(&ch).is_some() {
        removed += 1;
      }
    }
  }
  if removed == 0 {
    CommandResult::Error("delmarks: no matching marks".to_string())
  } else {
    CommandResult::Continue
  }
}

fn cmd_highlights(reader: &mut Reader, _: &CmdCtx, _: &[&str]) -> CommandResult {
  let lines: Vec<String> = if reader.highlights.highlights.is_empty() {
    vec!["(no highlights — select in visual mode and press H)".to_string()]
  } else {
    reader.highlights.highlights.iter().map(|h| {
      // Find the first VL that overlaps this highlight to extract a snippet.
      let snippet = reader.visual_lines.iter().find_map(|vl| {
        if vl.block_idx == h.block_idx
          && h.byte_start < vl.block_byte_end
          && h.byte_end > vl.block_byte_start
        {
          let lo = h.byte_start.saturating_sub(vl.block_byte_start).min(vl.text.len());
          let hi = (h.byte_end - vl.block_byte_start).min(vl.text.len());
          Some(vl.text.get(lo..hi).unwrap_or("").to_string())
        } else {
          None
        }
      }).unwrap_or_default();
      let snippet: String = snippet.chars().take(56).collect();
      format!("  block {:>4}  bytes {}-{}  {}",
        h.block_idx, h.byte_start, h.byte_end, snippet)
    }).collect()
  };
  reader.popup = Some(PopupContent { title: "Highlights".to_string(), lines });
  CommandResult::Continue
}

fn cmd_about(reader: &mut Reader, ctx: &CmdCtx, _: &[&str]) -> CommandResult {
  let mut lines: Vec<String> = Vec::new();
  if let Some(meta) = &reader.meta {
    if !meta.title.is_empty() {
      lines.push(format!("Title:    {}", meta.title));
    }
    if !meta.authors.is_empty() {
      lines.push(format!("Authors:  {}", meta.authors));
    }
  }
  if let Some(id) = &ctx.arxiv_id {
    lines.push(format!("arXiv ID: {id}"));
    lines.push(format!("URL:      https://arxiv.org/abs/{id}"));
  }
  if lines.is_empty() {
    lines.push("(no metadata available)".to_string());
  }
  reader.popup = Some(PopupContent { title: "About".to_string(), lines });
  CommandResult::Continue
}

// ── Clipboard / external ─────────────────────────────────────────────────────

fn cmd_url(_reader: &mut Reader, ctx: &CmdCtx, _: &[&str]) -> CommandResult {
  let Some(id) = &ctx.arxiv_id else {
    return CommandResult::Error("no arxiv id available".to_string());
  };
  let url = format!("https://arxiv.org/abs/{id}");
  crate::osc52_yank(&url);
  CommandResult::Continue
}

fn cmd_cite(reader: &mut Reader, ctx: &CmdCtx, _: &[&str]) -> CommandResult {
  let Some(id) = &ctx.arxiv_id else {
    return CommandResult::Error("no arxiv id available".to_string());
  };
  let mut entry = format!("@misc{{{id}");
  if let Some(meta) = &reader.meta {
    if !meta.title.is_empty() {
      entry.push_str(&format!(",\n  title={{{}}}", meta.title));
    }
    if !meta.authors.is_empty() {
      entry.push_str(&format!(",\n  author={{{}}}", meta.authors));
    }
  }
  entry.push_str(&format!(",\n  eprint={{{id}}},\n  archivePrefix={{arXiv}}\n}}"));
  crate::osc52_yank(&entry);
  CommandResult::Continue
}

fn cmd_open(_reader: &mut Reader, ctx: &CmdCtx, _: &[&str]) -> CommandResult {
  let Some(id) = &ctx.arxiv_id else {
    return CommandResult::Error("no arxiv id available".to_string());
  };
  let url = format!("https://arxiv.org/abs/{id}");
  // macOS ships `open`; Linux ships `xdg-open`.  Try macOS first.
  let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
  match std::process::Command::new(opener).arg(&url).spawn() {
    Ok(_) => CommandResult::Continue,
    Err(e) => CommandResult::Error(format!("open failed: {e}")),
  }
}

// ── Diagnostics ──────────────────────────────────────────────────────────────

fn cmd_placement(reader: &mut Reader, _: &CmdCtx, _: &[&str]) -> CommandResult {
  // Walk the block list, find each Matrix group, and report its current
  // location (block idx + caption snippet).  Useful for inspecting whether
  // PDF-anchor placement landed where expected.
  use doc_model::Block;
  let mut lines: Vec<String> = Vec::new();
  let mut n = 0;
  for (i, block) in reader.blocks.iter().enumerate() {
    if matches!(block, Block::Matrix { .. }) {
      n += 1;
      // Look one block back for the caption Line ("[Table: ...]").
      let cap = if i > 0 {
        if let Block::Line(s) = &reader.blocks[i - 1] {
          if s.starts_with("[Table:") { s.clone() } else { String::new() }
        } else { String::new() }
      } else { String::new() };
      let cap: String = cap.chars().take(56).collect();
      lines.push(format!("  Table {:<2} block {:>4}  {}", n, i, cap));
    }
  }
  if lines.is_empty() {
    lines.push("(no Matrix blocks in document)".to_string());
  }
  reader.popup = Some(PopupContent { title: "Table placement".to_string(), lines });
  CommandResult::Continue
}

#[cfg(test)]
mod tests {
  use super::*;

  fn ctx() -> CmdCtx { CmdCtx { arxiv_id: Some("1706.03762".to_string()), kitty_supported: false } }

  fn dummy_reader() -> Reader {
    use doc_model::Block;
    Reader::new(vec![Block::Line("hello".to_string()), Block::Line("world".to_string())], 80, 24)
  }

  #[test]
  fn empty_command_is_noop() {
    let mut r = dummy_reader();
    assert!(matches!(execute(&mut r, &ctx(), ""), CommandResult::Continue));
    assert!(matches!(execute(&mut r, &ctx(), "   "), CommandResult::Continue));
  }

  #[test]
  fn unknown_command_errors() {
    let mut r = dummy_reader();
    let out = execute(&mut r, &ctx(), "frobnicate");
    assert!(matches!(out, CommandResult::Error(_)));
  }

  #[test]
  fn quit_returns_quit() {
    let mut r = dummy_reader();
    assert!(matches!(execute(&mut r, &ctx(), "q"), CommandResult::Quit));
    assert!(matches!(execute(&mut r, &ctx(), "quit"), CommandResult::Quit));
    assert!(matches!(execute(&mut r, &ctx(), "exit"), CommandResult::Quit));
  }

  #[test]
  fn help_returns_open_help() {
    let mut r = dummy_reader();
    assert!(matches!(execute(&mut r, &ctx(), "help"), CommandResult::OpenHelp));
    assert!(matches!(execute(&mut r, &ctx(), "h"), CommandResult::OpenHelp));
  }

  #[test]
  fn bare_integer_jumps_to_line() {
    let mut r = dummy_reader();
    assert!(matches!(execute(&mut r, &ctx(), "2"), CommandResult::Continue));
    assert_eq!(r.current_line(), 1); // 1-indexed → line 2 = index 1
  }

  #[test]
  fn integer_clamps_to_total() {
    let mut r = dummy_reader();
    execute(&mut r, &ctx(), "9999");
    assert_eq!(r.current_line(), r.total_lines() - 1);
  }

  #[test]
  fn set_theme_unknown_errors() {
    let mut r = dummy_reader();
    let out = execute(&mut r, &ctx(), "set theme=nonsense");
    assert!(matches!(out, CommandResult::Error(_)));
  }

  #[test]
  fn set_theme_known_returns_change() {
    let mut r = dummy_reader();
    let out = execute(&mut r, &ctx(), "set theme=light");
    assert!(matches!(out, CommandResult::ChangeTheme(_)));
  }

  #[test]
  fn set_missing_eq_errors() {
    let mut r = dummy_reader();
    let out = execute(&mut r, &ctx(), "set theme light");
    assert!(matches!(out, CommandResult::Error(_)));
  }

  #[test]
  fn delmarks_no_args_errors() {
    let mut r = dummy_reader();
    let out = execute(&mut r, &ctx(), "delmarks");
    assert!(matches!(out, CommandResult::Error(_)));
  }
}
