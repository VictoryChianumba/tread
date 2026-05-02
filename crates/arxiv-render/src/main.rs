use arxiv_render::{fetch, parse};
use doc_model::build_visual_lines;

fn main() {
  let args: Vec<String> = std::env::args().collect();

  if args.get(1).map(|s| s.as_str()) == Some("--math") {
    let expr = args.get(2).map(|s| s.as_str()).unwrap_or("");
    if expr.is_empty() {
      eprintln!("usage: arxiv-render --math \"\\\\frac{{a}}{{b}}\"");
      std::process::exit(1);
    }
    use math_render::{MathInput, render};
    let inline = render(MathInput::Latex(expr));
    let flat = inline.split_whitespace().collect::<Vec<_>>().join(" ");
    println!("inline:  {flat}");
    println!("display:");
    for line in render(MathInput::Latex(expr)).lines() {
      println!("  {line}");
    }
    std::process::exit(0);
  }

  let arg = args.into_iter().nth(1).unwrap_or_default();
  if arg.is_empty() {
    eprintln!("usage: arxiv-render <arxiv-id-or-url>");
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
  let visual_lines = build_visual_lines(&blocks, 80);

  for vl in &visual_lines {
    println!("{}", vl.text);
  }

  eprintln!("--- {} blocks → {} visual lines ---", blocks.len(), visual_lines.len());
}
