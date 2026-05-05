# Features

`tread` is a terminal-native reader for arXiv papers. It fetches the LaTeX source, parses it into a typed block model, and renders it as styled, navigable text in your terminal — with vim-style motion, character-range highlights, named marks, and a command surface.

## What it does

Given an arXiv ID or URL, the reader:

1. Fetches the e-print tarball from arXiv and extracts every `.tex` file.
2. Pre-scans the source for LaTeX `\begin{tabular}` column specs (vertical rules, internal `\hline`s) so they survive the Pandoc round-trip.
3. Parses via Pandoc into a structured block list (`doc-model::Block`); falls back to a hand-rolled parser if Pandoc isn't installed.
4. Fetches the rendered PDF, extracts per-table placement anchors via `pdftotext`, and lifts each table to the position the LaTeX float algorithm chose. Falls back to source-order placement if the PDF or `pdftotext` isn't available.
5. Renders to a ratatui TUI with full styling (bold, italic, underline, monospace, colour, OSC 8 hyperlinks).

## Document features

- **Numbered sections** — `1  Introduction`, `2.1  Background`, `2.1.3  Detailed Steps`. Honours Pandoc's `unnumbered` class for `\section*{}`.
- **Numbered theorems** — `Theorem 1`, `Lemma 2`, with `∎` end markers on proofs.
- **Numbered equations** — `(1)`, `(2)`, right-justified in display math. Detects environment type: `equation`/`multline` get one number; `align`/`gather`/`eqnarray` claim one per `\\`-separated row; starred variants (`equation*`, `align*`, …) are unnumbered. Sub-envs (`split`, `aligned`, `cases`, matrix family) inherit from the parent.
- **Cross-reference resolution** — `\ref{eq:elbo}` resolves to the equation number; `\cite{vaswani}` to `[1]`.
- **Bibliography** — full references section with formatted entries.
- **Math rendering** — Greek letters, calculus, relations, set theory, arrows, fractions, square roots, super/subscripts. Compact one-line Unicode rendering with `dₘₒdₑₗ⁻⁰·⁵`-style super/subscripts; `\frac{a}{b}` → `a/b`, `\sqrt{x}` → `√x`. Disambiguation brackets `⁽…⁾` wrap superscripts containing fraction slashes so `pos/10000⁽²ⁱ⁄ᵈₘₒdₑₗ⁾` reads cleanly next to outer division. Decorative `\\` line-breaks in single-equation envs (`equation`, `split`) collapse to a space; multi-equation envs (`align`, `gather`, `eqnarray`) preserve the row separators.
- **Tables** — booktabs-style horizontal rules by default. Vertical rules from `tabular` column specs (`c|cccccc|ccc`). Body-internal `\hline` / `\specialrule` separators. Multi-row headers with proper spanning. Centered horizontally on screen.
- **Captions above tables** (academic convention); below figures.
- **Inline pixel figures** — on terminals that speak the Kitty graphics protocol (Kitty, WezTerm, Ghostty, iTerm2 3.5+), figures render as actual images via the protocol with PDF→PNG conversion through `pdftoppm`. Single figures, side-by-side subfigures, and stacked panels are detected from the LaTeX source structure (minipages, `\\` separators, `width=\textwidth`). Aspect-ratio-aware sizing scales each panel to fit the terminal. Captions render beneath, centered. On non-graphics terminals, figures degrade to `[Figure N: caption]` text — same behaviour as before pixel graphics.
- **Algorithms / pseudocode** — `algorithmic` environments rendered as plain-text indented blocks.
- **Code listings** — `lstlisting` and `verbatim` environments as `Block::CodeBlock` with language tags.
- **Lists** — bulleted, numbered, nested.
- **Quotes** — italic, indented `\begin{quote}` / `\begin{quotation}` / `epigraph`.
- **Hyperlinks** — `\url{}` and `\href{}` rendered as OSC 8 clickable links with underline fallback.

## Reader features

- **Per-paper persistence** — reading position, named marks, and highlights all save automatically to `~/.config/trench/` keyed by arXiv ID. Re-launch and you're back where you were.
- **Section-jump navigation** — `[` / `]` to walk section headers; `:goto 3.2` or `:goto Introduction` to jump by number or name.
- **Toggleable TOC panel** — `\` opens/closes a side pane listing every section header; current section is highlighted as you scroll.
- **Back-navigation stack** — every jump (search, section, mark, sentence) pushes onto a history; `Ctrl+O` rewinds.
- **Header bar** — title and authors pinned above the content area when paper metadata is available.
- **Help overlay** — `?` for a compact keybinding reference.
- **Visual mode** — `v` (char) and `V` (line) selection; `y` yanks to clipboard via OSC 52, `H` commits as a persistent highlight.
- **Text objects** — vim-style `yi<obj>` / `ya<obj>` for word, quote, paren/bracket/brace pair, paragraph, and sentence. `yy` yanks the current line.
- **Cross-reference jumping** — section/figure/table refs and citations render with the link colour and an underline. `Enter` on one jumps to the target (one line above so the target is fully visible); `Ctrl+O` rewinds. `K` (or `Shift+Enter` on supporting terminals) shows a citation's bib entry in a popup without leaving your reading position. Phrases like "Table 3" and "Section 6.2" are styled as a whole, not just the number.
- **Search** — `/` forward, `*` to search the word under the cursor, `n` / `N` to step.
- **Bookmarks (vim-style marks)** — `m{a}` to set, `'{a}` / `` `{a} `` to jump. Letter-keyed slots `a..z`. Marked lines tinted amber.
- **Persistent character-range highlights** — `H` in visual mode commits a selection as a highlight; `X` removes the one under the cursor. Stored at block-byte granularity so they survive terminal resize.
- **Command mode** — `:` opens an Ex-style command bar (see [hotkeys.md](hotkeys.md) for the command list). Includes `:reload` to re-fetch + re-parse the current paper while preserving cursor / scroll / bookmarks / highlights.
- **Voice / TTS playback** — see [Voice / TTS](#voice--tts) below.
- **Themes** — 16 built-in themes (Dark, Light, AMOLED, Solarized, Gruvbox, Nord, Tokyo Night, Catppuccin Mocha, Powder family). `:set theme=<id>` switches at runtime; `:set theme=trench` syncs with the trench feed UI's theme.

## Voice / TTS

Read papers aloud with `r` (current paragraph), `R` (cursor → paragraph end), or `Ctrl+P` (continuous from current paragraph onward). `Space` pauses, `Esc` stops. While playing, the active paragraph stays at full brightness, surrounding lines dim, and the word currently being spoken is highlighted.

Three providers, with automatic fallback:

| Provider | When chosen |
|---|---|
| **ElevenLabs** | `ELEVENLABS_API_KEY` is set in the environment AND a `voice_id` is configured |
| **macOS `say`** | Default fallback. Uses `Samantha` unless `say_voice` is overridden. |
| **Piper** | Offline TTS. Set `tts_provider = "piper"` and configure `piper_binary` + `piper_model`. |

Configure via the `voice` block in `~/.config/trench/block_reader.json`:

```json
{
  "voice": {
    "voice_id": "21m00Tcm4TlvDq8ikWAM",
    "tts_provider": "",
    "say_voice": "Samantha",
    "piper_binary": "",
    "piper_model": "",
    "playback_speed": 1.0
  }
}
```

`tts_provider = ""` lets the auto-fallback chain choose. The `ELEVENLABS_API_KEY` is read from the environment only; never persisted to the JSON file (security policy).

## Where state lives

| Data | Path |
|---|---|
| Reading progress | `~/.config/trench/reader_progress.json` |
| Bookmarks (marks) | `~/.config/trench/bookmarks_<arxiv-id>.json` |
| Highlights | `~/.config/trench/highlights_<arxiv-id>.json` |
| Theme override + voice config | `~/.config/trench/block_reader.json` |
| Trench's theme (read-only sync) | `~/.config/trench/config.json` |
| Source / asset cache (per arXiv ID) | `~/.cache/tread/sources/<arxiv-id>/` |
| Rasterised PDF figures (cached by hash) | `~/.cache/tread/figures/` |

## Environment variables

| Variable | Purpose |
|---|---|
| `ELEVENLABS_API_KEY` | Voice playback via ElevenLabs. Read once at startup; not persisted. |
| `TREAD_DISABLE_KITTY_GRAPHICS` | Force the text-only image fallback even on graphics-capable terminals. |
| `TREAD_FORCE_KITTY_GRAPHICS` | Override capability detection inside tmux when env-var hints don't survive the multiplexer. |
| `TREAD_TRACE_IMAGES` | Stderr trace of image placements per frame (gated; debug-only). |

## Dependencies

- **Pandoc** (recommended) — for high-fidelity LaTeX parsing. Install via `brew install pandoc` on macOS. The reader falls back to a hand-rolled parser if Pandoc isn't on `PATH`.
- **Poppler's `pdftotext`** (recommended) — for PDF-anchored table placement. Install via `brew install poppler`. The reader falls back to source-order placement if missing.
- **Poppler's `pdftoppm`** (recommended for pixel figures) — converts PDF figures to PNG for inline rendering. Same `brew install poppler` provides this. Without it, PDF figures degrade to `[Figure N: caption]` text.
- A modern terminal with 24-bit colour and OSC 52 / OSC 8 support recommended (iTerm2, Kitty, WezTerm, modern macOS Terminal). Plain `xterm` works but loses styling fidelity.
- **Optional: tmux passthrough** — for inline pixel graphics inside tmux, add `set -g allow-passthrough on` and `set -g focus-events on` to `~/.tmux.conf`. Without this, figures fall back to text placeholders even on Kitty-protocol-capable terminals.
- **Optional: ElevenLabs API key** — for premium voice quality. Without it, voice falls back to macOS `say`.

## Run

```sh
cargo run --release -p tread -- <arxiv-id-or-url>
```

Examples: `cargo run --release -p tread -- 1706.03762`, or `… -- https://arxiv.org/abs/2005.14165`.
