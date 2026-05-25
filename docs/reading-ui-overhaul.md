# Reading-UI overhaul (2026-05) — changelog

A running, repo-visible record of the reading-experience overhaul: what
changed, *why*, the key files, test deltas, and the commit. This is the
canonical narrative — `features.md` (tracked) holds the user-facing
summary; `todo.md` / `v2.md` are **gitignored** local working notes, so
this file must stand on its own for anyone reading a fresh checkout.

Scope: UI / formatting / design of the TUI reader. Baseline was an
exploratory comparison against terminal readers (epy, bk, Bookokrat) and
web/scholarly readers (ar5iv, arXiv Vanity, alphaXiv, Semantic Reader,
Distill), measured against reading-typography research (optimal measure
50–75 ch; ragged-right beats justified).

**How to update:** add an entry below in the same template when an
overhaul item ships, in the *same commit* as the change. Newest first
within each section.

---

## Conventions

- **No ADR for this work.** The repo writes ADRs only for the big
  structural refactors (`docs/adr/0004`–`0009`). UI work is recorded here
  and in the commit messages instead; golden-count bumps are annotated
  inline at the constants in `crates/arxiv-render/src/lib.rs`.
- The reader crate is `crates/tread` (the `block-reader` name in some
  older docs/`CLAUDE.md` is stale).
- Golden visual-line/block counts are pinned per-paper in
  `arxiv-render/src/lib.rs::golden_tests`; bump them when a layout change
  shifts the count intentionally.

---

## In progress / remaining

Roughly by reading-experience leverage:

- **Theme semantic layer** — colour roles are low-level (`bg_code`,
  `link_fg`); no `heading` / `quote_bar` / `inline_code_bg` naming layer.
- **Reading-comfort affordances** — line-spacing toggle, focus / sentence
  dimming.
- **TOC collapse/expand + resizable sidebar width** (fixed 28-col today).

---

## Shipped

### Math wrapping — over-wide display equations wrap at the relation
- **Commit:** `dc5e084`
- **What:** a display-math row wider than the screen now wraps at spaces
  instead of overflowing. Continuation lines align under the right-hand
  side of the first relation (`=`, `≤`, `→`, …) and lead with the
  binary operator carried down from the previous line; the equation
  number stays at the right edge of the final sub-line. Rows that fit are
  centred as before.
- **Why:** `math_render` emits a flat Unicode string per row; layout
  centred each row but `center_line` returned over-wide rows unpadded, so
  they ran off-screen. The `MathLine` kind's `block_width`/`is_first`/
  `is_last` fields turned out to be vestigial (only layout writes them;
  the renderer prints the pre-laid-out text), so wrapping one row into
  several `MathLine`s is free of downstream effects.
- **Style decision:** align-at-relation (chosen over a flat hanging
  indent or no indent). Falls back to a fixed 4-space hang when there's
  no relation or it sits too far right to leave a usable continuation
  width. Heuristic edge: when the leading `=` is glued to a symbol (no
  surrounding space) it isn't a standalone token, so alignment may land
  on a later relation and over-indent — readable, not pretty. Breaking is
  greedy-at-space; symbol clusters (sub/superscripts) carry no spaces so
  they never split.
- **Key files:** `doc-model/src/layout.rs` (`wrap_math_line`,
  `relation_indent`, `MATH_*` consts, the `DisplayMath` arm).
- **Tests:** +`display_math_wraps_overlong_line_at_relation`. Goldens
  re-pinned (math content): differential-algebra 1677→1733, gaussian
  1494→1495; Attention/GPT-3 unchanged.

### Preview-pane ratio — adjustable figure/text split
- **Commit:** `d5523f7`
- **What:** `:set preview=<n>` (20–70) gives the figure-preview pane n% of
  the content width; the reader text pane gets the rest. Persisted in
  `block_reader.json`, reflows immediately when the pane is open. Default
  40 preserves the previous 40/60 split.
- **Why:** the split was hard-coded, and in *two* places that had to be
  kept in sync by hand — a `PREVIEW_TEXT_PERCENT = 60` const drove the
  reflow width while `split_content_for_preview` hard-coded
  `Percentage(60)/Percentage(40)` for the draw Rect. The percentage is now
  the single source of truth: both `content_width_for` and the draw split
  read one `Reader::preview_pane_percent` field, so the wrapped text and
  the image Rect can't drift apart.
- **Key files:** `state/mod.rs` (`DEFAULT_PREVIEW_PANE_PERCENT` const,
  private field + getter + reflowing setter, `content_width_for` percent
  param), `config.rs` (persisted field), `commands.rs` (`set_preview`),
  `render/mod.rs` (split reads the field). Mirrors the `:set width`
  plumbing.
- **Tests:** +`set_preview_pane_percent_resizes_reader_pane` (state),
  +2 config tests. 167→170 tread. Goldens unchanged (default keeps shape).

### Paragraph rhythm — normalized at layout time
- **Commit:** `1015adc`
- **What:** `normalize_blank_rhythm`, a post-pass in `build_visual_lines`,
  collapses runs of blank visual lines to one and trims leading/trailing
  blanks — so inter-block spacing is exactly one line regardless of source.
  Policy chosen: flat (one blank everywhere), not hierarchical.
- **Why:** spacing was whatever each parser emitted as `Block::Blank`; the
  two parsers disagreed and the header-gap logic already had to dedupe
  against them. The pass only *removes* redundant blanks (never inserts),
  so the header's leading-gap insertion is preserved and a blank flanked by
  content (a multi-panel figure's inter-panel separator) survives.
- **Key files:** `doc-model/src/layout.rs`.
- **Tests:** +`normalize_collapses_runs_and_trims_edges`; updated
  `header_dedupes_*` to lead with content. Goldens re-pinned (visual lines
  only): Attention 678→676, GPT-3 3138→3134, differential-algebra
  1679→1677, gaussian 1500→1494.

### The original six priorities
- **Commits:** `c0165d9`, `9d1d6cd` (plus mid-stream fixes).
- Ranked by reading-experience impact:
  1. **Reading measure** — prose wraps to a centred ~72-col column
     (`:set width=<n>`, persisted); tables/figures/display math break out
     to full width. The fixed horizontal page margin was removed (centring
     slack replaces it), which also fixed wide-table clipping.
  2. **Heading hierarchy** — section numbers consistent across both parser
     paths (`Block::Header.number`, reader-owned: ar5iv reads the LaTeXML
     `.ltx_tag`, Pandoc its counters); blank line above every heading;
     feeds the TOC + `:goto`. No decorative glyph (per preference).
  3. **Inline code + block quotes** — inline-code background pill; block
     quotes lead with a coloured `▌` rule bar.
  4. **Contextual preview pane** — with the pane open (`i`), follows the
     cursor: a citation shows its reference, a `\ref{fig:N}` shows that
     figure; otherwise the manually-browsed figure. Works on figure-less
     papers (gated on figures OR a bibliography).
  5. **Full-screen contents view** — `:contents` (j/k browse, Enter jumps,
     Esc closes); the `\` sidebar marks the current section with `▸` and
     brightens its ancestor breadcrumb.
  6. **Table column alignment** — per-column `l`/`c`/`r` honoured on both
     parser paths and in markdown; wide tables shrink-to-fit instead of
     clipping.
- **Mid-stream fixes:** table clipping (margin removal), double section
  numbering (ar5iv strip), `§`-glyph removal (per preference), help-overlay
  column rebalance, figure-less contextual pane, markdown alignment.
