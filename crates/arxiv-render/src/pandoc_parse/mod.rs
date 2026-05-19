// Pandoc-based LaTeX parser.
//
// Runs `pandoc -f latex -t json` as a subprocess, walks the resulting AST, and
// emits Vec<Block> — the same type produced by parse.rs.  The caller tries this
// path first; if Pandoc is unavailable or returns nothing, it falls back to the
// hand-rolled parser.

use doc_model::{Block, InlineSpan};
use serde_json::Value;

mod figure;
mod inline;
mod preprocess;
mod spec;

// Re-export so figure.rs sees `super::TableSpec` unchanged.
pub(crate) use spec::TableSpec;
use inline::{synthesize_bibliography, walk_inlines_spans, walk_inlines_text};
use preprocess::preprocess_latex_source;
use spec::extract_tabular_specs;

// ── Cross-cutting state for Cite / bibliography rendering ────────────────────

// `walk_inlines_spans` is called from many sites and threading two
// parameters down into all of them would be churn for no benefit; the
// state only matters during a single `try_pandoc` invocation.  Stored
// thread-locals; the scope guard clears them on exit.
thread_local! {
    pub(super) static CITE_NUMBERS: std::cell::RefCell<std::collections::HashMap<String, usize>>
        = std::cell::RefCell::new(std::collections::HashMap::new());
    pub(super) static BIBITEMS_ORDERED: std::cell::RefCell<Vec<(String, String)>>
        = std::cell::RefCell::new(Vec::new());
    /// Set by the `thebibliography` Div arm when we hand off rendering
    /// to `synthesize_bibliography`.  Consulted at the end of
    /// `try_pandoc` to decide whether to auto-append a synthesized
    /// References section for papers (like 2605.04035) where Pandoc
    /// emitted no bibliography Div because the source uses
    /// `\bibliography{external}` instead of inline `thebibliography`.
    pub(super) static BIBLIOGRAPHY_EMITTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

struct CiteScopeGuard;
impl Drop for CiteScopeGuard {
    fn drop(&mut self) {
        CITE_NUMBERS.with(|c| c.borrow_mut().clear());
        BIBITEMS_ORDERED.with(|b| b.borrow_mut().clear());
        BIBLIOGRAPHY_EMITTED.with(|f| f.set(false));
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn try_pandoc(sources: &[(String, String)]) -> Result<Vec<Block>, String> {
    // Quick availability check — bail early if pandoc is not on PATH.
    std::process::Command::new("pandoc")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|_| "pandoc not found".to_string())?;

    // Write all source files to a temp dir so \input{} resolution works.
    // (Pandoc resolves \input{} relative to cwd, not the file path.)
    // Each file is preprocessed first to rewrite LaTeX commands that Pandoc
    // mishandles (e.g. \multirow drops content silently when on its own line).
    let tmp = tempfile::TempDir::new().map_err(|e| e.to_string())?;
    for (name, content) in sources {
        let dest = tmp.path().join(name);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let processed = preprocess_latex_source(content);
        std::fs::write(&dest, processed.as_bytes()).map_err(|e| format!("write {name}: {e}"))?;
    }

    let root = find_root_name(sources);

    let output = std::process::Command::new("pandoc")
        .args(["-f", "latex", "-t", "json", "--quiet", &root])
        .current_dir(tmp.path())
        .output()
        .map_err(|e| format!("pandoc exec: {e}"))?;

    if output.stdout.is_empty() {
        return Err(format!(
            "pandoc produced no output: {}",
            String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(200)
                .collect::<String>()
        ));
    }

    let ast: Value =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("pandoc JSON: {e}"))?;

    // Walk every source for tabular layout info — Pandoc strips `|` characters
    // (vertical rules) and mid-body `\hline` directives, so we recover both
    // directly from the source.
    let mut tabular_specs: Vec<TableSpec> = sources
        .iter()
        .flat_map(|(_, content)| extract_tabular_specs(content))
        .collect();

    // Pre-extract bibitems in source order.  Used both to assign
    // sequential `[N]` numbers to citations (Cite arm consults
    // `CITE_NUMBERS`) and to synthesize a numbered bibliography
    // (Div arm with class="thebibliography" replaces inner content).
    // Threaded via thread-locals to avoid plumbing through every walker
    // signature; cleared on scope exit.
    let bibitems_ordered = crate::bibitems::extract_bibitems_ordered(sources);
    let cite_numbers: std::collections::HashMap<String, usize> = bibitems_ordered
        .iter()
        .enumerate()
        .map(|(i, (k, _))| (k.clone(), i + 1))
        .collect();
    CITE_NUMBERS.with(|c| *c.borrow_mut() = cite_numbers);
    BIBITEMS_ORDERED.with(|b| *b.borrow_mut() = bibitems_ordered);
    let _scope_guard = CiteScopeGuard;

    let mut blocks = Vec::new();
    if let Some(meta) = ast.get("meta") {
        blocks.extend(extract_meta_blocks(meta));
    }
    let mut counters = SectionCounters::new();
    if let Some(arr) = ast["blocks"].as_array() {
        blocks.extend(walk_blocks(arr, 0, &mut tabular_specs, &mut counters));
    }

    // Auto-append a References section when we have bibitems but Pandoc
    // emitted no `thebibliography` Div.  Affects papers that use
    // `\bibliography{external}` (BibLaTeX / bibtex without --citeproc)
    // — 2605.04035 is one — where the citations have numbers but the
    // document body would otherwise just end.
    let bibliography_emitted = BIBLIOGRAPHY_EMITTED.with(|f| f.get());
    let has_bibitems = BIBITEMS_ORDERED.with(|b| !b.borrow().is_empty());
    if !bibliography_emitted && has_bibitems {
        blocks.push(Block::Header {
            level: 1,
            text: "References".to_string(),
        });
        blocks.extend(synthesize_bibliography());
    }

    if blocks.iter().all(|b| matches!(b, Block::Blank)) {
        return Err("pandoc produced no content".to_string());
    }
    Ok(blocks)
}

// ── Root file detection ───────────────────────────────────────────────────────

fn find_root_name(sources: &[(String, String)]) -> String {
    for (name, content) in sources {
        if content.contains(r"\begin{document}") {
            return name.clone();
        }
    }
    sources
        .iter()
        .max_by_key(|(_, c)| c.len())
        .map(|(name, _)| name.clone())
        .unwrap_or_default()
}

// ── Metadata ──────────────────────────────────────────────────────────────────

fn extract_meta_blocks(meta: &Value) -> Vec<Block> {
    let mut out = vec![Block::Blank];

    if let Some(title_meta) = meta.get("title") {
        if let Some(inlines) = title_meta["c"].as_array() {
            let text = walk_inlines_text(inlines);
            if !text.is_empty() {
                out.push(Block::Header { level: 1, text });
            }
        }
    }

    if let Some(author_meta) = meta.get("author") {
        let names = extract_author_names(author_meta);
        if !names.is_empty() {
            out.push(Block::Line(names.join(", ")));
        }
    }

    out.push(Block::Blank);
    out
}

// Each author's name line is immediately followed by a Note or Span (footnote mark) in the
// flat MetaInlines block Pandoc produces for \author{Name\thanks{} \\ Inst \And ...}.
fn extract_author_names(author_meta: &Value) -> Vec<String> {
    let items = match author_meta["t"].as_str() {
        Some("MetaList") => author_meta["c"]
            .as_array()
            .map(|a| a.as_slice())
            .unwrap_or(&[]),
        _ => return vec![],
    };

    if items.len() > 1 {
        // Multiple MetaList items — each is one author; take text before first LineBreak.
        return items
            .iter()
            .filter_map(|item| {
                let inlines: Vec<Value> = item["c"]
                    .as_array()?
                    .iter()
                    .take_while(|n| n["t"].as_str() != Some("LineBreak"))
                    .cloned()
                    .collect();
                let text = walk_inlines_text(&inlines).trim().to_string();
                if text.is_empty() { None } else { Some(text) }
            })
            .collect();
    }

    // Single MetaList item: all authors are concatenated as one flat MetaInlines.
    // Split at LineBreak; name lines are identified by a trailing Note or Span marker.
    let inlines = match items.first().and_then(|i| i["c"].as_array()) {
        Some(v) => v,
        None => return vec![],
    };

    let mut segments: Vec<Vec<Value>> = vec![vec![]];
    for n in inlines {
        if n["t"].as_str() == Some("LineBreak") {
            segments.push(vec![]);
        } else {
            segments.last_mut().unwrap().push(n.clone());
        }
    }

    let names: Vec<String> = segments
        .iter()
        .filter_map(|seg| {
            // Skip email lines (contain a Code inline).
            if seg.iter().any(|n| n["t"].as_str() == Some("Code")) {
                return None;
            }
            // Name lines have a Note or Span footnote marker.
            let has_marker = seg
                .iter()
                .any(|n| matches!(n["t"].as_str(), Some("Note") | Some("Span")));
            if !has_marker {
                return None;
            }
            // Extract text before the marker.
            let text_inlines: Vec<Value> = seg
                .iter()
                .take_while(|n| !matches!(n["t"].as_str(), Some("Note") | Some("Span")))
                .cloned()
                .collect();
            let text = walk_inlines_text(&text_inlines).trim().to_string();
            if text.is_empty() { None } else { Some(text) }
        })
        .collect();

    if names.is_empty() {
        // Fallback for papers without \thanks{} markers: take the first line.
        segments
            .first()
            .map(|seg| walk_inlines_text(seg).trim().to_string())
            .filter(|s| !s.is_empty())
            .into_iter()
            .collect()
    } else {
        names
    }
}

// ── Block walker ──────────────────────────────────────────────────────────────

/// Section / subsection / subsubsection counters threaded through `walk_blocks`
/// so that headers carry their LaTeX-style number prefix ("1  Introduction",
/// "2.1  Background", "2.1.3  Detailed Steps"). Pandoc emits unnumbered headers
/// (`\section*{}` → `class="unnumbered"`); those are skipped here and rendered
/// without a prefix. Mirrors the legacy parser convention at `parse.rs:805`.
struct SectionCounters {
    sec: [u32; 3],
    table: u32,
    figure: u32,
    kitty_id: u32,
    equation: u32,
}

impl SectionCounters {
    fn new() -> Self {
        Self {
            sec: [0; 3],
            table: 0,
            figure: 0,
            kitty_id: 0,
            equation: 0,
        }
    }

    /// Increment the equation counter by `count` and return the LAST
    /// allocated number — used so that an `align` block with N lines
    /// claims N consecutive numbers and the rendered tag shows the
    /// final one beside the last line.  v2 will refine this to per-line
    /// numbers and `\notag`/`\nonumber` suppression.
    fn bump_equation(&mut self, count: u32) -> u32 {
        self.equation = self.equation.saturating_add(count);
        self.equation
    }

    /// Allocate the next Kitty graphics protocol image id.  Starts at 1
    /// because some Kitty implementations reject id=0.
    fn next_kitty_id(&mut self) -> u32 {
        self.kitty_id += 1;
        self.kitty_id
    }

    /// Increment the counter at `level` (1–3) and reset deeper levels.
    /// Returns the formatted prefix string, e.g. "2", "2.1", "2.1.3".
    fn bump(&mut self, level: u8) -> String {
        let lv = level.clamp(1, 3) as usize;
        self.sec[lv - 1] += 1;
        for i in lv..3 {
            self.sec[i] = 0;
        }
        match lv {
            1 => format!("{}", self.sec[0]),
            2 => format!("{}.{}", self.sec[0], self.sec[1]),
            _ => format!("{}.{}.{}", self.sec[0], self.sec[1], self.sec[2]),
        }
    }

    /// Increment table counter, return new number for the caption.
    fn bump_table(&mut self) -> u32 {
        self.table += 1;
        self.table
    }

    /// Increment figure counter, return new number for the caption.
    fn bump_figure(&mut self) -> u32 {
        self.figure += 1;
        self.figure
    }
}

fn walk_blocks(
    nodes: &[Value],
    list_depth: u8,
    specs: &mut Vec<TableSpec>,
    counters: &mut SectionCounters,
) -> Vec<Block> {
    let mut out = Vec::new();

    for node in nodes {
        let t = node["t"].as_str().unwrap_or("");
        let c = &node["c"];

        match t {
            "Para" | "Plain" => {
                if let Some(inlines) = c.as_array() {
                    let blocks = para_to_block(inlines, counters);
                    if !blocks.is_empty() {
                        out.extend(blocks);
                        out.push(Block::Blank);
                    }
                }
            }

            "Header" => {
                let level = c[0].as_u64().unwrap_or(1).min(3) as u8;
                // c[1] = attr = [id, classes, key_vals]. Pandoc tags `\section*{}`
                // and friends with class="unnumbered" — skip prefix for those.
                let unnumbered = c[1][1].as_array().map_or(false, |classes| {
                    classes.iter().any(|cl| cl.as_str() == Some("unnumbered"))
                });
                // attr.id is the LaTeX label (or Pandoc-generated slug).
                // Emit an Anchor so reader can resolve `\ref{}` jumps.
                if let Some(id) = c[1][0].as_str() {
                    if !id.is_empty() {
                        out.push(Block::Anchor(id.to_string()));
                    }
                }
                if let Some(inlines) = c[2].as_array() {
                    let raw = walk_inlines_text(inlines);
                    if !raw.is_empty() {
                        let text = if unnumbered {
                            raw
                        } else {
                            format!("{}  {}", counters.bump(level), raw)
                        };
                        out.push(Block::Header { level, text });
                    }
                }
            }

            // Suppress prose-level horizontal rules (bibliography separators, \hrule, etc.).
            // Block::Rule is reserved for programmatic table separators in parse_table().
            "HorizontalRule" => out.push(Block::Blank),

            "CodeBlock" => {
                let lang = c[0][1]
                    .as_array()
                    .and_then(|cls| cls.first())
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                if let Some(code) = c[1].as_str() {
                    if !code.is_empty() {
                        out.push(Block::CodeBlock {
                            lang,
                            lines: code.lines().map(|l| l.to_string()).collect(),
                        });
                    }
                }
            }

            "BlockQuote" => {
                if let Some(inner) = c.as_array() {
                    let spans: Vec<InlineSpan> = inner
                        .iter()
                        .flat_map(|b| match b["t"].as_str() {
                            Some("Para") | Some("Plain") => b["c"]
                                .as_array()
                                .map(|il| walk_inlines_spans(il))
                                .unwrap_or_default(),
                            _ => vec![],
                        })
                        .collect();
                    if !spans.is_empty() {
                        out.push(Block::Quote(spans));
                    }
                }
            }

            "BulletList" => {
                if let Some(items) = c.as_array() {
                    for item in items {
                        if let Some(item_blocks) = item.as_array() {
                            out.extend(list_item_blocks(
                                item_blocks,
                                list_depth,
                                "•",
                                specs,
                                counters,
                            ));
                        }
                    }
                }
            }

            "OrderedList" => {
                // c = [list_attrs, [[blocks], ...]]
                // list_attrs = [start_num, style, delimiter]
                let start = c[0][0].as_u64().unwrap_or(1) as usize;
                if let Some(items) = c[1].as_array() {
                    for (i, item) in items.iter().enumerate() {
                        let marker = format!("{}.", start + i);
                        if let Some(item_blocks) = item.as_array() {
                            out.extend(list_item_blocks(
                                item_blocks,
                                list_depth,
                                &marker,
                                specs,
                                counters,
                            ));
                        }
                    }
                }
            }

            "Table" => out.extend(parse_table(c, specs, counters)),

            "Div" => {
                // c = [attr, [blocks]].  attr.id may carry a label
                // (e.g. table/figure/equation envs in newer Pandoc); emit
                // an Anchor so reader can resolve refs.  Bib entry divs
                // (id starts with "ref-") are picked up here too.
                if let Some(id) = c[0][0].as_str() {
                    if !id.is_empty() {
                        out.push(Block::Anchor(id.to_string()));
                    }
                }
                // Special-case `\begin{thebibliography}` divs: Pandoc
                // emits the inner Paras as plain prose without
                // cite-keys, which means citations can't jump or popup.
                // We replace the inner content with a synthesized
                // numbered bibliography pulled from the source-extracted
                // bibitem map.
                let is_thebib = c[0][1].as_array().map_or(false, |classes| {
                    classes
                        .iter()
                        .any(|cl| cl.as_str() == Some("thebibliography"))
                });
                if is_thebib {
                    // synthesize_bibliography reads from BIBITEMS_ORDERED
                    // (filled by extract_bibitems' \bibitem{key} scan).
                    // Papers using \bibliography{file.bib} (BibLaTeX,
                    // bibtex external) leave BIBITEMS_ORDERED empty,
                    // so synthesize would return [] and the user sees
                    // a blank bibliography body.  Fall through to walk
                    // the inner Paras instead — anchored cite-jumping
                    // is lost but the entries are at least visible.
                    let has_bibitems = BIBITEMS_ORDERED.with(|b| !b.borrow().is_empty());
                    if has_bibitems {
                        BIBLIOGRAPHY_EMITTED.with(|f| f.set(true));
                        out.extend(synthesize_bibliography());
                        continue;
                    }
                    // Else fall through to default inner-walk below.
                }
                if let Some(inner) = c[1].as_array() {
                    out.extend(walk_blocks(inner, list_depth, specs, counters));
                }
            }

            "Figure" => {
                // c = [attr, caption, [blocks]] — see `figure::extract_figure`.
                out.extend(figure::extract_figure(c, counters, specs));
            }

            "LineBlock" => {
                // c = [[inlines], [inlines], ...]  — each line is an inline list
                if let Some(lines) = c.as_array() {
                    for line in lines {
                        if let Some(inlines) = line.as_array() {
                            let text = walk_inlines_text(inlines);
                            if !text.is_empty() {
                                out.push(Block::Line(text));
                            }
                        }
                    }
                }
            }

            // RawBlock, Null — skip
            _ => {}
        }
    }

    out
}

// ── Para → Block ──────────────────────────────────────────────────────────────

fn para_to_block(inlines: &[Value], counters: &mut SectionCounters) -> Vec<Block> {
    // Find every DisplayMath inline in the stream.  arxiv source
    // commonly puts equation environments inside text Paras without
    // blank lines around them — pandoc reads that as one Para with
    // [text, Math DisplayMath, text, ...].  We split such Paras so
    // each equation renders centred on its own line with its
    // equation number, and the surrounding prose flows around it
    // as separate StyledLine blocks.  Lone DisplayMath (no prose
    // around it) is the empty-surround case of the same code path.
    let display_indices: Vec<usize> = inlines
        .iter()
        .enumerate()
        .filter_map(|(i, n)| {
            if n["t"].as_str() == Some("Math") && n["c"][0]["t"].as_str() == Some("DisplayMath") {
                Some(i)
            } else {
                None
            }
        })
        .collect();

    if display_indices.is_empty() {
        // No display math at all — original prose path.
        let spans = walk_inlines_spans(inlines);
        if spans.is_empty() {
            return vec![Block::Blank];
        }
        return vec![Block::StyledLine(spans)];
    }

    // Walk segments: prose-before each DisplayMath → StyledLine,
    // then the math itself → DisplayMath, then continue with the
    // remainder.  Trailing prose after the last DisplayMath flushes
    // at the end.  Empty segments (whitespace-only) are dropped via
    // walk_inlines_spans's existing filtering.
    let mut out: Vec<Block> = Vec::new();
    let mut cursor = 0usize;
    for &di in &display_indices {
        if cursor < di {
            let before_spans = walk_inlines_spans(&inlines[cursor..di]);
            if !before_spans.is_empty() {
                out.push(Block::StyledLine(before_spans));
                out.push(Block::Blank);
            }
        }
        let math_node = &inlines[di];
        let latex = math_node["c"][1].as_str().unwrap_or("");
        if let Some(label) = extract_math_label(latex) {
            out.push(Block::Anchor(label));
        }
        let rendered = render_math(latex);
        let lines: Vec<String> = rendered.lines().map(|l| l.to_string()).collect();
        let count = equation_count_for_source(latex);
        let num = if count > 0 {
            Some(counters.bump_equation(count) as usize)
        } else {
            None
        };
        out.push(Block::DisplayMath { lines, num });
        cursor = di + 1;
    }
    // Trailing prose after the last DisplayMath, if any.
    if cursor < inlines.len() {
        let after_spans = walk_inlines_spans(&inlines[cursor..]);
        if !after_spans.is_empty() {
            out.push(Block::Blank);
            out.push(Block::StyledLine(after_spans));
        }
    }

    if out.is_empty() {
        return vec![Block::Blank];
    }
    out
}

/// Scan a display-math LaTeX source for the first `\label{X}`.  Used to
/// emit a `Block::Anchor` so `\ref{eq:X}` resolves at runtime —
/// Pandoc doesn't lift math labels to `attr.id`.
/// Count how many equation numbers a display-math source claims.
/// Numbered top-level envs:
///   - `equation`, `multline` → 1 number total (multline only the last
///     line is numbered, but for our purposes one number per block).
///   - `align`, `gather`, `eqnarray` → one number per `\\`-separated row.
/// Unnumbered envs (`*`-variants) → 0.  Sub-envs that don't number
/// themselves (`aligned`, `gathered`, `cases`, matrix family) → 0.
/// Bare display math without a `\begin{}` wrapper → 1 (treated as
/// `equation` by default; matches LaTeX's `\[…\]`).
///
/// **v2 refinements**: `\notag` and `\nonumber` directives suppress
/// individual rows; this function ignores them today.  See `v2.md`.
fn equation_count_for_source(latex: &str) -> u32 {
    let trimmed = latex.trim_start();
    let env = trimmed.strip_prefix("\\begin{").and_then(|rest| {
        let close = rest.find('}')?;
        Some(&rest[..close])
    });
    let Some(env) = env else {
        // No `\begin{...}` — bare display math, treat as one numbered eq.
        return 1;
    };
    let starred = env.ends_with('*');
    if starred {
        return 0;
    }
    let base = env;
    match base {
        "equation" | "multline" => 1,
        "align" | "gather" | "eqnarray" => {
            // Count `\\` occurrences in the body.  N separators mean
            // N+1 rows, each numbered.  Source has them as literal
            // backslash-backslash; in the Rust string that's "\\\\".
            (latex.matches("\\\\").count() as u32) + 1
        }
        // Sub-envs (aligned, split, cases, matrix family) don't claim
        // their own numbers — their parent `equation` already did.
        _ => 0,
    }
}

fn extract_math_label(math_src: &str) -> Option<String> {
    let bytes = math_src.as_bytes();
    let mut i = 0;
    while i + 7 <= bytes.len() {
        if !bytes[i..].starts_with(b"\\label{") {
            i += 1;
            continue;
        }
        let key_start = i + 7;
        let key_end = (key_start..bytes.len()).find(|&k| bytes[k] == b'}')?;
        let label = math_src.get(key_start..key_end)?.trim();
        if !label.is_empty() {
            return Some(label.to_string());
        }
        i = key_end + 1;
    }
    None
}

// ── List item helper ──────────────────────────────────────────────────────────

fn list_item_blocks(
    item_blocks: &[Value],
    depth: u8,
    marker: &str,
    specs: &mut Vec<TableSpec>,
    counters: &mut SectionCounters,
) -> Vec<Block> {
    let mut out = Vec::new();
    let mut emitted_item = false;

    for block in item_blocks {
        let t = block["t"].as_str().unwrap_or("");
        if !emitted_item && (t == "Para" || t == "Plain") {
            if let Some(inlines) = block["c"].as_array() {
                let content = walk_inlines_spans(inlines);
                if !content.is_empty() {
                    out.push(Block::ListItem {
                        depth,
                        marker: marker.to_string(),
                        content,
                    });
                    emitted_item = true;
                    continue;
                }
            }
        }
        // Nested lists or extra blocks inside an item.
        out.extend(walk_blocks(
            std::slice::from_ref(block),
            depth.saturating_add(1),
            specs,
            counters,
        ));
        emitted_item = true;
    }

    out
}

// ── Table parsing ─────────────────────────────────────────────────────────────

fn parse_table(
    c: &Value,
    specs: &mut Vec<TableSpec>,
    counters: &mut SectionCounters,
) -> Vec<Block> {
    // Pandoc 3.x: c = [attr, caption, colspec, head, [bodies…], foot]
    let mut out = Vec::new();

    let caption = extract_caption_text(&c[1]);

    // Head: c[3] = [row_attr, [rows]]
    let mut head_rows = extract_rows(&c[3][1]);

    // Bodies: c[4] = [[attr, head_col_count, head_rows, body_rows], …]
    let mut data_rows = Vec::new();
    if let Some(bodies) = c[4].as_array() {
        for body in bodies {
            data_rows.extend(extract_rows(&body[3]));
        }
    }

    // When Pandoc gives us no head (common after we strip cmidrule, since
    // Pandoc was using cmidrule as a soft head/body hint), promote any leading
    // header-like rows from the body into the head. This puts the midrule in
    // the right place — between the column labels and the actual data.
    //
    // Cap at MAX_PROMOTED so the heuristic fails safely on tables where data
    // rows happen to be text-heavy (e.g. complexity tables full of "O(...)"
    // expressions). Academic papers very rarely use more than 2 header rows.
    if head_rows.is_empty() {
        const MAX_PROMOTED: usize = 2;
        let promote = count_header_prefix(&data_rows).min(MAX_PROMOTED);
        if promote > 0 && promote < data_rows.len() {
            head_rows = data_rows.drain(..promote).collect();
        }
    }

    // Match this table to a TableSpec from the queue.  Pandoc gives us the
    // column count via c[2] (colspec array length).  We pop the next entry
    // whose column count matches, with a small look-ahead to tolerate cases
    // where source-list order differs from Pandoc's traversal order.
    let table_cols = c[2].as_array().map(|a| a.len()).unwrap_or(0);
    let spec = take_matching_spec(specs, table_cols);
    let vertical_rules = spec.vertical_rules;

    let head_size = head_rows.len();
    let total_rows = head_size + data_rows.len();

    // Bump the table counter regardless of caption presence so numbering
    // stays consistent even for caption-less tables.
    let n = counters.bump_table();
    // Caption goes ABOVE the table (academic convention for tables;
    // figures are the opposite, caption below).  Format: "[Table N: …]".
    if !caption.is_empty() {
        out.push(Block::Line(format!("[Table {n}: {caption}]")));
    } else {
        out.push(Block::Line(format!("[Table {n}]")));
    }

    if !head_rows.is_empty() {
        out.push(Block::Matrix {
            rows: head_rows,
            vertical_rules: vertical_rules.clone(),
        });
        out.push(Block::Rule);
    }

    // Translate source-row rule positions into body-row offsets.  Skip rules
    // at row 0 (toprule — already emitted by render's top_done logic),
    // at head_size (head/body boundary — already emitted above), and at
    // total_rows (bottomrule — emitted by the trailing Rule below).  What
    // remains splits the data Matrix into multiple Matrix+Rule chunks.
    let mut body_rule_offsets: Vec<usize> = spec
        .horizontal_rules
        .into_iter()
        .filter_map(|r| {
            if r > head_size && r < total_rows {
                Some(r - head_size)
            } else {
                None
            }
        })
        .collect();
    body_rule_offsets.sort_unstable();
    body_rule_offsets.dedup();

    if !data_rows.is_empty() {
        if body_rule_offsets.is_empty() {
            out.push(Block::Matrix {
                rows: data_rows,
                vertical_rules,
            });
            out.push(Block::Rule);
        } else {
            let mut start = 0usize;
            for offset in &body_rule_offsets {
                if *offset > start {
                    let chunk: Vec<_> = data_rows[start..*offset].to_vec();
                    out.push(Block::Matrix {
                        rows: chunk,
                        vertical_rules: vertical_rules.clone(),
                    });
                    out.push(Block::Rule);
                }
                start = *offset;
            }
            if start < data_rows.len() {
                let chunk: Vec<_> = data_rows[start..].to_vec();
                out.push(Block::Matrix {
                    rows: chunk,
                    vertical_rules,
                });
                out.push(Block::Rule);
            }
        }
    }

    // Trailing blank separates the table from the next paragraph of prose.
    // The caption sits above (emitted near the top of `out`).
    out.push(Block::Blank);

    out
}

/// Pop the first entry from `specs` whose column count equals `table_cols`,
/// looking ahead up to `LOOKAHEAD` entries.  Returns the matched
/// `TableSpec`, or a default (no rules) if no match is found.  Tolerant of
/// source-list ordering that doesn't perfectly mirror Pandoc's table
/// traversal — falls back to "no rules" rather than risk attaching the
/// wrong rules to a table.
fn take_matching_spec(specs: &mut Vec<TableSpec>, table_cols: usize) -> TableSpec {
    const LOOKAHEAD: usize = 3;
    let limit = specs.len().min(LOOKAHEAD);
    for k in 0..limit {
        if specs[k].col_count == table_cols {
            return specs.remove(k);
        }
    }
    TableSpec {
        col_count: table_cols,
        vertical_rules: Vec::new(),
        horizontal_rules: Vec::new(),
        column_gaps_after: Vec::new(),
    }
}

/// Count the leading rows of `rows` that look like header rows.  Stops at the
/// first data-like row.  Used to recover the head/body boundary when Pandoc
/// puts every row in the body (which happens for booktabs tables once cmidrule
/// commands have been stripped).
fn count_header_prefix(rows: &[Vec<(String, usize)>]) -> usize {
    rows.iter().take_while(|r| !looks_like_data_row(r)).count()
}

/// A row is "data-like" if its leading non-empty cells fit the
/// `text-then-number` pattern of a typical data row: either the first
/// non-empty cell starts with a digit (e.g. "1", "23.75"), or the second
/// non-empty cell does (e.g. "ByteNet", "23.75").
fn looks_like_data_row(row: &[(String, usize)]) -> bool {
    let mut non_empty = row.iter().map(|(t, _)| t.trim()).filter(|t| !t.is_empty());
    let first = match non_empty.next() {
        Some(s) => s,
        None => return false, // empty row — neither header nor data
    };
    if starts_with_ascii_digit(first) {
        return true;
    }
    if let Some(second) = non_empty.next() {
        if starts_with_ascii_digit(second) {
            return true;
        }
    }
    false
}

fn starts_with_ascii_digit(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_digit())
}

fn extract_rows(rows_json: &Value) -> Vec<Vec<(String, usize)>> {
    // Pandoc 3.x cell shape: cell = [attr, alignment, rowspan, colspan, [blocks]].
    // When a cell has rowspan > 1, subsequent rows omit cells in the columns the
    // rowspan covers — we have to insert blank placeholders so column alignment
    // is preserved (this is what \multirow{4}{*}{(A)} compiles to).
    let raw_rows = match rows_json.as_array() {
        Some(rs) => rs,
        None => return Vec::new(),
    };

    // carry[col] = number of further rows that column `col` is occupied by
    // an open rowspan from above.
    let mut carry: Vec<usize> = Vec::new();
    let mut out: Vec<Vec<(String, usize)>> = Vec::with_capacity(raw_rows.len());

    for row in raw_rows {
        let cells = match row[1].as_array() {
            Some(cs) => cs,
            None => {
                out.push(Vec::new());
                continue;
            }
        };

        let mut row_out: Vec<(String, usize)> = Vec::new();
        let mut col = 0usize;
        let mut cell_iter = cells.iter();

        loop {
            // If this column position is already claimed by a rowspan from
            // above, emit a blank filler and decrement the carry.
            if col < carry.len() && carry[col] > 0 {
                row_out.push((String::new(), 1));
                carry[col] -= 1;
                col += 1;
                continue;
            }
            // Otherwise consume the next user-supplied cell.
            let Some(cell) = cell_iter.next() else { break };
            let rowspan = cell[2].as_u64().unwrap_or(1).max(1) as usize;
            let colspan = cell[3].as_u64().unwrap_or(1).max(1) as usize;
            let text = extract_cell_text(&cell[4]);
            row_out.push((text, colspan));

            if rowspan > 1 {
                let end = col + colspan;
                if carry.len() < end {
                    carry.resize(end, 0);
                }
                for c in col..end {
                    carry[c] = rowspan - 1;
                }
            }
            col += colspan;
        }

        out.push(row_out);
    }

    out
}

fn extract_cell_text(blocks: &Value) -> String {
    blocks
        .as_array()
        .map(|bs| {
            bs.iter()
                .filter_map(|b| match b["t"].as_str()? {
                    "Para" | "Plain" => b["c"].as_array().map(|il| walk_inlines_text(il)),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

fn extract_caption_text(cap: &Value) -> String {
    // Pandoc 3.x serialises Caption as [short_or_null, [blocks]].
    // Older serialisations use {"t":"Caption","c":[short,[blocks]]}.
    let blocks = if cap.is_array() {
        &cap[1]
    } else {
        &cap["c"][1]
    };
    blocks
        .as_array()
        .map(|bs| {
            bs.iter()
                .filter_map(|b| match b["t"].as_str()? {
                    "Para" | "Plain" => b["c"].as_array().map(|il| walk_inlines_text(il)),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

// ── Math rendering ─────────────────────────────────────────────────────────────

// Display math: pass the full source — including the `\begin{X}…\end{X}`
// wrapper if present — to math_render.  Its preprocess uses the env
// name to decide whether `\\` is structural (multi-equation systems
// like `align`/`aligned`/`gather`) or decorative (single-equation
// `equation`/`equation*`/`split`).  strip_latex then drops the
// `\begin{X}` / `\end{X}` commands silently via its catch-all list.
pub(super) fn render_math(latex: &str) -> String {
    math_render::render(math_render::MathInput::Latex(latex.trim()))
}

// Inline math: strip_latex only — tui_math's vertical output fragments into
// separate words when split_whitespace runs on the resulting span text.
pub(super) fn render_inline_math(latex: &str) -> String {
    let s = latex.trim();
    let inner = strip_env_wrapper(s).unwrap_or(s);
    math_render::render_inline(inner)
}

// Strip \begin{env}...\end{env} wrappers that Pandoc includes in Math node content
// (e.g. \begin{align*}MultiHead...\end{align*} → just the body).
fn strip_env_wrapper(s: &str) -> Option<&str> {
    if !s.starts_with("\\begin{") {
        return None;
    }
    let close = s.find('}')?;
    let env = &s[7..close];
    let end_tag_len = 6 + env.len() + 1; // \end{ + env + }
    let body = s[close + 1..].trim_start_matches('\n');
    // Find last occurrence of \end{env} to handle nested envs correctly.
    let end_pos = body.rfind(&format!("\\end{{{}}}", env))?;
    let inner = body[..end_pos].trim();
    // Guard against infinite recursion: if inner still starts with the same env, give up.
    if inner.starts_with("\\begin{") && inner[7..].starts_with(env) {
        return None;
    }
    let _ = end_tag_len; // suppress unused warning
    Some(inner)
}

#[cfg(test)]
mod para_to_block_tests {
    use super::*;
    use serde_json::json;

    fn str_inline(s: &str) -> Value {
        json!({"t": "Str", "c": s})
    }
    fn space() -> Value {
        json!({"t": "Space"})
    }
    fn display_math(latex: &str) -> Value {
        json!({"t": "Math", "c": [{"t": "DisplayMath"}, latex]})
    }

    fn block_kind(b: &Block) -> &'static str {
        match b {
            Block::Line(_) => "Line",
            Block::DisplayMath { .. } => "DisplayMath",
            Block::Header { .. } => "Header",
            Block::Matrix { .. } => "Matrix",
            Block::Blank => "Blank",
            Block::StyledLine(_) => "StyledLine",
            Block::ListItem { .. } => "ListItem",
            Block::CodeBlock { .. } => "CodeBlock",
            Block::Rule => "Rule",
            Block::Quote(_) => "Quote",
            Block::Anchor(_) => "Anchor",
            Block::Figure { .. } => "Figure",
        }
    }

    #[test]
    fn lone_display_math_emits_centered_block() {
        // Regression coverage for the existing fast path.  A Para
        // containing only a DisplayMath inline should produce a
        // Block::DisplayMath, not a StyledLine.
        let inlines = vec![display_math("x = y")];
        let mut counters = SectionCounters::new();
        let blocks = para_to_block(&inlines, &mut counters);
        assert!(
            blocks
                .iter()
                .any(|b| matches!(b, Block::DisplayMath { .. })),
            "expected DisplayMath block, got kinds: {:?}",
            blocks.iter().map(block_kind).collect::<Vec<_>>()
        );
        assert!(!blocks.iter().any(|b| matches!(b, Block::StyledLine(_))));
    }

    #[test]
    fn mixed_para_splits_text_math_text() {
        // The bug: arxiv source like
        //   "... cross-attention layers:
        //    \begin{equation}Z = f(x)\end{equation}
        //    When discussing..."
        // (no blank lines around the equation) parses as ONE Para
        // with inlines [text, Math DisplayMath, text].  The old fast
        // path only recognized lone-DisplayMath, so the math
        // rendered inline with the surrounding prose.  Now we split.
        let inlines = vec![
            str_inline("layers:"),
            display_math("Z = f(x)"),
            str_inline("When"),
            space(),
            str_inline("discussing..."),
        ];
        let mut counters = SectionCounters::new();
        let blocks = para_to_block(&inlines, &mut counters);

        let has_display_math = blocks
            .iter()
            .any(|b| matches!(b, Block::DisplayMath { .. }));
        let before_styled = blocks.iter().any(|b| match b {
            Block::StyledLine(spans) => spans.iter().any(|s| s.text.contains("layers")),
            _ => false,
        });
        let after_styled = blocks.iter().any(|b| match b {
            Block::StyledLine(spans) => spans.iter().any(|s| s.text.contains("discussing")),
            _ => false,
        });
        let kinds: Vec<&str> = blocks.iter().map(block_kind).collect();
        assert!(
            has_display_math,
            "expected a DisplayMath block; got kinds: {kinds:?}"
        );
        assert!(
            before_styled,
            "expected a StyledLine carrying the prose before the math; got kinds: {kinds:?}"
        );
        assert!(
            after_styled,
            "expected a StyledLine carrying the prose after the math; got kinds: {kinds:?}"
        );

        // Order check: before-text, then math, then after-text.
        let before_idx = blocks
            .iter()
            .position(|b| matches!(b, Block::StyledLine(s) if s.iter().any(|x| x.text.contains("layers"))))
            .expect("before idx");
        let math_idx = blocks
            .iter()
            .position(|b| matches!(b, Block::DisplayMath { .. }))
            .expect("math idx");
        let after_idx = blocks
            .iter()
            .position(|b| matches!(b, Block::StyledLine(s) if s.iter().any(|x| x.text.contains("discussing"))))
            .expect("after idx");
        assert!(before_idx < math_idx, "before-text must precede math");
        assert!(math_idx < after_idx, "math must precede after-text");
    }

    #[test]
    fn empty_para_returns_blank() {
        // Sanity: empty inline list still produces Block::Blank, not panic.
        let inlines: Vec<Value> = vec![];
        let mut counters = SectionCounters::new();
        let blocks = para_to_block(&inlines, &mut counters);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(blocks[0], Block::Blank));
    }
}

#[cfg(test)]
mod math_label_tests {
    use super::extract_math_label;

    #[test]
    fn finds_label_in_equation() {
        let src = r"\begin{equation}\label{eq:elbo} x = y \end{equation}";
        assert_eq!(extract_math_label(src).as_deref(), Some("eq:elbo"));
    }

    #[test]
    fn finds_label_when_indented() {
        let src = "\\begin{equation}\n  \\label{eq:foo}\n  x = y\n\\end{equation}";
        assert_eq!(extract_math_label(src).as_deref(), Some("eq:foo"));
    }

    #[test]
    fn returns_first_label_in_align() {
        // Multi-equation align: take the first label (v1 simplification).
        let src = r"\begin{align} a &= b \label{eq:one} \\ c &= d \label{eq:two} \end{align}";
        assert_eq!(extract_math_label(src).as_deref(), Some("eq:one"));
    }

    #[test]
    fn no_label_returns_none() {
        let src = r"\begin{equation} x = y \end{equation}";
        assert!(extract_math_label(src).is_none());
    }

    #[test]
    fn empty_label_returns_none() {
        let src = r"\begin{equation}\label{} x = y \end{equation}";
        assert!(extract_math_label(src).is_none());
    }

    #[test]
    fn handles_label_at_end_without_brace() {
        // Truncated input — must not panic / over-index.
        let src = r"\label{eq:x";
        assert!(extract_math_label(src).is_none());
    }
}

#[cfg(test)]
mod section_counter_tests {
    use super::SectionCounters;

    #[test]
    fn single_section_increments() {
        let mut c = SectionCounters::new();
        assert_eq!(c.bump(1), "1");
        assert_eq!(c.bump(1), "2");
        assert_eq!(c.bump(1), "3");
    }

    #[test]
    fn nested_levels_format() {
        let mut c = SectionCounters::new();
        assert_eq!(c.bump(1), "1");
        assert_eq!(c.bump(2), "1.1");
        assert_eq!(c.bump(2), "1.2");
        assert_eq!(c.bump(3), "1.2.1");
        assert_eq!(c.bump(3), "1.2.2");
    }

    #[test]
    fn outer_bump_resets_inner() {
        let mut c = SectionCounters::new();
        c.bump(1); // 1
        c.bump(2); // 1.1
        c.bump(3); // 1.1.1
        assert_eq!(c.bump(1), "2"); // resets sub & subsub
        assert_eq!(c.bump(2), "2.1"); // sub starts fresh
        assert_eq!(c.bump(3), "2.1.1"); // subsub starts fresh
    }

    #[test]
    fn subsection_bump_resets_subsubsection() {
        let mut c = SectionCounters::new();
        c.bump(1); // 1
        c.bump(2); // 1.1
        c.bump(3); // 1.1.1
        c.bump(3); // 1.1.2
        assert_eq!(c.bump(2), "1.2"); // sub bump resets subsub
        assert_eq!(c.bump(3), "1.2.1");
    }

    #[test]
    fn level_clamped_to_three() {
        let mut c = SectionCounters::new();
        // Levels above 3 (e.g. \paragraph in LaTeX) are clamped to subsubsection.
        assert_eq!(c.bump(4), "0.0.1");
        assert_eq!(c.bump(5), "0.0.2");
    }
}

