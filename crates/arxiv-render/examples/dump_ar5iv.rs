//! Fetch an arXiv paper from ar5iv, parse it via `ar5iv_parse`, and dump
//! a textual rendering of the resulting block stream to stdout.  Used to
//! diff against `pandoc_parse` output and eyeball fidelity gaps.
//!
//! Usage:
//!   cargo run -p arxiv-render --example dump_ar5iv -- 1706.03762
//!   cargo run -p arxiv-render --example dump_ar5iv -- --file /tmp/ar5iv_attention.html

use arxiv_render::ar5iv_parse;
use doc_model::Block;

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: dump_ar5iv <arxiv-id> | --file <path>");
        std::process::exit(2)
    });

    let html = if arg == "--file" {
        let path = std::env::args().nth(2).expect("--file needs a path");
        std::fs::read_to_string(path).expect("read html")
    } else {
        let url = format!("https://ar5iv.labs.arxiv.org/html/{arg}");
        reqwest::blocking::get(&url)
            .and_then(|r| r.text())
            .expect("fetch ar5iv")
    };

    let blocks = ar5iv_parse::to_blocks(&html);
    eprintln!("# blocks: {}", blocks.len());
    for b in &blocks {
        match b {
            Block::Header { level, text } => println!("{} {text}", "#".repeat(*level as usize)),
            Block::Line(s) => println!("{s}"),
            Block::StyledLine(spans) => {
                let s: String = spans.iter().map(|sp| sp.text.as_str()).collect();
                println!("{s}");
            }
            Block::DisplayMath { lines, .. } => {
                for l in lines {
                    println!("    {l}");
                }
            }
            Block::Matrix { rows, .. } => {
                for row in rows {
                    let cells: Vec<&str> = row.iter().map(|(t, _)| t.as_str()).collect();
                    println!("| {} |", cells.join(" | "));
                }
            }
            Block::Blank => println!(),
            Block::Anchor(id) => println!("[anchor: {id}]"),
            Block::Rule => println!("---"),
            Block::ListItem { marker, content, depth } => {
                let s: String = content.iter().map(|sp| sp.text.as_str()).collect();
                println!("{}{marker}{s}", "  ".repeat(*depth as usize));
            }
            Block::CodeBlock { lines, .. } => {
                println!("```");
                for l in lines { println!("{l}"); }
                println!("```");
            }
            Block::Quote(spans) => {
                let s: String = spans.iter().map(|sp| sp.text.as_str()).collect();
                println!("> {s}");
            }
            Block::Figure { alt, figure_id, rows, .. } => {
                let paths: Vec<String> = rows
                    .iter()
                    .flatten()
                    .map(|it| format!("{}#{}", it.path.display(), it.kitty_id))
                    .collect();
                println!("[figure {figure_id}: {alt} | imgs: {}]", paths.join(", "));
            }
        }
    }
}
