//! Smoke-test the post-ar5iv fetch_paper pipeline.  Runs the full
//! ingest (PDF anchors + ar5iv parse, falling back to tarball+Pandoc
//! when ar5iv misses) and reports block / bibitem / anchor counts.
//!
//! Usage:
//!   cargo run -p tread --release --example smoke_fetch -- 1706.03762

fn main() {
    let id = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: smoke_fetch <arxiv-id>");
        std::process::exit(2)
    });

    let t0 = std::time::Instant::now();
    let data = tread::fetch_paper(&id, false).expect("fetch_paper");
    let elapsed = t0.elapsed();

    let mut headers = 0;
    let mut display_math = 0;
    let mut matrix = 0;
    let mut figures = 0;
    let mut anchors = 0;
    for b in &data.blocks {
        match b {
            doc_model::Block::Header { .. } => headers += 1,
            doc_model::Block::DisplayMath { .. } => display_math += 1,
            doc_model::Block::Matrix { .. } => matrix += 1,
            doc_model::Block::Figure { .. } => figures += 1,
            doc_model::Block::Anchor(_) => anchors += 1,
            _ => {}
        }
    }
    println!("=== {id} ===");
    println!("wall:     {:?}", elapsed);
    println!("blocks:   {}", data.blocks.len());
    println!("bibitems: {}", data.bibitems.len());
    println!("headers:  {headers}");
    println!("displayM: {display_math}");
    println!("matrix:   {matrix}");
    println!("figures:  {figures}");
    println!("anchors:  {anchors}");
}
