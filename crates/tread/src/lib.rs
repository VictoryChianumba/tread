pub mod bench;
mod commands;
mod config;
mod epub;
mod highlights;
mod html;
mod images;
mod markdown;
mod pdf;
mod persist;
mod progress;
mod render;
mod state;
mod text_objects;
mod voice;

use crossterm::{
  event::{
    self, DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture, Event,
    KeyCode, KeyModifiers, KeyboardEnhancementFlags, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
  },
  execute,
  terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use doc_model::Block;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use state::{FindKind, Mode, Operator};
use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::Duration;

// Public embed surface — host TUIs (trench) and the standalone tread
// binary both build against these.  See `crates/tread/CLAUDE.md` for
// the embed guide.  Stable from v1; signature changes are a contract
// break.
pub use commands::ReaderAction;
pub use images::{BurstTracker, ImageState};
pub use state::{PaperMeta, Reader};
pub use voice::PlaybackController;
// Re-export the ui_theme crate so embedding hosts can name `tread::Theme`
// (and its `Color` enum) without taking a separate path-dep on the same
// crate.  When a host's own theme module diverged from tread's, an
// adapter on the host side maps host::Theme → tread::Theme — see
// trench/src/app.rs::theme_for_tread for the trench-side converter.
pub use ui_theme;
pub use ui_theme::Theme;

/// What `fetch_paper` returns: everything the reader needs to build/refresh
/// its block tree and bib lookups for one arXiv paper.  The `asset_dir`
/// is kept on disk for image lookups — the reader doesn't need to track it
/// explicitly, but holding the value here lets callers know it exists.
pub struct PaperData {
  pub blocks: Vec<Block>,
  pub bibitems: HashMap<String, String>,
  pub asset_dir: std::path::PathBuf,
}

impl PaperData {
  /// Build a `PaperData` from pre-extracted plain-text lines.  Used by
  /// host TUIs (trench) to embed tread for non-arXiv content sources —
  /// PDF / EPUB / HTML / local files where the host has already done
  /// the extraction and only needs a typed reader on the resulting
  /// `Vec<String>`.  Each input line becomes one `Block::Line`; no
  /// math, no tables, no images, no cross-refs.  `:reload`, `:cite`,
  /// `:url`, `:open` are no-ops with a sensible error (they all
  /// require an arxiv_id, which `Reader::init` leaves unset for this
  /// path — pass `progress_key = None`).
  ///
  /// All non-prose Reader features still work: navigation, marks,
  /// highlights, search, themes, voice, text objects.
  pub fn from_plain_lines(lines: Vec<String>) -> Self {
    Self {
      blocks: lines.into_iter().map(Block::Line).collect(),
      bibitems: HashMap::new(),
      asset_dir: std::path::PathBuf::new(),
    }
  }

  /// Build a `PaperData` from a markdown source string.  Headers,
  /// lists, code blocks, blockquotes, links, bold/italic/inline-code
  /// styling, and rules all map to their `doc-model::Block` /
  /// `InlineSpan` equivalents.  Unlocks GitHub READMEs, generic
  /// markdown READMEs, blog posts that ship as `.md`, and any host
  /// content already in markdown form.
  ///
  /// Out of scope (deliberately):
  /// - Math: markdown has no standard syntax; mixing TeX with
  ///   shell-prompt-style code samples in fences is fragile.  v2
  ///   if needed.
  /// - Image fetching: tread doesn't speak HTTP for image bodies, so
  ///   `![alt](url)` degrades to an italic `[Image: alt]` placeholder.
  ///   Hosts that want pixel images should pre-fetch and inject via
  ///   the arxiv-style image_paths flow (v2).
  /// - Tables: pulldown-cmark emits them but tread's current Matrix
  ///   block expects LaTeX-style cells; v2 backlog.
  pub fn from_markdown(md: &str) -> Self {
    Self {
      blocks: markdown::parse(md),
      bibitems: HashMap::new(),
      asset_dir: std::path::PathBuf::new(),
    }
  }

  /// Build a `PaperData` from in-memory PDF bytes.  Wraps the
  /// `pdf_extract` crate (same dep `cli-pdf-to-text` used in trench's
  /// old pipeline) so a host can pass bytes from its HTTP layer
  /// without writing to disk.  Each output line maps to a
  /// `Block::Line` — heuristic structure detection (headers, lists,
  /// columns) is deferred to v2 to avoid mis-parsing reading order.
  ///
  /// Errors when the buffer is empty, corrupt, or password-protected.
  /// Hosts can fall back to `from_plain_lines` with a notification.
  pub fn from_pdf_bytes(bytes: &[u8]) -> Result<Self, String> {
    let blocks = pdf::pdf_to_blocks(bytes)?;
    Ok(Self {
      blocks,
      bibitems: HashMap::new(),
      asset_dir: std::path::PathBuf::new(),
    })
  }

  /// Build a `PaperData` from in-memory EPUB bytes.  Wraps the
  /// `epub` and `html2text` crates (same deps `cli-epub-to-text`
  /// used) so a host can pass a buffer fetched from any source.
  /// Each spine item becomes a chapter with a `Block::Header` (using
  /// the EPUB's NavPoint label when available, else the idref),
  /// followed by line-by-line content.  Paragraph breaks are
  /// preserved as `Block::Blank`.
  ///
  /// Out of scope (v2 backlog):
  /// - Inline images: EPUB images live as separate manifest
  ///   resources, would need integration with the image_paths flow.
  /// - Encryption: surfaces as parse error; tread doesn't support
  ///   DRM key handling.
  /// - Adaptive wrap: html2text wraps at a fixed 110 cols today;
  ///   v2 routes raw HTML through `from_html` once that lands.
  pub fn from_epub_bytes(bytes: &[u8]) -> Result<Self, String> {
    let blocks = epub::epub_to_blocks(bytes)?;
    Ok(Self {
      blocks,
      bibitems: HashMap::new(),
      asset_dir: std::path::PathBuf::new(),
    })
  }

  /// Build a `PaperData` from an HTML document.  Walks the parsed
  /// DOM (via `html5ever` through the `scraper` crate) and emits
  /// the same Block tree the other paths produce — headers, lists,
  /// code blocks, blockquotes, images-as-placeholders, links carry
  /// their `href` on the span, inline bold/italic/monospace/strike.
  ///
  /// Hosts that pre-clean noisy pages with `readability` should
  /// pass the cleaned HTML here.  Tread doesn't error on malformed
  /// HTML — html5ever applies the standard browser repair, so at
  /// worst this returns an empty Vec.
  ///
  /// Out of scope (v2 backlog):
  /// - Tables: render flattened (one row per paragraph) rather
  ///   than as Block::Matrix; cell-shape conversion is its own task.
  /// - Inline images: degrade to `[Image: alt]` placeholders.
  ///   Hosts that want pixel rendering need to download bytes and
  ///   integrate with the image_paths flow.
  pub fn from_html(html: &str) -> Self {
    Self {
      blocks: html::html_to_blocks(html),
      bibitems: HashMap::new(),
      asset_dir: std::path::PathBuf::new(),
    }
  }

  /// Build a `PaperData` from a local file path.  Reads the file
  /// once, sniffs the extension (case-insensitive), and dispatches
  /// to the matching `from_*` builder.  Unknown / missing extensions
  /// fall back to `from_plain_lines` so any text file still opens.
  ///
  /// Extension dispatch:
  /// - `.md`, `.markdown`           → from_markdown
  /// - `.pdf`                       → from_pdf_bytes
  /// - `.epub`                      → from_epub_bytes
  /// - `.html`, `.htm`              → from_html
  /// - `.txt` / unknown / no ext    → from_plain_lines
  ///
  /// Errors propagate from the underlying builder (parse failures,
  /// empty buffers, …) plus an I/O error if the file can't be read.
  /// Hosts that want richer error context wrap this with their own.
  pub fn from_path(path: impl AsRef<std::path::Path>) -> Result<Self, String> {
    let path = path.as_ref();
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let ext = path
      .extension()
      .and_then(|e| e.to_str())
      .map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
      Some("pdf") => Self::from_pdf_bytes(&bytes),
      Some("epub") => Self::from_epub_bytes(&bytes),
      Some("md") | Some("markdown") => {
        let text = String::from_utf8(bytes)
          .map_err(|e| format!("{}: invalid UTF-8 ({e})", path.display()))?;
        Ok(Self::from_markdown(&text))
      }
      Some("html") | Some("htm") => {
        let text = String::from_utf8(bytes)
          .map_err(|e| format!("{}: invalid UTF-8 ({e})", path.display()))?;
        Ok(Self::from_html(&text))
      }
      _ => {
        // .txt / unknown / no ext — read as text and emit one
        // Block::Line per line.  Best-effort fallback so any text
        // file still opens.
        let text = String::from_utf8(bytes)
          .map_err(|e| format!("{}: invalid UTF-8 ({e})", path.display()))?;
        let lines: Vec<String> = text.split('\n').map(|l| l.trim_end().to_string()).collect();
        Ok(Self::from_plain_lines(lines))
      }
    }
  }
}

/// Fetch + parse + post-process a paper.  Mirrors the bootstrap steps
/// in `main.rs` so both initial load and `:reload` go through the same
/// pipeline: fetch the e-print tarball, parse to blocks, resolve image
/// paths (or degrade to captions on non-graphics terminals), lift table
/// floats to PDF-rendered position.  Network-bound; runs synchronously
/// so the caller will block for ~2s on a typical paper.
/// True when the host terminal speaks the Kitty graphics protocol —
/// i.e. tread can render figures as actual pixel images via APC
/// escapes after each draw.  Wraps `kitty_graphics::detect()` so
/// embedding hosts (trench) don't need a separate path-dep on
/// `kitty-graphics`.  Standalone tread calls this in main.rs;
/// embedding hosts call it once at startup and pass the bool into
/// `Reader::init` plus the per-frame `tread::after_draw`.
pub fn detect_kitty_supported() -> bool {
  matches!(
    kitty_graphics::detect(),
    kitty_graphics::Capability::Supported
  )
}

/// Re-export of `kitty_graphics::in_zellij()` so embedding hosts
/// (trench) can surface the Zellij-over-iTerm2 graphics warning
/// without taking a direct dep on the kitty-graphics crate.
pub fn in_zellij() -> bool {
  kitty_graphics::in_zellij()
}

/// Re-export of `kitty_graphics::is_iterm2()`.  Used together with
/// `in_zellij()` to scope the figure-preview warning to the
/// host/multiplexer combo where Kitty graphics rendering is known
/// to fail silently.
pub fn is_iterm2() -> bool {
  kitty_graphics::is_iterm2()
}

/// Re-export of `kitty_graphics::tmux_passthrough_enabled()`.  Hosts
/// (trench) call this at startup to detect the silent-failure mode
/// where tmux is dropping every DCS passthrough envelope, and surface
/// a loud warning before alt-screen entry.  See the kitty-graphics
/// crate for the full semantics including the `None`-on-failure cases.
pub fn tmux_passthrough_enabled() -> Option<bool> {
  kitty_graphics::tmux_passthrough_enabled()
}

/// Extract an arXiv id from an URL or bare-id string.  Returns `None`
/// if the input doesn't look like an arXiv reference.  Hosts use this
/// to decide whether a paper should route through `fetch_paper`
/// (LaTeX source, full math/tables/figures) or fall back to plain-
/// text ingestion.  Wraps `arxiv_render::fetch::extract_id`.
pub fn extract_arxiv_id(url_or_id: &str) -> Option<String> {
  arxiv_render::fetch::extract_id(url_or_id)
}

pub fn fetch_paper(id: &str, kitty_supported: bool) -> Result<PaperData, String> {
  fetch_paper_inner(id, kitty_supported, false)
}

/// Same as `fetch_paper` but forces both the source tarball and the
/// rendered PDF to bypass the etag conditional cache.  Used by the
/// in-reader `:refresh` command for the case where a paper has been
/// revised on arXiv and the user wants a guaranteed-fresh copy.
pub fn fetch_paper_refresh(id: &str, kitty_supported: bool) -> Result<PaperData, String> {
  fetch_paper_inner(id, kitty_supported, true)
}

fn fetch_paper_inner(
  id: &str,
  kitty_supported: bool,
  force_refresh: bool,
) -> Result<PaperData, String> {
  use arxiv_render::{fetch, parse, pdf_anchors, placement};

  // Two pairs of independent work overlap in this scope:
  //
  //   1. Source tarball and PDF are independent HTTPS GETs against
  //      arxiv.org; total network phase = max(both) instead of sum.
  //   2. parse_to_blocks (Pandoc subprocess on the source bytes) and
  //      extract_anchors (pdftotext subprocess on the PDF bytes) are
  //      also independent — they consume different inputs and meet
  //      only at lift_tables.  Running them in parallel saves
  //      ~min(parse, anchors) ≈ 500–600ms off the warm-cache wall.
  //
  // The anchor thread starts as soon as fetch_pdf completes (it
  // joins pdf_handle from inside) and runs alongside whatever the
  // main thread is doing — fetch_source, parse, absolutize.  arXiv
  // tolerates the two-concurrent-request pattern; trench's openreview
  // audit hit the same shape.
  std::thread::scope(|s| {
    let pdf_handle = s.spawn(|| {
      bench::time("fetch_pdf", || {
        if force_refresh {
          fetch::fetch_pdf_refresh(id)
        } else {
          fetch::fetch_pdf(id)
        }
      })
    });
    // Anchor extraction joins fetch_pdf itself, then shells to
    // pdftotext.  Spawned alongside parse_to_blocks so its ~600ms
    // overlaps the Pandoc run instead of stacking after it.  Both
    // threads' subprocesses (pdftotext + pandoc) run truly in parallel
    // on a multi-core box.  Best-effort: a panic or HTTP failure
    // yields an empty anchor list and placement falls back to source
    // order.
    let anchor_handle = s.spawn(move || {
      let pdf_result = pdf_handle
        .join()
        .unwrap_or_else(|_| Err("pdf fetch thread panicked".to_string()));
      match pdf_result {
        Ok(pdf) => bench::time("extract_anchors", || pdf_anchors::extract_anchors(&pdf)),
        Err(_) => Vec::new(),
      }
    });

    let fetched = bench::time("fetch_source", || {
      if force_refresh {
        fetch::fetch_source_refresh(id)
      } else {
        fetch::fetch_source(id)
      }
    })?;
    let sources = fetched.tex;
    let asset_dir = fetched.asset_dir;
    let bibitems = bench::time("extract_bibitems", || {
      arxiv_render::extract_bibitems(&sources)
    });
    let mut blocks = bench::time("parse_to_blocks", || parse::to_blocks(sources));
    if kitty_supported {
      bench::time("absolutize_image_paths", || {
        arxiv_render::absolutize_image_paths(&mut blocks, &asset_dir)
      });
    } else {
      arxiv_render::degrade_images_to_captions(&mut blocks);
    }
    let anchors = anchor_handle
      .join()
      .unwrap_or_else(|_| Vec::new());
    let blocks = bench::time("lift_tables", || placement::lift_tables(blocks, &anchors));
    bench::emit_fields(
      "fetch_paper_done",
      &[
        ("blocks", blocks.len() as i64),
        ("bibitems", bibitems.len() as i64),
        ("anchors", anchors.len() as i64),
      ],
    );
    Ok(PaperData {
      blocks,
      bibitems,
      asset_dir,
    })
  })
}

pub fn run(
  blocks: Vec<Block>,
  meta: Option<PaperMeta>,
  progress_key: Option<String>,
  bibitems: HashMap<String, String>,
) -> Result<(), Box<dyn std::error::Error>> {
  // Resolve theme via the new config layer: respect override, else follow
  // trench's theme, else fall back to the built-in dark default.
  let theme = config::resolve_theme();
  let kitty_supported = matches!(
    kitty_graphics::detect(),
    kitty_graphics::Capability::Supported
  );
  run_with_theme(blocks, meta, progress_key, bibitems, theme, kitty_supported)
}

pub fn run_with_theme(
  blocks: Vec<Block>,
  meta: Option<PaperMeta>,
  progress_key: Option<String>,
  bibitems: HashMap<String, String>,
  theme: Theme,
  kitty_supported: bool,
) -> Result<(), Box<dyn std::error::Error>> {
  // One-shot figure-cache eviction.  Cheap (a read_dir + sort + a few
  // unlinks); harmless if the cache is under cap or doesn't exist.
  // Done before the runtime starts so any rasterisation during the
  // session writes into an already-trimmed dir.
  images::enforce_cache_cap();

  // Standalone tread: tread itself owns terminal setup and teardown.
  // When embedded in a host TUI (trench), the host owns terminal state
  // and constructs the Reader directly via `Reader::init`.
  enable_raw_mode()?;
  let mut stdout = io::stdout();
  // Focus events let us detect tmux pane switches: when our pane
  // loses focus we delete all image placements (otherwise iTerm2
  // keeps them painted at absolute screen coordinates that now
  // belong to a different tmux pane).  Requires `set -g focus-events
  // on` in the user's tmux config; if absent, FocusLost never fires
  // and behaviour is what we had before — image bleed across panes.
  execute!(
    stdout,
    EnterAlternateScreen,
    EnableMouseCapture,
    EnableFocusChange
  )?;
  // Opt into the kitty keyboard protocol so Shift+Enter (and other
  // modified specials) are distinguishable from plain Enter.  Terminals
  // that don't speak the protocol silently ignore the push; on those,
  // `K` is the universal fallback for the citation-popup binding.
  let _ = execute!(
    stdout,
    PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
  );
  let backend = CrosstermBackend::new(stdout);
  let mut terminal = Terminal::new(backend)?;

  // Build the voice playback controller from saved config + the
  // env-only ELEVENLABS_API_KEY.  Wrapped in `Arc` so the embed surface
  // signature is uniform between standalone and host-embedded use
  // (trench clones the same Arc into each tab — see
  // `tread::build_voice_controller`).
  let voice_controller = Some(build_voice_controller());

  let size = terminal.size()?;
  let paper = PaperData {
    blocks,
    bibitems,
    asset_dir: std::path::PathBuf::new(),
  };
  let reader = Reader::init(
    paper,
    meta,
    progress_key.clone(),
    size.width,
    size.height,
    kitty_supported,
    voice_controller,
  );

  let runtime = ReaderRuntime::new(reader, theme, kitty_supported);
  let (reader, result) = runtime.run(&mut terminal);

  // Persist reading progress, bookmarks, and highlights on clean exit.
  bench::time("shutdown_save", || reader.save_progress());
  bench::dump_summary();

  // Balance the keyboard-enhancement push.  Ignored on terminals
  // that didn't accept it.
  let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
  disable_raw_mode()?;
  execute!(
    terminal.backend_mut(),
    LeaveAlternateScreen,
    DisableMouseCapture,
    DisableFocusChange
  )?;
  terminal.show_cursor()?;

  result
}

/// Post-draw hook: emit Kitty graphics escapes for visible image VLs.
/// Hosts call this after `terminal.draw(|f| tread::render::draw(...))`
/// returns.  Walks the visible window each frame; lazy emission inside
/// `place_visible` skips unchanged placements so the hot path is cheap
/// on idle frames.  No-op when `kitty_supported == false` — keeps
/// non-graphics terminals from paying any cost here.
pub fn after_draw(reader: &Reader, state: &mut ImageState, area: Rect, kitty_supported: bool) {
  if !kitty_supported {
    return;
  }
  let (_, _, content_area, _, _) = render::split_layout(area, reader);
  let (reader_area, preview_area) = render::split_content_for_preview(content_area, reader);
  images::place_visible(reader, state, reader_area, kitty_supported);
  emit_preview(
    reader,
    state,
    preview_area.map(render::preview_image_area),
    area,
    kitty_supported,
  );
}

/// Emit the figure-preview placement.  Separated from `after_draw`'s
/// inline path so the burst gate can suppress one without the other —
/// the preview only changes on toggle / step, never on scroll, so it
/// has no reason to coalesce during scroll bursts.
fn emit_preview(
  reader: &Reader,
  state: &mut ImageState,
  preview_area: Option<Rect>,
  fallback_rect: Rect,
  kitty_supported: bool,
) {
  let kid = preview_area.and_then(|_| reader.current_figure_kitty_id());
  reader.set_preview_geometry(preview_area);
  let rect = preview_area.unwrap_or(fallback_rect);
  images::place_one_figure(reader, state, kid, rect, kitty_supported);
}

/// True when the current runtime should suppress image re-emission
/// during queued-input bursts.  This only applies to terminals that
/// lack a persistent Kitty image cache: on those hosts every placement
/// change forces a full `a=T` retransmit, which can overwhelm the
/// terminal during rapid scroll. Native Kitty keeps image bytes cached
/// and uses the cheap `a=p` path instead, so burst-skip would only add
/// flicker for no gain there.
pub fn burst_skip_enabled(kitty_supported: bool) -> bool {
  let user_disabled = std::env::var("TREAD_IMAGE_BURST_HIDE")
    .map(|v| v == "0")
    .unwrap_or(false);
  kitty_supported && !kitty_graphics::has_persistent_image_cache() && !user_disabled
}

/// Like [`after_draw`] but coalesces image emission during input bursts.
///
/// Hosts compute `burst_in_progress` from a [`BurstTracker`] kept
/// alongside the per-pane [`ImageState`]; on a non-cache host with the
/// gate enabled, this helper returns without emitting anything for the
/// frame.  The existing placement stays on the terminal (we don't
/// delete) so the prior image lingers in its old spot rather than
/// blinking out — far less jarring than the previous behaviour during
/// scroll on iTerm2.  When the burst ends, the next non-skipped frame
/// emits a normal delete+place and the image snaps to its new row.
pub fn after_draw_guarded(
  reader: &Reader,
  state: &mut ImageState,
  area: Rect,
  kitty_supported: bool,
  burst_in_progress: bool,
) {
  if !kitty_supported {
    return;
  }
  let (_, _, content_area, _, _) = render::split_layout(area, reader);
  let (reader_area, preview_area) = render::split_content_for_preview(content_area, reader);
  // Burst gate only suppresses the inline path: that's where a full
  // `a=T` retransmit fires every scroll line on iTerm2 and overwhelms
  // the terminal.  The preview figure only changes on `i` / `]f` /
  // `[f` — never on scroll — and its lazy fast path makes steady-
  // state emission ~zero-cost.  Suppressing it during scroll bursts
  // means the user can press `i` while scrolling and see no figure
  // until the burst window expires, which is the symptom we're
  // unblocking here.
  if !(burst_skip_enabled(kitty_supported) && burst_in_progress) {
    images::place_visible(reader, state, reader_area, kitty_supported);
  }
  emit_preview(
    reader,
    state,
    preview_area.map(render::preview_image_area),
    area,
    kitty_supported,
  );
}

/// Render one figure into a dedicated preview pane (or clear it).
///
/// Hosts pair this with `Reader::set_text_only(true)` to drive a
/// "figure preview" side pane: the reader pane stays text-only,
/// while `kitty_id = Some(id)` shows that figure scaled to fit
/// `area` and centered.  Pass `kitty_id = None` to clear any
/// active preview.  Stepping between figures (different `Some(id)`
/// across calls) deletes the old placement and emits the new one.
///
/// See `images::place_one_figure` for the full state machine.
pub fn place_one_figure(
  reader: &Reader,
  state: &mut ImageState,
  kitty_id: Option<u32>,
  area: Rect,
  kitty_supported: bool,
) {
  images::place_one_figure(reader, state, kitty_id, area, kitty_supported);
}

/// Public draw entry point — wraps `render::draw` so hosts don't need
/// to import `render`.  Hosts pass the sub-rectangle they want the
/// reader to occupy (e.g. one half of a split pane); standalone tread
/// passes `frame.area()` for the whole terminal.
pub fn draw(frame: &mut ratatui::Frame, area: Rect, reader: &Reader, theme: &Theme) {
  render::draw(frame, area, reader, theme);
}

/// Clear the host-owned image cache.  Call after `:reload` returns
/// `ReaderAction::Reload`, on terminal resize, and on `Event::FocusLost`
/// (tmux pane switch).  Idempotent and cheap.
pub fn clear_images(state: &mut ImageState) {
  images::clear_all(state);
}

/// Fetch a URL and parse into `PaperData`, auto-detecting the format.
/// One-call entry point for embedding hosts that don't want to manage
/// the URL → format → builder chain themselves; standalone tread
/// uses it for `tread <https://...>` invocations.
///
/// Detection order:
/// 1. arXiv URL pattern via `extract_arxiv_id` → `fetch_paper`
///    (gets the full LaTeX path with math / tables / figures).
/// 2. URL path extension (`.pdf`, `.epub`, `.md`, `.markdown`,
///    `.html`, `.htm`, `.txt`) — fast and reliable when present.
/// 3. HTTP `Content-Type` header — fallback for clean URLs that
///    don't expose a file extension.
/// 4. Default: treat the body as HTML.  Browsers do the same when
///    Content-Type is missing or generic.
///
/// Network-bound; runs synchronously like `fetch_paper` does.
/// Hosts that need non-blocking ingestion should background this
/// on a worker thread (see trench's existing fulltext pipeline).
pub fn fetch_any(url: &str) -> Result<PaperData, String> {
  // 1. arXiv fast path.  fetch_paper handles its own HTTP and
  // returns a richer PaperData (bibitems, asset_dir for images).
  if let Some(id) = extract_arxiv_id(url) {
    let kitty_supported = matches!(
      kitty_graphics::detect(),
      kitty_graphics::Capability::Supported
    );
    return fetch_paper(&id, kitty_supported);
  }

  // 2. URL extension sniff before the HTTP roundtrip.
  let path_part = url.split('?').next().unwrap_or(url);
  let ext = std::path::Path::new(path_part)
    .extension()
    .and_then(|e| e.to_str())
    .map(|e| e.to_ascii_lowercase());

  // 3. Fetch the body.  Defenses against pathological servers (C5):
  // - 30s timeout matches arxiv-render's fetch — long enough for slow
  //   academic mirrors, short enough to avoid hanging the TUI when a
  //   server stalls mid-transfer.
  // - Redirect cap matches reqwest's default (10) but stated
  //   explicitly so future reqwest version bumps can't surprise us.
  // - Body cap: take (MAX + 1) bytes via the Read implementation, then
  //   reject if we got more than MAX — distinguishes "body exactly
  //   equals MAX" (allowed) from "server streaming more than we'll
  //   accept" (rejected before OOM).  Doesn't help against a server
  //   that streams forever AT a reasonable rate within the timeout;
  //   for that we rely on the timeout to fire.
  const FETCH_ANY_TIMEOUT_SECS: u64 = 30;
  const FETCH_ANY_MAX_REDIRECTS: usize = 10;
  const FETCH_ANY_MAX_BYTES: usize = 64 * 1024 * 1024; // 64 MB
  let client = reqwest::blocking::Client::builder()
    .timeout(std::time::Duration::from_secs(FETCH_ANY_TIMEOUT_SECS))
    .redirect(reqwest::redirect::Policy::limited(FETCH_ANY_MAX_REDIRECTS))
    .build()
    .map_err(|e| format!("build HTTP client for {url}: {e}"))?;
  let resp = client
    .get(url)
    .send()
    .map_err(|e| format!("fetch {url}: {e}"))?;
  if !resp.status().is_success() {
    return Err(format!("fetch {url}: HTTP {}", resp.status()));
  }
  let content_type = resp
    .headers()
    .get(reqwest::header::CONTENT_TYPE)
    .and_then(|v| v.to_str().ok())
    .unwrap_or("")
    .to_ascii_lowercase();

  use std::io::Read;
  let mut bytes = Vec::new();
  resp
    .take((FETCH_ANY_MAX_BYTES + 1) as u64)
    .read_to_end(&mut bytes)
    .map_err(|e| format!("read body of {url}: {e}"))?;
  if bytes.len() > FETCH_ANY_MAX_BYTES {
    return Err(format!(
      "{url}: response exceeds {FETCH_ANY_MAX_BYTES} bytes; refusing to load"
    ));
  }

  // 4. Pick a parser based on extension first, then Content-Type,
  // then fall through to HTML.
  let format = ext
    .as_deref()
    .and_then(format_from_extension)
    .or_else(|| format_from_content_type(&content_type))
    .unwrap_or(Format::Html);

  match format {
    Format::Pdf => PaperData::from_pdf_bytes(&bytes),
    Format::Epub => PaperData::from_epub_bytes(&bytes),
    Format::Markdown => {
      let text = String::from_utf8(bytes).map_err(|e| format!("{url}: invalid UTF-8 ({e})"))?;
      Ok(PaperData::from_markdown(&text))
    }
    Format::Html => {
      let text = String::from_utf8(bytes).map_err(|e| format!("{url}: invalid UTF-8 ({e})"))?;
      Ok(PaperData::from_html(&text))
    }
    Format::PlainText => {
      let text = String::from_utf8(bytes).map_err(|e| format!("{url}: invalid UTF-8 ({e})"))?;
      let lines: Vec<String> = text.split('\n').map(|l| l.trim_end().to_string()).collect();
      Ok(PaperData::from_plain_lines(lines))
    }
  }
}

#[derive(Clone, Copy)]
enum Format {
  Pdf,
  Epub,
  Markdown,
  Html,
  PlainText,
}

fn format_from_extension(ext: &str) -> Option<Format> {
  Some(match ext {
    "pdf" => Format::Pdf,
    "epub" => Format::Epub,
    "md" | "markdown" => Format::Markdown,
    "html" | "htm" => Format::Html,
    "txt" => Format::PlainText,
    _ => return None,
  })
}

fn format_from_content_type(ct: &str) -> Option<Format> {
  // Strip any `; charset=…` suffix browsers append.
  let base = ct.split(';').next().unwrap_or("").trim();
  Some(match base {
    "application/pdf" => Format::Pdf,
    "application/epub+zip" => Format::Epub,
    "text/markdown" | "text/x-markdown" => Format::Markdown,
    "text/html" | "application/xhtml+xml" => Format::Html,
    "text/plain" => Format::PlainText,
    _ => return None,
  })
}

#[cfg(test)]
mod fetch_any_tests {
  use super::*;

  fn fmt_disc(f: Format) -> &'static str {
    match f {
      Format::Pdf => "pdf",
      Format::Epub => "epub",
      Format::Markdown => "markdown",
      Format::Html => "html",
      Format::PlainText => "txt",
    }
  }

  #[test]
  fn extension_dispatch_covers_known_formats() {
    let cases = [
      ("pdf", "pdf"),
      ("epub", "epub"),
      ("md", "markdown"),
      ("markdown", "markdown"),
      ("html", "html"),
      ("htm", "html"),
      ("txt", "txt"),
    ];
    for (ext, want) in cases {
      let got = format_from_extension(ext).unwrap_or_else(|| panic!("ext {ext} returned None"));
      assert_eq!(fmt_disc(got), want, "extension {ext}");
    }
  }

  #[test]
  fn extension_unknown_returns_none() {
    assert!(format_from_extension("xyz").is_none());
    assert!(format_from_extension("").is_none());
  }

  #[test]
  fn content_type_dispatch_covers_known_mimes() {
    let cases = [
      ("application/pdf", "pdf"),
      ("application/epub+zip", "epub"),
      ("text/markdown", "markdown"),
      ("text/x-markdown", "markdown"),
      ("text/html", "html"),
      ("application/xhtml+xml", "html"),
      ("text/plain", "txt"),
    ];
    for (ct, want) in cases {
      let got = format_from_content_type(ct).unwrap_or_else(|| panic!("ct {ct} returned None"));
      assert_eq!(fmt_disc(got), want, "ct {ct}");
    }
  }

  #[test]
  fn content_type_charset_suffix_ignored() {
    let f = format_from_content_type("text/html; charset=utf-8")
      .expect("charset-suffixed text/html should resolve");
    assert_eq!(fmt_disc(f), "html");
  }

  #[test]
  fn content_type_unknown_returns_none() {
    assert!(format_from_content_type("application/octet-stream").is_none());
    assert!(format_from_content_type("").is_none());
  }
}

/// Build a shared TTS playback controller from the saved voice config
/// plus the env-only `ELEVENLABS_API_KEY`.  Hosts call this once at
/// app startup and `clone()` the returned `Arc` into every Reader tab
/// — the underlying audio thread + `rodio::Sink` are shared, so only
/// one paper speaks at a time across tabs (cross-tab preemption is
/// handled by the session-id machinery in `voice/playback.rs`).
///
/// Construction never fails: if the audio device isn't available, the
/// playback thread surfaces the error via `take_error()` instead of
/// panicking.  Standalone tread embeds the same call in
/// `run_with_theme`.
pub fn build_voice_controller() -> Arc<PlaybackController> {
  let cfg = config::load();
  let voice_cfg = cfg.voice.clone().unwrap_or_default();
  let api_key = std::env::var("ELEVENLABS_API_KEY").ok();
  let provider = voice::make_provider(&voice_cfg, api_key.as_deref());
  Arc::new(PlaybackController::new(provider))
}

impl Reader {
  /// Embed-surface event handler.  Hosts call this for every crossterm
  /// `Event` they want the reader to process — keystrokes, mouse,
  /// resize, focus events.  Returns a `ReaderAction` the host reacts
  /// to: `Quit` to close the reader, `ChangeTheme(t)` to update the
  /// active theme, `Reload` to clear image caches, `Error(msg)` to
  /// optionally surface in a host-side status bar (the reader's own
  /// status bar already shows `cmd_error`).  `OpenHelp` is a side-
  /// effect-only action — `help_visible` has already been set on the
  /// Reader; the host can ignore it.
  ///
  /// Resize: this method calls `Reader::resize`, but the host should
  /// also call `tread::clear_images(&mut state)` because reflow can
  /// move every image VL.  FocusLost: same — the host should clear
  /// the image cache.  Both can be done before or after this call.
  pub fn handle_event(&mut self, ev: Event) -> ReaderAction {
    match ev {
      Event::Key(key) => {
        // Any keystroke clears the previous command's error and dismisses
        // any open popup so it doesn't linger across input.
        self.cmd_error = None;
        if self.popup.is_some() {
          self.popup = None;
          return ReaderAction::Continue;
        }
        // Clone the active mode out of the borrow so we can call
        // `handle_*(self, ...)` (which needs `&mut self`).  Mode is
        // small (one enum tag plus a few primitives in the largest
        // variant) so the clone is negligible.
        let mode = self.mode().clone();
        match mode {
          Mode::Normal => {
            if handle_normal(self, key.code, key.modifiers) {
              ReaderAction::Quit
            } else {
              ReaderAction::Continue
            }
          }
          Mode::Search => {
            handle_search(self, key.code);
            ReaderAction::Continue
          }
          Mode::Visual { .. } => {
            handle_visual(self, key.code);
            ReaderAction::Continue
          }
          Mode::AwaitingChar { kind } => {
            handle_awaiting_char(self, key.code, kind);
            ReaderAction::Continue
          }
          Mode::AwaitingMarkName { for_set } => {
            handle_awaiting_mark_name(self, key.code, for_set);
            ReaderAction::Continue
          }
          Mode::AwaitingG => {
            handle_awaiting_g(self, key.code);
            ReaderAction::Continue
          }
          Mode::AwaitingBracket { forward } => {
            handle_awaiting_bracket(self, key.code, forward);
            ReaderAction::Continue
          }
          Mode::AwaitingOperator { op } => {
            handle_awaiting_operator(self, key.code, op);
            ReaderAction::Continue
          }
          Mode::AwaitingTextObject { op, around } => {
            handle_awaiting_text_object(self, key.code, op, around);
            ReaderAction::Continue
          }
          Mode::Command => handle_command(self, key.code),
        }
      }
      Event::Mouse(mouse) => {
        match mouse.kind {
          MouseEventKind::ScrollDown => {
            for _ in 0..3 {
              self.nav_down();
            }
          }
          MouseEventKind::ScrollUp => {
            for _ in 0..3 {
              self.nav_up();
            }
          }
          _ => {}
        }
        ReaderAction::Continue
      }
      Event::Resize(w, h) => {
        // Reflow visual_lines for the new size.  The host is responsible
        // for clearing its image cache — see method docs.
        self.resize(w, h);
        ReaderAction::Continue
      }
      // FocusLost / FocusGained / Paste: no Reader-side state change
      // here.  Hosts should clear the image cache on FocusLost so
      // placements don't bleed across tmux panes; the next draw will
      // re-emit visible images via `after_draw` anyway.
      _ => ReaderAction::Continue,
    }
  }

  /// Per-frame tick: sync voice playback state and run continuous-
  /// reading auto-advance.  Hosts call this once per frame (typically
  /// on `event::poll` timeout in their main loop) so the loading
  /// spinner animates and the active-word highlight repaints during
  /// playback.  Cheap when `voice_controller` is None.
  ///
  /// Returns `true` if the tick mutated user-visible state (voice
  /// status / active word / paragraph advance) so the host can mark
  /// its own dirty flag and redraw on the next frame.  Returns
  /// `false` on a fully idle tick — hosts can skip redrawing.
  pub fn tick(&mut self) -> bool {
    if self.voice_controller().is_none() {
      return false;
    }
    let prev_status = self.voice_status().clone();
    let prev_started_at = self.voice_started_at();
    let prev_chars_before = self.voice_chars_before();
    crate::state::voice_control::sync_voice_status(self);
    let mut changed = *self.voice_status() != prev_status
      || self.voice_started_at() != prev_started_at
      || self.voice_chars_before() != prev_chars_before;
    if self.continuous_reading()
      && matches!(prev_status, voice::PlaybackStatus::Playing)
      && matches!(self.voice_status(), voice::PlaybackStatus::Idle)
    {
      // Chunk just ended → roll forward to the next paragraph.
      let advanced = crate::state::voice_control::advance_to_next_paragraph_for_continuous_reading(self);
      if !advanced {
        self.stop_continuous_reading();
      }
      changed = true;
    }
    // While voice is actively playing, the active-word highlight
    // moves continuously based on wall-clock time, so every tick
    // changes visible state even if no field flipped.
    if matches!(
      self.voice_status(),
      voice::PlaybackStatus::Playing | voice::PlaybackStatus::Loading
    ) {
      changed = true;
    }
    changed
  }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReaderEvent {
  Input,
  Resize,
  Tick,
  ReloadComplete,
  ImageReady,
  ConfigChange,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct DirtyState {
  content: bool,
  status: bool,
  layout: bool,
  images: bool,
  voice: bool,
}

impl DirtyState {
  fn all() -> Self {
    Self {
      content: true,
      status: true,
      layout: true,
      images: true,
      voice: true,
    }
  }

  fn any(self) -> bool {
    self.content || self.status || self.layout || self.images || self.voice
  }

  fn clear_after_draw(&mut self) {
    *self = Self::default();
  }

  fn mark(&mut self, event: ReaderEvent) {
    match event {
      ReaderEvent::Input => {
        self.content = true;
        self.status = true;
      }
      ReaderEvent::Resize => {
        self.content = true;
        self.status = true;
        self.layout = true;
        self.images = true;
      }
      ReaderEvent::Tick => {
        self.status = true;
        self.voice = true;
      }
      ReaderEvent::ReloadComplete => {
        self.content = true;
        self.status = true;
        self.layout = true;
        self.images = true;
      }
      ReaderEvent::ImageReady => {
        self.images = true;
      }
      ReaderEvent::ConfigChange => {
        self.content = true;
        self.status = true;
      }
    }
  }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ReaderUpdate {
  dirty: DirtyState,
  quit: bool,
}

impl ReaderUpdate {
  fn quit() -> Self {
    Self {
      quit: true,
      dirty: DirtyState::default(),
    }
  }

  fn from_event(event: ReaderEvent) -> Self {
    let mut dirty = DirtyState::default();
    dirty.mark(event);
    Self { dirty, quit: false }
  }
}

struct ReaderRuntime {
  reader: Reader,
  img_state: ImageState,
  theme: Theme,
  kitty_supported: bool,
  dirty: DirtyState,
  first_draw_done: bool,
  pending_event_start: Option<std::time::Instant>,
  // Wall-clock of the most recent user-driven event (key / mouse /
  // resize / focus).  Drives the idle-poll cadence in `poll_timeout`:
  // if more than IDLE_THRESHOLD has passed without input, we drop the
  // poll timeout from 16ms (60 wakeups/s, ~zero-latency redraw) to
  // 250ms (4 wakeups/s) so a paper open in the background doesn't
  // burn battery.  The next keypress pays at most one idle-poll cycle
  // of input latency — acceptable cost for an order-of-magnitude
  // wakeup reduction during long reading sessions.
  last_input_at: Option<std::time::Instant>,
}

impl ReaderRuntime {
  fn new(reader: Reader, theme: Theme, kitty_supported: bool) -> Self {
    Self {
      reader,
      img_state: ImageState::default(),
      theme,
      kitty_supported,
      dirty: DirtyState::all(),
      first_draw_done: false,
      pending_event_start: None,
      last_input_at: None,
    }
  }

  fn run(
    mut self,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
  ) -> (Reader, Result<(), Box<dyn std::error::Error>>) {
    let result = self.run_inner(terminal);
    // Clear any lingering image placements so they don't bleed onto the
    // user's shell after we leave the alt screen.
    images::clear_all(&mut self.img_state);
    (self.reader, result)
  }

  fn run_inner(
    &mut self,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
  ) -> Result<(), Box<dyn std::error::Error>> {
    // TREAD_BENCH_AUTOQUIT=<secs>: clean exit after N seconds so
    // cold/warm benchmark runs terminate deterministically without
    // manual `:q`.
    let autoquit_deadline = std::env::var("TREAD_BENCH_AUTOQUIT")
      .ok()
      .and_then(|s| s.parse::<f64>().ok())
      .map(|secs| std::time::Instant::now() + Duration::from_secs_f64(secs));

    // TREAD_BENCH_JCOUNT=<n>: after the first frame, inject N
    // synthetic `j` keypresses one per loop iteration.  Bypasses
    // crossterm so we don't need a PTY — measures pure scroll-loop
    // cost (handle_event → apply_update → draw_if_dirty).  Combine
    // with TREAD_BENCH=<path> to capture frame + event_to_frame
    // distributions under sustained input.
    let mut remaining_synthetic_j: u32 = std::env::var("TREAD_BENCH_JCOUNT")
      .ok()
      .and_then(|s| s.parse().ok())
      .unwrap_or(0);

    loop {
      self.draw_if_dirty(terminal)?;

      if let Some(deadline) = autoquit_deadline
        && std::time::Instant::now() >= deadline
      {
        bench::emit_us("autoquit", 0);
        break;
      }

      // Synthetic-input path: only after the first real draw, so we
      // measure steady-state and not the warm-up frame.
      if self.first_draw_done && remaining_synthetic_j > 0 {
        remaining_synthetic_j -= 1;
        let ev = Event::Key(crossterm::event::KeyEvent::new(
          crossterm::event::KeyCode::Char('j'),
          crossterm::event::KeyModifiers::NONE,
        ));
        self.pending_event_start = Some(std::time::Instant::now());
        let update = self.handle_event(ev);
        self.apply_update(update);
        if update.quit {
          break;
        }
        continue;
      }

      if event::poll(self.poll_timeout())? {
        let ev = event::read()?;
        // Refresh idle clock on every real input.  Keeps the poll
        // cadence at the snappy 16ms tier while the user is active;
        // we drop to 250ms after IDLE_THRESHOLD of silence (see
        // `poll_timeout`).
        if matches!(
          &ev,
          Event::Key(_) | Event::Mouse(_) | Event::Resize(_, _)
        ) {
          self.last_input_at = Some(std::time::Instant::now());
        }
        // Per-event latency: start clock when the event arrives;
        // stop at the end of the next draw_if_dirty.  Skip mouse
        // motion so trackpad spam doesn't dominate the histogram.
        if matches!(
          &ev,
          Event::Key(_) | Event::Resize(_, _) | Event::FocusLost | Event::FocusGained
        ) {
          self.pending_event_start = Some(std::time::Instant::now());
        }
        let update = self.handle_event(ev);
        self.apply_update(update);
        if update.quit {
          break;
        }
      } else if images::poll_ready(&mut self.img_state) {
        self.apply_update(ReaderUpdate::from_event(ReaderEvent::ImageReady));
      } else if self.reader.tick() {
        self.apply_update(ReaderUpdate::from_event(ReaderEvent::Tick));
      }
    }
    Ok(())
  }

  fn draw_if_dirty(
    &mut self,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
  ) -> Result<(), Box<dyn std::error::Error>> {
    if !self.dirty.any() {
      return Ok(());
    }
    let frame_start = std::time::Instant::now();
    let mut drawn_area = Rect::default();
    terminal.draw(|f| {
      drawn_area = f.area();
      render::draw(f, drawn_area, &self.reader, &self.theme);
    })?;

    if self.kitty_supported {
      let burst_in_progress =
        burst_skip_enabled(self.kitty_supported) && event::poll(Duration::ZERO)?;
      after_draw_guarded(
        &self.reader,
        &mut self.img_state,
        drawn_area,
        self.kitty_supported,
        burst_in_progress,
      );
    }
    self.dirty.clear_after_draw();

    let frame_us = frame_start.elapsed().as_micros().min(u32::MAX as u128) as u32;
    bench::record_frame(frame_us);
    if let Some(t0) = self.pending_event_start.take() {
      let lat = t0.elapsed().as_micros().min(u32::MAX as u128) as u32;
      bench::record_event_to_frame(lat);
    }
    if !self.first_draw_done {
      self.first_draw_done = true;
      bench::emit_us("startup_to_interactive", 0);
    }
    Ok(())
  }

  fn poll_timeout(&self) -> Duration {
    // After this much silence we assume the reader is parked and
    // drop poll cadence from 16ms to 250ms.  Under that, every key
    // still drives a redraw within one poll cycle of receipt; the
    // worst-case input latency on resume is the idle timeout itself,
    // ~250ms.
    const IDLE_THRESHOLD: Duration = Duration::from_secs(1);
    const IDLE_POLL: Duration = Duration::from_millis(250);
    const ACTIVE_POLL: Duration = Duration::from_millis(16);

    if self.dirty.any() {
      Duration::ZERO
    } else if images::has_pending_jobs(&self.img_state) {
      ACTIVE_POLL
    } else if matches!(
      self.reader.voice_status(),
      voice::PlaybackStatus::Playing | voice::PlaybackStatus::Loading
    ) {
      Duration::from_millis(33)
    } else if self
      .last_input_at
      .is_none_or(|t| t.elapsed() > IDLE_THRESHOLD)
    {
      IDLE_POLL
    } else {
      ACTIVE_POLL
    }
  }

  fn handle_event(&mut self, ev: Event) -> ReaderUpdate {
    let event_kind = match &ev {
      Event::Resize(_, _) => ReaderEvent::Resize,
      Event::Key(_) | Event::Mouse(_) | Event::Paste(_) => ReaderEvent::Input,
      _ => ReaderEvent::Input,
    };
    let needs_image_clear = matches!(&ev, Event::Resize(_, _) | Event::FocusLost);

    let action = self.reader.handle_event(ev);
    let mut update = match action {
      ReaderAction::Quit => ReaderUpdate::quit(),
      ReaderAction::ChangeTheme(t) => {
        self.theme = t;
        ReaderUpdate::from_event(ReaderEvent::ConfigChange)
      }
      ReaderAction::Reload => {
        images::clear_all(&mut self.img_state);
        ReaderUpdate::from_event(ReaderEvent::ReloadComplete)
      }
      ReaderAction::Error(msg) => {
        self.reader.cmd_error = Some(msg);
        ReaderUpdate::from_event(ReaderEvent::Input)
      }
      ReaderAction::OpenHelp => {
        self.reader.help_visible = true;
        ReaderUpdate::from_event(ReaderEvent::Input)
      }
      ReaderAction::Continue => ReaderUpdate::from_event(event_kind),
    };

    if needs_image_clear {
      images::clear_all(&mut self.img_state);
      update.dirty.mark(ReaderEvent::Resize);
    }
    update
  }

  fn apply_update(&mut self, update: ReaderUpdate) {
    self.dirty.content |= update.dirty.content;
    self.dirty.status |= update.dirty.status;
    self.dirty.layout |= update.dirty.layout;
    self.dirty.images |= update.dirty.images;
    self.dirty.voice |= update.dirty.voice;
  }
}

#[cfg(test)]
mod runtime_tests {
  use super::*;

  #[test]
  fn dirty_state_marks_redraw_for_visible_events() {
    let cases = [
      (ReaderEvent::Input, (true, true, false, false, false)),
      (ReaderEvent::Resize, (true, true, true, true, false)),
      (ReaderEvent::ReloadComplete, (true, true, true, true, false)),
      (ReaderEvent::ImageReady, (false, false, false, true, false)),
      (ReaderEvent::ConfigChange, (true, true, false, false, false)),
      (ReaderEvent::Tick, (false, true, false, false, true)),
    ];

    for (event, expected) in cases {
      let mut dirty = DirtyState::default();
      dirty.mark(event);
      assert_eq!(
        (
          dirty.content,
          dirty.status,
          dirty.layout,
          dirty.images,
          dirty.voice
        ),
        expected,
        "event {event:?}"
      );
    }
  }

  #[test]
  fn reader_update_quit_does_not_force_redraw() {
    let update = ReaderUpdate::quit();
    assert!(update.quit);
    assert!(!update.dirty.any());
  }
}

#[cfg(test)]
mod acceptance_tests {
  use super::*;
  use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
  use doc_model::{Block, ImageItem, InlineSpan};
  use std::collections::HashMap;

  fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
  }

  fn ctrl(c: char) -> Event {
    Event::Key(KeyEvent::new(
      KeyCode::Char(c),
      KeyModifiers::CONTROL,
    ))
  }

  fn char_key(c: char) -> Event {
    key(KeyCode::Char(c))
  }

  fn send_chars(reader: &mut Reader, text: &str) {
    for c in text.chars() {
      reader.handle_event(char_key(c));
    }
  }

  fn acceptance_reader() -> Reader {
    let blocks = vec![
      Block::Header {
        level: 1,
        text: "Abstract".to_string(),
      },
      Block::Line("alpha beta gamma delta".to_string()),
      Block::Line("method paragraph with mark target".to_string()),
      Block::Anchor("sec:results".to_string()),
      Block::Header {
        level: 1,
        text: "Results".to_string(),
      },
      Block::StyledLine(vec![
        InlineSpan::plain("See "),
        InlineSpan::internal_link("Results", "sec:results"),
        InlineSpan::plain(" and "),
        InlineSpan::citation("[smith]", "smith"),
        InlineSpan::plain("."),
      ]),
      Block::Figure {
        rows: vec![vec![ImageItem {
          path: "figure-a.png".into(),
          kitty_id: 1,
          dims: Some((640, 360)),
        }]],
        alt: "first figure".to_string(),
        figure_id: 1,
        column_gaps_after: Vec::new(),
        header_rows: Vec::new(),
      },
      Block::Figure {
        rows: vec![vec![
          ImageItem {
            path: "figure-b.png".into(),
            kitty_id: 2,
            dims: Some((320, 240)),
          },
          ImageItem {
            path: "figure-c.png".into(),
            kitty_id: 3,
            dims: Some((320, 240)),
          },
        ]],
        alt: "row figure".to_string(),
        figure_id: 2,
        column_gaps_after: Vec::new(),
        header_rows: Vec::new(),
      },
      Block::Anchor("ref-smith".to_string()),
      Block::Line("Smith 2024. A useful reference.".to_string()),
    ];
    Reader::new_with_bibitems(
      blocks,
      100,
      28,
      HashMap::from([("smith".to_string(), "Smith 2024. A useful reference.".to_string())]),
    )
  }

  #[test]
  fn acceptance_preview_scroll_and_resize_stays_coherent() {
    let mut reader = acceptance_reader();

    reader.set_figure_preview_active(true);
    assert!(reader.figure_preview_visible());
    assert_eq!(reader.current_figure_kitty_id(), Some(1));
    assert_eq!(reader.content_width(), 60);

    for _ in 0..8 {
      reader.handle_event(char_key('j'));
    }
    assert_eq!(reader.current_figure_kitty_id(), Some(1));
    assert!(reader.figure_preview_visible());

    reader.handle_event(Event::Resize(80, 20));
    assert_eq!(reader.width, 80);
    assert_eq!(reader.height, 20);
    assert_eq!(reader.content_width(), 48);
    assert!(reader.current_line() < reader.total_lines());

    reader.handle_event(char_key(']'));
    reader.handle_event(char_key('f'));
    assert_eq!(reader.current_figure_kitty_id(), Some(2));
  }

  #[test]
  fn acceptance_search_toc_and_command_mode_keep_navigation_sane() {
    let mut reader = acceptance_reader();

    reader.handle_event(char_key('/'));
    send_chars(&mut reader, "method");
    assert!(matches!(reader.mode(), Mode::Search));
    assert!(!reader.search_matches.is_empty());
    reader.handle_event(key(KeyCode::Enter));
    assert!(matches!(reader.mode(), Mode::Normal));
    assert!(reader.visual_lines[reader.current_line()].text.contains("method"));

    reader.handle_event(char_key('\\'));
    assert!(reader.toc_visible);
    assert_eq!(reader.content_width(), 71);

    reader.handle_event(char_key(':'));
    send_chars(&mut reader, "2");
    let action = reader.handle_event(key(KeyCode::Enter));
    assert!(matches!(action, ReaderAction::Continue));
    assert!(matches!(reader.mode(), Mode::Normal));
    assert_eq!(reader.current_line(), 1);
  }

  #[test]
  fn acceptance_marks_highlights_and_voice_keys_round_trip() {
    let mut reader = acceptance_reader();

    reader.handle_event(char_key('j'));
    let marked_line = reader.current_line();
    reader.handle_event(char_key('m'));
    reader.handle_event(char_key('a'));
    reader.handle_event(char_key('j'));
    assert_ne!(reader.current_line(), marked_line);
    reader.handle_event(char_key('\''));
    reader.handle_event(char_key('a'));
    assert_eq!(reader.current_line(), marked_line);

    reader.handle_event(char_key('V'));
    reader.handle_event(char_key('j'));
    reader.handle_event(char_key('H'));
    assert!(matches!(reader.mode(), Mode::Normal));
    assert!(
      !reader.highlights.highlights.is_empty(),
      "visual highlight should create persistent block-byte highlights"
    );
    let highlight_count = reader.highlights.highlights.len();

    reader.cursor_set_for_test(marked_line.saturating_sub(reader.offset()), 0);
    reader.handle_event(char_key('X'));
    assert!(
      reader.highlights.highlights.len() < highlight_count,
      "X should remove the highlight under the cursor"
    );

    reader.handle_event(char_key('r'));
    assert!(reader.reading_mode());
    assert!(!reader.tick(), "no controller means idle ticks are invisible");
    reader.handle_event(ctrl('p'));
    assert!(reader.continuous_reading());
    reader.handle_event(key(KeyCode::Esc));
    assert!(!reader.reading_mode());
    assert!(!reader.continuous_reading());
  }
}

fn take_count(reader: &mut Reader) -> usize {
  if reader.count_buf.is_empty() {
    1
  } else {
    let n: usize = reader.count_buf.parse().unwrap_or(1).max(1).min(9999);
    reader.count_buf.clear();
    n
  }
}

fn handle_normal(reader: &mut Reader, code: KeyCode, mods: KeyModifiers) -> bool {
  // Dismiss help overlay on any key.
  if reader.help_visible {
    reader.help_visible = false;
    return false;
  }

  // Voice keys take priority — `r`, `R`, `Ctrl+P` enter or restart
  // reading mode; `Space`, `c`, `Esc` only fire while reading_mode is
  // true.  Putting this BEFORE the digit accumulator and the global
  // `Esc`-quits match means reading-mode Esc stops audio without
  // also quitting the reader.
  let key_event = crossterm::event::KeyEvent::new(code, mods);
  if crate::state::voice_control::handle_voice_keys(reader, key_event) {
    reader.count_buf.clear();
    return false;
  }

  // Digit accumulation for count prefix (1–9 to start, 0 only after first digit).
  if let KeyCode::Char(c) = code {
    if c.is_ascii_digit() && (c != '0' || !reader.count_buf.is_empty()) {
      reader.count_buf.push(c);
      return false;
    }
  }

  match code {
    KeyCode::Char('q') | KeyCode::Esc => {
      reader.count_buf.clear();
      return true;
    }
    KeyCode::Char('j') | KeyCode::Down => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_down();
      }
    }
    KeyCode::Char('k') | KeyCode::Up => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_up();
      }
    }
    KeyCode::Char('g') => {
      reader.enter_awaiting_g();
    }
    KeyCode::Char('G') => {
      if reader.count_buf.is_empty() {
        reader.nav_bottom();
      } else {
        let n = take_count(reader);
        let target = n
          .saturating_sub(1)
          .min(reader.total_lines().saturating_sub(1));
        reader.push_nav_mark();
        reader.jump_to_line(target);
      }
    }
    KeyCode::Char('d') if mods.contains(KeyModifiers::CONTROL) => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_half_page_down();
      }
    }
    KeyCode::Char('u') if mods.contains(KeyModifiers::CONTROL) => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_half_page_up();
      }
    }
    KeyCode::PageDown => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_page_down();
      }
    }
    KeyCode::PageUp => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_page_up();
      }
    }
    KeyCode::Char('}') => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.jump_next_paragraph();
      }
    }
    KeyCode::Char('{') => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.jump_prev_paragraph();
      }
    }
    KeyCode::Char('H') => {
      reader.count_buf.clear();
      reader.jump_screen_top();
    }
    KeyCode::Char('M') => {
      reader.count_buf.clear();
      reader.jump_screen_middle();
    }
    KeyCode::Char('L') => {
      reader.count_buf.clear();
      reader.jump_screen_bottom();
    }
    KeyCode::Char('z') => {
      reader.count_buf.clear();
      reader.center_cursor();
    }
    KeyCode::Char('h') | KeyCode::Left => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_left();
      }
    }
    KeyCode::Char('l') | KeyCode::Right => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_right();
      }
    }
    // Word motions — `w`/`W` forward, `b`/`B` back, `e`/`E` to word-end.
    KeyCode::Char('w') => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_word_forward(false);
      }
      reader.remember_column();
    }
    KeyCode::Char('W') => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_word_forward(true);
      }
      reader.remember_column();
    }
    KeyCode::Char('b') => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_word_back(false);
      }
      reader.remember_column();
    }
    KeyCode::Char('B') => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_word_back(true);
      }
      reader.remember_column();
    }
    KeyCode::Char('e') => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_word_end(false);
      }
      reader.remember_column();
    }
    KeyCode::Char('E') => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_word_end(true);
      }
      reader.remember_column();
    }
    // Line edges — `0` to byte 0, `^` to first non-blank, `$` to last char.
    KeyCode::Char('0') => {
      reader.count_buf.clear();
      reader.nav_line_start();
      reader.remember_column();
    }
    KeyCode::Char('^') => {
      reader.count_buf.clear();
      reader.nav_line_first_nonblank();
      reader.remember_column();
    }
    KeyCode::Char('$') => {
      reader.count_buf.clear();
      reader.nav_line_end();
      reader.remember_column();
    }
    // Find char on current line — enters AwaitingChar mode for the next keystroke.
    KeyCode::Char('f') => {
      reader.enter_awaiting_char(FindKind::F);
    }
    KeyCode::Char('F') => {
      reader.enter_awaiting_char(FindKind::ShiftF);
    }
    KeyCode::Char('t') => {
      reader.enter_awaiting_char(FindKind::T);
    }
    KeyCode::Char('T') => {
      reader.enter_awaiting_char(FindKind::ShiftT);
    }
    // Matching brace — `%` jumps between paired brackets on the current line.
    KeyCode::Char('%') => {
      reader.count_buf.clear();
      reader.nav_match_brace();
      reader.remember_column();
    }
    // Sentence motion — `)` next, `(` previous.  Cross-line.
    KeyCode::Char(')') => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_sentence_forward();
      }
      reader.remember_column();
    }
    KeyCode::Char('(') => {
      let n = take_count(reader);
      for _ in 0..n {
        reader.nav_sentence_back();
      }
      reader.remember_column();
    }
    // Cross-reference / citation actions.  `Enter` jumps to the link
    // target under the cursor (no-op if not on a link); `K` and
    // `Shift+Enter` show a citation popup instead of jumping.
    // (`Shift+Enter` requires the kitty keyboard protocol; `K` is the
    // universal fallback — vim's canonical "look up" gesture, distinct
    // from lowercase `k` which is cursor-up.)
    KeyCode::Enter if mods.contains(KeyModifiers::SHIFT) => {
      reader.count_buf.clear();
      popup_citation_at_cursor(reader);
    }
    KeyCode::Enter => {
      reader.count_buf.clear();
      follow_link_at_cursor(reader);
    }
    KeyCode::Char('K') => {
      reader.count_buf.clear();
      popup_citation_at_cursor(reader);
    }
    // Remove highlight under cursor — eXcise.
    KeyCode::Char('X') => {
      reader.count_buf.clear();
      if let Some(vl) = reader.visual_lines.get(reader.current_line()) {
        if vl.block_byte_end > vl.block_byte_start {
          let local = reader
            .cursor_x()
            .min(vl.block_byte_end - vl.block_byte_start - 1);
          let byte_in_block = vl.block_byte_start + local;
          reader.highlights.remove_at(vl.block_idx, byte_in_block);
        }
      }
    }
    KeyCode::Char('*') => {
      reader.count_buf.clear();
      if let Some(word) = reader.word_at_cursor() {
        reader.search_query = word;
        reader.update_search_matches();
        if !reader.search_matches.is_empty() {
          reader.push_nav_mark();
          let idx = reader.search_idx;
          reader.jump_to_match(idx);
        }
      }
    }
    KeyCode::Char(':') => {
      reader.enter_command_mode();
    }
    KeyCode::Char('/') => {
      reader.count_buf.clear();
      reader.enter_search();
    }
    KeyCode::Char('n') => {
      reader.count_buf.clear();
      reader.search_next();
    }
    KeyCode::Char('N') => {
      reader.count_buf.clear();
      reader.search_prev();
    }
    KeyCode::Char(']') => {
      // `]` is now a prefix (vim convention): `]]` jumps section,
      // `]f` steps the figure preview.  Section jump is one keystroke
      // longer than before but matches vim's section-motion idiom.
      reader.enter_awaiting_bracket(true);
    }
    KeyCode::Char('[') => {
      reader.enter_awaiting_bracket(false);
    }
    KeyCode::Char('i') => {
      // Figure-preview side pane.  Toggle is a single keystroke
      // because tread has no insert mode to collide with.
      reader.count_buf.clear();
      reader.toggle_figure_preview();
    }
    // TOC moved off `t` to free the key for vim's `t<char>` find motion.
    KeyCode::Char('\\') => {
      reader.count_buf.clear();
      reader.toggle_toc();
    }
    KeyCode::Char('o') if mods.contains(KeyModifiers::CONTROL) => {
      reader.count_buf.clear();
      reader.nav_back();
    }
    KeyCode::Char('?') => {
      reader.count_buf.clear();
      reader.toggle_help();
    }
    KeyCode::Char('m') => {
      reader.enter_awaiting_mark_name(true);
    }
    KeyCode::Char('\'') | KeyCode::Char('`') => {
      reader.enter_awaiting_mark_name(false);
    }
    KeyCode::Char('y') => {
      // Vim operator-pending: bare `y` waits for a follow-up.  `yy`
      // yanks the current line, `yi<obj>` / `ya<obj>` yanks a text
      // object.  Any other key cancels and returns to Normal.
      reader.enter_awaiting_operator(Operator::Yank);
    }
    KeyCode::Char('v') => {
      reader.enter_visual_mode(false);
    }
    KeyCode::Char('V') => {
      reader.enter_visual_mode(true);
    }
    _ => {
      reader.count_buf.clear();
    }
  }
  false
}

fn handle_search(reader: &mut Reader, code: KeyCode) {
  match code {
    KeyCode::Esc => reader.cancel_search(),
    KeyCode::Enter => reader.confirm_search(),
    KeyCode::Backspace => {
      reader.search_query.pop();
      reader.update_search_matches();
    }
    KeyCode::Char(c) => {
      reader.search_query.push(c);
      reader.update_search_matches();
    }
    _ => {}
  }
}

fn handle_awaiting_char(reader: &mut Reader, code: KeyCode, kind: FindKind) {
  // One-shot: any keystroke ends AwaitingChar.  A Char(c) performs the find;
  // anything else (Esc, arrow keys, etc.) is a quiet cancel.
  if let KeyCode::Char(c) = code {
    if let Some(idx) = reader.find_char_in_line(c, kind) {
      reader.set_cursor_x(idx);
    }
  }
  reader.return_to_normal();
}

fn handle_command(reader: &mut Reader, code: KeyCode) -> ReaderAction {
  match code {
    KeyCode::Esc => {
      reader.return_to_normal();
      ReaderAction::Continue
    }
    KeyCode::Enter => {
      let line = std::mem::take(&mut reader.cmd_buf);
      reader.return_to_normal();
      commands::execute(reader, &line)
    }
    KeyCode::Backspace => {
      if reader.cmd_buf.pop().is_none() {
        // Empty buffer: backspace exits command mode (matches search bar UX).
        reader.return_to_normal();
      }
      ReaderAction::Continue
    }
    KeyCode::Char(c) => {
      reader.cmd_buf.push(c);
      ReaderAction::Continue
    }
    _ => ReaderAction::Continue,
  }
}

fn handle_awaiting_operator(reader: &mut Reader, code: KeyCode, op: Operator) {
  // After an operator key (currently only `y`):
  //  - Doubled key (`yy`) applies to the current line.
  //  - `i` / `a` enter text-object mode.
  //  - Anything else cancels back to Normal.
  match code {
    KeyCode::Char('y') if op == Operator::Yank => {
      if let Some(vl) = reader.visual_lines.get(reader.current_line()) {
        let text = vl.text.clone();
        osc52_yank(&text);
      }
      reader.return_to_normal();
    }
    KeyCode::Char('i') => {
      reader.enter_awaiting_text_object(op, false);
    }
    KeyCode::Char('a') => {
      reader.enter_awaiting_text_object(op, true);
    }
    _ => {
      reader.return_to_normal();
    }
  }
}

fn handle_awaiting_text_object(reader: &mut Reader, code: KeyCode, op: Operator, around: bool) {
  // Look up the text object spec character and produce the yank target.
  // `b` aliases parens, `B` aliases braces (vim convention).
  let yanked: Option<String> = match code {
    KeyCode::Char('w') => text_objects::word(reader, false, around),
    KeyCode::Char('W') => text_objects::word(reader, true, around),
    KeyCode::Char('"') => text_objects::quote(reader, '"', around),
    KeyCode::Char('\'') => text_objects::quote(reader, '\'', around),
    KeyCode::Char('`') => text_objects::quote(reader, '`', around),
    KeyCode::Char('(') | KeyCode::Char(')') | KeyCode::Char('b') => {
      text_objects::pair(reader, '(', ')', around)
    }
    KeyCode::Char('[') | KeyCode::Char(']') => text_objects::pair(reader, '[', ']', around),
    KeyCode::Char('{') | KeyCode::Char('}') | KeyCode::Char('B') => {
      text_objects::pair(reader, '{', '}', around)
    }
    KeyCode::Char('p') => text_objects::paragraph(reader, around),
    KeyCode::Char('s') => text_objects::sentence(reader, around),
    _ => None,
  };
  if let Some(text) = yanked {
    match op {
      Operator::Yank => osc52_yank(&text),
    }
  }
  reader.return_to_normal();
}

/// Resolve the `LinkTarget` (if any) under the cursor by scanning the
/// current visual line's `StyledProse` spans for the one containing
/// `cursor_x`.  Returns `None` for non-styled lines, non-prose blocks,
/// or when the cursor doesn't sit on a linked span.
fn link_at_cursor(reader: &Reader) -> Option<doc_model::LinkTarget> {
  let vl = reader.visual_lines.get(reader.current_line())?;
  let spans = match &vl.kind {
    doc_model::VisualLineKind::StyledProse(s) => s,
    _ => return None,
  };
  let mut byte = 0usize;
  for span in spans {
    let next = byte + span.text.len();
    if reader.cursor_x() >= byte && reader.cursor_x() < next {
      return span.link_target.clone();
    }
    byte = next;
  }
  None
}

/// Enter dispatch: jump to whatever the cursor is sitting on.  No-op if
/// nothing is there.  Pushes onto the back-nav stack so `Ctrl+O` rewinds.
fn follow_link_at_cursor(reader: &mut Reader) {
  let Some(target) = link_at_cursor(reader) else {
    return;
  };
  let line = match &target {
    doc_model::LinkTarget::Internal(label) => reader.label_lines.get(label).copied(),
    doc_model::LinkTarget::Citation(key) => reader.bib_entry_lines.get(key).copied(),
  };
  let Some(line) = line else { return };
  // Land one line before the labeled element so it's fully visible
  // below the cursor (vim convention for jumps).  Clamp at 0.
  let target_line = line.saturating_sub(1);
  reader.push_nav_mark();
  reader.jump_to_line(target_line);
}

/// `K` / `Shift+Enter`: show the citation entry in a popup.  Only acts
/// on `LinkTarget::Citation`; `Internal` targets are silently ignored
/// (Enter is the right key for those).
fn popup_citation_at_cursor(reader: &mut Reader) {
  let Some(target) = link_at_cursor(reader) else {
    return;
  };
  let key = match target {
    doc_model::LinkTarget::Citation(k) => k,
    _ => return,
  };
  let entry = reader
    .bib_entries
    .get(&key)
    .cloned()
    .unwrap_or_else(|| "(no entry available)".to_string());
  // Wrap to ~60 chars for readability in the popup.
  let lines: Vec<String> = wrap_for_popup(&entry, 60);
  reader.popup = Some(state::PopupContent {
    title: format!("[{key}]"),
    lines,
  });
}

fn wrap_for_popup(text: &str, width: usize) -> Vec<String> {
  let mut out = Vec::new();
  let mut line = String::new();
  for word in text.split_whitespace() {
    if line.is_empty() {
      line.push_str(word);
    } else if line.chars().count() + 1 + word.chars().count() <= width {
      line.push(' ');
      line.push_str(word);
    } else {
      out.push(std::mem::take(&mut line));
      line.push_str(word);
    }
  }
  if !line.is_empty() {
    out.push(line);
  }
  if out.is_empty() {
    out.push(String::new());
  }
  out
}

fn handle_awaiting_g(reader: &mut Reader, code: KeyCode) {
  // `gg` → top of doc; `ge` / `gE` → backward word-end.  Any other key cancels.
  match code {
    KeyCode::Char('g') => {
      reader.nav_top();
    }
    KeyCode::Char('e') => {
      reader.nav_word_end_back(false);
      reader.remember_column();
    }
    KeyCode::Char('E') => {
      reader.nav_word_end_back(true);
      reader.remember_column();
    }
    _ => {}
  }
  reader.return_to_normal();
}

fn handle_awaiting_bracket(reader: &mut Reader, code: KeyCode, forward: bool) {
  // `]]` / `[[` jump section (legacy `]` / `[` semantics moved here
  // so the second keystroke disambiguates from `]f` / `[f`).
  // `]f` / `[f` step the figure-preview cursor.  Anything else
  // cancels with no movement — matches the `Mode::AwaitingG` shape.
  match code {
    KeyCode::Char(']') if forward => {
      reader.jump_next_section();
    }
    KeyCode::Char('[') if !forward => {
      reader.jump_prev_section();
    }
    KeyCode::Char('f') => {
      reader.step_figure(if forward { 1 } else { -1 });
    }
    _ => {}
  }
  reader.return_to_normal();
}

fn handle_awaiting_mark_name(reader: &mut Reader, code: KeyCode, for_set: bool) {
  // One-shot: a letter `Char(c)` either sets or jumps; anything else cancels.
  if let KeyCode::Char(c) = code {
    if c.is_ascii_alphabetic() {
      if for_set {
        reader.set_mark(c);
      } else {
        reader.jump_to_mark(c);
      }
    }
  }
  reader.return_to_normal();
}

fn handle_visual(reader: &mut Reader, code: KeyCode) {
  match code {
    KeyCode::Esc | KeyCode::Char('v') | KeyCode::Char('V') => {
      reader.return_to_normal();
    }
    KeyCode::Char('j') | KeyCode::Down => reader.nav_down(),
    KeyCode::Char('k') | KeyCode::Up => reader.nav_up(),
    KeyCode::Char('h') | KeyCode::Left => reader.nav_left(),
    KeyCode::Char('l') | KeyCode::Right => reader.nav_right(),
    KeyCode::Char('y') => {
      let text = yank_selection(reader);
      osc52_yank(&text);
      reader.return_to_normal();
    }
    KeyCode::Char('H') => {
      commit_selection_as_highlights(reader);
      reader.return_to_normal();
    }
    _ => {}
  }
}

/// Convert the current visual selection into one or more `Highlight`
/// entries (one per *block* the selection touches), and add them to
/// `reader.highlights`.  No-op for empty char-visual selections.  Save
/// to disk happens on clean exit alongside bookmarks.
fn commit_selection_as_highlights(reader: &mut Reader) {
  use crate::highlights::Highlight;

  let cur = reader.current_line();
  let anchor = reader.visual_anchor;
  let (lo, hi) = (cur.min(anchor), cur.max(anchor));
  let is_line_mode = matches!(reader.mode(), Mode::Visual { line_mode: true });
  let ax = reader.visual_anchor_x;
  let cx = reader.cursor_x();

  // Empty char-visual selection: anchor and cursor identical → no-op.
  if !is_line_mode && lo == hi && ax == cx {
    return;
  }

  // Normalize first/last column endpoints based on which end is the anchor.
  let (start_x, end_x_incl) = if anchor <= cur { (ax, cx) } else { (cx, ax) };

  // Walk VLs lo..=hi, building per-VL byte ranges within their parent
  // block, and merging consecutive same-block runs into one highlight.
  let mut current: Option<(usize, usize, usize)> = None; // (block_idx, byte_start, byte_end)
  for i in lo..=hi {
    let Some(vl) = reader.visual_lines.get(i) else {
      continue;
    };
    // Skip non-text blocks (Matrix, Rule, Blank) — they have zero byte range.
    if vl.block_byte_end == vl.block_byte_start {
      continue;
    }

    let local_start = if !is_line_mode && i == lo {
      start_x.min(vl.text.len())
    } else {
      0
    };
    let local_end_excl = if !is_line_mode && i == hi {
      next_char_boundary(&vl.text, end_x_incl)
    } else {
      vl.text.len()
    };
    let local_start = snap_back_to_boundary(&vl.text, local_start);
    let local_end_excl = local_end_excl.min(vl.text.len());

    let byte_start = vl.block_byte_start + local_start;
    let byte_end = vl.block_byte_start + local_end_excl;
    if byte_end <= byte_start {
      continue;
    }

    match &mut current {
      Some((blk, _, end)) if *blk == vl.block_idx => {
        *end = byte_end;
      }
      _ => {
        if let Some((blk, s, e)) = current.take() {
          reader.highlights.add(Highlight {
            block_idx: blk,
            byte_start: s,
            byte_end: e,
          });
        }
        current = Some((vl.block_idx, byte_start, byte_end));
      }
    }
  }
  if let Some((blk, s, e)) = current {
    reader.highlights.add(Highlight {
      block_idx: blk,
      byte_start: s,
      byte_end: e,
    });
  }
}

/// Snap `byte_idx` down to the nearest UTF-8 char boundary at or before it.
fn snap_back_to_boundary(text: &str, byte_idx: usize) -> usize {
  let mut i = byte_idx.min(text.len());
  while i > 0 && !text.is_char_boundary(i) {
    i -= 1;
  }
  i
}

/// Return the byte position immediately *after* the codepoint that starts
/// at (or contains) `byte_idx`.  Used to compute exclusive end positions
/// in selections.
fn next_char_boundary(text: &str, byte_idx: usize) -> usize {
  let start = snap_back_to_boundary(text, byte_idx);
  let mut i = start + 1;
  while i < text.len() && !text.is_char_boundary(i) {
    i += 1;
  }
  i.min(text.len())
}

fn yank_selection(reader: &Reader) -> String {
  let cur = reader.current_line();
  let anchor = reader.visual_anchor;
  let (lo, hi) = (cur.min(anchor), cur.max(anchor));
  let is_line_mode = matches!(reader.mode(), Mode::Visual { line_mode: true });

  let lines: Vec<&str> = (lo..=hi)
    .filter_map(|i| reader.visual_lines.get(i))
    .map(|vl| vl.text.as_str())
    .collect();

  if is_line_mode || (lo == hi && reader.cursor_x() == reader.visual_anchor_x) {
    lines.join("\n")
  } else {
    let first = lines.first().copied().unwrap_or("");
    let last = lines.last().copied().unwrap_or("");
    let ax = reader.visual_anchor_x;
    let cx = reader.cursor_x();
    if lo == hi {
      let (s, e) = (ax.min(cx), ax.max(cx) + 1);
      first.get(s..e.min(first.len())).unwrap_or("").to_string()
    } else {
      let (first_start, last_end) = if anchor <= cur {
        (ax, cx + 1)
      } else {
        (cx, ax + 1)
      };
      let mut parts = vec![first.get(first_start..).unwrap_or("").to_string()];
      if lines.len() > 2 {
        parts.extend(lines[1..lines.len() - 1].iter().map(|s| s.to_string()));
      }
      parts.push(
        last
          .get(..last_end.min(last.len()))
          .unwrap_or("")
          .to_string(),
      );
      parts.join("\n")
    }
  }
}

pub(crate) fn osc52_yank(text: &str) {
  use std::io::Write;
  let encoded = base64_encode(text.as_bytes());
  let _ = std::io::stdout().write_all(format!("\x1b]52;c;{encoded}\x07").as_bytes());
  let _ = std::io::stdout().flush();
}

fn base64_encode(data: &[u8]) -> String {
  const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
  for chunk in data.chunks(3) {
    let b0 = chunk[0] as usize;
    let b1 = chunk.get(1).copied().unwrap_or(0) as usize;
    let b2 = chunk.get(2).copied().unwrap_or(0) as usize;
    out.push(T[b0 >> 2] as char);
    out.push(T[((b0 & 3) << 4) | (b1 >> 4)] as char);
    out.push(if chunk.len() > 1 {
      T[((b1 & 15) << 2) | (b2 >> 6)] as char
    } else {
      '='
    });
    out.push(if chunk.len() > 2 {
      T[b2 & 63] as char
    } else {
      '='
    });
  }
  out
}
