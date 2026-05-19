# ADR-0005 — doc-model internal split

- **Status:** Accepted (2026-05-19) — split landed across four commits,
  one ADR commit.
- **Crate:** `doc-model`
- **Relates to:** [ADR-0004 — Reader public surface](0004-reader-public-surface.md) — establishes the
  "one seam at a time" cadence and the in-module test pattern reused here.

## Context

`doc-model` is the producer-facing block API and the layout engine that
turns `Vec<Block>` into `Vec<VisualLine>` at render time.  Through
2026-05 it lived in a single `src/lib.rs` of **1,396 lines with zero
tests**, mixing six concerns:

| Concern | Lines (approx) | Public API surface |
|---|---:|---|
| Data types (`Block`, `InlineSpan`, `VisualLine`, …) | 265 | yes |
| Figure sizing math (cell footprint, panel budget) | 105 | `compute_cell_footprint` |
| Caption + image-row emission | 60 | none |
| Table rendering (matrix, rules, multicolumn) | 450 | none |
| Word-wrap (`wrap_spans`, `wrap_list_item`) | 140 | none |
| `build_visual_lines` orchestrator | 290 | `build_visual_lines` |

The file was the largest unsupervised mega-file in the workspace and
the only crate with no tests.  A 2026-05-19 audit confirmed the
external API was still narrow — only `Block`, `InlineSpan`,
`LinkTarget`, `HeaderCell`, `ImageItem`, `VisualLine`,
`VisualLineKind`, `build_visual_lines`, and `compute_cell_footprint`
are reached from outside the crate.  `from_lines` had zero callers and
was dead code.

The smoking gun was the table rendering: ~450 lines of column-collapse,
rule-translation, and multicolumn-expansion logic with no automated
test coverage, despite being the load-bearing path for every
booktabs-style table in every paper.  Regressions here would have to
be caught by eyeballing rendered output on a benchmark paper.

## Decision

Split `lib.rs` into four sibling modules (`wrap`, `figure`, `table`,
`layout`) behind a byte-identical public API.  `lib.rs` becomes a
types + re-exports header.  Same pattern as ADR-0004's per-seam
extraction — one module at a time, each commit independently green.

Naming: the entry points are `emit_figure_lines` and
`emit_table_group_lines`, **not** `render_*`.  In this repo "render"
is reserved for the ratatui drawing path in `tread::render`; doc-model
*emits* visual lines, it doesn't draw them.

### Landed module map

```
crates/doc-model/src/
├── lib.rs       281 lines — public types + re-exports + pub(crate) visual_width
├── layout.rs    298 lines — pub fn build_visual_lines (orchestrator)
│                          + private center_line
├── wrap.rs      206 lines — pub(crate) wrap_spans, wrap_list_item
├── figure.rs    341 lines — pub fn compute_cell_footprint (re-exported via lib)
│                          + pub(crate) emit_figure_lines
│                          + private: figure_row_budget, panel_row_budget,
│                            emit_image_rows, emit_image_row_rows,
│                            wrap_caption, emit_caption
└── table.rs     593 lines — pub(crate) emit_table_group_lines
                           (476 non-test) + private: translate_rules_to_active,
                             make_rule_line, render_table_group,
                             rows_look_like_header, render_row_with_spans
```

Public API is byte-identical to the pre-split state.  External callers
(`crates/arxiv-render`, `crates/tread`, `crates/math-render`) compile
unchanged.

### Seam 1 — `wrap` (commit e483553)

`wrap_spans` and `wrap_list_item` moved out as `pub(crate)`.  No call
site outside doc-model touched them, so the move was mechanical.
`from_lines` deleted in the same commit (zero callers — confirmed by
grep across the workspace before deletion).

Tests added (0 → 2):
- `wrap_spans_coalesces_same_style_runs` — pins the span-coalesce
  contract.  A regression silently inflates the spans vec per
  paragraph, which costs render time.
- `wrap_list_item_continuation_indent_matches_marker_width` — pins
  the continuation-line indent math (`depth*2 + marker.len()` spaces).

### Seam 2 — `figure` (commit 0329af9)

The figure sizing constants (`CELL_W_PX`, `CELL_H_PX`,
`MIN_IMAGE_ROWS`, `CAPTION_ROWS_BUDGET`, `STACK_BONUS_ROWS`,
`PANEL_ROW_CAP`), the budgeting functions (`figure_row_budget`,
`panel_row_budget`), and the VL emission helpers (`emit_image_rows`,
`emit_image_row_rows`, `wrap_caption`, `emit_caption`) all moved into
`figure.rs`.

The `Block::Figure` arm of `build_visual_lines` (~70 lines of stacking
logic) was lifted into `figure::emit_figure_lines`.  The orchestrator's
dispatch shrunk to a one-liner.

`compute_cell_footprint` stays `pub` and is re-exported from `lib.rs`,
keeping the path `doc_model::compute_cell_footprint` stable for the
one external call site (`crates/tread/src/state/figures.rs`).

Tests added (2 → 4):
- `compute_cell_footprint_preserves_aspect_when_height_clamped` — pins
  the "shrink cols when rows clamped" defence against squished images.
- `panel_row_budget_stack_bonus_caps_at_panel_row_cap` — pins
  `PANEL_ROW_CAP=21`, which keeps solo figures and stacked panels at
  uniform visual mass across the document.

### Seam 3 — `table` (commit 444453a)

`COL_SEP`, `RULED_COL_SEP`, `translate_rules_to_active`,
`make_rule_line`, `render_table_group`, `rows_look_like_header`, and
`render_row_with_spans` all moved into `table.rs`.

The table-group dispatch (matrix-bearing vs. rule-only paths) was
lifted from `build_visual_lines` into `emit_table_group_lines`.  As
with `figure`, the orchestrator now reads as a one-liner per table
group.

Tests added (4 → 7):
- `translate_rules_to_active_handles_edges_and_dedup` — edge / internal
  / duplicate / sort cases for the raw-rule → active-rule translation.
- `column_collapse_drops_always_blank_columns` — regression pin on the
  `active_cols` computation; historical source of "table eats width
  for empty padding columns" bugs.
- `multicolumn_cell_expands_columns_when_text_overflows` — pins the
  span-cell width-expansion path that keeps Attention's Table 3
  multicolumn headers from being truncated.

### Seam 4 — `layout` (commit 91675b3)

`build_visual_lines` moved into `layout.rs`, along with the private
`center_line` helper.  `lib.rs` lost its last function body and became
a types-only header.  `visual_width` was promoted to `pub(crate)` —
not strictly required (child modules already see private items in
their parent), but the explicit visibility documents the cross-module
shared-helper intent.

Tests added (7 → 8):
- `display_math_emits_centered_lines_with_eq_num` — pins the
  orchestrator's `DisplayMath` dispatch plus the right-aligned
  equation tag on the last line.

## Validation

Per commit:
- `cargo test --release` workspace-wide passes (no test count drop).
- `cargo clippy -p doc-model --release` produces 9 warnings — same
  set as pre-split, just relocated as code moved.  No new warning
  classes introduced.

Final smoke (commit 4):
- `cargo run --release -p arxiv-render -- 1706.03762` produces 379
  blocks → 675 visual lines.
- All 5 figures render with their captions (`[Figure N: …]` lines
  emitted at the right positions).
- Table 3 (the `c|cccccc|ccc` case) renders with `│` vertical rules at
  the left edge and after the 9th raw column — the load-bearing
  rule-translation path.
- Workspace `cargo build --release` succeeds with no changes to any
  external callsite.

Test count went **0 → 8** in doc-model (workspace total +8: 241 → 249).

## Consequences

**Good:**
- Every concern has a named home.  Locating "how does
  multicolumn width expansion work" is now a one-step file open
  (`table.rs::render_table_group`).
- Each module ships with at least one regression-pinning test.
  `column_collapse_drops_always_blank_columns` and
  `multicolumn_cell_expands_columns_when_text_overflows` catch the
  historical bug classes the user has hit before.
- Future deepening (e.g. Unicode-aware `visual_width`, terminal-cell
  pixel-ratio query) lands in one module instead of mutating a
  1,400-line file.
- The naming convention (`emit_*` for VL emission, `render_*` reserved
  for ratatui drawing) is now visible and enforceable.

**Costs:**
- 5 files where there was 1.  Cross-file jumps for the orchestrator
  (which now imports four modules) — partly offset by the dispatch
  becoming one line per block variant.
- `table.rs` at 593 lines (476 non-test) is the largest file post-
  split.  A future deepening of the rule / column-collapse code could
  warrant a further sub-split (`table/rules.rs`, `table/columns.rs`)
  if `table.rs` grows much beyond this.  Not done now — premature
  for the current call sites.
- Public API trimming is **out of scope** for this ADR.  `from_lines`
  was the only dead export and is gone.  `compute_cell_footprint`
  remains `pub` because one external caller uses it.  A future ADR
  may audit whether all 13 `Block` variants are externally
  constructed.

## Pattern reuse

The ADR-0004 → ADR-0005 sequence has now established a repeating
template for mega-file refactors in this workspace:

1. Audit the file: list concerns, line counts, public surface, test
   count.  Decide internal-only vs split-and-trim before writing
   code (this ADR was internal-only).
2. Lock the module map up-front; name the entry points; pre-decide
   which helpers cross module boundaries.
3. Leaf-first commits, one module per commit, each independently
   green.  Each commit ships its own invariant-pinning tests.
4. Final commit moves the orchestrator (or the public entry point)
   into its own module and trims the root file to types +
   re-exports.
5. ADR commit records the per-seam diff and the validation gates
   actually run.

Future mega-files (`crates/tread/src/render.rs` at 1,583 lines and
`crates/arxiv-render/src/pandoc_parse.rs` at 2,427 lines) should
follow the same shape.
