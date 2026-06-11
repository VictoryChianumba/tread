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
