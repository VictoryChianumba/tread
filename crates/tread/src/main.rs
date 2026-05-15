use std::path::Path;

fn main() {
  let arg = std::env::args().nth(1).unwrap_or_default();
  if arg.is_empty() {
    eprintln!(
      "usage: tread <arxiv-id-or-url> | tread <path-to-file>\n\n\
       Local files: .md / .markdown, .pdf, .epub, .html, .txt"
    );
    std::process::exit(1);
  }

  // Detect terminal capability up front — gates all inline-graphics work.
  let kitty_capability = kitty_graphics::detect();
  eprintln!("kitty graphics: {:?}", kitty_capability);
  // Active probe instead of an env-only check: shell out to `tmux show
  // -gv allow-passthrough` so we can distinguish "passthrough on" from
  // "passthrough off" from "couldn't determine".  The old advisory
  // warning here was correct but easy to miss — alt-screen takes over
  // within milliseconds and clears it.  Phrasing the OFF case as a
  // banner with separators and a copy-pasteable fix turns a silent
  // failure mode (every DCS envelope dropped, figures invisible) into
  // a loud one the user can actually act on.
  if matches!(kitty_capability, kitty_graphics::Capability::Supported) {
    match kitty_graphics::tmux_passthrough_enabled() {
      Some(true) => {
        eprintln!("tmux detected: allow-passthrough is on — figures should render.");
      }
      Some(false) => {
        eprintln!(
          "\n\
           ═══════════════════════════════════════════════════════════════════\n\
           tmux detected with allow-passthrough OFF.\n\
           Figures will NOT render — every DCS envelope is being dropped\n\
           by tmux before it reaches the host terminal.\n\
           \n\
           Fix:\n  \
             echo 'set -g allow-passthrough on' >> ~/.tmux.conf\n  \
             tmux source-file ~/.tmux.conf\n\
           \n\
           Verify:\n  \
             tmux show -gv allow-passthrough   # should print: on\n\
           ═══════════════════════════════════════════════════════════════════\n"
        );
      }
      None if kitty_graphics::in_tmux() => {
        eprintln!(
          "tmux detected but `allow-passthrough` could not be probed.\n  \
           If figures don't render, add to ~/.tmux.conf:\n    \
             set -g allow-passthrough on\n    \
             set -g focus-events on"
        );
      }
      None => {}
    }
  }
  // Zellij has no passthrough envelope.  When Ghostty/Kitty hosts
  // Zellij the figure pipeline still works because Zellij re-renders
  // via the host's image protocol.  Over iTerm2 that re-render is
  // broken (empty figure pane, no error).  Warn the user up-front so
  // they don't waste time debugging a tread bug that isn't one.
  if kitty_graphics::in_zellij()
    && kitty_graphics::is_iterm2()
    && matches!(kitty_capability, kitty_graphics::Capability::Supported)
  {
    eprintln!(
      "zellij over iTerm2 detected: the figure preview pane may stay\n  \
       empty because Zellij's Kitty-graphics re-render path is unreliable\n  \
       on iTerm2.  Workarounds:\n    \
       - run a Zellij fullscreen pane (Alt+f) for the tread session\n    \
       - run tread outside Zellij\n    \
       - switch host terminal to Ghostty"
    );
  }
  let kitty_supported = matches!(kitty_capability, kitty_graphics::Capability::Supported);

  // Routing precedence:
  // 1. existing file path on disk (arxiv ids look like 1706.03762 —
  //    a valid file path shape, so explicit existence wins);
  // 2. arxiv id or arxiv URL — fast-path with `fetch_paper`;
  // 3. any other http(s) URL — auto-format-detected via fetch_any.
  let path = Path::new(&arg);
  if path.exists() && path.is_file() {
    open_local_file(path, kitty_supported);
  } else if let Some(id) = tread::extract_arxiv_id(&arg) {
    open_arxiv(&id, kitty_supported);
  } else if arg.starts_with("http://") || arg.starts_with("https://") {
    open_url(&arg);
  } else {
    eprintln!(
      "error: {arg:?} is neither an existing file, a valid arXiv ID/URL, \
       nor an http(s) URL"
    );
    std::process::exit(1);
  }
}

fn open_url(url: &str) {
  eprintln!("fetching {url} ...");
  let data = match tread::fetch_any(url) {
    Ok(d) => d,
    Err(e) => {
      eprintln!("error: {e}");
      std::process::exit(1);
    }
  };
  // Use the URL itself as the persistence key — same identity
  // across re-opens of the same page so marks/highlights survive.
  eprintln!(
    "{} blocks — launching reader ...",
    data.blocks.len()
  );
  if let Err(e) = tread::run(data.blocks, None, Some(url.to_string()), data.bibitems) {
    eprintln!("reader error: {e}");
    std::process::exit(1);
  }
}

fn open_arxiv(id: &str, kitty_supported: bool) {
  eprintln!("fetching source for arXiv:{id} ...");
  let data = match tread::fetch_paper(id, kitty_supported) {
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
  if let Err(e) = tread::run(data.blocks, None, Some(id.to_string()), data.bibitems) {
    eprintln!("reader error: {e}");
    std::process::exit(1);
  }
}

fn open_local_file(path: &Path, _kitty_supported: bool) {
  eprintln!("opening local file: {} ...", path.display());
  let data = match tread::PaperData::from_path(path) {
    Ok(d) => d,
    Err(e) => {
      eprintln!("error: {e}");
      std::process::exit(1);
    }
  };
  // Use the canonicalized absolute path as the persistence key so
  // marks / highlights / cursor survive across invocations of the
  // same file.  Unique per file; consistent with how arxiv uses ids.
  let progress_key = std::fs::canonicalize(path)
    .ok()
    .and_then(|p| p.to_str().map(str::to_string));
  // Synthesize a header-bar title from the file stem.  Authors empty.
  let meta = path
    .file_stem()
    .and_then(|s| s.to_str())
    .map(|stem| tread::PaperMeta {
      title: stem.to_string(),
      authors: String::new(),
    });
  eprintln!(
    "{} blocks — launching reader ...",
    data.blocks.len()
  );
  if let Err(e) = tread::run(data.blocks, meta, progress_key, data.bibitems) {
    eprintln!("reader error: {e}");
    std::process::exit(1);
  }
}
