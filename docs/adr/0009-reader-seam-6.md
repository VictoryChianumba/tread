# ADR-0009 — Reader Seam 6

- **Status:** Accepted (2026-05-19) — landed across two commits, one
  ADR commit.
- **Crate:** `tread` (`crates/tread/src/state/mod.rs`)
- **Relates to:** [ADR-0008 — Reader Seam 5](0008-reader-seam-5.md) —
  Seam 6 implements the two candidates ADR-0008's closing section flagged
  as the only remaining fields with "non-trivial leverage."

## Context

ADR-0008's closing section listed two natural Seam 6 candidates:
- The four LayoutCache projections (`label_lines`, `bib_entries`,
  `bib_entry_lines`, `image_paths`) — same shape as Seam 5a, with
  external readers but zero external writers.
- `count_buf` — same shape as Seam 5a's `cmd_buf` (getter + scoped
  mutators).

The audit before this commit:

| Field | External reads | External mutations |
|---|---:|---:|
| `label_lines` | 1 | 0 |
| `bib_entries` | 1 | 0 |
| `bib_entry_lines` | 1 | 0 |
| `image_paths` | 2 | 0 |
| `count_buf` | 31 | 25 (22 `.clear()`, 3 `.push(c)`) |

The LayoutCache projections were the lowest-leverage targets in the
workspace (1-2 read sites each), but they completed a pattern: all
LayoutCache projections on Reader are now encapsulated symmetrically
with `visual_lines` and `sections` from Seam 5a.

`count_buf` was the highest-leverage remaining target — 56 total
external touch points — but the mutation pattern was uniform (22 of
25 mutations were the per-motion `.clear()` after a count was
consumed).  Closing it captures the "every motion consumes the count
exactly once" invariant in one method.

## Decision

Two sub-seams, same shape templates from ADR-0008:

- **Seam 6a — LayoutCache projections:** privatize the four fields,
  add four getters returning `&HashMap<...>`.  No new mutators —
  these fields are read-only outside `LayoutCache::rebuild_layout`.
- **Seam 6b — count_buf:** privatize, add `count_buf()` getter +
  `push_count_char(c)` mutator + `clear_count()` mutator.

### Seam 6a — LayoutCache projections (commit c516491)

Four new getter methods:
```rust
impl Reader {
    pub fn label_lines(&self) -> &HashMap<String, usize>;
    pub fn bib_entries(&self) -> &HashMap<String, String>;
    pub fn bib_entry_lines(&self) -> &HashMap<String, usize>;
    pub fn image_paths(&self) -> &HashMap<u32, std::path::PathBuf>;
}
```

5 external read sites updated:
- `keys.rs::follow_link_at_cursor` — calls both `label_lines()` and
  `bib_entry_lines()` for link/citation resolution.
- `keys.rs::popup_citation_at_cursor` — calls `bib_entries()` for
  the citation entry text.
- `images/inline.rs` — calls `image_paths()` twice (placement
  diagnostics + actual byte-load lookup).

The unused `PopupContent` import in `commands.rs` (left over from
Seam 5b's `open_popup` collapse) is dropped in the same commit
since clippy flagged it.

### Seam 6b — count_buf (commit e42401c)

Three new methods (matching the cmd_buf template from Seam 5a):
```rust
impl Reader {
    pub fn count_buf(&self) -> &str;
    pub fn push_count_char(&mut self, c: char);
    pub fn clear_count(&mut self);
}
```

The mutation pattern was almost entirely uniform: keys.rs has 22
`.clear()` calls (one after each motion that consumes the count) plus
3 `.push(c)` (digit accumulation in the count-prefix path).  Reads
were `.is_empty()` checks (motion handlers / count-display gating)
and one `.parse()` (the count→`usize` conversion in `take_count`).

The "every motion consumes count exactly once" rule was previously
maintained by convention — each match arm in `handle_normal` ends
with `reader.count_buf.clear();`.  After Seam 6b that's a method
call, and forgetting it shows up at code-review time as "this motion
arm didn't call `clear_count`."

## Validation

Per commit:
- `cargo test --release` workspace-wide passes (151 tread tests
  preserved across both sub-commits).
- `cargo build --release` clean; clippy clean (the one warning during
  development was the dropped `PopupContent` import, fixed in 6a).

No new automated tests added — the closures don't introduce new
invariants beyond what was already implicit, and the count-prefix
paths are exercised end-to-end by the existing motion-combination
tests.

## Consequences

**Good:**
- Reader's `pub` field count goes from 22 (post-Seam-5) to 17 — a
  further 5-field reduction.
- All LayoutCache projections on Reader (visual_lines, sections,
  label_lines, bib_entries, bib_entry_lines, image_paths) are now
  encapsulated symmetrically.  Adding a new projection follows the
  same getter pattern.
- The count-prefix "consume on motion" invariant is now method-
  enforced.  A future motion that forgets `clear_count()` would
  leave a stale `count_buf` visible in the status bar — caught at
  code review by the missing method call.

**Costs:**
- ~36 mechanical call-site updates (5 for Seam 6a + 31 for Seam 6b
  reads + 25 for Seam 6b writes), all done via perl regex substitution.
- Two methods (`push_count_char`, `clear_count`) for what was
  previously two direct field operations.  Marginal API expansion;
  symmetric with the cmd_buf trio from Seam 5a.

## Reader public surface, post-Seam-6

17 remaining `pub` fields on Reader — down from the ADR-0004 starting
point of 47, a 30-field reduction over the six seams:

| Concern | Fields |
|---|---|
| Document content | `blocks` |
| Help / TOC overlay state | `toc_visible`, `help_visible`, `help_query`, `help_selected` |
| Window geometry | `width`, `height` |
| Search query | `search_query`, `search_idx` |
| Navigation | `nav_history` |
| Paper metadata | `meta`, `arxiv_id`, `kitty_supported` |
| Persistent highlights | `highlights` |
| Visual selection | `visual_anchor`, `visual_anchor_x` |
| Command error | `cmd_error` |

The remaining 17 fields share one trait: each has zero ambiguity about
who writes it and when, with no multi-field invariant to enforce.  The
search-query / search-idx pair is the closest thing to a remaining
invariant (the index is only meaningful relative to the matches), but
that invariant is already centralized in `update_search_matches`.

I'd characterize the audit as **complete**: there are no remaining
fields where a seam would capture a real invariant.  Future closures
would be ceremony.

## Six-seam tally (ADR-0004 → 0008 → 0009)

| Seam | Concern | Fields closed | New methods | Tests |
|---|---|---:|---:|---:|
| 1 | Mode state machine | 1 enum + 3 buffers | 11 | +3 |
| 2 | Cursor / scroll | 4 | 8 | +3 |
| 3 | Voice | 10 | 8 | +1 |
| 4 | Bookmarks | 1 (with block-byte rewrite) | 5 | +2 |
| 5a | Read-mostly closures | 4 | 7 | 0 |
| 5b | Popup | 1 | 3 | 0 |
| 5c | Figure-preview triple | 3 | 0 | 0 |
| 6a | LayoutCache projections | 4 | 4 | 0 |
| 6b | count_buf | 1 | 3 | 0 |
| **Total** | | **30 fields** | **49 methods** | **+9 tests** |

The pattern is established; future Reader work should add new fields
as private with focused accessors from the start rather than going
through the privatize-and-update cycle.
