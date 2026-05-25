# Hotkeys

`tread` follows vim conventions. Bindings are organized by mode below, with deviations from vim's defaults flagged.

## Movement (Normal mode)

### Single-line motion

| Key | Action |
|---|---|
| `h` / `←` | Cursor left (wraps to end of previous line at column 0) |
| `l` / `→` | Cursor right (wraps to start of next line at end of line) |
| `0` | Jump to byte 0 of the current line |
| `^` | Jump to first non-whitespace char |
| `$` | Jump to last char of the current line |

### Word motion

| Key | Action |
|---|---|
| `w` / `W` | Forward to next word start (small / BIG word). Wraps across lines. |
| `b` / `B` | Backward to previous word start |
| `e` / `E` | Forward to current/next word end |
| `ge` / `gE` | Backward to previous word end |

Small word: alphanumeric + `_`. BIG word: any non-whitespace run.

### Line / page / document

| Key | Action |
|---|---|
| `j` / `↓` | One line down (preserves desired column across short lines) |
| `k` / `↑` | One line up |
| `gg` | Jump to top of document |
| `G` | Jump to bottom (or `<N>G` to line N) |
| `Ctrl+D` / `Ctrl+U` | Half-page down / up |
| `PageDown` / `PageUp` | Full page |
| `H` | Top of visible screen |
| `M` | Middle of visible screen |
| `L` | Bottom of visible screen |
| `z` | Center the cursor's line |

### Paragraph / section / sentence

| Key | Action |
|---|---|
| `}` | Next paragraph |
| `{` | Previous paragraph |
| `]]` | Next section header (was `]`; second key disambiguates from `]f`) |
| `[[` | Previous section header |
| `)` | Next sentence (cross-line) |
| `(` | Previous sentence |
| `Ctrl+O` | Back to previous position (unwinds jumps) |

### Figure preview pane

| Key | Action |
|---|---|
| `i` | Toggle figure-preview side pane (reader+figure split; figure pane defaults to 40%, adjustable via `:set preview=<n>`). Sticky default persisted in `block_reader.json`. |
| `]f` | Step to next figure |
| `[f` | Step to previous figure |

### Find / search

| Key | Action |
|---|---|
| `f<char>` | Forward to next occurrence of `<char>` on this line |
| `F<char>` | Backward to previous occurrence |
| `t<char>` | Forward, land *before* the match |
| `T<char>` | Backward, land *after* the match |
| `%` | Jump between matching brackets `()` `[]` `{}` |
| `*` | Search forward for the word under the cursor |
| `/` | Open search bar (Enter to confirm, Esc to cancel) |
| `n` / `N` | Next / previous search match |

### Counts

| Pattern | Effect |
|---|---|
| `<N><motion>` | Repeat motion N times — e.g. `5j`, `3w`, `10G`, `2)` |

## Marks (vim-style)

| Key | Action |
|---|---|
| `m<letter>` | Set mark `<letter>` at current line (a–z) |
| `'<letter>` | Jump to mark `<letter>` |
| `` `<letter> `` | Same as `'<letter>` (vim distinguishes; we don't) |

Marks persist across sessions per arXiv ID.

## Visual mode

| Key | Action |
|---|---|
| `v` | Enter character-visual mode |
| `V` | Enter line-visual mode |
| `j` / `k` / `h` / `l` / `w` / `b` / `e` | Extend selection |
| `y` | Yank selection to clipboard via OSC 52 |
| `H` | Commit selection as a persistent highlight |
| `Esc` / `v` / `V` | Cancel visual mode |

## Highlights

| Key | Mode | Action |
|---|---|---|
| `H` | Visual | Commit current selection as a highlight (one per block touched) |
| `X` | Normal | Remove the highlight whose range contains the cursor |

Highlights persist across sessions and survive terminal resize (stored at block-byte offsets, not visual-line offsets).

## Cross-references and citations

Section/figure/table refs and bibliography citations render with an underline (same treatment as external URLs). Phrases like "Table 3" and "Section 6.2" are uniformly styled — the prefix word is included, not just the number. Place the cursor on one and:

| Key | Action |
|---|---|
| `Enter` | Jump to the labeled element (one line above so it's fully visible). Cite jumps to the bib entry. `Ctrl+O` rewinds. |
| `K` | Popup the bib entry text without leaving your reading position. Citation-only. Lowercase `k` is cursor-up; uppercase `K` (Shift+k) is this binding — they're distinct. |
| `Shift+Enter` | Same as `K`, on terminals that support the kitty keyboard protocol (Kitty, WezTerm, Ghostty, iTerm2 3.5+ with "Report modifiers using CSI u" enabled). |

Internal refs that point to elements we don't index yet (e.g. equation labels) are silent no-ops on `Enter`.

## Other

| Key | Action |
|---|---|
| `\` | Toggle TOC side panel |
| `?` | Toggle help overlay |
| `m` / `q` / `Esc` | Quit (Esc / q work outside Command/Visual/Search/Reading) |
| `yy` | Yank current line to clipboard (OSC 52) |
| `yi<obj>` | Yank **inner** text object (see [Text objects](#text-objects)) |
| `ya<obj>` | Yank **around** text object |

## Voice / TTS playback

| Key | Action |
|---|---|
| `r` | Read the current paragraph aloud. Press again to re-read |
| `R` | Read from the cursor to the end of the current paragraph |
| `Ctrl+P` | Continuous reading — auto-advances paragraph by paragraph |
| `Space` | Pause / resume (only in reading mode) |
| `c` | Recenter the viewport on the cursor (only in reading mode) |
| `Esc` | Stop playback and exit reading mode |

While playing, the active paragraph stays at full brightness, surrounding lines dim, and the word currently being spoken is highlighted. The status bar shows `[♪ Playing]`, `[⏸ Paused]`, `[⠋ Loading]` (animated spinner during network round-trip), or `[Voice: <error>]` if synthesis fails.

Provider auto-selection (no config needed):
1. **ElevenLabs** if `ELEVENLABS_API_KEY` is set in the environment AND a `voice_id` is configured.
2. **macOS `say`** (default `Samantha` voice) — universal fallback.

Override with `tts_provider` in the `voice` section of `~/.config/trench/block_reader.json`. Available providers: `"elevenlabs"`, `"say"`, `"piper"`. See [features.md](features.md#voice--tts) for the full config schema.

## Text objects

After pressing `y`, the reader enters operator-pending mode. Then:

- `y` again → yank the current line (vim's `yy`).
- `i<obj>` → yank **inner** text object (excludes delimiters).
- `a<obj>` → yank **around** text object (includes delimiters / trailing whitespace).
- Any other key → cancel.

| `<obj>` | Object |
|---|---|
| `w` / `W` | small word / BIG word |
| `"` / `'` / `` ` `` | quote-delimited region |
| `(` / `)` / `b` | parenthesized group (handles nesting) |
| `[` / `]` | bracket pair |
| `{` / `}` / `B` | brace pair |
| `p` | paragraph |
| `s` | sentence |

Text objects that don't apply to the cursor's location are silent no-ops (e.g. `yi"` on an unquoted line). Single-line objects (word, quote, pair) operate on the current visual line; multi-line objects (paragraph, sentence) walk neighbouring lines.

> **Note:** bare `y` no longer yanks the line directly — that was a pre-text-objects shortcut. The vim-canonical `yy` retains that behaviour.

## Command mode

Press `:` to open the command bar. Type, then `Enter` to execute or `Esc` to cancel. Backspace at column 0 also exits.

### Built-in commands

| Command | Aliases | Action |
|---|---|---|
| `:quit` | `:q`, `:exit` | Quit |
| `:<N>` | — | Jump to line N (1-indexed) |
| `:goto <N|N.M|text>` | `:g` | Jump to section by number (`3.2`) or case-insensitive substring of header text |
| `:abstract` | — | Jump to the Abstract section |
| `:references` | `:bib`, `:r` | Jump to References / Bibliography |
| `:set theme=<id>` | — | Change theme (see [Themes](#themes) below) |
| `:set width=<n>` | — | Cap the body-text reading column at `<n>` cells (centred); `0` / `off` flows edge-to-edge. Default `72`. Persists. |
| `:set preview=<n>` | — | Give the figure-preview pane `<n>`% of the width (`20`–`70`); the reader text gets the rest. Default `40`. Persists. |
| `:marks` | — | Popup listing all set marks with their lines and snippets |
| `:delmarks <letter>` | `:dm` | Delete one or more marks |
| `:highlights` | `:hl` | Popup listing all highlights in this paper |
| `:about` | — | Popup with paper metadata (title, authors, arXiv ID, URL) |
| `:url` | `:link` | Copy `https://arxiv.org/abs/<id>` to clipboard |
| `:cite` | `:bibtex` | Copy a minimal BibTeX entry to clipboard |
| `:open` | — | Open the arXiv abstract URL in the system browser |
| `:back` | `:bk` | Same as `Ctrl+O` |
| `:help` | `:h` | Open help overlay |
| `:toc` | `:tree` | Toggle the TOC side panel (passive; follows reading position) |
| `:contents` | `:cont` | Open the full-screen contents view — `j`/`k` browse, `Enter` jumps to a section, `Esc` closes |
| `:reload` | `:e` | Re-fetch source and re-parse the paper in place. Preserves cursor, scroll, bookmarks, highlights. |
| `:placement` | — | Diagnostic popup showing each parsed Matrix table's block index and caption |

### Themes

Pass any `<id>` from this list to `:set theme=<id>`:

`dark`, `light`, `amoled`, `solarized-dark`, `solarized-light`, `gruvbox-dark`, `nord`, `tokyo-night`, `catppuccin-mocha`, `powder-blue`, `powder-sage`, `powder-lavender`, `powder-rose`, `powder-mint`, `powder-sand`, `powder-slate`.

Special value: **`:set theme=trench`** — sync with whatever theme the trench feed UI is using. This is the default if no override has been set.

The chosen theme persists across sessions in `~/.config/trench/block_reader.json`.

## Status bar indicators

The bottom row shows `<line>/<total>  <pct>%` plus, when relevant:

- `[1/3]` — current search match index of N total
- `VISUAL` / `VISUAL LINE` — active visual mode
- `f_`, `t_`, `m_`, `'_`, `g_`, `y_`, `yi_`, `ya_` — awaiting the next keystroke for a multi-key motion or text object
- `5_` — count prefix in progress (e.g. you've typed `5` waiting for a motion)
- `[♪ Playing]` / `[⏸ Paused]` / `[⠋ Loading]` — voice playback status
- `[Voice: <error>]` — the TTS provider failed (missing API key, audio device unavailable, etc.)
- A red error message — the most recent `:` command failed; clears on next keystroke

## Modes at a glance

```
Normal ──:──> Command ──Enter──> dispatch + back to Normal
Normal ──/──> Search  ──Enter──> match navigation
Normal ──v──> Visual  ──Esc/v──> back to Normal
Normal ──f──> AwaitingChar ──<char>──> jump + back to Normal
Normal ──m──> AwaitingMarkName ──<letter>──> set + back to Normal
Normal ──g──> AwaitingG ──{g,e,E}──> motion + back to Normal
Normal ──y──> AwaitingOperator ──y──> yank line + back to Normal
                              ──i/a──> AwaitingTextObject ──<obj>──> yank + back
```
