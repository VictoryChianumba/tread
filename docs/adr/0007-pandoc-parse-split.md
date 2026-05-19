# ADR-0007 — pandoc_parse split

- **Status:** Accepted (2026-05-19) — split landed across four commits,
  one ADR commit.
- **Crate:** `arxiv-render` (`crates/arxiv-render/src/pandoc_parse/`)
- **Relates to:** [ADR-0005 — doc-model internal split](0005-doc-model-internal-split.md),
  [ADR-0006 — render region split](0006-render-region-split.md) — third (and final)
  application of the mega-file template in this workspace.

## Context

`crates/arxiv-render/src/pandoc_parse.rs` was a 2,427-line file
covering every step of the Pandoc-based parse pipeline: pre-Pandoc
LaTeX preprocessing, root-file detection, metadata extraction, JSON
AST walker, per-block parsers (para, list, table, figure, math), and
inline-span walkers.

A previous session had already extracted `pandoc_parse/figure.rs`
(507 lines) into a sibling file, leaving `pandoc_parse.rs` as the
parent module via the Rust 2018 sibling-file convention.  The
remaining 2,427 lines still mixed six concerns:

| Concern | Lines (approx) | Items |
|---|---:|---|
| try_pandoc orchestrator + root detection | 150 | `try_pandoc`, `find_root_name` |
| Metadata extraction | 110 | `extract_meta_blocks`, `extract_author_names` |
| SectionCounters + block walker | 270 | `SectionCounters`, `walk_blocks` |
| para → Block + math helpers | 140 | `para_to_block`, math fns, list_item helper |
| Table parsing | 280 | `parse_table` + 7 row/cell/caption helpers |
| Inline walkers | 350 | `walk_inlines_*`, `dedup_spaces`, `synthesize_bibliography` |
| LaTeX preprocessing | 280 | `preprocess_latex_source`, 5 `strip_*`, 6 brace/delim helpers |
| Tabular column spec | 270 | `TableSpec`, `ParsedColumnSpec`, parser fns, byte utilities |
| Tests | ~400 | 4 inline test mods |

Four existing test modules (`para_to_block_tests`, `math_label_tests`,
`section_counter_tests`, `spec_parser_tests`) provided ~30 unit tests
— good algorithmic coverage compared to render's ~5, but still
clustered at the bottom of one mega-file.

## Decision

Split the parent file into five sibling modules behind the existing
`figure.rs` sibling, all under `pandoc_parse/`.  Convert
`pandoc_parse.rs` → `pandoc_parse/mod.rs` to formalize the layout
(matches `state/` in tread and `render/` from ADR-0006).

The split lines match the user's recommendation from the planning
session: tables, inline, and preprocessing each get their own home;
the column-spec parser was a fourth natural separation since it has
zero Pandoc dependency and is the largest pure-Rust-byte-scanning
chunk.  Math helpers stay in `mod.rs` because they're small (~30
lines) and tightly coupled to `para_to_block`.

### Landed module map

```
crates/arxiv-render/src/pandoc_parse/
├── mod.rs       983 — try_pandoc + walk_blocks + SectionCounters
│                     + para_to_block + math helpers + metadata
│                     + CITE_NUMBERS / BIBITEMS_ORDERED / BIBLIOGRAPHY_EMITTED
│                       thread_locals
│                     + 3 test mods (para_to_block, math_label,
│                                    section_counter)
├── figure.rs    509 — (unchanged; pre-existing from a prior session)
├── spec.rs      562 — TableSpec, ParsedColumnSpec, extract_tabular_specs,
│                     parse_column_spec*, byte utilities + 6 spec tests
├── preprocess.rs 293 — preprocess_latex_source + 5 strip_* +
│                     6 brace/delim primitives
├── inline.rs    359 — walk_inlines_text/spans, dedup_spaces,
│                     extend_link_back_to_prefix, synthesize_bibliography
└── table.rs     291 — parse_table + row/cell/caption helpers
```

Total post-split: 2,997 lines across 6 files (~+570 lines over the
2,427 original — accounted for by module preambles, the duplicated
import lines, and the `pub(super)` visibility markers added at
cross-module boundaries).

### Seam 1 — directory + `spec.rs` (commit 3087676)

Rename `pandoc_parse.rs` → `pandoc_parse/mod.rs` (via `git mv`).
Extract the tabular column-spec parser (TableSpec, ParsedColumnSpec,
extract_tabular_specs, scan_table_horizontal_rules, parse_column_spec,
parse_column_spec_full, utf8_char_width, preceded_by_odd_backslashes)
into a new sibling.  Pure byte-level scanning with no Pandoc
dependency, no thread-local writes — the cleanest leaf to start with.

Visibility scaffolding:
- TableSpec re-exported as `pub(crate)` from mod.rs so figure.rs's
  existing `super::TableSpec` import stays valid.
- utf8_char_width promoted to `pub(super)` in spec.rs; mod.rs's
  preprocess code (still inline at this point) imports it back.
- The byte/delim primitives in mod.rs (skip_ascii_ws, match_delim,
  match_brace, parse_*_brace_args, parse_cmidrule_args) promoted to
  `pub(super)` so spec.rs can call them; they move out in commit 2.

spec_parser_tests (6 tests) migrate with the code.

### Seam 2 — `preprocess.rs` (commit ceeebc3)

Move preprocess_latex_source + the five strip_* rewriters
(resizebox, adjustbox, scalebox, multirow, cmidrule) + the six
byte/delim primitives that were temporarily `pub(super)` in mod.rs.

Two consumers: try_pandoc (in mod.rs) calls preprocess_latex_source
on each source file before writing to the tmp dir Pandoc reads from;
spec.rs's column-spec scanner reuses the byte primitives.  spec.rs's
imports rewrite from `super::*` to `super::preprocess::*`.

utf8_char_width stays in spec.rs (its commit-1 home) and is imported
into preprocess.rs as `super::spec::utf8_char_width` — sibling-
sibling, no cycle.

### Seam 3 — `inline.rs` (commit 2f3adda)

Move the inline AST walkers: walk_inlines_text / walk_inlines_spans
(plain-text and span-emitting variants), the cite-key /
ref-target back-extension helpers, dedup_spaces, and
synthesize_bibliography.

The thread-local store (CITE_NUMBERS, BIBITEMS_ORDERED,
BIBLIOGRAPHY_EMITTED) in mod.rs is promoted to `pub(super)` so
inline.rs's Cite arm can read/write the citation-number map.
render_math and render_inline_math (still in mod.rs) similarly
promoted so inline.rs's math arms can call them.

figure.rs's `use super::walk_inlines_text` rewrites to
`use super::inline::walk_inlines_text` — direct sibling path.  The
attempted `pub(crate) use inline::walk_inlines_text` re-export from
mod.rs hit a Rust visibility-downgrade error (can't re-export a
`pub(super)` item at `pub(crate)`); the sibling-path import is
cleaner anyway.

### Seam 4 — `table.rs` (commit 1869509)

Move parse_table + the row/cell/caption extractors
(take_matching_spec, count_header_prefix, looks_like_data_row,
starts_with_ascii_digit, extract_rows, extract_cell_text,
extract_caption_text).

The module owns the Pandoc Table JSON arm.  parse_table takes a
`&mut Vec<TableSpec>` queue (matched per-table by column count via
take_matching_spec) and emits one `Block::Matrix` per header zone
plus one per data zone, separated by `Block::Rule` blocks.

figure.rs's `super::extract_caption_text` updates to
`super::table::extract_caption_text`.  table.rs imports
`super::SectionCounters` (a private struct in mod.rs accessible via
the child-can-see-parent rule), `super::TableSpec` (re-exported),
and `super::inline::walk_inlines_text`.

## Validation

Per commit:
- `cargo build -p arxiv-render --release` succeeds.
- `cargo test --release` workspace-wide passes — arxiv-render test
  count stays at 53, including the 6 migrated spec_parser_tests.
- No new clippy warnings introduced; the existing unused-import
  warnings were trimmed as imports became unused on each extract.

Final smoke (commit 1869509):
- `cargo run --release -p arxiv-render -- 1706.03762` produces 379
  blocks → 675 visual lines, byte-identical to the pre-split baseline
  established at the end of ADR-0005.

## Consequences

**Good:**
- Each Pandoc-parse concern has a named home.  Locating "how do we
  read vertical rules from the LaTeX source" is a one-step file open
  (`spec.rs::extract_tabular_specs`).  Same for figure handling
  (existing `figure.rs`), inline walking, preprocessing, and table
  parsing.
- The cross-module dependency graph is now explicit:
  - `spec` depends on `preprocess` (byte primitives)
  - `preprocess` depends on `spec` (utf8_char_width — a clean
    sibling-sibling import, no cycle)
  - `inline` depends on mod.rs's thread-locals and math helpers
  - `table` depends on `spec` (TableSpec) and `inline`
    (walk_inlines_text)
  - `figure` (pre-existing) depends on `inline` and `table`
- mod.rs at 983 lines is still the largest file but contains a
  coherent orchestration story (try_pandoc + walk_blocks +
  para_to_block + math + metadata + counters).  Further extraction
  (e.g. moving SectionCounters + walk_blocks to a separate module)
  was considered and rejected: those two are the orchestrator and
  belong at the top level.

**Costs:**
- 6 files where there was 1 (counting the pre-existing figure.rs).
  Reading the parser end-to-end now means open multiple files; per-
  commit visibility scaffolding (`pub(super)` on cross-module
  helpers, the thread_locals' visibility bump, the temporary
  pub(super) on byte primitives in commit 1 that moved to
  preprocess.rs in commit 2) added scaffolding lines that future
  consolidation could shave back.
- The Rust visibility / use-import rules required two real
  workarounds documented above:
  1. Children of a module CAN see private items of the parent for
     direct path access (`super::X`) but CANNOT use-import private
     items (`use super::X;` fails).  Resolution: promote shared
     helpers to `pub(super)` at extraction time.
  2. `pub(crate) use` cannot re-export a `pub(super)` item.
     Resolution: import siblings directly via `super::sibling::X`.

## Series complete

This is the third and final application of the ADR-0004 → 0005 → 0006
→ 0007 mega-file template in this workspace.  The three biggest
single-file targets in the codebase are now split:

| ADR | File | Original lines | Modules now |
|---|---|---:|---:|
| 0005 | `doc-model/src/lib.rs` | 1,396 | 5 |
| 0006 | `tread/src/render.rs` | 1,583 | 6 |
| 0007 | `arxiv-render/src/pandoc_parse.rs` | 2,427 | 6 |

The pattern itself is now load-bearing across the workspace; future
splits should follow the same shape:

1. Audit: list concerns, line counts, public surface, test count.
2. Lock the module map up-front; name entry points; pre-decide which
   helpers cross module boundaries and at what visibility.
3. Leaf-first commits, each independently green, each migrates the
   tests for its concern.
4. Validate per commit with workspace `cargo test` and (for parse /
   layout work) the Attention smoke.
5. Final commit is the ADR; metrics observed, not aspirational.

Remaining mega-file candidates as of 2026-05-19:
- Reader Seam 5 (`crates/tread/src/state/mod.rs`) — still open per the
  user's earlier recommendation despite ADR-0004 declaring four prior
  seams complete.  Candidates: `visual_lines`, `sections`,
  `search_matches`, `cmd_buf`, `popup`, `text_only`,
  `figure_preview_active`, `current_figure`.  Smaller in lines than
  the three completed ADRs but adopts a similar invariant-pinning
  pattern.
- No other files in the workspace exceed 1,500 lines.
