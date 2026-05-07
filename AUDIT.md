# Tread / Trench Diagnostic Audit

Compiled 2026-05-07 from three parallel codebase audits (integration verification / latent crash audit / rendering issues investigation). 27 distinct items across four categories. Tier 1 (six user-visible items) is the active fix plan in `~/.claude/plans/humming-cuddling-wadler.md`. Tier 2 and Tier 3 below are deferred backlog — pick from these when something specific bites.

Severities:
- **CRASH** — panic / abort under reachable input
- **DEGRADED** — feature visibly broken or absent
- **SILENT-CORRUPT** — wrong output without error path
- **COSMETIC** — visual polish

---

## Test paper context

- **`1706.03762`** (Attention Is All You Need) — works correctly today. Uses inline `\bibitem{}`, blank lines around display math, simple tabular structure. Regression baseline.
- **`2605.04035`** (3D Gaussian Head Reconstruction) — exhibits all six Tier 1 bugs. Standalone fetch parses to 386 blocks, 19 images, **0 bibitems**. End-to-end smoke target.

The Attention paper avoids the Tier 1 bugs because it: uses inline `\bibitem`, has blank lines around display equations, uses simple tabular structure. 2605.04035 hits the Pandoc-citeproc bibliography path, has many equations inside text-Paragraphs, and uses richer table layouts.

---

## A. Embedding integration gaps (visible in trench right now)

| # | Issue | Location | Severity |
|---|---|---|---|
| A1 | `tread::after_draw` never called from trench | `trench/trench/src/ui/layout.rs` (zero call sites for `after_draw`) | DEGRADED |
| A2 | `tread::clear_images` never called from trench | trench-wide (zero call sites) | LATENT (moot until A1) |
| A3 | `Reader::init` hardcodes width=80, height=24 | `trench/src/main.rs:1286`, `:1355` | DEGRADED |
| A4 | `theme_for_tread()` hardcodes `bg_highlight` + `link_fg` to dark-theme RGB | `trench/src/app.rs:756-757` | COSMETIC |
| A5 | Popup reader hardcodes `kitty_supported = false` | `trench/src/main.rs:1355` | DEGRADED |
| A6 | (verified clean — voice tick is wired correctly) | `trench/src/main.rs:1193-1206` | OK |

**Symptoms:** arxiv figures are blank rectangles in trench (A1). Reopened paper lands a few lines off saved position (A3). Light-theme citation underlines and highlight tints use dark-theme defaults (A4). Popup reader never shows pixel figures even on Kitty terminals (A5).

---

## B. Parser gaps (rendering bugs visible in standalone too)

| # | Issue | Location | Severity |
|---|---|---|---|
| B1 | Display equations rendered inline with prose (mixed Para case) | `arxiv-render/src/pandoc_parse.rs:602` (`para_to_block` only emits `Block::DisplayMath` when `meaningful.len() == 1`) | DEGRADED |
| B2 | Paragraph break visibility weak | `pandoc_parse.rs:315` + `doc-model/src/lib.rs:455-464` (Block::Blank emitted, render path may collapse) | DEGRADED |
| B3 | References section empty in TOC (`bibitems == 0`) | `arxiv-render/src/parse.rs:152-196` (`extract_bibitems` only finds inline `\bibitem`; misses BibLaTeX, `\bibliography{file.bib}`, Pandoc-citeproc Div blocks) | DEGRADED |
| B4 | Authors truncated / first-author-only | `arxiv-render/src/pandoc_parse.rs:160-234` (extract expects Note/Span markers between LineBreaks) + `tread/src/render.rs:91` (header truncation) | COSMETIC |
| B5 | Tables don't highlight best-result cells | `pandoc_parse.rs:745-851` + `doc-model/src/lib.rs:90-93` (`Block::Matrix` is `Vec<Vec<(String, usize)>>` — no per-cell style) | DEGRADED |
| B6 | Some tables structurally wrong | `pandoc_parse.rs:778-783` (`take_matching_spec` falls back to default no-rules spec when col-count diverges) | DEGRADED |
| B7 | Captions absent on some tables | `pandoc_parse.rs:987-1005` (`extract_caption_text` hardcoded to Pandoc 3.x Caption AST shape) | COSMETIC |
| B8 | Pictures don't render even on standalone | unknown — needs interactive investigation. 19 images parsed for 2605.04035, none render. Possibly: `arxiv-render/src/lib.rs::absolutize_image_paths` PROBE_EXTS doesn't match this paper's image extensions, OR `kitty-graphics/src/pdf.rs` pdftoppm fails silently. | DEGRADED |

---

## C. Latent crash / panic risks (waiting to bite)

| # | Issue | Location | Severity | Trigger |
|---|---|---|---|---|
| C1 | Text slicing on non-UTF-8 char boundary | `tread/src/render.rs:405,468` | CRASH | Highlight byte offsets land mid-codepoint on multibyte text |
| C2 | `search_matches` not invalidated on resize | `tread/src/state.rs:417-433` | CRASH | search → resize → `n` → out-of-bounds index in `jump_to_match` (state.rs:542) |
| C3 | List-stack underflow on malformed input | `tread/src/markdown.rs:244` and `tread/src/html.rs:227,237` | DEGRADED | Parser emits `End(List)` without matching `Start` |
| C4 | `colspan` integer overflow | `tread/src/html.rs:386-391` (no upper bound) | DEGRADED | `colspan="999999999"` → sparse row → matrix renderer math fails |
| C5 | HTTP fetch missing protections | `tread/src/lib.rs:406-419` (`fetch_any`) | CRASH/DEGRADED | No status check, no redirect limit, no Content-Length cap, no timeout |
| C6 | Render array indexing race | `tread/src/render.rs:122` (bounds uses `total_lines()` but indexes `visual_lines` directly) | CRASH | rare, race with concurrent offset mutation |
| C7 | Lock poisoning recovers wrong value | `tread/src/voice/playback.rs:91,107,123,128` (`unwrap_or_else(\|e\| e.into_inner())` recovers data but doesn't clear poison) | DEGRADED | Panic in voice provider while holding any lock |
| C8 | Code-block empty-trailing-newline brittle | `tread/src/markdown.rs:231-237` | DEGRADED | Code fence containing only whitespace |

---

## D. Silent data loss

| # | Issue | Location | Trigger |
|---|---|---|---|
| D1 | Persistence files silently reset on parse error | `tread/src/progress.rs:23`, `bookmarks.rs:45`, `highlights.rs:80`, `config.rs:79` (`unwrap_or_default()`) | Truncated/corrupted JSON → user loses all marks/highlights/progress with no log |
| D2 | `yank_selection` returns "" on out-of-bounds | `tread/src/lib.rs:1316-1317` | Race / unusual offset state → silent empty clipboard entry |
| D3 | `fetch_any` URL with `?query` and no extension treated as HTML | `tread/src/lib.rs:398` (`split('?').next().unwrap_or(url)`) | URLs like `https://example.com/?download=paper.pdf` misroute |

---

## Tier classification (summary)

**Tier 1** (active fix plan): A1, A2, A3, B1, B2, B3, B8 — six items the user feels right now.

**Tier 2** (latent crash defenses): C1, C2, C5, C6, C7, D1 — pre-release resilience pass.

**Tier 3** (cosmetic / edge): A4, A5, B4, B5, B6, B7, C3, C4, C8, D2, D3 — pick from when specific symptom appears.

---

## Provenance

Three Explore agents (May 2026 session):
1. Codebase bug/crash audit (10 items: C1-C8 + D1-D2)
2. Trench/tread integration verification (8 items: A1-A6 + B3 root cause + B1 root cause)
3. Earlier rendering issues investigation (9 items: B1-B8 root causes)

Plus one bash run: `./target/release/tread 2605.04035 → exit 0, 386 blocks, 19 images, 0 bibitems` — confirmed B3 reproduces and gave concrete evidence.

Cross-referenced and deduplicated into the 27 items above.
