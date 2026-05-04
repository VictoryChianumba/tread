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
- **Numbered equations** — `(1)`, `(2)`, right-justified in display math.
- **Cross-reference resolution** — `\ref{eq:elbo}` resolves to the equation number; `\cite{vaswani}` to `[1]`.
- **Bibliography** — full references section with formatted entries.
- **Math rendering** — Greek letters, calculus, relations, set theory, arrows, fractions, square roots, super/subscripts. Uses `tui-math` for display blocks; falls back to a Unicode strip for inline math.
- **Tables** — booktabs-style horizontal rules by default. Vertical rules from `tabular` column specs (`c|cccccc|ccc`). Body-internal `\hline` / `\specialrule` separators. Multi-row headers with proper spanning. Centered horizontally on screen.
- **Captions above tables** (academic convention); below figures.
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
- **Cross-reference jumping** — section/figure/table refs and citations render in a dedicated link colour with an underline. `Enter` on one jumps to the target (one line above so the target is fully visible); `Ctrl+O` rewinds. `K` (or `Shift+Enter` on supporting terminals) shows a citation's bib entry in a popup without leaving your reading position.
- **Search** — `/` forward, `*` to search the word under the cursor, `n` / `N` to step.
- **Bookmarks (vim-style marks)** — `m{a}` to set, `'{a}` / `` `{a} `` to jump. Letter-keyed slots `a..z`. Marked lines tinted amber.
- **Persistent character-range highlights** — `H` in visual mode commits a selection as a highlight; `X` removes the one under the cursor. Stored at block-byte granularity so they survive terminal resize.
- **Command mode** — `:` opens an Ex-style command bar (see [hotkeys.md](hotkeys.md) for the command list).
- **Themes** — 16 built-in themes (Dark, Light, AMOLED, Solarized, Gruvbox, Nord, Tokyo Night, Catppuccin Mocha, Powder family). `:set theme=<id>` switches at runtime; `:set theme=trench` syncs with the trench feed UI's theme.

## Where state lives

| Data | Path |
|---|---|
| Reading progress | `~/.config/trench/reader_progress.json` |
| Bookmarks (marks) | `~/.config/trench/bookmarks_<arxiv-id>.json` |
| Highlights | `~/.config/trench/highlights_<arxiv-id>.json` |
| Theme override | `~/.config/trench/block_reader.json` |
| Trench's theme (read-only sync) | `~/.config/trench/config.json` |

## Dependencies

- **Pandoc** (recommended) — for high-fidelity LaTeX parsing. Install via `brew install pandoc` on macOS. The reader falls back to a hand-rolled parser if Pandoc isn't on `PATH`.
- **Poppler's `pdftotext`** (recommended) — for PDF-anchored table placement. Install via `brew install poppler`. The reader falls back to source-order placement if missing.
- A modern terminal with 24-bit colour and OSC 52 / OSC 8 support recommended (iTerm2, Kitty, WezTerm, modern macOS Terminal). Plain `xterm` works but loses styling fidelity.
