# ADR-0001 — Figure model

- **Status:** Accepted (2026-05-18)
- **Crate:** `doc-model`, with the consumer projection in `tread::state`
- **Supersedes:** the heuristic figure grouping in earlier `tread` revisions
  that reconstructed multi-panel figures from blank lines and alt text in
  `Block::Image` / `Block::ImageRow` streams.

## Context

The reader needs a figure-aware model in three places:

1. **Inline rendering.** A figure with stacked subfigure rows (e.g.
   `[[a, b], [c, d], [e, f]]`) should render as one visual unit — captions
   attached once, column gaps matching the source tabular's
   `@{\hspace{...}}`, header labels above the right images.
2. **Preview pane.** When the host opens the side preview, it tiles the
   selected figure's 2D grid into the pane area; it needs the same row /
   column structure the inline path uses.
3. **Figure-step navigation.** `]f` / `[f` walks figures in source order.
   A multi-panel figure is one step, not N.

Earlier revisions emitted a flat `Block::Image` / `Block::ImageRow` stream
plus surrounding `Block::Line(alt_text)` / `Block::Blank`. Consumers had to
reconstruct grouping from blank-line positions and caption strings.

That approach broke in three concrete ways on real papers (`2605.04035`
exercised every one):

- Captions duplicated when a multi-row figure crossed a blank line.
- `]f` stepped into individual subfigures, not whole figures.
- Column-group gaps (`@{\hspace{1mm}}` between groups of N=4 / N=6 / N=16
  panels) and tabular header labels above the image grid had nowhere to
  live in the block model, so they were silently dropped.

## Decision

`Block::Figure` carries the full semantic figure as one unit:

```rust
Figure {
    rows: Vec<Vec<ImageItem>>,   // 2D grid: outer = stack row, inner = side-by-side
    alt: String,                  // single caption, attached once
    figure_id: u32,               // parser's per-document counter
    column_gaps_after: Vec<usize>,// flat-column indices after which to draw a gap
    header_rows: Vec<Vec<HeaderCell>>, // textual header from a tabular above the images
}
```

The Pandoc parser walks `Figure` nodes (and the LaTeX-fallback path walks
`\begin{figure}` environments) and emits one `Block::Figure` per source
figure with the structure already lifted.

`tread` builds **`FigureIndex`** as a straight projection of the
`Block::Figure` stream — no heuristics, no blank-line counting:

```rust
impl FigureIndex {
    pub fn build(blocks: &[Block]) -> Self { /* one entry per Block::Figure */ }
}
```

`FigureEntry::layout(area)` computes a `FigureLayout { headers, image_rows,
caption }` that both the renderer (`render::draw_preview_pane`) and the
image tiler (`images::place_one_figure`) consume. They cannot drift —
column positions and row heights come from one function.

## Consequences

**Good:**
- Multi-panel figures are one navigation step, with one caption, in one
  block — captions never duplicate.
- `column_gaps_after` and `header_rows` have a home; the Ava-256-style
  tables that wrap subfigures with column labels survive end-to-end.
- `FigureIndex::build` is deterministic and trivially testable;
  `FigureEntry::layout` is the one place inline / preview drift could
  occur, and it has a single implementation.
- `figure_id` is parser-assigned, so downstream consumers don't recount
  from block positions (which would re-break under reload).

**Costs:**
- The Pandoc parser does more work per `Figure` node: detect stacked vs
  side-by-side layout, match an outstanding `TableSpec` for the gap /
  header recovery, lift labels onto `Block::Anchor`. The extraction is
  ~150 lines and embedded in `pandoc_parse::walk_blocks` — currently a
  large knowledge sink, flagged for follow-up (deepen into a dedicated
  figure-extraction module).
- Two parsers (Pandoc primary, hand-rolled fallback) both have to emit
  the richer shape, doubling the surface that has to stay correct.

## Layout-detection precedence

Inside the Pandoc walk, layout is decided in this order:

1. Multiple top-level blocks inside the `Figure` → side-by-side
   (minipage / subfigure pattern).
2. Single block with explicit `\\` LineBreak inlines between images →
   stacked.
3. Single block where any image has `width=\textwidth` / `\linewidth`
   (or scalar ≥ 0.85) → stacked.
4. Otherwise → side-by-side.

These rules are empirical; they cover every paper in `test-papers.txt`
including the Attention and Ava-256 stress cases.

## Validation

- `crates/arxiv-render/examples/dump_figures.rs` exercises the figure
  extraction end-to-end on a chosen arXiv ID.
- Acceptance tests for figure-step navigation (`]f` / `[f`) and preview
  toggle live in `crates/tread/`.
- The Attention paper (`1706.03762`) is the regression baseline; the
  Ava-256 paper (`2605.04035`) is the stress case for multi-row
  figures with column gaps and headers.

## Open follow-up

- ~~Deepen the Pandoc figure extraction into its own module behind a
  `Vec<Value> → Vec<Block::Figure>` seam.~~ Done 2026-05 (commit
  `42cb3c7`; `arxiv-render/src/pandoc_parse/figure.rs`).
- ~~The fallback hand-rolled LaTeX parser doesn't emit `Block::Figure`
  at all — figures degrade to a plain `Block::Line("[Figure: <cap>]")`
  text caption when Pandoc is missing.~~ Moot — the hand-rolled
  parser was removed entirely in commit `0a60ea7`.  The parser
  layout is now ar5iv (primary) + Pandoc (fallback).  ADR-0007
  documents the Pandoc-side split that consolidated figure extraction
  behind `pandoc_parse::figure`.
- ~~ar5iv (the primary path) emitted captions only, never
  `Block::Figure` — so the ~95% of papers on that path showed a bold
  caption line with no image (backlog B9).~~ Done 2026-05 (this
  session): `ar5iv_parse::emit_figure` emits `Block::Figure` with
  tarball-relative paths (subfigures flattened to one row); a new
  `fetch::fetch_ar5iv_assets` downloads the referenced PNGs into
  `~/.cache/tread/ar5iv-assets/<id>/` on graphics terminals, and
  `paper.rs` wires that dir as `asset_dir` so the shared
  `absolutize_image_paths` resolves+dims them.  Both parser paths now
  genuinely emit figures.
