# tread — internal development guide

Local working notes for the dev workflow of this repo. Companion to `features.md` (user-facing) and `todo.md` (working notes).

## Workspace layout

```
crates/
├── doc-model/         — Block, VisualLine, build_visual_lines (no I/O, no parsing)
├── arxiv-render/      — fetch + parse + table-placement; pulls source from arXiv
│   ├── fetch.rs       — fetch_ar5iv (HTML) + fetch_source (tarball) + fetch_pdf
│   ├── ar5iv_parse.rs — primary parser, walks ar5iv (LaTeXML) HTML DOM
│   ├── pandoc_parse.rs — fallback parser, walks Pandoc JSON AST on the tarball
│   ├── bibitems.rs    — scrape \bibitem{key} entries directly from LaTeX
│   ├── pdf_anchors.rs — extract per-table placement anchors from PDF text
│   └── placement.rs   — lift Block::Matrix groups to PDF-rendered position
├── block-reader/      — TUI reader; bin entry at main.rs
│   ├── state.rs       — Reader struct, Mode enum
│   ├── nav.rs         — all motion methods (impl Reader)
│   ├── render.rs      — ratatui rendering pipeline
│   ├── lib.rs         — event loop + handlers + run() entry
│   ├── commands.rs    — Ex-command parser + dispatcher
│   ├── config.rs      — theme persistence + trench sync
│   ├── highlights.rs  — character-range highlight storage
│   ├── bookmarks.rs   — multi-letter mark storage
│   └── progress.rs    — reading-position storage
├── math-render/       — tui-math wrapper for display + inline math
└── ui-theme/          — Theme struct, ThemeId enum (16 themes)
```

## Critical invariants

### `wrap_spans` does whitespace normalization

`doc-model::wrap_spans` (and `wrap_list_item`) **do not** produce output where byte offsets map 1:1 to the input. They split spans on whitespace and rejoin words with single spaces. Consequence: "block text" for byte-addressing purposes is the *concatenation of wrapped-line plain texts joined by single spaces*, not the original `Block::StyledLine` contents.

This affects highlights, cursor positions, and anywhere else byte offsets cross the wrap boundary.

### `cursor_x` vs `desired_column`

- `cursor_x: usize` — the *effective* byte column on the current line. Always the rendered position. All horizontal motions read and write here.
- `desired_column: usize` — vim's `curswant`. Set by horizontal motions via `remember_column()`. Vertical motions call `clamp_cursor_after_line_change()` which sets `cursor_x = desired_column.min(new_line.len() - 1)`.

This is what makes `j` from a 60-char line through short ones and back to a long line return to col 60.

### Block-byte vs visual-line-byte

Highlights, the cursor, and any rendering that needs to project content onto a visual line use `VisualLine.block_byte_start / block_byte_end`. These describe the byte range of the parent block's canonical (post-wrap) text that this visual line covers. **Stable across resize**; visual-line indices are not.

### Theme is owned by the event loop

`event_loop` takes `mut theme: Theme` (owned, in scope). `render::draw` always reads `&theme`. `:set theme=…` returns `CommandResult::ChangeTheme(new)`; the event loop reassigns `theme = new`. Keeping it out of `Reader` avoided spreading swap logic across every render call.

### `Mode::Command` reuses the search-bar slot

The split layout treats `Mode::Search | Mode::Command` as the same case — both allocate a 1-row bar at the bottom. Different prefix glyph (`/` vs `:`), same shape. New modes that need a similar input bar should join this group.

### One-shot modes follow the AwaitingChar pattern

`Mode::AwaitingChar { kind: FindKind }`, `Mode::AwaitingMarkName { for_set }`, `Mode::AwaitingG` all share a shape:
- Pressing the prefix key (`f`, `m`, `g`) sets the mode.
- The next keystroke is consumed by a dedicated `handle_awaiting_*` function.
- Always returns to `Mode::Normal` afterwards (no escape needed; non-matching keys cancel silently).

Adding a new prefix follows the same template.

### Popup dismissal is centralized

`Reader.popup: Option<PopupContent>` is set by commands like `:marks`, `:about`. The event loop has a single check at the top of every key event: if popup is open, clear it and skip the keystroke. So commands never need to manage popup lifecycle — they just write to the field.

### `cmd_error` is a one-tick override

When a command returns `CommandResult::Error(msg)`, the event loop stashes it on `Reader.cmd_error`. The status bar's draw function checks this field and renders the error in red instead of the normal status. Cleared at the top of the next key event. So errors are visible until the user does anything else — no timer needed.

## Persistence layout

| Data | Path |
|---|---|
| Reading progress | `~/.config/trench/reader_progress.json` (shared map keyed by arXiv ID) |
| Bookmarks (marks) | `~/.config/trench/bookmarks_<id>.json` (one file per paper) |
| Highlights | `~/.config/trench/highlights_<id>.json` |
| Theme override | `~/.config/trench/block_reader.json` |
| Trench's theme (read-only) | `~/.config/trench/config.json` |

All loaded from `Reader::new` in `lib.rs::run`; saved on clean exit. Highlights/marks are fire-and-forget on save (no atomic rename); a crash mid-write loses the latest entry, not all entries.

The trench config is read **non-invasively via `serde_json::Value`**, not by importing trench's `Config` struct — that would create a circular dependency (block-reader can't depend on the trench binary). Only the `theme` field is consumed.

## Working with the codebase

### Build commands

```bash
cargo build -p block-reader --release    # main TUI binary
cargo test -p block-reader -p arxiv-render --release
cargo run -p block-reader --release -- 1706.03762
```

### Adding a new motion

1. Add a method to `nav.rs` `impl Reader { ... }`.
2. If it changes the line: call `self.clamp_cursor_after_line_change()` after writing `cursor_y` / `offset`.
3. If it changes the column: at the binding site in `lib.rs::handle_normal`, call `reader.remember_column()` after the motion completes (handles count loops cleanly).
4. Bind the key in `handle_normal`.
5. Add a row to the help overlay in `render.rs::draw_help_overlay`.
6. If you want it accessible from command mode, add a thin command wrapper in `commands.rs::command_table`.

### Adding a new command

1. Write a free function `fn cmd_foo(reader: &mut Reader, ctx: &CmdCtx, args: &[&str]) -> CommandResult` in `commands.rs`.
2. Register it in `command_table()` with `(canonical_name, &[aliases], cmd_foo)`.
3. If the command needs a new effect (not Quit / ChangeTheme / OpenHelp / Error), add a variant to `CommandResult` and handle it in `event_loop`.
4. Add it to `cmd_rows` in `draw_help_overlay` and to `hotkeys.md`.

### Adding a new theme

1. Add the variant to `ThemeId` in `crates/ui-theme/src/lib.rs`.
2. Add a constructor method (`pub fn my_theme() -> Self { ... }` returning a fully-populated `Theme`).
3. Wire it into `ThemeId::all()`, `ThemeId::label()`, `ThemeId::from_id()`, `ThemeId::theme()`.
4. Test by `:set theme=<your-id>` after build.

The 16 existing themes use a consistent `bg_highlight` of `Rgb(80, 60, 0)` for dark or `Rgb(255, 240, 180)` for light. New themes should pick something visible against `bg`.

### Verifying changes end-to-end

The benchmark paper is **`1706.03762`** (Attention Is All You Need). It exercises:
- Tables (1, 2, 3, 4 — Table 3 has vertical rules `c|cccccc|ccc`)
- Multi-section structure with numbered headers
- Cross-references and citations
- Inline and display math
- Footnotes
- PDF-anchored placement (all 4 tables get anchored)

Other benchmarks in `test-papers.txt`. Run any with `cargo run -p block-reader --release -- <id>`.

The integration smoke test for the PDF anchor extractor is in `arxiv-render/src/pdf_anchors.rs::tests::attention_pdf_anchors` — gated by `ATTENTION_PDF` env var to avoid network in CI:

```bash
ATTENTION_PDF=/tmp/tread-pdf-investigation/attention.pdf \
  cargo test -p arxiv-render attention_pdf_anchors --release -- --ignored --nocapture
```

Parser+layout goldens live in `arxiv-render/src/lib.rs::golden_tests` and mechanize the smoke claims in ADRs 0005 / 0007 (block / visual-line counts, table vertical rules, section structure).  Each is `#[ignore]`-gated so default `cargo test` skips them; each needs pandoc on PATH + network on first run (subsequent runs hit `~/.cache/tread/sources/`).

Pinned papers and their EXPECTED counts (Pandoc-fallback path):

| arXiv ID | Test | Blocks | Visual lines | Stress |
|---|---|---:|---:|---|
| 1706.03762 | `attention_parse_and_layout_golden` | 379 | 675 | Tables (Table 3 `c\|ccccccccc\|ccc`), 5 figures, external `\bibliography{}` (no .bib in tarball) |
| 2005.14165 | `gpt3_parse_and_layout_golden` | 1422 | 3138 | 50+ source-file `\input{}` resolution |
| 1707.09763 | `differential_algebra_parse_and_layout_golden` | 530 | 1679 | Math-heavy (≥30 numbered DisplayMath) |
| 2605.04035 | `gaussian_head_parse_and_layout_golden` | 789 | 1496 | External `\bibliography{}` WITH `.bib` shipped — pins the B3a bib-reader path (References auto-append from `bibtex::extract_bibtex_entries`) |

When parser or layout changes shift the counts intentionally, bump the per-test EXPECTED_BLOCKS / EXPECTED_VISUAL_LINES constants and note the new baseline in the ADR that justified the change.  The shared `parse_and_check_block_count` + `assert_visual_line_count` helpers in the module keep new-paper additions short.

Known gap: arXiv:2602.06006 fails Pandoc parse on `\newcolumntype{C}[1]{>{\centering\arraybackslash}p{#1}}` — no Pandoc-path golden today; would need an ar5iv-path golden (separate pipeline using `ar5iv_parse::to_blocks`).

```bash
# Run a single golden:
cargo test -p arxiv-render attention_parse_and_layout_golden --release -- --ignored --nocapture
# Run every golden:
cargo test -p arxiv-render golden --release -- --ignored --nocapture
```

## Known gotchas

- **Parser chain**: ar5iv (LaTeXML HTML at `ar5iv.labs.arxiv.org`) is primary; Pandoc on the e-print tarball is the fallback for papers ar5iv hasn't processed. Pandoc must be on PATH for the fallback path; without it, ar5iv-miss papers fail loudly. Empirical justification under `bench/results/`.
- **`pdftotext` must be on PATH** for table placement. Falls back to source-order if missing.
- **`open` command is macOS-specific.** `:open` falls back to `xdg-open` on non-macOS but isn't tested on Linux.
- **Mode::Search and Mode::Command share a layout slot.** If you add a third input mode, group it with these in `split_layout`.
- **`cursor_x` can drift past `vl.text.len() - 1`** in rare cases. The `apply_char_cursor` snap defends rendering; horizontal motions clamp; vertical motions use `desired_column`. Do not write `cursor_x` directly without thinking about clamping.
- **`block_byte_start == block_byte_end == 0`** indicates a non-text block (Matrix, Rule, Blank). Highlights and selections must skip these.
- **Theme changes don't trigger a redraw** automatically — the next event triggers redraw. So a `:set theme=...` followed by no input would leave stale colors momentarily. Acceptable.

## Files modified vs. read-only zones

Don't modify `crates/math-render/`, `Cargo.toml` workspace, or `crates/ui-theme/` lightly — they're shared with trench. The trench worktree depends on `path` references back into these.

Doc-model is the public block API; changing `VisualLine` requires sweeping every `VisualLine { ... }` construction site (there are ~12 in `build_visual_lines`).
