# Features

`tread` is a terminal-native reader for arXiv papers. It fetches the LaTeX source, parses it into a typed block model, and renders it as styled, navigable text in your terminal — with vim-style motion, character-range highlights, named marks, and a command surface.

## What it does

Given an arXiv ID or URL, the reader:

1. Fetches the **ar5iv** LaTeXML-rendered HTML from `ar5iv.labs.arxiv.org` and walks the DOM into a structured block list (`doc-model::Block`).  This is the primary path for the ~95% of papers ar5iv has processed.
2. For the long tail ar5iv hasn't reached yet (or where the prototype parser can't make sense of the output), falls back to: fetch the e-print tarball, pre-scan `\begin{tabular}` column specs (vertical rules, internal `\hline`s) so they survive the round-trip, then run Pandoc and walk the JSON AST.
3. On the Pandoc fallback path only: fetches the rendered PDF, extracts per-table placement anchors via `pdftotext`, and lifts each table to the position the LaTeX float algorithm chose.  Falls back to source-order placement if the PDF or `pdftotext` isn't available.  (The ar5iv source already has tables in render order, so this step is a no-op there.)
4. Renders to a ratatui TUI with full styling (bold, italic, underline, monospace, colour, OSC 8 hyperlinks).

## Document features

- **Numbered sections** — `1  Introduction`, `2.1  Background`, `2.1.3  Detailed Steps`. Honours Pandoc's `unnumbered` class for `\section*{}`.
- **Numbered theorems** — `Theorem 1`, `Lemma 2`, with `∎` end markers on proofs.
- **Numbered equations** — `(1)`, `(2)`, right-justified in display math. Detects environment type: `equation`/`multline` get one number; `align`/`gather`/`eqnarray` claim one per `\\`-separated row; starred variants (`equation*`, `align*`, …) are unnumbered. Sub-envs (`split`, `aligned`, `cases`, matrix family) inherit from the parent.
- **Cross-reference resolution** — `\ref{eq:elbo}` resolves to the equation number; `\cite{vaswani}` to `[1]`.
- **Bibliography** — full references section with formatted entries.
- **Math rendering** — Greek letters, calculus, relations, set theory, arrows, fractions, square roots, super/subscripts. Compact one-line Unicode rendering with `dₘₒdₑₗ⁻⁰·⁵`-style super/subscripts; `\frac{a}{b}` → `a/b`, `\sqrt{x}` → `√x`. Disambiguation brackets `⁽…⁾` wrap superscripts containing fraction slashes so `pos/10000⁽²ⁱ⁄ᵈₘₒdₑₗ⁾` reads cleanly next to outer division. Decorative `\\` line-breaks in single-equation envs (`equation`, `split`) collapse to a space; multi-equation envs (`align`, `gather`, `eqnarray`) preserve the row separators.
- **Tables** — booktabs-style horizontal rules by default. Vertical rules from `tabular` column specs (`c|cccccc|ccc`). Per-column text alignment (`l`/`c`/`r`) is honoured — numeric columns right-align instead of everything flushing left. Body-internal `\hline` / `\specialrule` separators. Multi-row headers with proper spanning. Centered horizontally on screen.
- **Captions above tables** (academic convention); below figures.
- **Inline pixel figures** — on terminals that speak the Kitty graphics protocol (Kitty, WezTerm, Ghostty, iTerm2 3.5+), figures render as actual images. On the ar5iv primary path, the LaTeXML-rendered PNGs are downloaded directly from `ar5iv.labs.arxiv.org` and cached under `~/.cache/tread/ar5iv-assets/`; on the Pandoc fallback path, PDF figures are converted via `pdftoppm`. Subfigures render side-by-side, and on the Pandoc path stacked panels are detected from the LaTeX source structure (minipages, `\\` separators, `width=\textwidth`). Aspect-ratio-aware sizing scales each panel to fit the terminal. Captions render beneath, centered. On non-graphics terminals, figures degrade to `[Figure N: caption]` text.
- **Algorithms / pseudocode** — `algorithmic` environments rendered as plain-text indented blocks.
- **Code listings** — `lstlisting` and `verbatim` environments as `Block::CodeBlock` with language tags.
- **Lists** — bulleted, numbered, nested.
- **Quotes** — italic, indented `\begin{quote}` / `\begin{quotation}` / `epigraph`.
- **Hyperlinks** — `\url{}` and `\href{}` rendered as OSC 8 clickable links with underline fallback.

## Reader features

- **Per-paper persistence** — reading position, named marks, and highlights all save automatically to `~/.config/trench/` keyed by arXiv ID. Re-launch and you're back where you were.
- **Section-jump navigation** — `[` / `]` to walk section headers; `:goto 3.2` or `:goto Introduction` to jump by number or name.
- **Toggleable TOC panel** — `\` opens/closes a side pane listing every section header; the current section is marked with a `▸` and its ancestor sections (the breadcrumb to where you are) are brightened, so you keep your place even deep in a subsection.
- **Full-screen contents view** — `:contents` opens a browsable overview of the whole section tree; `j`/`k` move a selection, `Enter` jumps to that section (and `Ctrl+O` rewinds), `Esc` closes. Distinct from the passive `\` sidebar.
- **Back-navigation stack** — every jump (search, section, mark, sentence) pushes onto a history; `Ctrl+O` rewinds.
- **Header bar** — title and authors pinned above the content area when paper metadata is available.
- **Help overlay** — `?` for a compact keybinding reference.
- **Visual mode** — `v` (char) and `V` (line) selection; `y` yanks to clipboard via OSC 52, `H` commits as a persistent highlight.
- **Text objects** — vim-style `yi<obj>` / `ya<obj>` for word, quote, paren/bracket/brace pair, paragraph, and sentence. `yy` yanks the current line.
- **Cross-reference jumping** — section/figure/table refs and citations render with the link colour and an underline. `Enter` on one jumps to the target (one line above so the target is fully visible); `Ctrl+O` rewinds. `K` (or `Shift+Enter` on supporting terminals) shows a citation's bib entry in a popup without leaving your reading position. Phrases like "Table 3" and "Section 6.2" are styled as a whole, not just the number.
- **Contextual preview pane** — with the side pane open (`i`), it follows the cursor: land on a citation and it shows that reference; land on a `\ref{fig:N}` and it shows that figure. When the cursor isn't on a cross-reference it falls back to the manually-browsed figure (`]f` / `[f` still step through figures). The pane takes 40% of the width by default; `:set preview=<n>` (20–70) resizes it and persists the choice.- **Search** — `/` forward, `*` to search the word under the cursor, `n` / `N` to step.
- **Bookmarks (vim-style marks)** — `m{a}` to set, `'{a}` / `` `{a} `` to jump. Letter-keyed slots `a..z`. Marked lines tinted amber.
- **Persistent character-range highlights** — `H` in visual mode commits a selection as a highlight; `X` removes the one under the cursor. Stored at block-byte granularity so they survive terminal resize.
- **Command mode** — `:` opens an Ex-style command bar (see [hotkeys.md](hotkeys.md) for the command list). Includes `:reload` to re-fetch + re-parse the current paper while preserving cursor / scroll / bookmarks / highlights.
- **Voice / TTS playback** — see [Voice / TTS](#voice--tts) below.
- **Themes** — 16 built-in themes (Dark, Light, AMOLED, Solarized, Gruvbox, Nord, Tokyo Night, Catppuccin Mocha, Powder family). `:set theme=<id>` switches at runtime; `:set theme=trench` syncs with the trench feed UI's theme.
- **Reading measure** — body prose wraps to a comfortable column (default 72 cells) centred on wide terminals, while tables, figures and display math break out to the full width so they're never clipped. `:set width=<n>` adjusts the prose column; `:set width=0` (or `off`) disables the cap. Persists across sessions.
- **Section headings** — every heading gets a blank line above it for breathing room while scrolling. Numbered sections show their number (`2  Background`, `2.1  …`); numbering is consistent across both parser paths and feeds the TOC and `:goto 3.2`. The paper title, Abstract, and References stay unnumbered.
- **Paragraph rhythm** — spacing between blocks is normalised at layout time to exactly one blank line, regardless of how many the source parser emitted; leading and trailing blank lines are trimmed. The rhythm reads the same on every paper and on both parser paths.
- **Inline code & block quotes** — inline `code` spans get a subtle background "pill" so they stand out from prose instead of blending in. Block quotes lead with a coloured left rule bar (`▌`) and dimmed italic text rather than a bare indent.

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

- **No required local binaries for the primary parser path.**  Ar5iv (`ar5iv.labs.arxiv.org`) hosts LaTeXML-rendered HTML for the bulk of arXiv; the reader fetches it directly.
- **Pandoc** (recommended for the fallback path) — Install via `brew install pandoc` on macOS.  Used only when ar5iv hasn't processed a paper or returned HTML the prototype parser couldn't make sense of.  Without Pandoc, those papers fail loudly.
- **Poppler's `pdftotext`** (recommended) — for PDF-anchored table placement on the Pandoc fallback path.  Install via `brew install poppler`.  The reader falls back to source-order placement if missing.  Not needed on the ar5iv path.
- **Poppler's `pdftoppm`** (recommended for pixel figures) — converts PDF figures to PNG for inline rendering. Same `brew install poppler` provides this. Without it, PDF figures degrade to `[Figure N: caption]` text.
- A modern terminal with 24-bit colour and OSC 52 / OSC 8 support recommended (iTerm2, Kitty, WezTerm, modern macOS Terminal). Plain `xterm` works but loses styling fidelity.
- **Optional: tmux passthrough** — for inline pixel graphics inside tmux, add `set -g allow-passthrough on` and `set -g focus-events on` to `~/.tmux.conf`. Without this, figures fall back to text placeholders even on Kitty-protocol-capable terminals.
- **Optional: ElevenLabs API key** — for premium voice quality. Without it, voice falls back to macOS `say`.

## Run

```sh
cargo run --release -p tread -- <arxiv-id-or-url>
```

Examples: `cargo run --release -p tread -- 1706.03762`, or `… -- https://arxiv.org/abs/2005.14165`.
