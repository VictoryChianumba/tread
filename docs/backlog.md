# Backlog — open audit items

Current-state pointer for unresolved findings. Companion to:

- [`AUDIT.md`](../AUDIT.md) — May 2026 audit, historical snapshot.
  Item numbers (A1, B3, C2, …) below reference that doc.
- [`docs/adr/`](adr/) — settled decisions. Each ADR has its own
  `Open follow-up` section for smaller loose ends specific to that
  decision.

Last revalidation: **2026-05-18**.

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
| **C1** Text slicing on non-UTF-8 char boundary | `clamp_to_char_boundary` snaps highlight / search range endpoints before slicing; pre-fix the affected segment was silently dropped. Three regression tests cover multi-byte cases. | _this commit_ |
| **C5** `fetch_any` had no timeout / redirect cap / body cap | Built a `reqwest::blocking::Client` with 30s timeout, 10-redirect limit; capped body at 64 MB via `Read::take`. | _this commit_ |
| **C6** Render array indexing race | `&reader.visual_lines[vl_idx]` switched to `.get(vl_idx)` with a blank-line fallback — defends against future refactors of `total_lines` decoupling from `visual_lines.len()`. | _this commit_ |
| **C7** Lock poisoning recovery didn't clear poison | New `MutexExt::lock_clearing_poison` calls `Mutex::clear_poison()` after recovering the guard; bulk-migrated 21 sites in `voice/playback.rs`. | _this commit_ |

The resilience cluster is closed.  Remaining open items are
parser/rendering (B*) and trench integration (A*); those need
papers / a separate repo audit respectively.

## Open — parser / rendering

| # | Item | Location |
|---|---|---|
| **B3a** | `\bibliography{file.bib}` external BibTeX — Pandoc doesn't expand without `--citeproc`; needs a BibTeX file reader stage | `arxiv-render/src/fetch.rs` + new stage |
| **B5** | Tables don't highlight best-result cells | `pandoc_parse.rs` + `doc-model::Block::Matrix` cell shape |
| **B6** | Some tables structurally wrong when `take_matching_spec` falls back to default | `pandoc_parse::take_matching_spec` |
| **B8** | Image rendering edge cases on standalone tread (image-emit corruption / AAAA walls) — needs reclassification per May 2026 note; image subsystem changed substantially since the audit | `tread/src/images/` |

## Open — trench integration (A-series)

Not revalidated since May 2026. A separate audit pass in the
`trench` repo is needed before scheduling.

| # | Item |
|---|---|
| **A1** | `tread::after_draw` never called from trench |
| **A2** | `tread::clear_images` never called from trench (moot until A1) |
| **A3** | `Reader::init` hardcodes width=80, height=24 in trench callsites |
| **A4** | `theme_for_tread()` hardcodes dark-theme `bg_highlight` + `link_fg` |
| **A5** | Popup reader hardcodes `kitty_supported = false` |

## Open — ADR-level follow-ups

Each ADR's `Open follow-up` section lists smaller loose ends. The
notable ones:

- **ADR-0001:** Pandoc figure extraction *was* deepened this session
  into `pandoc_parse/figure.rs`. ✓ Remaining: the fallback hand-rolled
  parser doesn't emit `column_gaps_after` or `header_rows`; figures
  degrade to a flat grid when Pandoc is absent.
- **ADR-0002:** `FigurePreviewState::last_geometry` invalidation
  relies on call-site discipline; a stale geometry after a layout
  rebuild could mis-place once. User-visible damage is one frame at
  worst; seam is worth tightening eventually.
- **ADR-0003:** Split private implementation modules behind
  `ImageState` *was* done this session (`inline`, `preview`,
  `worker`, `png`). ✓ Remaining: capability re-detection on focus
  regained (detached/reattached tmux); investigation of iTerm2's
  native inline-image protocol vs its Kitty emulation for our
  payload sizes.

## How to use this document

- When picking up: scan **Open — resilience** for bug-fix work,
  **Open — parser** for feature work, **Open — trench integration**
  for cross-repo work.  (Architecture refactor is closed: ADR-0004's
  four-seam plan landed in commits `5f6312c` / `984b86c` / `db741fe`
  / `945eb3b`.)
- When closing items: move the row from the open table to the
  **Recently resolved** table with the commit ref. If an item is
  large enough to need a design doc, open a new ADR and link it from
  the row.
- When revalidating: bump the "Last revalidation" date and re-spot-
  check each open item against current code. The May 2026 note in
  AUDIT.md is the prior example of how to do this.
