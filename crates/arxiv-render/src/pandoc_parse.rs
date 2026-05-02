// Pandoc-based LaTeX parser.
//
// Runs `pandoc -f latex -t json` as a subprocess, walks the resulting AST, and
// emits Vec<Block> — the same type produced by parse.rs.  The caller tries this
// path first; if Pandoc is unavailable or returns nothing, it falls back to the
// hand-rolled parser.

use doc_model::{Block, InlineSpan};
use serde_json::Value;

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
    let tmp = tempfile::TempDir::new().map_err(|e| e.to_string())?;
    for (name, content) in sources {
        let dest = tmp.path().join(name);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&dest, content.as_bytes())
            .map_err(|e| format!("write {name}: {e}"))?;
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

    let ast: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("pandoc JSON: {e}"))?;

    let mut blocks = Vec::new();
    if let Some(meta) = ast.get("meta") {
        blocks.extend(extract_meta_blocks(meta));
    }
    if let Some(arr) = ast["blocks"].as_array() {
        blocks.extend(walk_blocks(arr, 0));
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
        Some("MetaList") => author_meta["c"].as_array().map(|a| a.as_slice()).unwrap_or(&[]),
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
            let has_marker =
                seg.iter().any(|n| matches!(n["t"].as_str(), Some("Note") | Some("Span")));
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

fn walk_blocks(nodes: &[Value], list_depth: u8) -> Vec<Block> {
    let mut out = Vec::new();

    for node in nodes {
        let t = node["t"].as_str().unwrap_or("");
        let c = &node["c"];

        match t {
            "Para" | "Plain" => {
                if let Some(inlines) = c.as_array() {
                    if let Some(b) = para_to_block(inlines) {
                        out.push(b);
                        out.push(Block::Blank);
                    }
                }
            }

            "Header" => {
                let level = c[0].as_u64().unwrap_or(1).min(3) as u8;
                if let Some(inlines) = c[2].as_array() {
                    let text = walk_inlines_text(inlines);
                    if !text.is_empty() {
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
                            out.extend(list_item_blocks(item_blocks, list_depth, "•"));
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
                            out.extend(list_item_blocks(item_blocks, list_depth, &marker));
                        }
                    }
                }
            }

            "Table" => out.extend(parse_table(c)),

            "Div" => {
                // c = [attr, [blocks]]
                if let Some(inner) = c[1].as_array() {
                    out.extend(walk_blocks(inner, list_depth));
                }
            }

            "Figure" => {
                // c = [attr, caption, [blocks]]
                let cap = extract_caption_text(&c[1]);
                if !cap.is_empty() {
                    out.push(Block::Line(format!("[Figure: {cap}]")));
                }
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

fn para_to_block(inlines: &[Value]) -> Option<Block> {
    // Check for a lone DisplayMath inline (possibly surrounded by whitespace).
    let meaningful: Vec<&Value> = inlines
        .iter()
        .filter(|n| !matches!(n["t"].as_str(), Some("Space") | Some("SoftBreak")))
        .collect();

    if meaningful.len() == 1 {
        let node = meaningful[0];
        if node["t"].as_str() == Some("Math")
            && node["c"][0]["t"].as_str() == Some("DisplayMath")
        {
            let latex = node["c"][1].as_str().unwrap_or("");
            let rendered = render_math(latex);
            let lines: Vec<String> = rendered.lines().map(|l| l.to_string()).collect();
            return Some(Block::DisplayMath { lines, num: None });
        }
    }

    let spans = walk_inlines_spans(inlines);
    if spans.is_empty() {
        return Some(Block::Blank);
    }

    // Always use StyledLine so build_visual_lines wraps the text to terminal_width.
    // Block::Line assumes the producer already wrapped it; Pandoc gives us full paragraphs.
    Some(Block::StyledLine(spans))
}

// ── List item helper ──────────────────────────────────────────────────────────

fn list_item_blocks(item_blocks: &[Value], depth: u8, marker: &str) -> Vec<Block> {
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
        ));
        emitted_item = true;
    }

    out
}

// ── Table parsing ─────────────────────────────────────────────────────────────

fn parse_table(c: &Value) -> Vec<Block> {
    // Pandoc 3.x: c = [attr, caption, colspec, head, [bodies…], foot]
    let mut out = Vec::new();

    let caption = extract_caption_text(&c[1]);

    // Head: c[3] = [row_attr, [rows]]
    let head_rows = extract_rows(&c[3][1]);
    if !head_rows.is_empty() {
        out.push(Block::Matrix { rows: head_rows });
        out.push(Block::Rule);
    }

    // Bodies: c[4] = [[attr, head_col_count, head_rows, body_rows], …]
    let mut data_rows = Vec::new();
    if let Some(bodies) = c[4].as_array() {
        for body in bodies {
            data_rows.extend(extract_rows(&body[3]));
        }
    }
    if !data_rows.is_empty() {
        out.push(Block::Matrix { rows: data_rows });
        out.push(Block::Rule);
    }

    if !caption.is_empty() {
        out.push(Block::Line(format!("[Table: {caption}]")));
    }

    out
}

fn extract_rows(rows_json: &Value) -> Vec<Vec<(String, usize)>> {
    rows_json
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    // row = [row_attr, [cells]]
                    row[1]
                        .as_array()
                        .map(|cells| {
                            cells
                                .iter()
                                .map(|cell| {
                                    // cell = [attr, alignment, rowspan, colspan, [blocks]]
                                    let colspan =
                                        cell[3].as_u64().unwrap_or(1).max(1) as usize;
                                    let text = extract_cell_text(&cell[4]);
                                    (text, colspan)
                                })
                                .collect()
                        })
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn extract_cell_text(blocks: &Value) -> String {
    blocks
        .as_array()
        .map(|bs| {
            bs.iter()
                .filter_map(|b| match b["t"].as_str()? {
                    "Para" | "Plain" => {
                        b["c"].as_array().map(|il| walk_inlines_text(il))
                    }
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
    let blocks = if cap.is_array() { &cap[1] } else { &cap["c"][1] };
    blocks
        .as_array()
        .map(|bs| {
            bs.iter()
                .filter_map(|b| match b["t"].as_str()? {
                    "Para" | "Plain" => {
                        b["c"].as_array().map(|il| walk_inlines_text(il))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

// ── Inline walkers ────────────────────────────────────────────────────────────

fn walk_inlines_text(inlines: &[Value]) -> String {
    walk_inlines_spans(inlines)
        .into_iter()
        .map(|s| s.text)
        .collect()
}

fn walk_inlines_spans(inlines: &[Value]) -> Vec<InlineSpan> {
    let mut out = Vec::new();

    for node in inlines {
        let t = node["t"].as_str().unwrap_or("");
        let c = &node["c"];

        match t {
            "Str" => {
                if let Some(s) = c.as_str() {
                    // U+00A0 (non-breaking space from LaTeX ~) → regular space.
                    let s = s.replace('\u{00A0}', " ");
                    let s = s.trim();
                    if !s.is_empty() {
                        out.push(InlineSpan::plain(s));
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

            // No terminal equivalent for these — pass text through unchanged.
            "SmallCaps" | "Superscript" | "Subscript" => {
                if let Some(inner) = c.as_array() {
                    out.extend(walk_inlines_spans(inner));
                }
            }

            "Code" => {
                // c = [attr, text]
                if let Some(text) = c[1].as_str() {
                    out.push(InlineSpan { monospace: true, ..InlineSpan::plain(text) });
                }
            }

            "Math" => {
                let kind = c[0]["t"].as_str().unwrap_or("InlineMath");
                let latex = c[1].as_str().unwrap_or("");
                let rendered = render_math(latex);
                if kind == "DisplayMath" {
                    // Normally caught in para_to_block; guard the in-line fall-through.
                    out.push(InlineSpan::plain(format!("  {rendered}  ")));
                } else {
                    out.push(InlineSpan::plain(rendered));
                }
            }

            "Link" => {
                // c = [attr, inlines, [url, title]]
                // Set url for OSC 8 terminal hyperlinks; don't force underline — it clutters prose.
                let url = c[2][0].as_str().map(|s| s.to_string());
                if let Some(inner) = c[1].as_array() {
                    for mut span in walk_inlines_spans(inner) {
                        if span.url.is_none() {
                            span.url = url.clone();
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
                if let Some(inner) = c[1].as_array() {
                    out.extend(walk_inlines_spans(inner));
                }
            }

            "Span" => {
                // c = [attr, inlines]
                if let Some(inner) = c[1].as_array() {
                    out.extend(walk_inlines_spans(inner));
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

    out
}

// ── Math rendering ─────────────────────────────────────────────────────────────

fn render_math(latex: &str) -> String {
    let s = latex.trim();
    let inner = strip_env_wrapper(s).unwrap_or(s);
    math_render::render(math_render::MathInput::Latex(inner))
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
