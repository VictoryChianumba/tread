//! Side-by-side parser parity probe.
//!
//! Runs both the ar5iv (primary) and Pandoc (fallback) parsers against
//! one paper and prints a per-`Block`-kind count table.  Useful for
//! spotting new divergences when either parser changes — the historical
//! ar5iv figure/list gaps (B9 / B10 in `docs/backlog.md`) are now closed,
//! so Figure and ListItem counts should track the Pandoc reference.
//!
//! Invocation:
//!
//!     cargo run --release -p arxiv-render --example parity_probe -- <arxiv-id>
//!
//! Example:
//!
//!     cargo run --release -p arxiv-render --example parity_probe -- 1706.03762
//!
//! Output is a table:
//!
//!     === <id> parity probe ===
//!     ar5iv:  N blocks
//!     pandoc: M blocks
//!
//!     kind            ar5iv pandoc
//!     ...

use std::collections::BTreeMap;

use arxiv_render::{ar5iv_parse, fetch, pandoc_parse};
use doc_model::Block;

fn count_kinds(blocks: &[Block]) -> BTreeMap<&'static str, usize> {
    let mut m = BTreeMap::new();
    for b in blocks {
        let k = match b {
            Block::Line(_) => "Line",
            Block::StyledLine(_) => "StyledLine",
            Block::DisplayMath { .. } => "DisplayMath",
            Block::Header { .. } => "Header",
            Block::Matrix { .. } => "Matrix",
            Block::Blank => "Blank",
            Block::ListItem { .. } => "ListItem",
            Block::CodeBlock { .. } => "CodeBlock",
            Block::Rule => "Rule",
            Block::Quote(_) => "Quote",
            Block::Anchor(_) => "Anchor",
            Block::Figure { .. } => "Figure",
        };
        *m.entry(k).or_insert(0) += 1;
    }
    m
}

fn main() {
    let id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "1706.03762".to_string());

    eprintln!("fetching ar5iv HTML for {id} ...");
    let html = match fetch::fetch_ar5iv(&id) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("ar5iv fetch failed: {e}");
            std::process::exit(1);
        }
    };
    let ar5iv_blocks = ar5iv_parse::to_blocks(&html);

    eprintln!("fetching tarball + running pandoc for {id} ...");
    let pandoc_blocks = match fetch::fetch_source(&id) {
        Ok(f) => match pandoc_parse::try_pandoc(&f.tex) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("pandoc parse failed: {e}");
                eprintln!("(continuing with ar5iv-only counts)");
                Vec::new()
            }
        },
        Err(e) => {
            eprintln!("tarball fetch failed: {e}");
            eprintln!("(continuing with ar5iv-only counts)");
            Vec::new()
        }
    };

    println!("=== {id} parity probe ===");
    println!("ar5iv:  {} blocks", ar5iv_blocks.len());
    println!("pandoc: {} blocks", pandoc_blocks.len());
    println!();
    println!("{:<14} {:>6} {:>6}", "kind", "ar5iv", "pandoc");
    let ar5iv_kinds = count_kinds(&ar5iv_blocks);
    let pandoc_kinds = count_kinds(&pandoc_blocks);
    let all: std::collections::BTreeSet<&&str> = ar5iv_kinds.keys().chain(pandoc_kinds.keys()).collect();
    for kind in all {
        let a = ar5iv_kinds.get(*kind).copied().unwrap_or(0);
        let p = pandoc_kinds.get(*kind).copied().unwrap_or(0);
        let marker = if a == 0 && p > 0 {
            " ← ar5iv gap"
        } else if p == 0 && a > 0 {
            " ← pandoc gap"
        } else {
            ""
        };
        println!("{:<14} {:>6} {:>6}{}", kind, a, p, marker);
    }
}
