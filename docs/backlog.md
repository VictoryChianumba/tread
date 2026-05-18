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

## Open — architecture (ADR-0004 four-seam plan)

| # | Seam | What it absorbs |
|---|---|---|
| Seam 1 | Mode state machine | ~30 `reader.mode = Mode::*` writes across `lib.rs`; clears `count_buf`/`cmd_buf`/`search_query`/`cmd_error` on transition |
| Seam 2 | Cursor / scroll | bare writes to `offset`, `cursor_y`, `cursor_x` route through a `cursor::*` seam |
| Seam 3 | Voice state | 8 fields collapse behind `voice::*` |
| Seam 4 | Bookmarks | Move from `HashMap<char, usize>` to block-byte addressing (like highlights); reflow survival as side benefit |

Pickup phrase: "continue ADR-0004" or "do Seam N". The ADR has the
proposed shape, validation sweep, and risk notes.

## Open — resilience (Tier 2 latent crashes)

| # | Item | Location |
|---|---|---|
| **C1** | Text slicing on non-UTF-8 char boundary | `tread/src/render.rs` highlight rendering |
| **C5** | `fetch_any` has no status check, redirect limit, content-length cap, or timeout | `tread/src/lib.rs` (or wherever fetch_any moved) |
| **C6** | Render array indexing race — bounds checked with `total_lines()` but indexes `visual_lines` directly | `tread/src/render.rs` |
| **C7** | Lock poisoning recovers wrong value — `unwrap_or_else(\|e\| e.into_inner())` doesn't clear poison | `tread/src/voice/playback.rs` |
| **D1** | Persistence files silently reset on parse error — corrupted JSON loses marks/highlights/progress with no log | `tread/src/{progress,bookmarks,highlights,config}.rs` |

D1 is the most user-impacting of these — a truncated write loses
state without warning. The other Cs are reachable but rare.

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

- When picking up: scan **Open — architecture** for plan-shaped work,
  **Open — resilience** for bug-fix work, **Open — parser** for
  feature work.
- When closing items: move the row from the open table to the
  **Recently resolved** table with the commit ref. If an item is
  large enough to need a design doc, open a new ADR and link it from
  the row.
- When revalidating: bump the "Last revalidation" date and re-spot-
  check each open item against current code. The May 2026 note in
  AUDIT.md is the prior example of how to do this.
