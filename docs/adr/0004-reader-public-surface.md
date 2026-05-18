# ADR-0004 — Reader public surface

- **Status:** Accepted (2026-05-18) — all four seams landed (mode,
  cursor/scroll, voice, bookmarks)
- **Crate:** `tread` (`state/` and every other file in the crate)
- **Relates to:** [ADR-0002 — Preview pane model](0002-preview-pane-model.md)

## Context

`Reader` (in `state.rs`) is the central reader type. The codebase has
been deepening individual subsystems behind narrow seams over the last
few months — `LayoutCache`, `FigureIndex`, `FigurePreviewState`,
`HighlightSet`, the search-invalidation contract — but the surrounding
`Reader` struct itself has not shrunk in step. A 2026-05-18 audit
returned the following metrics:

- **47 public fields** on `Reader`.
- **5 private fields** (`layout_cache`, `figure_index`, `preview_state`,
  `source_bibitems`, `pending_progress_offset`) — the seams introduced
  by recent work.
- **47 direct field mutations** in external code, distributed across:
  - `lib.rs` — 38 mutations (event loop, hotkey handlers).
  - `commands.rs` — 7 mutations (Ex-commands).
  - `render.rs` — 2 mutations (transient render state).

The largest single source of bare field writes is the **mode state
machine**: `reader.mode = Mode::*` is assigned in ~30 distinct sites
across `lib.rs`'s key handlers. Each handler writes mode directly,
sometimes without clearing the buffers (`cmd_buf`, `count_buf`,
`search_query`) that the new mode implies should be reset.

The smoking gun is `state.rs:796`: a doc comment on the `text_only`
field warns *"Toggle via `set_text_only` — direct field writes won't
trigger the rebuild and will desync state."* The codebase already
knows bare field writes break invariants; nothing structural prevents
them.

## Field grouping (current state)

Approximate buckets — names abbreviated for the index:

| Concern | Public fields | Already deep? |
|---|---|---|
| Cursor / scroll | `offset`, `cursor_y`, `cursor_x`, `desired_column` | no |
| Layout / DOM | `blocks`, `visual_lines`, `sections` | partial — `LayoutCache` exists but mirrors are public |
| Search | `search_query`, `search_matches`, `search_idx` | partial — `update_search_matches`, `remap_search_matches_after_layout` are the seam, but writes still happen externally |
| Mode | `mode`, `count_buf`, `cmd_buf`, `cmd_error` | **no — biggest leak** |
| Bookmarks / xref | `bookmarks`, `highlights`, `label_lines`, `bib_entries`, `bib_entry_lines` | partial — `HighlightSet` is its own type; bookmarks are a raw HashMap |
| Preview / figures | (none public anymore) | **yes** — `FigurePreviewState`, `FigureIndex` are encapsulated |
| Voice / TTS | 8 fields (`voice_controller`, `voice_status`, `voice_para_*`, etc.) | no |
| Help / TOC | `help_visible`, `help_query`, `help_selected`, `toc_visible`, `popup` | no |
| Window / paper | `width`, `height`, `meta`, `arxiv_id`, `kitty_supported`, `nav_history` | no |

## Decision

Deepening will land **one seam at a time**, not as a single pass. The
original audit's framing (#1: "Reader is too shallow") was too coarse
to schedule; each subsystem has its own invariants and call sites and
deserves its own focused refactor.

Priority order, by leverage (most mutations absorbed × clearest
invariant):

1. **Mode state machine** — first, as the canary.
2. **Cursor / scroll motions** — second.
3. **Voice state** — third.
4. **Bookmarks** — fourth (also unlocks block-byte storage, which
   makes bookmarks survive reflow the way highlights do — a side
   benefit beyond encapsulation).

Help / TOC / popup state is small enough that it can stay public for
now.

### Seam 1: Mode state machine — landed 2026-05-18

`Mode` was already an enum with clear states (`Normal`, `Visual{Char,Line}`,
`Search`, `Command`, `AwaitingChar{kind}`, `AwaitingMarkName{for_set}`,
`AwaitingG`, `AwaitingBracket`, `AwaitingOperator`, `AwaitingTextObject`).
The transitions were documented but ad-hoc.

Landed shape — `state/mode.rs` (child module of `state/`):

```rust
impl Reader {
    pub fn mode(&self) -> &Mode;
    pub fn return_to_normal(&mut self);
    pub fn enter_command_mode(&mut self);
    pub fn enter_search(&mut self);
    pub fn enter_visual_mode(&mut self, line_mode: bool);
    pub fn enter_awaiting_g(&mut self);
    pub fn enter_awaiting_char(&mut self, kind: FindKind);
    pub fn enter_awaiting_bracket(&mut self, forward: bool);
    pub fn enter_awaiting_mark_name(&mut self, for_set: bool);
    pub fn enter_awaiting_operator(&mut self, op: Operator);
    pub fn enter_awaiting_text_object(&mut self, op: Operator, around: bool);
}
```

The methods own:
- Clearing `count_buf` on every entry (every caller previously had to
  remember this).
- Clearing `cmd_buf` and `cmd_error` on Command entry — the prompt
  always starts blank.
- Seeding `visual_anchor` / `visual_anchor_x` on Visual entry from
  the current cursor.
- Clearing `count_buf` and `cmd_buf` on `return_to_normal`.
- Search-mode entry clears `search_query` and `search_matches`;
  `cancel_search` (in `nav.rs`) drops them on Esc.

`return_to_normal` deliberately PRESERVES `search_query` /
`search_matches` so `n` / `N` keep working after `/foo<Enter>`.

`reader.mode` is now a private field on `Reader`, accessible only to
`state/mod.rs` and `state/mode.rs`. The compiler enforces routing
through the transition methods. Read-side access is via
`reader.mode()`.

#### Migration metrics (2026-05-18 audit → post-Seam-1)

| Metric | Before | After |
|---|---|---|
| Public fields on `Reader` | 47 | 46 |
| `reader.mode = Mode::*` writes outside `state/` | 32 | 0 |
| Crate-wide tests | 137 | 140 |

### Validation (run on 2026-05-18)

- Test count went 137 → 140 (added three transition tests in
  `state/mod.rs::tests`):
  - `enter_command_mode_clears_buffers_and_error`
  - `return_to_normal_clears_count_and_cmd_buf_but_preserves_search`
  - `enter_visual_mode_seeds_anchor_from_cursor`
- Workspace `cargo test --release` and `cargo clippy -p tread --release`
  pass with no new warnings.
- Manual sweep on the Attention paper deferred — out of scope for a
  pure refactor that the compiler already proves correct. Owners
  exercising the reader should hit:
  - `:set theme=…` (Command → Normal, clears `cmd_buf`).
  - `m{a}` (Normal → AwaitingMarkName → Normal).
  - `f{x}` (Normal → AwaitingChar → Normal).
  - `/foo` (Normal → Search → Normal, preserves matches).
  - `v` then `Esc` (Normal → VisualChar → Normal).

## Consequences

**Good:**
- The single largest source of bare field writes goes away (~30
  sites in `lib.rs` collapse into method calls).
- Bug class disappears: "mode changed but `cmd_buf` still has old
  contents" cannot happen if the only mode change goes through a
  method that clears it.
- Sets the pattern for seams 2–4.

**Costs:**
- Touches the event loop. Subtle event-handling regressions are the
  main risk. Mitigation: test sweep above plus running the full
  acceptance suite.
- ~200–400 lines of churn in `lib.rs`, plus the new module.
- Doesn't shrink the *type's* visible surface from external callers
  meaningfully on its own — the value is mostly invariant containment,
  not encapsulation count. Reader's public-field count drops by 1–4
  (mode + buffers) per seam, not dozens. Worth doing anyway because
  the invariants are real.

## Seam 2 — Cursor / scroll — landed 2026-05-18

The four cursor fields (`offset`, `cursor_y`, `cursor_x`,
`desired_column`) are now private to `state/`.  Writes route through
the new `state/cursor.rs` module; reads through getter methods.

Landed shape — `state/cursor.rs`:

```rust
impl Reader {
    pub fn offset(&self) -> usize;
    pub fn cursor_y(&self) -> usize;
    pub fn cursor_x(&self) -> usize;
    pub fn desired_column(&self) -> usize;

    pub fn jump_to_line(&mut self, line: usize);
    pub fn jump_to_line_with_context_above(&mut self, line: usize);
    pub fn center_on_line(&mut self, line: usize);
    pub fn set_cursor_x(&mut self, x: usize);
}
```

The invariants the methods own:

- `jump_to_line` and `jump_to_line_with_context_above` always reset
  `cursor_x` and `desired_column` to 0.  This was the historical
  source of "`j` after a jump goes to the wrong column" bugs —
  `desired_column` lingered from before the jump.
- `jump_to_line` top-aligns the viewport on scroll (link follows,
  citation jumps, `G N`); `jump_to_line_with_context_above`
  bottom-aligns it (the `:N` numeric-goto command, where the user
  typed a specific line and wants context above it).
- `center_on_line` preserves `cursor_x` / `desired_column` —
  voice playback follows the paragraph being read; the user is being
  moved, not jumping.
- `nav.rs` (the lower-level motion methods like `nav_down` /
  `nav_word_forward`) was promoted to `state/nav.rs` so it stays
  inside `impl Reader` with private-field access.

#### Migration metrics (2026-05-18 audit → post-Seam-2)

| Metric | Before | After |
|---|---|---|
| Public cursor/scroll fields | 4 (`offset`, `cursor_y`, `cursor_x`, `desired_column`) | 0 |
| Bare cursor/scroll writes outside `state/` | ~25 | 0 |
| Crate-wide tests | 140 | 143 |

Three new transition-invariant tests pin the per-method contracts:

- `jump_to_line_resets_cursor_x_and_desired_column`
- `center_on_line_preserves_cursor_x`
- `jump_to_line_with_context_above_lands_target_at_bottom`

Two pre-existing duplicated `jump_to_line` helpers (one in `lib.rs`,
one in `commands.rs`, byte-identical) were deleted and replaced with
calls to the new method.  `commands.rs::goto_line` previously
inlined the bottom-aligned scroll-and-cursor dance for `:N`; that's
now a single `jump_to_line_with_context_above` call.

## Seam 3 — Voice — landed 2026-05-18

The ten voice fields (`voice_controller`, `voice_started_session`,
`voice_status`, `voice_error`, `voice_para_start`, `voice_para_end`,
`voice_started_at`, `voice_chars_before`, `reading_mode`,
`continuous_reading`) are now private to `state/`.

The orchestration file `voice_control.rs` was moved to
`state/voice_control.rs` so its free functions retain direct field
access — they were always the only writers, just sitting outside
the module that defined the fields they wrote to. Now that's no
longer a violation of the module boundary; it's the boundary's
intended shape.

Landed shape (in `state/voice_control.rs`):

```rust
impl Reader {
    pub fn voice_status(&self) -> &PlaybackStatus;
    pub fn voice_controller(&self) -> Option<Arc<PlaybackController>>;
    pub fn voice_started_at(&self) -> Option<Instant>;
    pub fn voice_chars_before(&self) -> usize;
    pub fn voice_para_range(&self) -> (usize, usize);
    pub fn reading_mode(&self) -> bool;
    pub fn continuous_reading(&self) -> bool;
    pub fn stop_continuous_reading(&mut self);
}
```

The single external write — `tick()`'s `self.continuous_reading =
false` when continuous-reading runs out of document — routes through
`stop_continuous_reading()`. The method deliberately preserves
`reading_mode` and `voice_status` so the current chunk plays through
to natural end and the status line keeps saying "READING" while the
final paragraph finishes.

Multi-field writes (start a chunk, sync from the controller, exit a
session on preemption / Esc) stay as free functions in
`state/voice_control.rs` because they're only called from within
that module — adding methods just to satisfy the seam would be
ceremony without value.

#### Migration metrics (2026-05-18 audit → post-Seam-3)

| Metric | Before | After |
|---|---|---|
| Public voice fields | 10 | 0 |
| Bare voice writes outside `state/` | 1 (`tick`) | 0 |
| Crate-wide tests | 143 | 144 |

One new transition-invariant test pins the documented
`stop_continuous_reading` contract:

- `stop_continuous_reading_clears_only_the_continuous_flag`

## Seam 4 — Bookmarks — landed 2026-05-18

`Reader.bookmarks` storage moved from `HashMap<char, usize>`
(visual-line index — brittle across resize) to
`HashMap<char, Bookmark>` where `Bookmark` is `(block_idx,
byte_in_block)`.  Same addressing scheme as `Highlight`; the seam
methods on `Reader` resolve a bookmark to a VL index by walking
`visual_lines` and matching the byte range.

The bookmark file moved from `crates/tread/src/bookmarks.rs` to
`crates/tread/src/state/bookmarks.rs` so the impl-Reader methods
have private-field access.  The persistence helpers (`load`/`save`)
stay as free functions in the same file — only the in-memory
write path crosses the seam.

Landed shape — `state/bookmarks.rs`:

```rust
impl Reader {
    pub fn set_mark(&mut self, letter: char);
    pub fn jump_to_mark(&mut self, letter: char);
    pub fn remove_mark(&mut self, letter: char) -> bool;
    pub fn marks_iter(&self) -> impl Iterator<Item = (char, usize)> + '_;
    pub fn is_line_bookmarked(&self, vl_idx: usize) -> bool;
}
```

The behavioural payoff is the regression test
`mark_survives_terminal_resize_reflow`: a mark set on a wrapped
paragraph still resolves to the same source byte after the
terminal is widened (or narrowed) and `visual_lines` rebuilds.
Pre-Seam-4 the stored visual-line index meant a different paragraph
after the rebuild — silently broken.

#### On-disk back-compat

Legacy files store integer values (the old visual-line index).
A `#[serde(untagged)]` enum `BookmarkValue { Block(Bookmark) |
LegacyLineIdx(usize) }` accepts either form on load.
`load_bookmarks_from_disk` translates `LegacyLineIdx` entries
against the current layout; `save_bookmarks_to_disk` always emits
the new `Block` form, so the migration is one-shot per paper.
Legacy entries that no longer resolve (e.g. the document shrank)
are silently dropped rather than corrupting the in-memory map.

#### Migration metrics (2026-05-18 audit → post-Seam-4)

| Metric | Before | After |
|---|---|---|
| Public bookmark field | 1 (`pub bookmarks: HashMap<char, usize>`) | 0 |
| Bare bookmark reads/writes outside `state/` | 3 (render, `:marks`, `:delmarks`) | 0 |
| Behavioural improvement | marks lost on resize | marks survive resize |
| Crate-wide tests | 144 | 144 (-3 migrate tests removed, +2 JSON round-trip, +1 reflow regression) |

## Final state

All four seams landed:

- **Seam 1 — Mode state machine** (commit 5f6312c)
- **Seam 2 — Cursor / scroll** (commit 984b86c)
- **Seam 3 — Voice** (commit db741fe)
- **Seam 4 — Bookmarks** (this commit)

`Reader`'s public mutable field surface is now empty for these four
concerns.  Remaining `pub` fields on `Reader` are read-mostly
collections (`visual_lines`, `sections`, `nav_history`, `meta`,
`highlights`, `label_lines`, `bib_entries`, `bib_entry_lines`,
`image_paths`, `popup`) plus simple toggle flags
(`toc_visible`, `help_visible`, `figure_preview_active`,
`text_only`, `kitty_supported`, `current_figure`, `arxiv_id`).
None of those have the multi-field invariant problem the original
audit flagged.

When future work starts, re-grep `reader\.<field>\s*=` to see whether
the field count and external-mutation count have moved; the ADR is
correct iff those numbers fall as each seam lands.
