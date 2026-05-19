# ADR-0008 — Reader Seam 5

- **Status:** Accepted (2026-05-19) — landed across three commits, one
  ADR commit.
- **Crate:** `tread` (`crates/tread/src/state/mod.rs`)
- **Relates to:** [ADR-0004 — Reader public surface](0004-reader-public-surface.md) —
  reopens the four-seam project ADR-0004 declared complete; this is "Seam 5,"
  split into three sub-seams targeting eight specific fields the user flagged
  during a 2026-05-19 grilling session.

## Context

ADR-0004 closed four Reader seams (mode, cursor/scroll, voice,
bookmarks) and explicitly judged the remaining `pub` fields acceptable
— "read-mostly collections plus simple toggle flags, none with the
multi-field invariant problem the original audit flagged."

A subsequent user audit recommended further shrinking based on a
field-by-field re-read, naming eight specific candidates:

| Field | Audit finding | Shape |
|---|---|---|
| `visual_lines` | LayoutCache projection — 22 external reads, 0 writes | Read-only |
| `sections` | LayoutCache projection — 6 external reads, 0 writes | Read-only |
| `search_matches` | Owned by `update_search_matches` — 7 reads, 0 external writes | Read-only |
| `cmd_buf` | Mutated only during `Mode::Command` keypress handling | Read + scoped writes |
| `popup` | 5 external `Some(PopupContent { ... })` construction sites + 2 reads | Multi-field constructor invariant |
| `text_only` | 0 external readers; doc-comment warns "direct writes desync state" | Pure privatization |
| `figure_preview_active` | 0 external readers; setter `set_figure_preview_active` already maintains coherence with `text_only` | Pure privatization |
| `current_figure` | 0 external readers; `step_figure` is the controlled mutator | Pure privatization |

The eight fields cluster into three distinct shapes (read-only, multi-
field constructor, pure privatization).  Each shape gets its own
sub-seam.

## Decision

Split Seam 5 into three sub-commits to match the shapes:

- **Seam 5a — read-mostly closures** (`visual_lines`, `sections`,
  `search_matches`, `cmd_buf`): privatize the field, expose a
  `pub fn <name>(&self) -> &T` getter, and add focused mutation
  methods for `cmd_buf` (since the Command-mode key handler mutates
  it character-by-character).
- **Seam 5b — popup encapsulation** (`popup`): privatize, add
  `open_popup(title, lines)` that constructs `PopupContent`
  atomically, `close_popup()` for centralized dismissal, and a
  `popup()` getter.  Removes 5 repeated `PopupContent { ... }`
  constructions and the `self.popup = None` invariant from the
  event loop.
- **Seam 5c — figure-preview triple** (`text_only`,
  `figure_preview_active`, `current_figure`): pure privatization.
  Existing setter / step / visibility methods already cover the
  public API; the audit confirmed zero external readers.

### Seam 5a — read-mostly closures (commit dfaa0a8)

Four new getter methods:
```rust
impl Reader {
    pub fn visual_lines(&self) -> &[VisualLine];
    pub fn sections(&self) -> &[(usize, u8, String)];
    pub fn search_matches(&self) -> &[usize];
    pub fn cmd_buf(&self) -> &str;
}
```

Plus three focused `cmd_buf` mutators used by the Command-mode key
handler in `keys.rs`:
```rust
impl Reader {
    pub fn push_cmd_char(&mut self, c: char);
    pub fn pop_cmd_char(&mut self) -> bool;
    pub fn take_cmd_buf(&mut self) -> String;
}
```

39 external read sites updated (22 + 6 + 7 + 4 across keys.rs,
text_objects.rs, lib.rs, commands.rs, render/*, images/inline.rs,
plus the bench example).

**One borrow-checker gotcha encountered in
`keys.rs::collect_highlight_runs`.** Pre-getter code held a "split
borrow" on `reader.visual_lines` only, which Rust allowed alongside
`&mut reader.highlights`.  The method getter returns `&[VisualLine]`
from `&self` — a wider borrow that covers the whole Reader.  The
mutating arm at line 680 (`reader.highlights.add(...)`) needed the
mut borrow.  Resolution: bind `vl.block_idx` into a Copy local, drop
the `vl` reference, then enter the match.  Documented inline with a
comment so the next reader doesn't reintroduce the conflict.

### Seam 5b — popup encapsulation (commit 792ce3d)

Three new methods:
```rust
impl Reader {
    pub fn popup(&self) -> Option<&PopupContent>;
    pub fn open_popup(&mut self, title: String, lines: Vec<String>);
    pub fn close_popup(&mut self);
}
```

`open_popup` collapses 5 repeated constructions:
```rust
// Before, at 5 sites:
reader.popup = Some(PopupContent { title: "Marks".into(), lines });
// After:
reader.open_popup("Marks".to_string(), lines);
```

`close_popup` collapses the event loop's dismissal pattern:
```rust
// Before, in lib.rs event_loop:
if self.popup.is_some() {
    self.popup = None;
    return ReaderAction::Continue;
}
// After:
if self.popup().is_some() {
    self.close_popup();
    return ReaderAction::Continue;
}
```

The invariant captured here is small but real: `PopupContent`
construction has two required fields (title + lines), and prior code
inlined the struct literal at 5 places.  A future fourth field would
have meant updating all 5 sites; now it's one method body.

### Seam 5c — figure-preview triple (commit a8a602e)

Pure visibility change: `pub text_only` / `pub figure_preview_active`
/ `pub current_figure` → private.  Zero call-site updates needed
(the audit confirmed no external readers).

The `text_only` doc-comment previously warned *"Toggle via
`set_text_only` — direct field writes won't trigger the rebuild and
will desync state."*  Privatization makes that warning compiler-
enforced.

## Validation

Per commit:
- `cargo test --release` workspace-wide passes (151 tread tests
  preserved across all three sub-commits — no test count change,
  no failures).
- `cargo build --release` clean (the one new clippy warning during
  development on `drop(vl)` was changed to `let _ = vl;` before
  commit).

No new automated tests added.  The eight-field encapsulation is
mechanical (privatize + getter/setter); the seams have no new
invariants worth pinning beyond what was already implicit (cmd_buf
is mutated only in Mode::Command; popup is constructed atomically;
figure-preview state is coherent through its setter).

## Consequences

**Good:**
- Reader's `pub` field count goes from 30 (pre-Seam-5) to 22 — an
  8-field reduction across one PR.
- The popup construction invariant becomes load-bearing: future
  contributors literally can't add a popup without going through
  `open_popup`, so any new required field on `PopupContent` is a
  one-line update.
- The `set_text_only` doc-warning ("direct writes desync state") is
  now compiler-enforced.
- The seam shape (getter for read-mostly, focused mutators for
  scoped writes, atomic constructors for multi-field types) is now
  established across all 5 Seams (4 from ADR-0004 + this).

**Costs:**
- 39 external `reader.field` access sites had to switch to
  `reader.field()`.  Pure mechanical churn but real diff size.
- The borrow-checker gotcha in keys.rs is a real cost — method
  getters return wider borrows than field accesses, so any code that
  held a split-borrow on a field plus `&mut reader.<other>` now has
  to drop the borrow first.  One site fixed in this PR; future
  splits should expect to hit the same pattern.
- The added focused `cmd_buf` mutators (`push_cmd_char`,
  `pop_cmd_char`, `take_cmd_buf`) are 3 methods for what was
  previously 3 direct field operations.  Marginal expansion of
  Reader's API surface for an invariant ("only the Command-mode key
  handler mutates `cmd_buf`") that the codebase already followed
  by convention.

## Reader public surface, post-Seam-5

22 remaining `pub` fields on Reader, grouped by audit category:

| Concern | Fields | Why still pub |
|---|---|---|
| Document content | `blocks` | Owned data; replaced wholesale by `reload_with`. Could close but has no invariant. |
| Help / TOC overlay state | `toc_visible`, `help_visible`, `help_query`, `help_selected` | Simple toggle flags; help_query is search-as-you-type text. No invariant. |
| Window geometry | `width`, `height` | Set only by `resize()` from the host. |
| Search query | `search_query`, `search_idx` | The query string is host-typeable; idx is a navigation cursor. |
| Navigation | `nav_history` | Stack of (offset, cursor_y) prior positions. Pushed via `push_nav_mark`; pops via `nav_back`. Could close but no invariant. |
| Paper metadata | `meta`, `arxiv_id`, `kitty_supported` | Set once at construction or via `init`. |
| Layout cross-refs | `label_lines`, `bib_entries`, `bib_entry_lines`, `image_paths` | LayoutCache projections; same shape as `visual_lines` was pre-Seam-5a. Closing them is the natural Seam 6 if anyone wants it. |
| Persistent highlights | `highlights` | `HighlightSet` is itself an encapsulated type; the Reader field is just the handle. |
| Visual selection | `visual_anchor`, `visual_anchor_x` | Set on `enter_visual_mode` and unchanged until exit; no host writes. |
| Count prefix | `count_buf` | Same shape as the pre-Seam-1 `cmd_buf` — could close on the same template; no invariant beyond "cleared on mode transitions" (already enforced). |
| Command error | `cmd_error` | One-tick override on the status bar. Set by command results; cleared by next keystroke. |

If a Seam 6 is ever justified, the highest-leverage targets are the
four LayoutCache projections (`label_lines`, `bib_entries`,
`bib_entry_lines`, `image_paths` — same pattern as Seam 5a) and the
`count_buf` (same pattern as `cmd_buf` in Seam 5a).  The remaining
fields don't have a compelling invariant to close them around.

## Pattern reuse

This is the 5th seam under the ADR-0004 invariant-encapsulation
pattern.  Combined with the three ADR-0005 / 0006 / 0007 mega-file
splits, the workspace's structural debt from the 2026-05 period is
fully retired: every audit candidate flagged by the user has been
addressed.
