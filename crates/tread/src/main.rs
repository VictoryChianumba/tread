use arxiv_render::fetch;

fn main() {
  let arg = std::env::args().nth(1).unwrap_or_default();
  if arg.is_empty() {
    eprintln!("usage: tread <arxiv-id-or-url>");
    std::process::exit(1);
  }

  let id = match fetch::extract_id(&arg) {
    Some(id) => id,
    None => {
      eprintln!("error: could not extract a valid arXiv ID from {:?}", arg);
      std::process::exit(1);
    }
  };

  // Detect terminal capability up front — gates all inline-graphics work.
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

  let kitty_supported = matches!(kitty_capability, kitty_graphics::Capability::Supported);

  eprintln!("fetching source for arXiv:{id} ...");

  let data = match tread::fetch_paper(&id, kitty_supported) {
    Ok(d) => d,
    Err(e) => {
      eprintln!("error: {e}");
      std::process::exit(1);
    }
  };

  let image_count: usize = data.blocks.iter().map(|b| match b {
    doc_model::Block::Image { .. } => 1,
    doc_model::Block::ImageRow { items, .. } => items.len(),
    _ => 0,
  }).sum();
  eprintln!(
    "{} blocks ({} images, {} bibitems) — launching reader ...",
    data.blocks.len(), image_count, data.bibitems.len()
  );

  if let Err(e) = tread::run(data.blocks, None, Some(id), data.bibitems) {
    eprintln!("reader error: {e}");
    std::process::exit(1);
  }
}
