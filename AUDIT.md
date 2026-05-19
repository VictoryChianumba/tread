# Tread / Trench Diagnostic Audit

Compiled 2026-05-07 from three parallel codebase audits (integration verification / latent crash audit / rendering issues investigation). 27 distinct items across four categories. Tier 1 (six user-visible items) is the active fix plan in `~/.claude/plans/humming-cuddling-wadler.md`. Tier 2 and Tier 3 below are deferred backlog — pick from these when something specific bites.

2026-05-16 revalidation note: this audit is now historical context, not the
current source of truth. The standalone reader architecture has since gained a
preview pane, `ReaderRuntime`, `LayoutCache`, `FigureIndex`, first-class
`FigurePreviewState`, and split inline/preview image invalidation. Re-audit
before using the table below as an implementation queue.

2026-05-19 file-path note: every `arxiv-render/src/pandoc_parse.rs:NNN`
reference below points at the pre-split layout. That file is now
`pandoc_parse/mod.rs` plus five siblings (`figure.rs`, `inline.rs`,
`table.rs`, `preprocess.rs`, `spec.rs`) per ADR-0007. Likewise
`arxiv-render/src/parse.rs` (the hand-rolled fallback parser) was
removed in commit `0a60ea7` — references to it are historical.

Current spot-checks:

- B1 mixed display math appears resolved for the Pandoc path.
- Inline `thebibliography` extraction is partially handled; external
  `\bibliography{file.bib}` still appears unresolved.
- B8 should be reclassified rather than treated as globally open; image
  rendering changed substantially with the preview pane and tmux/iTerm2 work.
- D1 silent reset on corrupted persistence still appears open.
- C2 stale search matches after reflow/resize still appears open.
- C5 fetch timeout/body-size protection still appears open.
- A-series trench integration findings were not revalidated from this repo.

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
| B3a | `\bibliography{file.bib}` (external BibTeX) leaves bib content out of the AST entirely — Pandoc doesn't expand without `--citeproc`. `synthesize_bibliography` fallback (S2) doesn't help; needs a BibTeX file reader stage. | `arxiv-render/src/fetch.rs` (asset_dir already extracted) + a new `extract_bibitems_from_bibtex` step | DEGRADED — applies to 2605.04035 |
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

---

## Z. Last-priority fix candidate — image-emit corruption ("AAAA walls")

**Refines B8.** On `2605.04035` (and possibly other image-heavy papers), running standalone tread doesn't just produce blank gaps — it produces visible walls of `'A'` characters (sometimes other base64 alphabet chars) interspersed with the document text. Looks like a "crash car" render: text + random AAAA blocks scattered through the content area where figures should sit.

### Root cause hypothesis

The `'A'` walls are almost certainly the kitty graphics protocol payload being **printed as text instead of decoded as escape sequences**:

1. The kitty graphics protocol wraps base64-encoded PNG bytes in APC escapes: `\x1b_G...,;<base64 payload>\x1b\\`.
2. Base64's value-zero character is `'A'`. A long run of `'A'`s = a long run of zero bytes in the underlying image (very common: PNG zlib padding, transparent regions, image header padding).
3. If the terminal doesn't process the escape — tmux passthrough off, malformed sequence, or the terminal lacks support — the payload appears in the text buffer instead of as pixels.

### Why specific to 2605.04035 (not Attention)

- 2605.04035's PDF figures convert to PNGs with heavy zero-padding (large white-background figures with sparse content). Their base64 has long `'A'` runs.
- Attention paper figures convert to denser PNGs without obvious zero-runs, so the same bug — if present — produces a less-visible failure (just weird-looking absent figures rather than visible AAAA walls).
- This means **the underlying bug may affect Attention too**, just less visibly.

### Diagnostic candidates (order of likelihood)

1. **Tmux passthrough disabled.** First check: `tmux show -g allow-passthrough`. If off, every kitty escape gets dumped as text. Cheapest possible fix. Tread's startup hint already flags this; user may have ignored.
2. **Kitty escape framing bug for chunked images.** kitty-graphics protocol uses `m=1` (more) and `m=0` (last) for chunked emission. If chunking is broken at certain image sizes, the terminal sees an unterminated escape and gives up. Inspect `kitty-graphics/src/transmit.rs` chunking path.
3. **Malformed payload from PNG conversion.** `pdftoppm` may produce PNGs that the terminal can't decode for these specific images. Test by manually viewing the cached PNGs at `~/.cache/tread/figures/`.
4. **Capability over-detection.** Tread might be detecting kitty support when the actual terminal doesn't fully support it. Verify `kitty_graphics::detect()` logic — does it check for full kitty protocol support, or just "looks like iTerm2"?

### Why this is deferred to last

- Likely fixed serendipitously by the tmux passthrough config check (no code change required).
- Investigation needs interactive runs in real terminals — hard to scope upfront.
- Most other Tier 1 / 2 / 3 fixes are more deterministic and self-contained.

### Implication for the active fix plan

**S3 (wire `tread::after_draw` in trench) should follow this fix, not parallel it.** Wiring `after_draw` while this bug remains will propagate the AAAA walls into trench too, where they currently appear as blank gaps. Updating the plan ordering before working S3 is a one-line edit.

### Reproduction sketch

1. Run `./target/release/tread 2605.04035` in iTerm2 (likely inside tmux with passthrough off).
2. Observe text + walls of `A` characters in the figure regions.
3. Repeat with `tmux show -g allow-passthrough` confirmed `on` (or run outside tmux entirely) — likely fixes the symptom.

If outside-tmux still shows AAAA walls, the bug is in tread/kitty-graphics, not tmux. That's the worth-investigating case.
