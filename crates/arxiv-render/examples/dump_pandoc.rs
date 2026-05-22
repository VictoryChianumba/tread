//! Sibling of `dump_ar5iv`: fetch an arXiv paper's tarball, run the
//! Pandoc parser, and dump the resulting block stream in the same text
//! format so we can diff against the ar5iv path.
//!
//! Usage:
//!   cargo run -p arxiv-render --example dump_pandoc -- 1706.03762

use arxiv_render::{fetch, pandoc_parse};
use doc_model::Block;

fn main() {
    let id = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: dump_pandoc <arxiv-id>");
        std::process::exit(2)
    });

    let src = fetch::fetch_source(&id).expect("fetch source");
    let blocks = pandoc_parse::try_pandoc(&src.tex).expect("pandoc parse");
    eprintln!("# blocks: {}", blocks.len());
    for b in &blocks {
        match b {
            Block::Header { level, text, number } => {
                let n = number.as_deref().map(|n| format!("{n}  ")).unwrap_or_default();
                println!("{} {n}{text}", "#".repeat(*level as usize))
            }
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
            Block::Figure { alt, .. } => println!("[figure: {alt}]"),
        }
    }
}
