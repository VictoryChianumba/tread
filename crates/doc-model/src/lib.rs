mod figure;
mod layout;
mod table;
mod wrap;

pub use figure::compute_cell_footprint;
pub use layout::build_visual_lines;

/// Internal jump target carried on an `InlineSpan` to make refs/citations
/// interactive in the reader.  The reader resolves these to line indices
/// at runtime via its `label_lines` / `bib_entries_lines` maps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// LaTeX label / Pandoc anchor id — e.g. `"sec:method"`, `"eq:elbo"`,
    /// `"tab:variations"`.  `Enter` jumps to (the line *before*) the
    /// labeled element.
    Internal(String),
    /// BibTeX cite-key — e.g. `"vaswani"`.  `Enter` jumps to the bib
    /// entry; `K` / `Shift+Enter` shows the entry in a popup.
    Citation(String),
}

/// Inline styled run within a paragraph line.
/// `color` uses raw RGB rather than a ratatui type — doc-model has no UI dependency.
#[derive(Debug, Clone, Default)]
pub struct InlineSpan {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub monospace: bool,
    pub color: Option<(u8, u8, u8)>,
    pub url: Option<String>,
    pub link_target: Option<LinkTarget>,
}

impl InlineSpan {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            monospace: false,
            color: None,
            url: None,
            link_target: None,
        }
    }

    /// Internal cross-reference span — `\ref{...}` / `\eqref{...}`.
    pub fn internal_link(text: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            link_target: Some(LinkTarget::Internal(label.into())),
            ..Self::plain(text)
        }
    }

    /// Citation span — `\cite{...}`.
    pub fn citation(text: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            link_target: Some(LinkTarget::Citation(key.into())),
            ..Self::plain(text)
        }
    }

    pub fn bold(text: impl Into<String>) -> Self {
        Self {
            bold: true,
            ..Self::plain(text)
        }
    }

    pub fn italic(text: impl Into<String>) -> Self {
        Self {
            italic: true,
            ..Self::plain(text)
        }
    }

    pub fn monospace(text: impl Into<String>) -> Self {
        Self {
            monospace: true,
            ..Self::plain(text)
        }
    }
}

/// Semantic block — the producer's view of the document.
#[derive(Debug, Clone)]
pub enum Block {
    /// A single line of prose, already word-wrapped by the producer.
    Line(String),
    /// A display math equation rendered as multiple Unicode lines, treated as one unit.
    /// `num` carries the equation number for numbered environments (equation, align, etc.).
    DisplayMath {
        lines: Vec<String>,
        num: Option<usize>,
    },
    /// A section header. level: 1=section, 2=subsection, 3=subsubsection/paragraph.
    Header { level: u8, text: String },
    /// A matrix rendered as a grid of cells (row-major).
    /// Each cell is `(text, col_span)` — `\multicolumn{N}` cells carry span > 1.
    /// `vertical_rules` lists raw column indices BEFORE which a `│` is drawn.
    /// Empty Vec = booktabs default (no vertical lines). 0 = left edge,
    /// raw_col_count = right edge. The renderer translates raw indices to
    /// active-column space (after blank-column collapse).
    Matrix {
        rows: Vec<Vec<(String, usize)>>,
        vertical_rules: Vec<usize>,
    },
    /// Explicit vertical space (blank line).
    Blank,
    /// A prose line carrying inline styling (bold, italic, monospace, etc.).
    /// The producer emits this when any span has a non-default style.
    /// build_visual_lines wraps it to terminal_width.
    StyledLine(Vec<InlineSpan>),
    /// A list item. depth=0 for top-level; marker is "• " or "1. " etc.
    ListItem {
        depth: u8,
        marker: String,
        content: Vec<InlineSpan>,
    },
    /// A verbatim / code-listing block. Lines are raw (no LaTeX processing).
    CodeBlock {
        lang: Option<String>,
        lines: Vec<String>,
    },
    /// A horizontal rule: \hline, \toprule, \midrule, \bottomrule.
    Rule,
    /// A block quote: \begin{quote}, \begin{quotation}, \begin{epigraph}.
    Quote(Vec<InlineSpan>),
    /// Invisible label marker.  Produced by parsers when they encounter a
    /// `\label{X}` (or Pandoc `attr.id`) so the reader can resolve
    /// `\ref{X}` jumps at runtime.  Emits no visual line; the reader
    /// associates the label with the *next* visible line's index.
    Anchor(String),
    /// One whole figure as the source intended it — possibly multi-row
    /// (stacked panels) and possibly multi-column-per-row (subfigures).
    ///
    /// `rows` is the 2D grid: outer index is the stack row, inner is the
    /// side-by-side items inside that row.  A standalone figure is just
    /// `rows = [[one_item]]`.  A 3-row table of 2 subfigures each is
    /// `rows = [[a, b], [c, d], [e, f]]`.
    ///
    /// `alt` is the figure caption (e.g. "Figure 3: …") attached once to
    /// the whole figure — not duplicated across rows.
    ///
    /// `figure_id` is the parser's per-document figure counter (1, 2, …).
    /// Carried so consumers that need a stable per-figure identifier can
    /// use it without rebuilding from block positions.
    ///
    /// `column_gaps_after` lists column indices (in the flat item space
    /// of each row) AFTER which a small horizontal gap should be drawn —
    /// recovered from the source tabular's `@{\hspace{...}}` separators.
    /// Empty when the source doesn't use column-group spacing.  Shared
    /// across all rows because tabular column groupings are uniform.
    ///
    /// `header_rows` carries column labels recovered from the figure's
    /// tabular header (e.g. "N=4", "Input", "Avat3r"…).  Each entry is
    /// a row of cells with text + col_span — the preview tiles them
    /// above the image grid, aligned to the columns they describe.
    /// Empty for figures without textual headers.
    Figure {
        rows: Vec<Vec<ImageItem>>,
        alt: String,
        figure_id: u32,
        column_gaps_after: Vec<usize>,
        header_rows: Vec<Vec<HeaderCell>>,
    },
}

/// One cell from a figure tabular's header row.  `text` is the flat
/// inline text (no formatting); `col_span` is how many image columns
/// the cell visually spans (1 for normal cells, N for `\multicolumn{N}`).
#[derive(Debug, Clone)]
pub struct HeaderCell {
    pub text: String,
    pub col_span: u16,
}

/// One sub-image inside a `Block::Figure` row.  Each carries its own
/// `kitty_id` so deletions and re-placements address them individually,
/// and its own pixel `dims` so the row's height is computed from the
/// most demanding sibling's aspect ratio.
#[derive(Debug, Clone)]
pub struct ImageItem {
    pub path: std::path::PathBuf,
    pub kitty_id: u32,
    pub dims: Option<(u32, u32)>,
}

/// A single screen row, fully expanded from a Block.
/// This is the flat table the reader indexes into — offset and cursor_y
/// are indices into Vec<VisualLine>, identical to how they used Vec<String>.
///
/// `block_byte_start` / `block_byte_end` describe the byte range within the
/// parent block's canonical text that this visual line covers.  Used by the
/// highlight system so that character-range highlights survive terminal
/// resize: highlights are stored at block-byte granularity and projected
/// onto the (potentially-rewrapped) visual line grid at render time.
///
/// For non-text blocks (Rule, Matrix, Blank), both fields are `0`.
#[derive(Debug, Clone)]
pub struct VisualLine {
    pub block_idx: usize,
    pub line_in_block: usize,
    pub text: String,
    pub kind: VisualLineKind,
    pub block_byte_start: usize,
    pub block_byte_end: usize,
}

#[derive(Debug, Clone)]
pub enum VisualLineKind {
    Prose,
    /// Part of a display math block. text is pre-centered with leading spaces.
    MathLine {
        block_width: usize,
        is_first: bool,
        is_last: bool,
    },
    Header(u8),
    MatrixLine {
        is_first: bool,
        is_last: bool,
        is_header: bool,
    },
    Blank,
    /// Prose with inline styling. text = plain concatenation (for search).
    /// Spans carry the styled runs for the renderer.
    StyledProse(Vec<InlineSpan>),
    /// A list item row. text already contains indent+marker prefix.
    ListItem {
        depth: u8,
        marker_len: u8,
        is_continuation: bool,
    },
    /// A line from a code/verbatim block.
    Code {
        is_first: bool,
        is_last: bool,
    },
    /// A horizontal rule; text = "─".repeat(terminal_width).
    Rule,
    /// A block quote; text = plain concatenation of spans.
    Quote {
        is_continuation: bool,
    },
    /// One row of an inline image figure.  `kitty_id` identifies the
    /// image to the Kitty graphics protocol.  `rows` × `cols` is the
    /// cell footprint chosen to preserve aspect ratio (computed in
    /// `build_visual_lines` from `Block::Image::dims` and the terminal
    /// width).  `is_first` flags the row where the renderer should emit
    /// the placement escape.
    Image {
        kitty_id: u32,
        cols: u16,
        rows: u16,
        is_first: bool,
    },
    /// One row of a multi-image figure (`Block::ImageRow`).  All sub-image
    /// kitty_ids share the same `rows`; each sibling renders at its own
    /// `cols` width side-by-side.  Per-image cols are stored as the
    /// `(kitty_id, cols)` pairs so siblings with different aspect ratios
    /// can take different horizontal slices.
    ImageRow {
        items: Vec<(u32, u16)>,
        rows: u16,
        is_first: bool,
    },
}

/// Approximate visual column width of a string (ASCII chars = 1, others = 1 for now).
/// A full Unicode-aware implementation can replace this without API changes.
pub(crate) fn visual_width(s: &str) -> usize {
    s.chars().count()
}

