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

  // Stage 1 of pixel-graphics work: detect terminal capability up front.
  // For now we just log the result; later stages gate image rendering on it.
  let kitty_capability = kitty_graphics::detect();
  eprintln!("kitty graphics: {:?}", kitty_capability);
  // Inside tmux, kitty escapes are wrapped in the tmux passthrough
  // envelope automatically — but tmux drops them unless the user has
  // `set -g allow-passthrough on`.  Print a one-time hint so a missing
  // figure doesn't read as silent breakage.
  if kitty_graphics::in_tmux() && matches!(kitty_capability, kitty_graphics::Capability::Supported) {
    eprintln!(
      "tmux detected: for inline graphics add to ~/.tmux.conf:\n  \
       set -g allow-passthrough on    (forwards Kitty escapes to iTerm2)\n  \
       set -g focus-events on         (lets the reader clear images on pane switch)"
    );
  }

  eprintln!("fetching source for arXiv:{id} ...");

  let fetched = match fetch::fetch_source(&id) {
    Ok(s) => s,
    Err(e) => {
      eprintln!("error: {e}");
      std::process::exit(1);
    }
  };
  let sources = fetched.tex;
  let asset_dir = fetched.asset_dir;

  eprintln!("found {} .tex file(s); parsing ...", sources.len());

  // Pre-scan source for `\bibitem{key}` entries so citations can pop up
  // their bib record on `K`/`Shift+Enter` even when Pandoc's AST doesn't
  // carry cite-keys down to the rendered bibliography paragraphs.
  let bibitems = arxiv_render::extract_bibitems(&sources);
  eprintln!("parsed {} bibitem entries from source", bibitems.len());

  let mut blocks = parse::to_blocks(sources);
  // Stage 6: terminal-capability fallback.  On terminals without any
  // inline-graphics protocol, replace every Image/ImageRow with its
  // caption text — no reserved blank rows, no would-be-emitted escapes.
  // Otherwise, resolve paths to absolute so the reader can load bytes.
  if matches!(kitty_capability, kitty_graphics::Capability::Supported) {
    arxiv_render::absolutize_image_paths(&mut blocks, &asset_dir);
  } else {
    arxiv_render::degrade_images_to_captions(&mut blocks);
  }

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

  let image_count: usize = blocks.iter().map(|b| match b {
    doc_model::Block::Image { .. } => 1,
    doc_model::Block::ImageRow { items, .. } => items.len(),
    _ => 0,
  }).sum();
  eprintln!("{} blocks ({} images) — launching reader ...", blocks.len(), image_count);

  if let Err(e) = block_reader::run(blocks, None, Some(id), bibitems) {
    eprintln!("reader error: {e}");
    std::process::exit(1);
  }
}
