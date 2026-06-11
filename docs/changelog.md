# tread — dev changelog

The canonical, repo-visible record of notable work: what changed, *why*,
the key files, test deltas, and the commit. Newest first.

This is the general log. A large multi-part workstream may keep its own
deep-dive doc (e.g. `docs/reading-ui-overhaul.md`) for per-item detail;
this file then carries a one-entry summary pointing at it.

**What counts as notable:** a shipped feature, an architecture/refactor
decision, a behaviour change a teammate would want explained, or anything
whose *why* isn't obvious from the diff. Trivial fixes (typos,
formatting, dependency bumps) don't need an entry.

**Convention:** add the entry in the *same commit* as the change, and
keep it self-contained — `todo.md` and `v2.md` are gitignored, so don't
rely on them for anyone reading a fresh checkout. Mirrors the
"docs in lockstep" rule in `CLAUDE.md`.

**Entry template:**

```
### <date> — <title>
- **Commit:** `<short-hash>`
- **What:** one or two sentences.
- **Why:** the structural reason / decision.
- **Key files:** the files a reader should open first.
- **Tests:** what was added/changed; pass counts or golden deltas.
```

---

### 2026-06-11 — ar5iv tables: render the caption (and wrap it)
- **Commit:** `03a210e`
- **What:** ar5iv table captions now appear above their table and wrap to
  the reading measure. Two parts: (1) `emit_table` emits the caption as
  `Block::Line("[Table N: …]")` — the exact shape the Pandoc path uses and
  that `placement::identify_groups` captures, so the caption travels with
  the table when it's lifted to its PDF-anchored position; (2) `layout`
  now wraps a bracketed `Block::Line` (a `[Table …]` / `[Figure …]`
  caption) to `prose_width` instead of leaving it as one over-long line.
- **Why:** the ar5iv path emitted the caption as a bold `StyledLine`, which
  placement didn't recognise — so when the table moved, the caption was
  stranded at the parse site and vanished from above the table. And even
  once emitted as a `Block::Line`, captions ran off the right edge because
  `Block::Line` was never wrapped. The wrap is scoped to the `[`-prefixed
  caption shape so PDF-extracted `Block::Line`s stay verbatim.
- **Key files:** `crates/arxiv-render/src/ar5iv_parse.rs::emit_table`
  (caption shape; `inline_spans_from` renders caption math once);
  `crates/doc-model/src/layout.rs` (`Block::Line` wrap). Also fixes the
  same latent wrap bug for Pandoc table captions and the figure-fallback
  caption (both bracketed `Block::Line`).
- **Tests:** +1 ar5iv (`table_caption_emits_as_placement_capturable_line`)
  and +1 doc-model (`caption_line_wraps_to_measure_but_plain_line_stays_verbatim`).
  arxiv-render 73 / doc-model 16 / tread 175 pass. The `#[ignore]`d layout
  goldens' visual-line counts shift (captions now wrap) — rebaseline when next run.

### 2026-06-11 — ar5iv figures: recover subfigure column labels
- **Commit:** `03a210e`
- **What:** `emit_figure` now recovers `header_rows` (the column labels above
  a labelled subfigure grid, e.g. Ava-256's "250 / 500 / 1K …"). Text-only
  `<tr>`s in a figure's layout table become header rows; leading label
  columns are trimmed so each label sits over its image column. Closes the
  `header_rows` half of the ADR-0001 figure-parity gap.
- **Why:** the ar5iv path left `header_rows` empty, so labelled subfigure
  grids lost their column labels (Pandoc already recovered them).
- **Key files:** `crates/arxiv-render/src/ar5iv_parse.rs`
  (`figure_table_grid`, `FigureGrid`). `column_gaps_after` stays empty —
  `@{\hspace}` gaps aren't reliably recoverable from LaTeXML HTML (still
  Pandoc-only).
- **Tests:** +1 (`labeled_subfigure_grid_recovers_header_row`). Verified
  against real 2605.04035 (Figures 3/5/6).

### 2026-06-11 — Images: keep figures in-pane inside a tmux split
- **Commit:** `862d2ef`
- **What:** inline (and preview) figures now render inside the reader's own
  tmux pane instead of leaking into an adjacent pane. Placement coordinates
  gain the pane's `#{pane_left}`/`#{pane_top}` offset when running under
  tmux.
- **Why:** the Kitty placement cursor-move (`\x1b[r;cH`) is forwarded to the
  host terminal *inside* the tmux passthrough envelope, which bypasses
  tmux's pane translation — so the host positions in absolute window
  coordinates while the reader only knows pane-local ones. In a non-origin
  pane (e.g. the right half of a vertical split) the image landed in the
  wrong pane. (This is the same defect that earlier read as "figures don't
  render in a narrow split" — they rendered, just off-pane.)
- **Key files:** `crates/kitty-graphics/src/transmit.rs` (`pane_offset` /
  `query_pane_offset` / `invalidate_pane_offset`, applied in both
  `BatchEmitter` placement paths); `crates/tread/src/runtime.rs` invalidates
  the cached offset on resize/focus change. The `tmux display-message`
  subprocess targets `$TMUX_PANE` (our own pane, not the focused one) and is
  cached — at most one call per resize. A single full-window pane yields
  offset `(0, 0)`, so non-split behaviour is unchanged.
- **Tests:** +1 `parse_pane_offset` unit test (left/right, stacked,
  malformed). kitty-graphics 37 pass. Verified in a real iTerm2+tmux
  left/right split.
- **Caveat:** the offset omits tmux's status-line height, so a *vertical*
  (stacked) split can still be off by the status rows; left/right
  (`pane_top == 0`) is exact.

### 2026-06-11 — ar5iv figures: recover stacked multi-panel layout
- **Commit:** `dc2bbf3`
- **What:** `emit_figure` now recovers a figure's 2D image grid
  (`rows[stack][side-by-side]`) instead of flattening every multi-panel
  figure into one side-by-side row. New `figure_image_rows` reads the two
  layouts LaTeXML emits: (1) **flexbox** (`ltx_flex_figure`) split into
  stack rows by `ltx_flex_break` divs — LaTeXML's rendering of the source
  `\\` row break (Attention's Figures 4/5); (2) **table grid** — one row
  per image-bearing `<tr>` (GPT-3's panel grids). Single-image and
  no-break subfigures stay a single side-by-side row.
- **Why:** the second half of the ar5iv↔pandoc parity gap. The ar5iv path
  flattened all subfigures into one row, so figures the author stacked
  rendered crammed side-by-side. The `ltx_flex_break` div is the same `\\`
  row break the pandoc fallback keyed on — ar5iv just encodes it in HTML.
- **Key files:** `crates/arxiv-render/src/ar5iv_parse.rs`
  (`figure_image_rows`, `ar5iv_image_item`). Renderer/model unchanged —
  `Block::Figure.rows` already carried the 2D grid (ADR-0001).
- **Tests:** +4 ar5iv figure tests (flex-break stacks; flex-no-break stays
  side-by-side; table-grid stacks; existing no-grid flatten). Layout
  verified against the real 1706.03762 (flex) and 2005.14165 (table-grid)
  HTML. arxiv-render 71 / doc-model 15 / tread 175 pass.
- **Caveat:** `column_gaps_after` / `header_rows` (ADR-0001's subfigure
  column labels) stay empty on the ar5iv path — still pandoc-only.

### 2026-06-11 — ar5iv tables: recover booktabs rules, vertical rules, rowspan
- **Commit:** `e1db8ae`
- **What:** the ar5iv (primary) parser now emits real booktabs tables.
  `emit_table` recovers (a) horizontal rules from LaTeXML cell border
  classes (`ltx_border_t{t}`/`_b{b}`) — splitting rows into `Matrix`
  segments separated by `Block::Rule` so mid/bottom rules render; (b)
  vertical rules from `ltx_border_l`/`_r` into `Matrix.vertical_rules`; and
  (c) `rowspan`, by reserving the covered column in later rows with an empty
  placeholder so multi-row header sub-labels stay under their parent group.
- **Why:** since ar5iv became the primary parser (`cef6df5`, 2026-05-19),
  `emit_table` flattened every `<thead>`/`<tbody>` table into one
  rule-less `Matrix` with empty `vertical_rules`, so the renderer could
  only draw its synthetic top rule — no midrule, bottomrule, `│`, and (with
  unhandled `rowspan`) misaligned stacked headers. The pandoc fallback
  already did all three; this closes the table half of that parity gap. The
  renderer (`doc-model/table.rs`) was already correct — it just never
  received the structure.
- **Key files:** `crates/arxiv-render/src/ar5iv_parse.rs::emit_table`. No
  change to `doc-model/table.rs`. Cost is parse-only (a few extra DOM class
  reads on already-fetched HTML) — no network/byte increase.
- **Tests:** +3 ar5iv unit tests (booktabs mid/bottom/vertical rules +
  alignment; borderless single-Matrix; rowspan offset). Verified the
  rowspan + rule logic against the real 1706.03762 HTML (Tables 2 & 3).
  arxiv-render 68 / doc-model 15 / tread 175 pass.
- **Caveat:** `\cmidrule` partial rules promote to full-width (border
  detected per-row, not per-column span) — fine for the common case;
  refine if needed.

### 2026-06-11 — Figures: fix caption double-indent under the reading measure
- **Commit:** `140b531`
- **What:** figure captions no longer run off the right edge when the
  reading measure is narrower than the screen. Captions are now centered
  within `prose_width` (the measure) instead of `terminal_width` (the full
  width).
- **Why:** a caption is a `Prose` visual line, so `emit_caption` centered
  it within the full `terminal_width`, and then the render path
  (`content.rs::draw_content`) *also* prepended the measure-centering
  `prose_pad` to every `Prose` line — double-indenting the caption off the
  right edge. Centering within `prose_width` makes the two pads telescope
  — `(W−P)/2 + (P−L)/2 = (W−L)/2` — so the caption lands centered in the
  full width, aligned under the (full-width) image. Latent since the
  reading-measure overhaul; surfaced when the figure-flatten fix made
  images opaque and drew the eye to the figure region.
- **Key files:** `crates/doc-model/src/figure.rs` (`emit_figure_lines` /
  `emit_caption` now take + use `prose_width`), one call site in
  `layout.rs`. With the measure off (`prose_width == terminal_width`)
  behaviour is unchanged.
- **Tests:** +1 doc-model unit test pinning caption centering to
  `prose_width`; doc-model 15 pass, tread 175 pass. Note: captions now
  *wrap* at `prose_width`, so the `#[ignore]`d figure-heavy layout goldens
  will need their visual-line counts rebaselined.

### 2026-06-09 — Figures: flatten transparency onto a padded white backdrop
- **Commit:** `eb18d59`
- **What:** every figure carrying an alpha channel is now alpha-composited
  onto an opaque white background — inset inside a proportional white
  margin — at resolve time, and the alpha channel is dropped. Fully-opaque
  sources keep their no-decode fast path via a cheap PNG colour-type gate.
- **Why:** ar5iv figure assets arrive as type-6 RGBA (some type-4
  gray+alpha) PNGs with transparent backdrops; on a dark terminal the
  background bled through and swallowed dark-ink labels and arrows. arXiv
  figures are authored for white paper, so compositing onto white restores
  legibility on every theme, and dropping alpha stops the terminal
  re-exposing the dark bg on a later placement. The margin keeps content
  drawn flush to the source canvas off the box edge. PDF figures were never
  affected — `pdftoppm` already rasterises onto white.
- **Key files:** `crates/tread/src/images/png.rs` — `flatten_alpha_over`
  (composite + margin), `png_may_have_alpha` / `png_has_trns` (the IHDR
  colour-type gate), wired into `normalize_png_for_terminal_with_limit`.
  The cross-session cache suffix is versioned `.norm` → `.norm3` so stale
  pre-fix artifacts (transparent, or flattened-but-unpadded) aren't
  re-served.
- **Tests:** +5 png unit tests (colour-type gate, composite maths, opaque
  no-op, padded margin, and an end-to-end `resolve_png` flatten assertion);
  full suite 175 pass.

### 2026-06-09 — Preview pane: drop the box for a single inset divider
- **Commit:** `78ed951`
- **What:** the figure/citation preview pane no longer draws a three-sided
  box (a `LEFT|TOP|BOTTOM` border with the label in the top edge). It now
  renders a single thin vertical rule down its left edge, inset two rows
  clear of the screen top and bottom, with the `Figure n/m` / `[key]`
  label as a plain dim header line. The citation pane drops its background
  fill so it reads as the same surface as the figure pane.
- **Why:** design language — a minimal seam reads quieter than a full
  enclosure, and insetting the rule keeps it from butting the screen
  edges. Detaching the title from a border and dropping the citation bg
  make the two pane variants read as one continuous surface.
- **Key files:** `crates/tread/src/render/preview.rs` (`draw_divider`,
  `draw_preview_title`). Figure/caption placement is untouched —
  `preview_image_area`'s fixed margin never depended on the border.
- **Tests:** none (pure rendering); build clean, full suite passes.

### 2026-06-09 — Follow host rename trench → one-research (shared config dir)
- **Commit:** `7c31b40`
- **What:** the embedded reader's persistence (progress, bookmarks,
  highlights, theme override) and its host-theme read now point at
  `~/.config/one-research/` instead of `~/.config/trench/`. `:set
  theme=one-research` is the new canonical "follow host theme" token;
  `:set theme=trench` stays as a back-compat alias.
- **Why:** the host TUI (formerly `trench`) was renamed to `one-research`
  and moved its config dir accordingly. The reader hardcoded the old path
  in each persistence module, so after the host migrated `config.json`
  away, reader data split-brained into the stale dir and theme-following
  silently fell back to the dark default.
- **Key files:** `config.rs` (path + `host_theme_id`), `progress.rs`,
  `highlights.rs`, `state/bookmarks.rs`, `commands.rs` (`set_theme` token).
- **Tests:** no new tests; existing suite unchanged (175 pass). Path
  constants aren't unit-covered (they hit the real config dir).

### 2026-05-25 — Record-keeping: dev changelog + commit-time guardrail
- **What:** added this changelog, a non-blocking `.githooks/pre-commit`
  reminder, and a CLAUDE.md workflow rule, so notable work is logged in a
  repo-visible place in-commit.
- **Why:** the deep implementation narrative previously lived only in
  agent working memory (not in the repo); `todo.md` / `v2.md` are
  gitignored. Teammates reading a checkout had only commit messages.
- **Key files:** `docs/changelog.md`, `.githooks/pre-commit`, `CLAUDE.md`.
  Enable the hook on a fresh clone with `git config core.hooksPath .githooks`.
- **Tests:** none (docs + tooling).

### Reading-UI overhaul (2026-05) — typography, panes, tables
- **Commits:** `c0165d9`, `9d1d6cd` (six priorities + fixes); `1015adc`
  (paragraph rhythm); `d5523f7` (preview-pane ratio); `dc5e084` (math
  wrapping).
- **What:** a multi-part pass on the reading experience — reading measure,
  heading hierarchy, inline-code/quote styling, contextual preview pane,
  full-screen contents view, table column alignment, paragraph-rhythm
  normalization, adjustable preview-pane ratio, and display-math wrapping.
- **Why / per-item detail:** see the deep-dive at
  [`docs/reading-ui-overhaul.md`](reading-ui-overhaul.md), which carries
  the what/why/files/test-deltas for each item.
- **Remaining:** theme semantic layer, reading-comfort affordances, TOC
  collapse/resize.
