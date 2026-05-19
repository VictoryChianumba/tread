# tread — domain context

The vocabulary an architecture audit or refactor should use when talking about
this codebase. If a term here drifts from the code, fix the code or update this
file — there should be one canonical name per concept.

Companions:
- `features.md` — user-facing capability list.
- `CLAUDE.md` — invariants and how-to for common edits.
- `docs/adr/` — decisions that shaped the current shape.

## What tread is

A terminal-native reader for arXiv papers. It fetches a paper's e-print
tarball, parses the LaTeX into a typed block model, lays the blocks out into
a flat visual line table, and renders it as a navigable TUI with optional
inline pixel figures via the Kitty graphics protocol.

Embeddable: the `tread` crate exposes `Reader::init`, `Reader::handle_event`,
`Reader::tick`, `tread::draw`, `after_draw`, and `ImageState` so the wider
`trench` UI can host the reader as a panel. The standalone binary wraps the
same surface in a `ReaderRuntime`.

## The pipeline

```
arXiv ID
  │
  ▼  fetch: ar5iv HTML (arxiv-render::fetch::fetch_ar5iv) — primary
  │         fallback: e-print tarball (fetch::fetch_source) + PDF
  │
  ▼  parse:
  │   primary  — ar5iv (LaTeXML HTML) → Block (arxiv-render::ar5iv_parse)
  │   fallback — Pandoc JSON → Block (arxiv-render::pandoc_parse)
  │              used only when ar5iv hasn't processed this paper or
  │              the parser couldn't make sense of its output
  │
  ▼  table placement: PDF anchors lift Block::Matrix groups to PDF-rendered
  │   position (arxiv-render::pdf_anchors, ::placement) — Pandoc path only;
  │   ar5iv source already has tables in render order
  │
  ▼  Vec<Block>  ─── doc-model boundary ───
  │
  ▼  layout: build_visual_lines(blocks, width, height) → Vec<VisualLine>
  │   (doc-model). LayoutCache wraps this with derived indexes.
  │
  ▼  Reader owns blocks, LayoutCache, cursor, mode, persistence
  │
  ├─► render: ratatui draw pass writes the character-cell buffer
  │   (split by region — see ADR-0006)
  │
  └─► images: post-draw Kitty `a=p` placements for VisualLineKind::Image
                rows (does not pass through the ratatui buffer; see ADR-0003)
```

Two side outputs of layout are derived from `Block` directly, not from
`VisualLine`:
- `FigureIndex` — ordered, semantic view of `Block::Figure`s for figure-step
  navigation and preview pane selection.
- `bib_entries` / `label_lines` / `bib_entry_lines` — cross-reference resolution
  maps consumed by `Enter`/`K` on links.

## Crates

| Crate | Role | Has I/O? |
|---|---|---|
| `doc-model` | `Block`, `VisualLine`, `build_visual_lines`, figure layout math | no |
| `arxiv-render` | fetch (ar5iv HTML + tarball + PDF), ar5iv parser (primary), Pandoc parser (fallback), PDF anchor extraction, placement | yes |
| `tread` | reader runtime, render, images, persistence, embed surface | yes |
| `math-render` | display + inline math; thin wrapper around `tui-math` | no |
| `ui-theme` | `Theme`, `ThemeId` (16 themes), shared with `trench` | no |
| `kitty-graphics` | Kitty protocol transmit/place/delete, capability detection | yes |

`math-render` and `ui-theme` are shared with the `trench` binary in a sibling
worktree. Treat them as read-only unless a change is explicitly cross-cutting.

## Domain vocabulary

### Document model (`doc-model`)

- **Block** — semantic unit produced by the parser: `Line`, `StyledLine`,
  `DisplayMath`, `Header`, `Matrix`, `Figure`, `ListItem`, `CodeBlock`,
  `Quote`, `Anchor`, `Rule`, `Blank`. Carries enough structure that the
  layout pass never needs to re-parse text.
- **InlineSpan** — one styled run inside a `StyledLine` / `ListItem` /
  `Quote`. Holds bold/italic/underline/monospace, optional RGB colour,
  optional URL, and an optional `LinkTarget` (for `\ref` / `\cite`).
- **LinkTarget** — `Internal(label)` or `Citation(key)`. The reader
  resolves these against `label_lines` / `bib_entries` at runtime.
- **VisualLine** — one screen row after wrapping: `block_idx`,
  `line_in_block`, `text`, `kind`, `block_byte_start`, `block_byte_end`.
  The reader indexes by visual-line position; persistence stores
  block-byte ranges so highlights survive resize.
- **VisualLineKind** — Prose / MathLine / Header / MatrixLine / Blank /
  StyledProse / ListItem / Code / Rule / Quote / **Image** / **ImageRow**.
  Image kinds carry `kitty_id`, `cols`, `rows`, `is_first` — the renderer
  paints blanks here and the post-draw injector places pixels into the
  same cells.
- **Figure** (`Block::Figure`) — one whole figure as the source intended
  it: `rows` is the 2D grid (stacked × side-by-side), `alt` the caption,
  `figure_id` the parser's per-document counter, `column_gaps_after` the
  recovered `@{\hspace{...}}` column-group separators, `header_rows` the
  textual header lifted from a tabular column above the images. See
  [ADR-0001](docs/adr/0001-figure-model.md).
- **ImageItem / HeaderCell** — one sub-image / one tabular header cell
  inside a `Figure`.

### Reader runtime (`tread`)

- **PaperData** — parsed paper bundle (blocks, asset dir, meta,
  bibitems) handed to `Reader::new_with_bibitems`.
- **Reader** — owns `blocks`, the `LayoutCache`, cursor, mode,
  bookmarks, highlights, search state, popup, preview state, and
  persistence. Field surface deepened across six seams (ADR-0004
  mode/cursor/voice/bookmarks; ADR-0008 read-mostly closures + popup +
  figure-preview triple; ADR-0009 LayoutCache projections + count_buf).
  Today's public fields are read-only collections and simple toggles
  with no remaining multi-field invariants.
- **LayoutCache** — derived from `(blocks, width, height, text_only)`.
  Holds `visual_lines`, `sections`, `label_lines`, `bib_entries`,
  `bib_entry_lines`. Rebuilt through `rebuild_layout` with a
  `LayoutRebuildReason` for benchmark spans.
- **LayoutRebuildReason** — Initial / Reload / Resize / TextOnlyToggle /
  TocToggle. Tags the benchmark span so a perf regression points at the
  cause.
- **FigureIndex** — `Vec<FigureEntry>` projected from `Block::Figure`s
  in source order, plus a `kitty_id → path` map. The single source of
  truth for figure-step navigation; the reader does not re-derive
  grouping from `visual_lines`.
- **FigureEntry** — `FigureIndex` element: rows of `FigurePart`s, alt,
  column gaps, header rows. Knows how to lay itself out inside a `Rect`
  via `FigureEntry::layout`. The output (`FigureLayout`) is consumed by
  both the preview pane text path and the Kitty image tiler so they
  cannot drift. See [ADR-0002](docs/adr/0002-preview-pane-model.md).
- **FigurePart** — one sub-image inside a `FigureEntry`: `kitty_id`,
  path, optional pixel dims.
- **FigurePreviewState** — `active`, `selected_index`,
  `selected_kitty_id`, `last_geometry` (cached for image invalidation).
  Lives on `Reader`. Toggle via `set_figure_preview_active` /
  `toggle_figure_preview`; never write the fields directly.
- **PreviewGeometry** — `(x, y, width, height)` of the preview pane on
  the last frame. Used by `ImageState` to decide whether a re-place is
  needed.
- **text_only** — when true, image VLs are dropped from `visual_lines`
  so text reflows past the figures. Hosts that show figures in a
  dedicated side pane set this; captions remain as prose blocks so
  `]f` / `[f` still have something to step through.
- **Mode** — Normal / Insert-like states are absent; tread is read-only.
  Modes are Normal / Visual{Char,Line} / Search / Command /
  AwaitingChar / AwaitingMarkName / AwaitingG / AwaitingBracket /
  AwaitingOperator / AwaitingTextObject.  `Search` and `Command` share
  the bottom prompt slot (different prefix glyph).  All transitions
  route through `state/mode.rs` per ADR-0004 Seam 1.
- **CommandResult** — what an Ex-command returns to the event loop:
  `Quit`, `ChangeTheme(new)`, `OpenHelp`, `Error(msg)`, or `None`.
- **HighlightSet** — persistent character-range highlights stored at
  block-byte granularity.
- **Bookmarks** — letter-keyed marks (`m{a}` / `'{a}` / `` `{a} ``);
  persisted per arXiv ID.
- **ReaderRuntime** — the standalone binary's event loop wrapper:
  owns the `Reader`, an `ImageState`, the `Theme`, dirty flags, and
  idle-poll timing. Not exported; the embed path supplies these
  pieces directly from `trench`.
- **DirtyState** — bitfield of "what changed this tick" the runtime
  uses to skip redraws when nothing visible moved.

### Image emission (`tread::images`)

- **ImageState** — frame-to-frame image bookkeeping: PNG byte cache,
  negative-load TTL cache, transmitted-id set, last-emitted geometry,
  preview-pane ids, worker thread for async byte loads. Public to the
  embed surface; private implementation modules live behind it.
  See [ADR-0003](docs/adr/0003-terminal-image-strategy.md).
- **ImageJob / ImageResult** — request/response messages between the
  reader thread and the image worker.
- **BatchEmitter** — `kitty-graphics`'s chunked APC writer. Handles the
  `m=1`/`m=0` chunking the protocol requires for large payloads.
- **kitty_id** — `u32` handle a `Block::Image` / `Figure` part carries
  to identify itself to the protocol. Stable for the life of a parse;
  changes only on `:reload`.
- **inline placement** — image VLs that appear in the main reader pane
  because `text_only == false`. Placed against the reader area each
  frame.
- **preview placement** — image VL placed in the right-hand preview
  pane when `FigurePreviewState::active`. Distinct invalidation from
  inline (`split_*` methods on `ImageState`).
- **negative cache** — `negative_loads` in `ImageState`: failed loads
  are remembered for `NEGATIVE_CACHE_TTL` (30 s) so a missing or
  unreadable figure doesn't re-spawn `pdftoppm` every scroll tick.
- **transmitted_ids** — Kitty ids the host terminal has already cached
  image bytes for, so subsequent placements can use the cheap `a=p`
  path instead of re-transmitting the full base64 payload. Empty on
  iTerm2 (no persistent image store) — see ADR-0003.

### Parsing & placement (`arxiv-render`)

- **ar5iv parser** — primary path.  Fetches the LaTeXML-rendered HTML
  from `ar5iv.labs.arxiv.org` and walks the DOM to emit `Block`s.
  No external binary required; only an HTTPS round-trip.  Lives in
  `ar5iv_parse.rs`.  Coverage is not 100% — for papers ar5iv hasn't
  processed yet, the fallback fires.
- **Pandoc parser** — fallback path.  Fetches the e-print tarball,
  runs `pandoc -f latex -t json`, walks the AST to emit `Block`s.
  Lives in `pandoc_parse/` (split across `mod.rs`, `figure.rs`,
  `inline.rs`, `table.rs`, `preprocess.rs`, `spec.rs` per ADR-0007).
  Requires `pandoc` on `PATH`.
- **PDF anchor extraction** — runs `pdftotext` over the PDF, locates
  per-table placement anchors, and reports rendered (page, y) for
  each table. `pdf_anchors.rs`.
- **placement lift** — re-orders `Block::Matrix` groups in the block
  stream so tables appear at the PDF-rendered position, not the
  LaTeX source position. `placement.rs`.
- **figure_id** — per-document counter assigned by the parser; carried
  on `Block::Figure` so downstream consumers don't need to recount.
- **TableSpec** — captured `\begin{tabular}{...}` column specs
  (vertical rules, `\hline`s, `@{\hspace{...}}`). Pre-scanned from the
  raw LaTeX and matched against Pandoc's stripped-down Table AST so
  vertical rules and column gaps survive the Pandoc round-trip.
- **bibitems** — bibliography entries by cite-key. Three extraction
  paths (in priority order): inline `\bibitem{}` scan, Pandoc-citeproc
  Div blocks, BibTeX `.bib` file scanner. Used for both popup display
  and `Enter`-on-citation jumping.
- **asset_dir** — extracted tarball root for one paper. Where the
  parser resolves image paths, BibTeX files, included `.tex` parts.

## Persistence layout

Loaded on `Reader::new`, saved on clean exit. All under `~/.config/trench/`:

| Data | File | Granularity |
|---|---|---|
| Reading progress | `reader_progress.json` | shared, keyed by arXiv ID |
| Bookmarks | `bookmarks_<id>.json` | one per paper |
| Highlights | `highlights_<id>.json` | one per paper |
| Theme + voice | `block_reader.json` | global |
| Trench theme (read) | `config.json` | global, read-only |

Highlights / marks save fire-and-forget (no atomic rename); a crash mid-write
loses the latest entry, not all entries. Progress is rewritten in full.

## Read-only zones

Touch only with a cross-crate plan:
- `crates/math-render/`, `crates/ui-theme/` — shared with `trench`.
- Workspace `Cargo.toml` — `trench` references these crates by path.
- `doc-model::VisualLine` — ~12 construction sites in `build_visual_lines`;
  any field change is a sweep.

## See also

- [ADR-0001 — Figure model](docs/adr/0001-figure-model.md)
- [ADR-0002 — Preview pane model](docs/adr/0002-preview-pane-model.md)
- [ADR-0003 — Terminal image strategy](docs/adr/0003-terminal-image-strategy.md)
- [ADR-0004 — Reader public surface (Seams 1–4)](docs/adr/0004-reader-public-surface.md)
- [ADR-0005 — doc-model internal split](docs/adr/0005-doc-model-internal-split.md)
- [ADR-0006 — render region split](docs/adr/0006-render-region-split.md)
- [ADR-0007 — pandoc_parse split](docs/adr/0007-pandoc-parse-split.md)
- [ADR-0008 — Reader Seam 5](docs/adr/0008-reader-seam-5.md)
- [ADR-0009 — Reader Seam 6](docs/adr/0009-reader-seam-6.md)
