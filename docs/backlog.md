# Backlog — open audit items

Current-state pointer for unresolved findings. Companion to:

- [`AUDIT.md`](../AUDIT.md) — May 2026 audit, historical snapshot.
  Item numbers (A1, B3, C2, …) below reference that doc.
- [`docs/adr/`](adr/) — settled decisions. Each ADR has its own
  `Open follow-up` section for smaller loose ends specific to that
  decision.

Last revalidation: **2026-05-21**.

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
| **Pandoc `\newcolumntype`** | `strip_newcolumntype` pass in `pandoc_parse/preprocess.rs` drops parameterised column-type preamble definitions (`\newcolumntype{X}[1]{...#1...}`) that made Pandoc abort with `unexpected #1`. Restores the Pandoc fallback for a recurring failure class — 1512.03385, 1406.2661, 2602.06006 all went 0 → 521 / 229 / 2138 blocks. 4 unit tests. | _this session_ |
| **B10** | ar5iv parser now emits `Block::ListItem`. `emit_list`/`emit_list_item` (`ar5iv_parse.rs`) walk `<li class="ltx_item">` under itemize/enumerate/description containers, reusing LaTeXML's `<span class="ltx_tag_item">` marker verbatim ("• ", "1. ", and custom enumerate labels like "(a) " for free) and recursing into nested sub-lists (nested as a sibling of the item `<p>` inside its `ltx_para` div) at `depth + 1`. 3 unit tests. Parity vs Pandoc reference: Attention 3=3, 2602.06006 49=49; survey 2303.18223 gains 40 items purely on the primary path (Pandoc can't parse it). Closes the last ar5iv parity gap. | _this session_ |
| **B9** | ar5iv parser now emits `Block::Figure`. `emit_figure` (`ar5iv_parse.rs`) pulls the `<img class="ltx_graphics">` src(s) + caption and emits tarball-relative paths (`assets/xN.png`); subfigures flatten into one row; `number_figures` assigns sequential `figure_id`/`kitty_id`. `fetch_ar5iv_assets` (`fetch.rs`) downloads the referenced images to `~/.cache/tread/ar5iv-assets/<id>/` (skip-if-cached, zip-slip guarded, per-file best-effort); `paper.rs` wires that dir as `asset_dir` on graphics terminals so the shared `absolutize_image_paths` resolves+dims them and `degrade_images_to_captions` covers non-kitty terminals. 4 parser unit tests + 2 `#[ignore]` network tests (download + full `fetch_paper` end-to-end on ResNet). Resolves the universal 0-figure gap on the ~95% ar5iv path. | _this session_ |
| **ar5iv stub fallback** | `paper.rs` fallback trigger changed from `blocks.is_empty()` to `ar5iv_is_stub` (content-bearing block count < 3). ar5iv serves an "Untitled Document" stub for papers it never processed (e.g. 1412.6980 → 1 title line + blank); the old guard accepted it and showed a near-blank reader. Now falls back to Pandoc. | _this session_ |
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
