# ADR-0004 — Reader public surface

- **Status:** Proposed (2026-05-18) — finding + plan, not yet acted on
- **Crate:** `tread` (`state.rs` and every other file in the crate)
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

### Seam 1: Mode state machine

`Mode` is already an enum with clear states (`Normal`, `Visual{Char,Line}`,
`Search`, `Command`, `AwaitingChar{kind}`, `AwaitingMarkName{for_set}`,
`AwaitingG`). The transitions are documented but ad-hoc.

Proposed shape:

```rust
// New module: crates/tread/src/mode.rs (or modes/mod.rs).
impl Reader {
    pub fn enter_mode(&mut self, next: Mode);
    pub fn return_to_normal(&mut self);
    // … one method per *named* transition family.
}
```

The methods own:
- Clearing `count_buf`, `cmd_buf`, `search_query` when the new mode
  implies they reset.
- Clearing `cmd_error` on any mode change (the existing one-tick
  rule).
- Centralizing the "exit visual mode" cursor restoration.

External code at every `reader.mode = Mode::X` site replaces the bare
write with the corresponding method call. The compiler walks the
migration: make `mode` private, fix call sites until it compiles.

### Validation

- Test count: 137 (current) → ≥ 137 (no regressions). Add at least
  three transition tests: visual → normal restores cursor; command →
  normal clears `cmd_buf`; search → normal preserves matches.
- Manual sweep on the Attention paper:
  - `:set theme=…` (Command → Normal, clears `cmd_buf`).
  - `m{a}` (Normal → AwaitingMarkName → Normal).
  - `f{x}` (Normal → AwaitingChar → Normal).
  - `/foo` (Normal → Search → Normal, preserves matches).
  - `v` then `Esc` (Normal → VisualChar → Normal, cursor restored).

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

## Open follow-up

After seam 1, the remaining seams in this ADR's priority order:

- **Seam 2 — Cursor / scroll.** Migrate the dozen-plus
  `reader.offset = …` / `reader.cursor_y = …` / `reader.cursor_x = …`
  sites onto `cursor::jump_to_line` / `cursor::scroll_by` etc.
  Likely-touched files: `lib.rs`, `commands.rs`, `nav.rs`.
- **Seam 3 — Voice.** 8 fields collapse behind a `voice::*` module
  surface. Likely-touched files: `voice_control.rs`, `voice/playback.rs`,
  `lib.rs`.
- **Seam 4 — Bookmarks.** Change storage from
  `HashMap<char, usize>` (visual-line index) to block-byte addressing
  like highlights, behind `BookmarkSet`. Survives reflow as a side
  benefit.

When future work starts, re-grep `reader\.<field>\s*=` to see whether
the field count and external-mutation count have moved; the ADR is
correct iff those numbers fall as each seam lands.
