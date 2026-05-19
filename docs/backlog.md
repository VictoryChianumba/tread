# Backlog — open audit items

Current-state pointer for unresolved findings. Companion to:

- [`AUDIT.md`](../AUDIT.md) — May 2026 audit, historical snapshot.
  Item numbers (A1, B3, C2, …) below reference that doc.
- [`docs/adr/`](adr/) — settled decisions. Each ADR has its own
  `Open follow-up` section for smaller loose ends specific to that
  decision.

Last revalidation: **2026-05-19**.

## Recently resolved

| Audit item | How | Commit / ADR |
|---|---|---|
| **#5** No architecture memory | CONTEXT.md + ADR-0001/0002/0003 | `9118293` |
| **#2** images.rs monolith | Split behind `ImageState` into 4 submodules | `6332f7f` |
| **#3** Pandoc figure extraction sprawl | Lifted to `pandoc_parse/figure.rs` behind one entry point | `42cb3c7` |
| **C2** Stale search_matches after resize | `rebuild_layout` now owns invalidation + re-anchor | `d1a23f1` |
| **#1** Reader public surface (planning only) | ADR-0004 lays out 4 seams in priority order | `7db3ece` |
| **B1** Mixed display math (Pandoc path) | Already fixed before this session — confirmed in May 2026 revalidation | — |
| **B3** Inline `thebibliography` extraction | Partial — `\bibliography{file.bib}` external still open (B3a) | `a7141f0`, `d2bf341` |
| **B3** Latin-1 source fetch | UTF-8 lossy conversion accepts ISO-8859 sources | `e5b042b` |
| **#1** Reader public surface — Seam 1 (mode state machine) | `state/mode.rs` owns every mode transition; `mode` field private | `5f6312c` |
| **#1** Reader public surface — Seam 2 (cursor/scroll) | `state/cursor.rs` owns jumps/centering; `offset`/`cursor_y`/`cursor_x`/`desired_column` private | `984b86c` |
| **#1** Reader public surface — Seam 3 (voice) | `voice_control.rs` moved into `state/`; 10 voice fields private | `db741fe` |
| **#1** Reader public surface — Seam 4 (bookmarks) | `state/bookmarks.rs` owns marks; storage moved to block-byte addressing; reflow-survival regression test | `945eb3b` |
| **D1** Persistence files silently reset on parse error | New `persist::load_json` helper renames corrupt files to `<name>.corrupt-<unix-ts>` before returning Default; eprintln warning. Applied to progress / bookmarks / highlights / config. | `1e7f690` |
| **C1** Text slicing on non-UTF-8 char boundary | `clamp_to_char_boundary` snaps highlight / search range endpoints before slicing; pre-fix the affected segment was silently dropped. Three regression tests cover multi-byte cases. | `d1db8e1` |
| **C5** `fetch_any` had no timeout / redirect cap / body cap | Built a `reqwest::blocking::Client` with 30s timeout, 10-redirect limit; capped body at 64 MB via `Read::take`. | `d1db8e1` |
| **C6** Render array indexing race | `&reader.visual_lines[vl_idx]` switched to `.get(vl_idx)` with a blank-line fallback — defends against future refactors of `total_lines` decoupling from `visual_lines.len()`. | `d1db8e1` |
| **C7** Lock poisoning recovery didn't clear poison | New `MutexExt::lock_clearing_poison` calls `Mutex::clear_poison()` after recovering the guard; bulk-migrated 21 sites in `voice/playback.rs`. | `d1db8e1` |
| **ADR-0002 follow-up** Preview geometry invalidation discipline | `Reader::rebuild_layout` now clears `last_geometry` unconditionally — auto-invalidates on every resize / reload / text-only / TOC toggle. | `d1e879d` |
| **ar5iv as primary parser** | `fetch_ar5iv` + `ar5iv_parse::to_blocks` is the default path; Pandoc demoted to the fallback when ar5iv hasn't processed the paper. | `cef6df5` |
| **ADR-0001 follow-up** Hand-rolled parser doesn't emit `Block::Figure` | Moot — the hand-rolled parser was removed entirely.  Pandoc + ar5iv are the only parsers now. | `0a60ea7` |
| **#4** doc-model lib.rs mixed concerns | Split into 5 modules (`lib`/`layout`/`wrap`/`figure`/`table`); 0→8 tests | ADR-0005 |
| **#4** render.rs mixed regions | Split into 6 region modules under `render/` | ADR-0006 |
| **#4** pandoc_parse.rs mixed concerns | Split into 6 sibling modules under `pandoc_parse/`; +1 inline test mod migrated | ADR-0007 |
| **#1** Reader public surface — Seam 5 (read-mostly + popup + figure-preview triple) | 8 fields closed; popup gains atomic `open_popup` constructor | ADR-0008 |
| **#1** Reader public surface — Seam 6 (LayoutCache projections + count_buf) | 5 fields closed; six-seam tally 47→17 pub fields | ADR-0009 |
| **Smoke-claim mechanization** | Attention parse+layout golden test pins block / visual-line counts + Table 3/4 vrules; `#[ignore]`-gated. | `cbbd035` |
| **ADR-0007 follow-up** Table 3 vrules `[1, 7]` vs `[1, 10]` divergence | Misleading inline-comment narrative, not a parser bug.  Attention's actual Table 3 spec is `c|ccccccccc|ccc` (9 inner c's, 13 cols → [1, 10]); the `c|cccccc|ccc → [1, 7]` synthetic in `spec::tests::two_rules` is a minimal pattern that never matched the live paper.  New `attention_table_3_spec` unit test pins the real shape; golden test comment + comments rewritten to stop confusing the two. | `57cc4a8` |
| **B3a** | `\bibliography{file.bib}` external BibTeX — the in-tarball case (e.g. 2605.04035, which ships its `.bib`) is handled end-to-end: `bibitems::extract_bibitems_ordered` reads `.bib`/`.bbl` files via `bibtex::extract_bibtex_entries`, `try_pandoc` auto-appends a References section when `bibitems_emitted == false && has_bibitems == true`. New `gaussian_head_parse_and_layout_golden` pins this. **Residual limitation** (un-solvable on Pandoc fallback alone): papers referencing `\bibliography{X}` without shipping the `.bib` (e.g. Attention referencing NIPS2017.bib) — production users hit the ar5iv primary path for these, which carries the rendered bibliography. | _this commit_ |

The resilience cluster and the actionable ADR follow-ups are closed.
What's left below is explicitly deferred — each item has a blocker
that this session can't satisfy.  Future picks should re-read the
blocker note before reopening, in case it's resolved (e.g. a real
test paper for B5/B6, or a trench-repo audit pass for A*).

## Deferred — parser / rendering

| # | Item | Blocker |
|---|---|---|
| **B5** | Tables don't highlight best-result cells | Feature, not a bug — no spec; need a paper with a clearly-marked best-result table to anchor the design |
| **B9** | ar5iv parser emits 0 `Block::Figure` — primary-path users see only the centered bold caption, no inline pixel image.  `ar5iv_parse::emit_figure_caption` is explicit prototype scaffolding ("don't try to fetch image assets").  Concrete evidence: `parity_probe` on 1706.03762 shows ar5iv emits 0 figures vs pandoc's 5. | Needs an ar5iv asset-fetch stage (figures hosted at `ar5iv.labs.arxiv.org/html/<id>/...` or fetched from the tarball even on the ar5iv path).  Substantial scope (~few hundred lines + image-dim probing + path resolution); pandoc fallback path handles figures correctly today, so production papers without ar5iv coverage are fine.  HIGH severity for adoption — ~95% of papers go through the primary path. |
| **B10** | ar5iv parser doesn't emit `Block::ListItem` — bulleted / numbered lists render as flowed prose paragraphs.  `parity_probe` on 1706.03762: ar5iv 0 vs pandoc 3 ListItem blocks. | LaTeXML emits `<ul>` / `<ol>` / `<li>` with `ltx_itemize` / `ltx_enumerate` classes; needs a walker pass in `ar5iv_parse::walk_section`'s child loop.  MEDIUM severity — lists are uncommon in conference papers but appear in survey papers and supplementary material. |
| **B6** | Some tables structurally wrong when `take_matching_spec` falls back to default | Need a paper that triggers the fallback so the corruption is reproducible; audit didn't capture an example |
| **B8** | Image rendering edge cases on standalone tread (image-emit corruption / AAAA walls) | Audit explicitly flags "needs reclassification" — the image subsystem changed materially in `6332f7f`; re-audit before fixing |
| **F1** | `state::tests::preview_toggle_preserves_current_source_position_after_reflow` and `resize_preserves_current_source_position_after_reflow` flake under parallel `cargo test` (both pass under `--test-threads=1`) | Likely a shared file-system or process-wide side effect (persistence/config layer); needs an isolated reproduction before changing the test or the code |

## Deferred — trench integration (A-series)

Not revalidated since May 2026. A separate audit pass in the
`trench` repo is required before any of these can be closed —
the fixes live in `trench`'s tread-binding code, not in `tread`
itself.

| # | Item |
|---|---|
| **A1** | `tread::after_draw` never called from trench |
| **A2** | `tread::clear_images` never called from trench (moot until A1) |
| **A3** | `Reader::init` hardcodes width=80, height=24 in trench callsites |
| **A4** | `theme_for_tread()` hardcodes dark-theme `bg_highlight` + `link_fg` |
| **A5** | Popup reader hardcodes `kitty_supported = false` |

## Deferred — ADR-level follow-ups

- **ADR-0003:** Two follow-ups remain. Investigating iTerm2's
  native inline-image protocol vs Kitty emulation needs a benchmark
  setup; capability re-detection on focus regained needs a real
  tmux-detach scenario to validate against the UX cost (extra query
  escape per focus event). **Blocker:** both need terminal-specific
  benchmarking environments.
<!-- ADR-0007 follow-up resolved 2026-05-19 — see Recently Resolved row above. -->


## How to use this document

- When picking up: scan **Deferred — parser / rendering** for feature
  work, **Deferred — trench integration** for cross-repo work.  The
  architecture refactor (ADR-0004 four-seam plan) and the resilience
  cluster (D1, C1, C5, C6, C7) are closed.
- When closing items: move the row from the deferred table to the
  **Recently resolved** table with the commit ref. If an item is
  large enough to need a design doc, open a new ADR and link it from
  the row.
- When revalidating: bump the "Last revalidation" date and re-spot-
  check each deferred item against current code AND the stated
  blocker — a blocker may have been resolved out-of-band (e.g. a
  new test paper surfaced for B5).
