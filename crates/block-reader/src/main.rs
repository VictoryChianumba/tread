use arxiv_render::{fetch, parse, pdf_anchors, placement};

fn main() {
  let arg = std::env::args().nth(1).unwrap_or_default();
  if arg.is_empty() {
    eprintln!("usage: block-reader <arxiv-id-or-url>");
    std::process::exit(1);
  }

  let id = match fetch::extract_id(&arg) {
    Some(id) => id,
    None => {
      eprintln!("error: could not extract a valid arXiv ID from {:?}", arg);
      std::process::exit(1);
    }
  };

  eprintln!("fetching source for arXiv:{id} ...");

  let sources = match fetch::fetch_source(&id) {
    Ok(s) => s,
    Err(e) => {
      eprintln!("error: {e}");
      std::process::exit(1);
    }
  };

  eprintln!("found {} .tex file(s); parsing ...", sources.len());

  let blocks = parse::to_blocks(sources);

  // Best-effort PDF anchor extraction.  Tables are floats — their source
  // position rarely matches the rendered position the LaTeX typesetter
  // chose.  We pull the rendered PDF, extract per-table anchor phrases,
  // and lift each Matrix group to where it belongs in reading order.
  // Failures (no network, missing pdftotext, no anchors found) leave the
  // blocks in source order.
  let anchors = match fetch::fetch_pdf(&id) {
    Ok(pdf) => {
      let a = pdf_anchors::extract_anchors(&pdf);
      eprintln!("placement: extracted {} table anchor(s) from PDF", a.len());
      a
    }
    Err(e) => {
      eprintln!("placement: PDF fetch failed ({e}); using source-order placement");
      Vec::new()
    }
  };
  let blocks = placement::lift_tables(blocks, &anchors);

  eprintln!("{} blocks — launching reader ...", blocks.len());

  if let Err(e) = block_reader::run(blocks, None, Some(id)) {
    eprintln!("reader error: {e}");
    std::process::exit(1);
  }
}
