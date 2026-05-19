# ADR-0006 — render region split

- **Status:** Accepted (2026-05-19) — split landed across five commits,
  one ADR commit.
- **Crate:** `tread` (`crates/tread/src/render/`)
- **Relates to:** [ADR-0005 — doc-model internal split](0005-doc-model-internal-split.md) —
  immediate-prior application of the same mega-file template; this ADR is the
  third in the series and the first to apply the pattern to a UI-side file.

## Context

`crates/tread/src/render.rs` was a single 1,583-line file covering every
ratatui draw path the reader uses:

| Region | Functions | Lines (approx) |
|---|---|---:|
| Public entry / layout | `draw`, `split_layout`, `split_content_for_preview`, `preview_image_area` | 150 |
| Header + status chrome | `draw_header`, `draw_status`, `mode_label`, `pending_input_label` | 160 |
| Content rows | `draw_content`, `render_visual_line` + 7 cursor / highlight / search overlays + 3 byte-boundary utils | 670 |
| TOC pane | `draw_toc`, `toc_trunc` | 65 |
| Preview pane | `draw_preview_pane` | 75 |
| Overlays + input bars | `draw_text_popup`, `draw_help_overlay`, `help_section`, `draw_search_bar`, `draw_command_bar`, `prompt_line` | 330 |

The mix was scope-by-accident: chrome changes had to scroll past
overlays to find headers; the figure-preview pane sat between two
unrelated draw paths; the cursor / highlight overlay helpers were
~340 lines deep into a 1,500-line file.

Five pre-existing tests guarded against multi-byte char boundary
regressions in the highlight overlays — those were the only
automated coverage on the render path.

## Decision

Split `render.rs` into six files behind a single `pub fn draw` entry
point.  Each child module is private (`mod content;` etc.) and exposes
its public surface as `pub(super)` to the parent.  No external callers
touch any path other than `crate::render::draw`,
`crate::render::split_content_for_preview`,
`crate::render::preview_image_area`, and `crate::render::split_layout`.

Naming follows the existing in-crate convention: `state/` is already
laid out as `state/mod.rs` + siblings, so `render/` adopts the same
shape.

### Landed module map

```
crates/tread/src/render/
├── mod.rs       216 — pub fn draw + layout helpers + pub(super) toc_trunc
├── content.rs   834 — draw_content + render_visual_line dispatch
│                     + 7 cursor/highlight/search overlay helpers
│                     + 3 byte-boundary utilities
│                     + 3 migrated overlay tests
├── chrome.rs    209 — draw_header + draw_status + mode_label + pending_input_label
│                     (+ 1 new mode_label test)
├── overlays.rs  304 — draw_search_bar + draw_command_bar + prompt_line
│                     + draw_text_popup + draw_help_overlay + help_section
├── preview.rs    95 — draw_preview_pane (figure-preview pane scaffolding)
└── toc.rs        64 — draw_toc (table-of-contents pane)
```

### Seam 1 — directory + `preview.rs` (commit dfea304)

Rename `render.rs` → `render/mod.rs` (via `git mv`, picked up as a
rename in history).  Extract `draw_preview_pane` into `preview.rs`
as `pub(super)` — the smallest fully-independent leaf, used as the
canary for the directory layout.

### Seam 2 — `toc.rs` (commit fae3d97)

Move `draw_toc` into its own module.  `toc_trunc` (the char-aware
truncation utility) stays in `mod.rs` and is promoted to `pub(super)`
because four downstream consumers (`draw_header`, `draw_toc`,
`draw_status`, `prompt_line`) all use it — leaving it in the parent
matches its actual scope.

### Seam 3 — `chrome.rs` (commit ce7a63c)

Move `draw_header`, `draw_status`, and the two label helpers
(`mode_label`, `pending_input_label`) into the chrome module.  The
two labels are pulled out alongside `draw_status` because they're
only called from there — colocation keeps the status-line logic
readable in one file.

Test added (150 → 151):
- `mode_label_covers_every_mode` — pins the mode → status-bar label
  mapping.  This text ships in the visible UX; a regression would
  silently rename "VISUAL LINE" to "VISUAL", etc.

### Seam 4 — `overlays.rs` (commit 28ba709)

Move `draw_search_bar`, `draw_command_bar`, and the shared
`prompt_line` formatter (input bars) plus `draw_text_popup`,
`draw_help_overlay`, and `help_section` (floating overlays) into one
module.  The two categories share a domain — "drawn on top of the
reader content, dismissed by Esc or a keystroke" — and share the
`prompt_line` helper.

mod.rs's ratatui-widget import list shrinks (drops `Borders`, `Clear`,
`Wrap` — only `Block` and `Paragraph` still in use).

### Seam 5 — `content.rs` (commit ab97f45)

The largest extract.  Move `draw_content`, `render_visual_line`, and
every overlay helper (7 functions covering cursor placement,
persistent character highlights, search-match highlighting on both
single-style and styled prose) plus the three private byte-boundary
utilities (`is_box_drawing`, `snap_to_char_boundary`,
`clamp_to_char_boundary`) into one module.

The cursor cell, persistent highlights, the active voice word, and
search-match highlighting all converge on `render_visual_line` —
splitting them across modules would mean every overlay change has to
walk multiple files in lockstep.  They stay co-located.

Three pre-existing tests migrate with the code:
- `overlay_highlights_renders_full_text_when_range_straddles_multibyte_char`
- `overlay_highlights_does_not_panic_on_misaligned_range`
- `highlight_query_handles_multibyte_text`

mod.rs now contains only the public layout surface and the
`toc_trunc` shared utility.  Imports collapse to four ratatui types
plus `Reader`/`Mode`/`Theme`.

## Validation

Per commit:
- `cargo build -p tread --release` succeeds.
- `cargo test --release` workspace-wide passes with no failures.
- No new clippy warnings introduced — the only post-split warnings
  are pre-existing in code that hasn't moved.

Test count: 150 → 151 in tread (+1 new chrome test; 3 overlay tests
migrated in place).  Five pre-existing tests still cover the
multi-byte boundary cases that motivated the C1 fix.

UI smoke: `cargo run -p tread --release -- 1706.03762` not exercised
from this session (requires interactive TTY).  Render correctness on
Attention's tables, figures, search overlay, voice-active highlighting,
and the help/marks/about popups should be verified by hand at PR
review time.

## Consequences

**Good:**
- Each rendered region has a named home.  A change to header chrome
  no longer requires scrolling past overlays; the figure-preview pane
  is self-contained.
- mod.rs is a 216-line entry point — small enough to read top-to-
  bottom and see the entire draw orchestration in one view.
- The "split by rendered region" framing is now visible in the
  filesystem.  New regions land in new sibling files instead of
  growing one file unboundedly.
- One new invariant test (`mode_label_covers_every_mode`) catches a
  class of visible-UX regression that previously had no automated
  guard.

**Costs:**
- 6 files where there was 1.  Reading `render_visual_line` and an
  overlay helper now means open two files instead of scrolling within
  one.  Justified because the previous all-in-one form made adding a
  new region force a 1,500-line scan.
- `content.rs` at 834 lines (including ~50 lines of tests) is the
  largest post-split file.  Further sub-splitting (separating cursor
  overlays from search overlays, etc.) is **not** done now because the
  helpers all feed `render_visual_line` and changing one usually means
  changing several — splitting buys nothing.  Revisit only if a future
  feature introduces a clean seam.
- UI surfaces are intrinsically test-poor.  This refactor doesn't
  change that — manual TTY smoke remains the validation backstop for
  visible output.

## Pattern reuse

This is the third application of the ADR-0004 → 0005 → 0006 template:

1. Audit the file: list concerns, line counts, public surface, test
   count.  Decide internal-only vs split-and-trim.
2. Lock the module map up-front; name the entry points; pre-decide
   which helpers cross module boundaries (here: `toc_trunc` shared in
   `mod.rs`, all overlay helpers co-located in `content.rs`).
3. Leaf-first commits, each independently green, each ships its own
   tests where the surface is testable (UI surfaces produce fewer
   tests than algorithmic ones — ADR-0005 added 8, this ADR adds 1).
4. Final commit is the ADR; it records observed metrics, not aspirational
   ones.

Next mega-file in the queue: `crates/arxiv-render/src/pandoc_parse.rs`
(2,427 lines).  The user has indicated table/spec or inline/math
extraction as the right next deepening — neither is figure (already
done).  The same template applies: rename to a directory if needed
(it's already split into `pandoc_parse/` with `figure.rs`), leaf-first
extractions, one ADR.
