use arxiv_render::{fetch, parse};

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
  eprintln!("{} blocks — launching reader ...", blocks.len());

  if let Err(e) = block_reader::run(blocks, None, Some(id)) {
    eprintln!("reader error: {e}");
    std::process::exit(1);
  }
}
