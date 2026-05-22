use std::collections::HashMap;
use std::sync::Arc;

use doc_model::{Block, VisualLine, VisualLineKind, build_visual_lines};
use ratatui::layout::Rect;

use crate::PaperData;
use crate::highlights::HighlightSet;

mod bookmarks;
mod cursor;
mod figures;
mod mode;
mod nav;
pub mod voice_control;

// Re-export the figure types so external consumers keep the
// `state::FigureIndex` / `state::FigureLayout` / … paths they had
// before the split.  The Reader struct stores `FigureIndex` and
// `FigurePreviewState` directly, so these are not just public API —
// they're also names used in the rest of this file.
#[allow(unused_imports)]
pub use figures::{
  CaptionPlacement, FigureEntry, FigureHeaderCell, FigureIndex, FigureLayout, FigurePart,
  FigurePreviewState, HeaderLabelPlacement, HeaderRowPlacement, ImageRowPlacement,
  PREVIEW_COLUMN_GAP_CELLS, PartPlacement, PreviewGeometry,
};

pub const TOC_WIDTH: usize = 28;
const PREVIEW_TEXT_PERCENT: usize = 60;
const READER_VERTICAL_MARGIN: usize = 2;
const READER_VERTICAL_MARGIN_MIN_HEIGHT: usize = 8;

/// Default reading measure (max body-text column width, in cells).
/// Long lines hurt readability — typographic research puts the
/// comfortable range at 50–75 characters — so on wide terminals body
/// prose wraps to this width and is centred.  There is no separate
/// horizontal page margin: the whitespace either side of the prose
/// column is exactly the centering slack, and tables / figures / math
/// use the full content width so they're never starved.  0 disables the
/// cap (prose flows full width); the user adjusts it with `:set width=N`
/// (persisted in `block_reader.json`).
pub(crate) const DEFAULT_MAX_MEASURE: usize = 72;

pub(crate) fn reader_vertical_margin(height: usize) -> usize {
    if height >= READER_VERTICAL_MARGIN_MIN_HEIGHT {
        READER_VERTICAL_MARGIN.min(height / 2)
    } else {
        0
    }
}

/// Build visual lines and optionally drop image rows.
///
/// Hosts that render figures in a dedicated preview pane (rather than
/// inline) set `text_only = true`, and the reader reflows text as if
/// the figures weren't there.  Captions stay because they live as
/// separate prose blocks, so users keep a textual anchor for `]f` /
/// `[f` navigation in the preview pane.
fn build_lines_for(
    blocks: &[Block],
    cw: usize,
    prose_width: usize,
    height: usize,
    text_only: bool,
) -> Vec<VisualLine> {
    let mut lines = build_visual_lines(blocks, cw, prose_width, height);
    if text_only {
        lines.retain(|vl| {
            !matches!(
                vl.kind,
                VisualLineKind::Image { .. } | VisualLineKind::ImageRow { .. },
            )
        });
    }
    lines
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutRebuildReason {
    Initial,
    Reload,
    Resize,
    TextOnlyToggle,
    TocToggle,
}

#[derive(Debug, Clone)]
pub struct LayoutCache {
    pub visual_lines: Vec<VisualLine>,
    pub sections: Vec<(usize, u8, String)>,
    pub label_lines: HashMap<String, usize>,
    pub bib_entries: HashMap<String, String>,
    pub bib_entry_lines: HashMap<String, usize>,
    /// Figure cross-reference label → figure entry index (preview slot).
    pub figure_labels: HashMap<String, usize>,
}

impl LayoutCache {
    pub fn rebuild_layout(
        reason: LayoutRebuildReason,
        blocks: &[Block],
        content_width: usize,
        prose_width: usize,
        height: usize,
        text_only: bool,
        external_bibitems: &HashMap<String, String>,
    ) -> Self {
        let _s = crate::bench::Span::new(match reason {
            LayoutRebuildReason::Initial => "layout_build_initial",
            LayoutRebuildReason::Reload => "layout_build_reload",
            LayoutRebuildReason::Resize => "layout_build_resize",
            LayoutRebuildReason::TextOnlyToggle => "layout_build_text_only",
            LayoutRebuildReason::TocToggle => "layout_build_toc_toggle",
        });
        let visual_lines = build_lines_for(blocks, content_width, prose_width, height, text_only);
        let sections = build_sections(&visual_lines);
        let (label_lines, mut bib_entries, bib_entry_lines, figure_labels) =
            build_link_indexes(blocks, &visual_lines);
        for (key, value) in external_bibitems {
            bib_entries.insert(key.clone(), value.clone());
        }
        Self {
            visual_lines,
            sections,
            label_lines,
            bib_entries,
            bib_entry_lines,
            figure_labels,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ReflowAnchor {
    block_idx: usize,
    line_in_block: usize,
    byte_in_block: usize,
    cursor_y: usize,
}

impl ReflowAnchor {
    fn capture(reader: &Reader) -> Option<Self> {
        let vl = reader.visual_lines.get(reader.current_line())?;
        let byte_in_block = if vl.block_byte_end > vl.block_byte_start {
            let max_byte = vl.block_byte_end.saturating_sub(1);
            vl.block_byte_start
                .saturating_add(reader.cursor_x)
                .min(max_byte)
        } else {
            vl.block_byte_start
        };
        Some(Self {
            block_idx: vl.block_idx,
            line_in_block: vl.line_in_block,
            byte_in_block,
            cursor_y: reader.cursor_y,
        })
    }

    fn restore(self, reader: &mut Reader) -> bool {
        let Some((target_line, cursor_x)) = reader
            .visual_lines
            .iter()
            .enumerate()
            .find_map(|(idx, vl)| {
                if vl.block_idx == self.block_idx
                    && vl.block_byte_end > vl.block_byte_start
                    && self.byte_in_block >= vl.block_byte_start
                    && self.byte_in_block < vl.block_byte_end
                {
                    Some((idx, self.byte_in_block - vl.block_byte_start))
                } else {
                    None
                }
            })
            .or_else(|| {
                reader
                    .visual_lines
                    .iter()
                    .enumerate()
                    .find(|(_, vl)| {
                        vl.block_idx == self.block_idx && vl.line_in_block == self.line_in_block
                    })
                    .map(|(idx, _)| (idx, 0))
            })
            .or_else(|| {
                reader
                    .visual_lines
                    .iter()
                    .enumerate()
                    .find(|(_, vl)| vl.block_idx == self.block_idx)
                    .map(|(idx, _)| (idx, 0))
            })
        else {
            return false;
        };

        let ch = reader.content_height();
        let max_offset = reader.total_lines().saturating_sub(ch);
        reader.offset = target_line.saturating_sub(self.cursor_y).min(max_offset);
        reader.cursor_y = target_line.saturating_sub(reader.offset);
        reader.cursor_x = cursor_x;
        reader.desired_column = cursor_x;
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FindKind {
    /// `f<c>` — forward to next occurrence.
    F,
    /// `F<c>` — backward to previous occurrence.
    ShiftF,
    /// `t<c>` — forward, land *before* the match.
    T,
    /// `T<c>` — backward, land *after* the match.
    ShiftT,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Normal,
    Search,
    Visual {
        line_mode: bool,
    },
    /// One-shot mode: the next `KeyCode::Char(c)` is consumed as the find
    /// target.  Any other key returns to Normal without moving.
    AwaitingChar {
        kind: FindKind,
    },
    /// One-shot mode: the next `KeyCode::Char(letter)` is consumed as a
    /// mark identifier.  When `for_set` is true the mark is saved at the
    /// current line; when false the cursor jumps to that mark (no-op if
    /// the mark is unset).  Any non-Char key cancels.
    AwaitingMarkName {
        for_set: bool,
    },
    /// One-shot mode after the user pressed `g`.  Awaits the second
    /// keystroke for vim's `g`-prefixed motions: `gg` → top, `ge` /
    /// `gE` → backward word-end (small / big).  Any other key cancels.
    AwaitingG,
    /// One-shot mode after the user pressed `]` or `[`.  Awaits the
    /// second keystroke to disambiguate: `]]` / `[[` → jump section
    /// (vim convention), `]f` / `[f` → step figure in the preview
    /// pane.  Any other key cancels.  `forward` records which bracket
    /// was pressed so the resolver knows direction.
    AwaitingBracket {
        forward: bool,
    },
    /// `:`-prefixed Ex-command input.  `cmd_buf` on Reader holds the
    /// in-progress command line; Esc cancels, Enter dispatches via
    /// `commands::execute`.
    Command,
    /// After pressing an operator (currently only `y`).  Awaits the
    /// follow-up: another `y` to apply to the current line, `i`/`a` to
    /// enter text-object mode, or any other key cancels.
    AwaitingOperator {
        op: Operator,
    },
    /// After `yi` or `ya`.  Awaits the text-object spec character (`w`,
    /// `"`, `(`, `p`, `s`, etc.) and dispatches to `text_objects::*`.
    AwaitingTextObject {
        op: Operator,
        around: bool,
    },
}

/// Which operator is currently pending.  In a read-only reader only
/// `Yank` is meaningful — `d`/`c`/`x` would mutate the buffer.  Kept as
/// an enum so the dispatch surface is uniform with vim's model and the
/// state machine can be extended without rewiring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Yank,
}

/// A modal popup surfaced by `:marks`, `:highlights`, `:about`,
/// `:placement`, etc.  `Reader.popup` is `Some(...)` while one is open;
/// any keystroke dismisses it.
#[derive(Debug, Clone)]
pub struct PopupContent {
    pub title: String,
    pub lines: Vec<String>,
}

/// Paper-level metadata shown in the header bar.
#[derive(Debug, Clone)]
pub struct PaperMeta {
    pub title: String,
    pub authors: String,
}

pub struct Reader {
    pub blocks: Vec<Block>,
    /// Layout-cache projection: visual lines as flowed for the current
    /// terminal width.  Private so writes only happen via the layout
    /// rebuild path in `LayoutCache::rebuild_layout`.  Read via
    /// `Reader::visual_lines()`.
    visual_lines: Vec<VisualLine>,
    /// Layout-cache projection: section index (line_idx, level, title).
    /// Same private/getter shape as `visual_lines`.  Read via
    /// `Reader::sections()`.
    sections: Vec<(usize, u8, String)>,
    layout_cache: LayoutCache,
    pub toc_visible: bool,
    pub help_visible: bool,
    pub help_query: String,
    pub help_selected: usize,
    /// Full-screen contents view (`:contents`).  Interactive: j/k move
    /// `contents_selected`, Enter jumps to that section, Esc closes.
    /// Distinct from the passive `toc_visible` sidebar.  Private; managed
    /// via `open_contents` / `close_contents` / `contents_*`.
    contents_visible: bool,
    /// Selected section index in the full-screen contents view.
    contents_selected: usize,
    /// Viewport top in absolute-line indices.  Private so every
    /// scroll/jump lands in `cursor.rs`.  Read via `Reader::offset()`.
    offset: usize,
    /// Cursor row within the viewport (0 = topmost visible line).
    /// Private; read via `Reader::cursor_y()`.
    cursor_y: usize,
    pub width: usize,
    pub height: usize,
    /// Reading-measure cap: max body-text column width in cells, 0 =
    /// off (flow edge-to-edge).  Defaults to [`DEFAULT_MAX_MEASURE`];
    /// hydrated from config in `init` and changed at runtime via
    /// `:set width=N` (`set_max_measure`).  Private so writes go through
    /// the setter, which reflows; read via `Reader::max_measure()`.
    max_measure: usize,
    pub search_query: String,
    /// Visual-line indices of `/`-search matches.  Private; writes
    /// happen via `update_search_matches` / `remap_search_matches_after_layout`
    /// only.  Read via `Reader::search_matches()`.
    search_matches: Vec<usize>,
    pub search_idx: usize,
    /// Active mode.  Private so every transition lands in `mode.rs`
    /// (`enter_*` / `return_to_normal`).  Read via `Reader::mode()`.
    mode: Mode,
    /// Back-navigation stack: (offset, cursor_y) entries pushed before jumps.
    pub nav_history: Vec<(usize, usize)>,
    /// Optional paper metadata shown in the header bar.
    pub meta: Option<PaperMeta>,
    /// Letter-keyed bookmarks (vim-style marks).  `m<letter>` sets,
    /// `'<letter>` jumps.  Persisted per arXiv ID.  Block-byte addressed
    /// (see `bookmarks::Bookmark`) so marks survive terminal resize.
    /// Private; read via the seam methods on `Reader` defined in
    /// `state/bookmarks.rs`.
    bookmarks: HashMap<char, bookmarks::Bookmark>,
    /// Persistent character-range highlights.  Stored at block-byte
    /// granularity so they survive resize.  Loaded on entry, saved on exit.
    pub highlights: HighlightSet,
    /// Resolution map for `\ref{X}` jumps: label → first visual-line index
    /// of the labeled element.  Built from `Block::Anchor` markers in
    /// `Reader::new` and mirrored from `LayoutCache.label_lines` on each
    /// rebuild.  For ref-targets we want the line *before* the labeled
    /// element when possible (so the equation/figure/table is fully
    /// visible after the jump); see `follow_link_target`.  Private;
    /// read via `Reader::label_lines()`.
    label_lines: HashMap<String, usize>,
    /// Bibliography entry text by cite-key.  Pandoc bib divs use
    /// `id="ref-<key>"`; we capture the rendered entry text for popup
    /// display by `:K` / `Shift+Enter` on a citation.  Private; read
    /// via `Reader::bib_entries()`.
    bib_entries: HashMap<String, String>,
    /// Bibliography entry first-VL index by cite-key.  `Enter` on a
    /// citation jumps here (line *before* the entry).  Private; read
    /// via `Reader::bib_entry_lines()`.
    bib_entry_lines: HashMap<String, usize>,
    /// Figure cross-reference label → figure entry index.  Lets the
    /// contextual preview pane show the figure a `\ref{fig:…}` under the
    /// cursor points at.  Mirrored from `LayoutCache.figure_labels` on
    /// each rebuild.  Private; consulted by `ref_figure_under_cursor`.
    figure_labels: HashMap<String, usize>,
    source_bibitems: HashMap<String, String>,
    /// Effective byte column of the cursor on the current line.  Always
    /// represents the rendered position — horizontal motions write here.
    /// Private; read via `Reader::cursor_x()`.
    cursor_x: usize,
    /// "Desired" column carried across `j`/`k` line changes so that
    /// returning to a long line restores the original column.  Matches
    /// vim's `curswant`.  Set by horizontal motions; consulted by vertical
    /// motions via `clamp_cursor_after_line_change`.  Private; read via
    /// `Reader::desired_column()`.
    desired_column: usize,
    /// Absolute line index where visual selection started.
    pub visual_anchor: usize,
    /// Column index where visual selection started.
    pub visual_anchor_x: usize,
    /// Accumulated digit prefix for count motions (e.g. "5" before `j`).
    /// Private; writes happen via `push_count_char` (typing a digit)
    /// and `clear_count` (after a motion consumes it).  Read via
    /// `Reader::count_buf()`.
    count_buf: String,
    /// In-progress text after `:` in Command mode.  Private; writes
    /// happen via the Command-mode key handler in `lib.rs` (push_char
    /// / pop_char on the buffer) and via `mode::*` transitions that
    /// clear it.  Read via `Reader::cmd_buf()`.
    cmd_buf: String,
    /// One-line error message shown in the status line after a command
    /// failed (e.g. unknown command, unknown theme).  Cleared on next event.
    pub cmd_error: Option<String>,
    /// Active modal popup (e.g. `:marks` listing).  Any keystroke
    /// dismisses (handled centrally in the event loop).  Private; opens
    /// route through `open_popup` so title + lines arrive atomically.
    /// Read via `Reader::popup()`.
    popup: Option<PopupContent>,
    /// Resolved on-disk path for every `Block::Image` in the document,
    /// keyed by its `kitty_id`.  Built once at construction; consulted
    /// post-draw to load PNG bytes for terminals that speak the Kitty
    /// graphics protocol.  Paths that fail to resolve are silently
    /// skipped — the caption row always renders, so degradation is
    /// graceful.  Private; read via `Reader::image_paths()`.
    image_paths: HashMap<u32, std::path::PathBuf>,
    figure_index: FigureIndex,
    preview_state: FigurePreviewState,
    /// When true, image rows are dropped from `visual_lines` so text
    /// reflows past where the figures would have been.  Hosts use this
    /// for "preview pane" reading modes: the reader pane stays text-only
    /// and a dedicated side pane shows one figure at a time via
    /// `images::place_one_figure`.  Private; toggle via `set_text_only`
    /// — direct field writes won't trigger the rebuild and will desync
    /// state.
    text_only: bool,
    /// User-facing toggle for the figure-preview side pane (`i` in
    /// normal mode).  When true, `render::split_content_for_preview`
    /// carves out the right 40% of the content area for a single
    /// figure, the reader is forced into `text_only` mode so figures
    /// don't double-render, and `after_draw` places the current
    /// figure via `images::place_one_figure`.
    ///
    /// Distinct from `text_only`: a host could plausibly want
    /// `text_only` without the preview pane (e.g. pure-text export),
    /// so they stay as separate fields.  Private; toggle via
    /// `set_figure_preview_active`, which keeps the two coherent.
    /// Read via `figure_preview_visible()` (the only consumer outside
    /// state/ is render::draw).
    figure_preview_active: bool,
    /// Index into `figure_kitty_ids()` for the figure currently shown
    /// in the side preview pane.  `None` when the preview is off or
    /// the paper has no figures.  Private; written by `step_figure`
    /// and `set_figure_preview_active` only.
    current_figure: Option<usize>,

    /// arXiv id this Reader is rendering, or `None` when running against a
    /// non-arxiv source.  Used by `:reload`, `:url`, `:cite`, `:open`, and
    /// the persistence layer (per-paper progress / marks / highlights).
    pub arxiv_id: Option<String>,
    /// Whether the host terminal speaks the Kitty graphics protocol.  Used
    /// by `:reload` to decide between absolute image paths and degraded
    /// captions.  Set once at construction; doesn't change at runtime.
    pub kitty_supported: bool,
    /// Saved reading-position offset waiting to be applied.  Set by
    /// `Reader::init` when persistence has a stored offset for this
    /// paper; consumed by the first `resize()` call once we know the
    /// real terminal/pane dimensions and `visual_lines` has been
    /// rebuilt at that width.  Without this defer, `init` would clamp
    /// the saved offset against the placeholder 80×24 reflow's
    /// `total_lines` and the user would land a few lines off where
    /// they left.
    pending_progress_offset: Option<usize>,

    // ── Voice / TTS playback state ──────────────────────────────────────────
    // All fields are `None` / `false` / `Idle` when voice is inactive.  The
    // controller is shared (Arc) so a host TUI can give the same audio
    // thread to every reader tab — only one paper speaks at a time across
    // tabs.  Per-tab playback bookkeeping (status, started_at, …) stays on
    // each Reader.  Standalone tread wraps its single controller in an Arc
    // too so the contract is uniform.
    /// Background TTS playback controller (shared via Arc), or `None` when
    /// audio init failed or voice was disabled by the host.  Private; read
    /// via `Reader::voice_controller()`.
    voice_controller: Option<Arc<crate::voice::PlaybackController>>,
    /// Session id stamped by `voice_controller.start(...)` when this
    /// Reader requested playback.  Compared against the controller's
    /// current session each tick: a mismatch means another Reader (in
    /// another tab) preempted us, so we silently exit `reading_mode`.
    voice_started_session: Option<u64>,
    /// Last-synced playback status; refreshed each tick from the
    /// controller's shared `Arc<Mutex>`.  Read via `Reader::voice_status()`.
    voice_status: crate::voice::PlaybackStatus,
    /// Pending error from the playback thread (e.g. ElevenLabs auth
    /// failure, audio device missing).  Cleared after display in the
    /// status bar.
    voice_error: Option<String>,
    /// First / last visual-line index of the paragraph currently being
    /// read.  Used for line dimming and word-position bookkeeping.
    /// Read via `Reader::voice_para_range()`.
    voice_para_start: usize,
    voice_para_end: usize,
    /// Wall-clock instant when the current chunk's audio started playing.
    /// `None` when nothing is playing.  Combined with a fixed chars-per-
    /// second rate, this drives the "active word" highlight.
    voice_started_at: Option<std::time::Instant>,
    /// Cumulative character count from chunks that completed BEFORE the
    /// current one, so word-position math knows what offset to start at.
    voice_chars_before: usize,
    /// True while the user is in voice mode (`r`/`R`/`Ctrl+P` started a
    /// playback session).  Allows navigation to keep working while audio
    /// plays, and gates `Space`/`c`/`Esc` voice handlers.  Read via
    /// `Reader::reading_mode()`.
    reading_mode: bool,
    /// True when continuous reading is active — on chunk-end, advance to
    /// the next paragraph and start playing it.  Read via
    /// `Reader::continuous_reading()`; toggle off via
    /// `Reader::stop_continuous_reading()`.
    continuous_reading: bool,
}

impl Reader {
    /// Construct a Reader without any pre-scanned bibitems.  Used by
    /// internal tests; production callers go through
    /// `new_with_bibitems` so cite-key popups have data.
    #[allow(dead_code)]
    pub fn new(blocks: Vec<Block>, width: usize, height: usize) -> Self {
        Self::new_with_bibitems(blocks, width, height, HashMap::new())
    }

    pub fn new_with_bibitems(
        blocks: Vec<Block>,
        width: usize,
        height: usize,
        bibitems: HashMap<String, String>,
    ) -> Self {
        // Constructor stays IO-free so tests are hermetic.  The figure-
        // preview sticky default is hydrated in `init` via
        // `set_figure_preview_active(config::load().figure_preview_default)`,
        // which handles the visual-line rebuild and current_figure seed.
        let figure_preview_active = false;
        let text_only = false;
        let current_figure: Option<usize> = None;
        let cw = content_width_for(width, false, false);
        let prose_width = if DEFAULT_MAX_MEASURE > 0 {
            DEFAULT_MAX_MEASURE.min(cw)
        } else {
            cw
        };
        let ch = content_height_for(height, false, false);
        let source_bibitems = bibitems;
        let layout_cache = LayoutCache::rebuild_layout(
            LayoutRebuildReason::Initial,
            &blocks,
            cw,
            prose_width,
            ch,
            text_only,
            &source_bibitems,
        );
        let figure_index = FigureIndex::build(&blocks);
        let image_paths = figure_index.path_map();
        Self {
            blocks,
            visual_lines: layout_cache.visual_lines.clone(),
            sections: layout_cache.sections.clone(),
            layout_cache: layout_cache.clone(),
            label_lines: layout_cache.label_lines.clone(),
            bib_entries: layout_cache.bib_entries.clone(),
            bib_entry_lines: layout_cache.bib_entry_lines.clone(),
            figure_labels: layout_cache.figure_labels.clone(),
            source_bibitems,
            toc_visible: false,
            help_visible: false,
            help_query: String::new(),
            help_selected: 0,
            contents_visible: false,
            contents_selected: 0,
            offset: 0,
            cursor_y: 0,
            width,
            height,
            max_measure: DEFAULT_MAX_MEASURE,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_idx: 0,
            mode: Mode::Normal,
            nav_history: Vec::new(),
            meta: None,
            bookmarks: HashMap::new(),
            highlights: HighlightSet::default(),
            cursor_x: 0,
            desired_column: 0,
            visual_anchor: 0,
            visual_anchor_x: 0,
            count_buf: String::new(),
            cmd_buf: String::new(),
            cmd_error: None,
            popup: None,
            image_paths,
            figure_index,
            preview_state: FigurePreviewState::inactive(),
            text_only,
            figure_preview_active,
            current_figure,
            arxiv_id: None,
            kitty_supported: false,
            pending_progress_offset: None,
            // Voice fields default to "no playback in progress."  The
            // controller is wired in `Reader::init` (or post-construction by
            // tests); leaving it None here lets tests skip audio entirely.
            voice_controller: None,
            voice_started_session: None,
            voice_status: crate::voice::PlaybackStatus::Idle,
            voice_error: None,
            voice_para_start: 0,
            voice_para_end: 0,
            voice_started_at: None,
            voice_chars_before: 0,
            reading_mode: false,
            continuous_reading: false,
        }
    }

    /// Embed-surface constructor.  Replaces ad-hoc post-construction
    /// wiring (meta, voice, progress restore) that used to live inside
    /// `run_with_theme`.  Standalone tread and host TUIs (trench) both
    /// build a Reader through this single entry point.
    ///
    /// Arguments:
    /// - `paper`: blocks + bibitems + asset_dir as returned by `fetch_paper`.
    ///   `asset_dir` is consumed — it's only useful during fetch / parse,
    ///   not during reading.
    /// - `meta`: optional title + authors for the header bar.
    /// - `progress_key`: arXiv id (or any opaque key).  When `Some`, the
    ///   reader restores prior reading position, marks, and highlights;
    ///   the same key is used by `save_progress` on exit.  When `None`,
    ///   no persistence happens.
    /// - `width` / `height`: terminal area in cells.
    /// - `kitty_supported`: whether to show inline pixel figures.
    /// - `voice_controller`: shared TTS playback handle.  Pass `None` for
    ///   a Reader that doesn't support audio.
    pub fn init(
        paper: PaperData,
        meta: Option<PaperMeta>,
        progress_key: Option<String>,
        width: u16,
        height: u16,
        kitty_supported: bool,
        voice_controller: Option<Arc<crate::voice::PlaybackController>>,
    ) -> Self {
        let mut reader = Self::new_with_bibitems(
            paper.blocks,
            width as usize,
            height as usize,
            paper.bibitems,
        );
        reader.meta = meta;
        reader.arxiv_id = progress_key.clone();
        reader.kitty_supported = kitty_supported;
        reader.voice_controller = voice_controller;
        // Hydrate the sticky figure-preview default now that we're past
        // the hermetic constructor.  The setter does the right rebuild
        // dance (text_only sync + current_figure seed) when the value
        // differs from the constructor's default of false.
        let cfg = crate::config::load();
        reader.set_figure_preview_active(cfg.figure_preview_default);
        // Apply the persisted reading measure.  Only reflows when it
        // differs from the constructor's DEFAULT_MAX_MEASURE.
        reader.set_max_measure(cfg.max_measure);

        if let Some(ref key) = progress_key {
            let map = crate::progress::load();
            if let Some(p) = map.get(key) {
                // Defer the offset clamp until the first `resize()` call so
                // it lands against the real terminal/pane width's reflow.
                // Clamping here against the placeholder 80×24 visual_lines
                // would put the user a few lines off saved position when
                // the actual pane is wider or narrower.
                reader.pending_progress_offset = Some(p.offset);
            }
            reader.load_bookmarks_from_disk(key);
            reader.highlights = crate::highlights::load(key);
        }

        reader
    }

    /// Whether the reader is in Normal mode (i.e. not in Search /
    /// Visual / Command / any awaiting one-shot mode).  Hosts use this
    /// to decide whether keys like `Esc` should fall through to the
    /// host's own back-out logic vs being consumed by the reader to
    /// cancel an in-progress mode.
    pub fn is_normal_mode(&self) -> bool {
        matches!(self.mode, Mode::Normal)
    }

    /// Flat visual-line table for the current layout.  Built by
    /// `LayoutCache::rebuild_layout` and refreshed on resize / reload /
    /// text-only toggle.
    pub fn visual_lines(&self) -> &[VisualLine] {
        &self.visual_lines
    }

    /// Section index built from `Block::Header` markers — one entry
    /// per section as `(visual_line_idx, level, title)`.  Rebuilt
    /// alongside `visual_lines`.
    pub fn sections(&self) -> &[(usize, u8, String)] {
        &self.sections
    }

    /// Visual-line indices of the current `/`-search matches.  Empty
    /// when no search is active or the query has no hits.  Writes
    /// happen via the internal `update_search_matches` path only.
    pub fn search_matches(&self) -> &[usize] {
        &self.search_matches
    }

    /// In-progress `:`-command line.  Empty outside `Mode::Command`.
    /// The Command-mode key handler is the only writer.
    pub fn cmd_buf(&self) -> &str {
        &self.cmd_buf
    }

    /// Active modal popup, if one is open.  `None` outside an explicit
    /// `:marks` / `:highlights` / `:about` / `:placement` invocation.
    /// The event loop centralizes dismissal: any keystroke calls
    /// `close_popup` before forwarding to the mode handler.
    pub fn popup(&self) -> Option<&PopupContent> {
        self.popup.as_ref()
    }

    /// Open a modal popup with `title` and `lines`.  Replaces any
    /// previously-open popup atomically — callers don't have to
    /// remember to clear the old one first.
    pub fn open_popup(&mut self, title: String, lines: Vec<String>) {
        self.popup = Some(PopupContent { title, lines });
    }

    /// Dismiss the popup (if any).  Idempotent.  Called by the event
    /// loop on every keystroke when a popup is open.
    pub fn close_popup(&mut self) {
        self.popup = None;
    }

    /// Resolution map for `\ref{X}` jumps.  Rebuilt with the layout
    /// cache; writes only happen via `LayoutCache::rebuild_layout`.
    pub fn label_lines(&self) -> &HashMap<String, usize> {
        &self.label_lines
    }

    /// Bibliography entry text by cite-key.  Rebuilt with the layout
    /// cache plus external bibitems extracted at fetch time.
    pub fn bib_entries(&self) -> &HashMap<String, String> {
        &self.bib_entries
    }

    /// Bibliography entry first-VL index by cite-key.  Rebuilt with
    /// the layout cache.
    pub fn bib_entry_lines(&self) -> &HashMap<String, usize> {
        &self.bib_entry_lines
    }

    /// Resolved on-disk path for every `Block::Image` in the document,
    /// keyed by its `kitty_id`.  Built once at construction; never
    /// mutated thereafter.
    pub fn image_paths(&self) -> &HashMap<u32, std::path::PathBuf> {
        &self.image_paths
    }

    /// Accumulated digit prefix for count motions (vim-style "5j" /
    /// "10gg").  Read by motion handlers to scale repetition.
    pub fn count_buf(&self) -> &str {
        &self.count_buf
    }

    /// Append a typed digit to the count prefix.  Called from the
    /// Normal-mode key handler when a digit (1-9, or 0 with a non-
    /// empty buffer) is pressed.
    pub fn push_count_char(&mut self, c: char) {
        self.count_buf.push(c);
    }

    /// Clear the count prefix.  Called by every motion handler after
    /// it consumes the count, and by mode transitions in `mode.rs`.
    pub fn clear_count(&mut self) {
        self.count_buf.clear();
    }

    /// Append a typed character to the `:`-command line.  Called from
    /// the Command-mode key handler on each `KeyCode::Char(c)`.
    pub fn push_cmd_char(&mut self, c: char) {
        self.cmd_buf.push(c);
    }

    /// Pop the last char (backspace in Command mode).  Returns false
    /// if the buffer was already empty so the caller can decide to
    /// cancel command mode instead.
    pub fn pop_cmd_char(&mut self) -> bool {
        self.cmd_buf.pop().is_some()
    }

    /// Take the in-progress `:`-command line, leaving an empty buffer.
    /// Called when the user presses Enter — the returned String is
    /// dispatched to `commands::execute`.
    pub fn take_cmd_buf(&mut self) -> String {
        std::mem::take(&mut self.cmd_buf)
    }

    /// Stop voice playback and clear every per-Reader voice flag.
    /// Hosts call this when the user navigates away from the reader
    /// (tab close, tab switch, leave-reader) so audio doesn't continue
    /// after the source it was reading is no longer in focus.
    /// Identical to the cleanup path the in-reader `Esc` keybinding
    /// already runs, but exposed as a method so trench can trigger it
    /// from its tab-management handlers without duplicating the field
    /// reset logic.  Idempotent.
    pub fn exit_voice_mode(&mut self) {
        if let Some(vc) = &self.voice_controller {
            vc.stop();
        }
        self.voice_started_at = None;
        self.voice_started_session = None;
        self.reading_mode = false;
        self.continuous_reading = false;
    }

    /// Persist this paper's reading position, bookmarks, and highlights.
    /// No-op when no `arxiv_id` is set.  Hosts call this on tab close /
    /// clean exit; safe to call multiple times.
    pub fn save_progress(&self) {
        let Some(key) = &self.arxiv_id else { return };
        let mut map = crate::progress::load();
        map.insert(
            key.clone(),
            crate::progress::ReaderProgress {
                offset: self.offset,
            },
        );
        crate::progress::save(&map);
        self.save_bookmarks_to_disk(key);
        crate::highlights::save(key, &self.highlights);
    }

    fn rebuild_layout(&mut self, reason: LayoutRebuildReason) {
        self.layout_cache = LayoutCache::rebuild_layout(
            reason,
            &self.blocks,
            self.content_width(),
            self.prose_width(),
            self.content_height(),
            self.text_only,
            &self.source_bibitems,
        );
        self.visual_lines = self.layout_cache.visual_lines.clone();
        self.sections = self.layout_cache.sections.clone();
        self.label_lines = self.layout_cache.label_lines.clone();
        self.bib_entries = self.layout_cache.bib_entries.clone();
        self.bib_entry_lines = self.layout_cache.bib_entry_lines.clone();
        self.figure_labels = self.layout_cache.figure_labels.clone();
        self.remap_search_matches_after_layout();
        // ADR-0002 follow-up: a rebuild can move every visual line, so
        // the cached preview geometry (in cell coords relative to the
        // OLD layout) is stale by definition.  Drop it here so the
        // next `after_draw` recomputes against the new content area
        // instead of mis-placing the image for one frame.
        self.preview_state.last_geometry.set(None);
    }

    /// Re-derive `search_matches` against the new `visual_lines` and
    /// re-anchor `search_idx` so a subsequent `n`/`N` continues from
    /// roughly where the user was.  Without this, a `/foo` followed by
    /// a resize leaves `search_matches` holding stale visual-line
    /// indices that may point past the new `visual_lines.len()` —
    /// the audit's C2 path.  Bookmarks and nav_history have the same
    /// shape of problem; they're mitigated separately
    /// (`jump_to_mark` no-ops on out-of-range targets, `clamp_position`
    /// catches stale offsets on the next motion).
    fn remap_search_matches_after_layout(&mut self) {
        if self.search_query.is_empty() {
            self.search_matches.clear();
            self.search_idx = 0;
            return;
        }
        // Anchor: the visual line the user was looking at before the
        // rebuild — either the current match or the cursor line.  The
        // re-anchor picks the first new match at or after this point so
        // pressing `n` continues forward, matching user intuition.
        let prior_anchor = self
            .search_matches
            .get(self.search_idx)
            .copied()
            .unwrap_or(self.offset + self.cursor_y);
        self.update_search_matches();
        if !self.search_matches.is_empty() {
            self.search_idx = self
                .search_matches
                .iter()
                .position(|&m| m >= prior_anchor)
                .unwrap_or(0);
        }
    }

    fn rebuild_figure_index(&mut self) {
        self.figure_index = FigureIndex::build(&self.blocks);
        self.image_paths = self.figure_index.path_map();
        self.refresh_preview_selection();
    }

    fn set_preview_selection(&mut self, selected_index: Option<usize>) {
        self.current_figure = selected_index;
        self.preview_state.selected_index = selected_index;
        self.preview_state.selected_kitty_id = selected_index.and_then(|idx| {
            self.figure_index
                .entries
                .get(idx)
                .map(FigureEntry::representative_kitty_id)
        });
    }

    fn refresh_preview_selection(&mut self) {
        let len = self.figure_index.entries.len();
        if len == 0 {
            self.set_preview_selection(None);
            return;
        }
        let selected_index = self.preview_state.selected_index.or(self.current_figure);
        let next = if self.preview_state.active {
            Some(selected_index.unwrap_or(0).min(len - 1))
        } else {
            selected_index.map(|idx| idx.min(len - 1))
        };
        self.set_preview_selection(next);
    }

    /// Replace the loaded paper with freshly-fetched blocks + bibitems
    /// in-place, preserving user state where it still makes sense.  Used
    /// by `:reload`.  We keep:
    ///   - `offset`, `cursor_y`, `cursor_x`, `desired_column` (clamped to new bounds)
    ///   - `bookmarks` and `highlights` (block-byte addressed; survive
    ///     re-parse iff the document structure is unchanged.  If the paper
    ///     has been edited upstream, some marks may land on different
    ///     content — acceptable for v1)
    ///   - `mode`, `search_query`, `search_matches`, `search_idx`, `nav_history`,
    ///     `toc_visible`, `meta`, `count_buf`, `cmd_buf`, `cmd_error`, `popup`
    ///
    /// Re-derives: `visual_lines`, `sections`, `label_lines`, `bib_entries`,
    /// `bib_entry_lines`, `image_paths`.
    pub fn reload_with(&mut self, blocks: Vec<Block>, bibitems: HashMap<String, String>) {
        self.blocks = blocks;
        self.source_bibitems = bibitems;
        self.rebuild_layout(LayoutRebuildReason::Reload);
        self.rebuild_figure_index();
        self.clamp_position();
    }

    /// Full content (structural) width after subtracting the TOC panel
    /// (if visible) and the preview pane.  Tables, figures, display math
    /// and the draw area span this; prose wraps to the narrower
    /// [`prose_width`](Self::prose_width).
    pub fn content_width(&self) -> usize {
        content_width_for(self.width, self.toc_visible, self.preview_layout_active())
    }

    /// Width body prose wraps to: the reading measure capped by the
    /// available content width (so prose never exceeds the column even
    /// when the measure is wider than the terminal).  Equals
    /// `content_width` when the measure is off (`max_measure == 0`).
    /// The renderer centres this column inside `content_width`.
    pub fn prose_width(&self) -> usize {
        let cw = self.content_width();
        if self.max_measure > 0 {
            self.max_measure.min(cw)
        } else {
            cw
        }
    }

    /// Current reading-measure cap (max body-text column, in cells; 0 =
    /// off / full width).  Read by the renderer to centre the prose
    /// column inside the full content width.
    pub fn max_measure(&self) -> usize {
        self.max_measure
    }

    /// The link the cursor currently sits on, if any.  Walks the spans of
    /// the current `StyledProse` visual line for the one covering
    /// `cursor_x`.  `None` on non-styled lines or when not over a link.
    /// Canonical home for cursor→link resolution (keys.rs delegates here,
    /// and the contextual preview pane consults it).
    pub fn link_under_cursor(&self) -> Option<doc_model::LinkTarget> {
        let vl = self.visual_lines().get(self.current_line())?;
        let spans = match &vl.kind {
            VisualLineKind::StyledProse(s) => s,
            _ => return None,
        };
        let mut byte = 0usize;
        for span in spans {
            let next = byte + span.text.len();
            if self.cursor_x() >= byte && self.cursor_x() < next {
                return span.link_target.clone();
            }
            byte = next;
        }
        None
    }

    /// If the cursor is on a citation whose bibliography entry is known,
    /// returns `(key, entry_text)`.  Drives the contextual preview pane
    /// (cursor on `\cite` → reference shown in the side pane) and mirrors
    /// what the `K` popup surfaces on demand.
    pub fn cursor_citation(&self) -> Option<(String, String)> {
        match self.link_under_cursor()? {
            doc_model::LinkTarget::Citation(key) => {
                let text = self.bib_entries.get(&key)?.clone();
                Some((key, text))
            }
            _ => None,
        }
    }

    /// Set the reading-measure cap and reflow.  `:set width=N` routes
    /// here (0 disables the cap).  Preserves the on-screen source
    /// position across the reflow, mirroring `resize`.
    pub fn set_max_measure(&mut self, measure: usize) {
        if self.max_measure == measure {
            return;
        }
        let anchor = ReflowAnchor::capture(self);
        self.max_measure = measure;
        self.rebuild_layout(LayoutRebuildReason::Resize);
        if let Some(anchor) = anchor
            && anchor.restore(self)
        {
            return;
        }
        self.clamp_position();
    }

    /// Reflow visual lines for a new terminal size.  Embedded hosts
    /// (trench) call this on every frame so the reader follows pane-
    /// size changes; standalone tread calls it on `Event::Resize`.
    /// Cheap when the dimensions match the cached size — the reflow is
    /// only re-run when something actually changed.
    pub fn resize(&mut self, width: u16, height: u16) {
        let w = width as usize;
        let h = height as usize;
        if self.width == w && self.height == h {
            // No reflow needed.  But if a pending saved-position offset is
            // still waiting (e.g. host called resize with the same width
            // it constructed at), apply it now anyway against the current
            // visual_lines — at least the offset clamps against THIS
            // width's total, which is closer to right than init's clamp.
            if let Some(saved_offset) = self.pending_progress_offset.take() {
                let max_offset = self.total_lines().saturating_sub(1);
                self.offset = saved_offset.min(max_offset);
                self.clamp_position();
            }
            return;
        }
        let anchor = if self.pending_progress_offset.is_none() {
            ReflowAnchor::capture(self)
        } else {
            None
        };
        self.width = w;
        self.height = h;
        self.rebuild_layout(LayoutRebuildReason::Resize);
        // Apply any deferred saved-position offset against the new reflow
        // BEFORE clamp_position() runs — clamp_position() is what would
        // otherwise discard a saved offset that's larger than the new
        // total_lines.  take() ensures this only fires on the first
        // resize after init; subsequent resizes leave the user's
        // current offset alone.
        if let Some(saved_offset) = self.pending_progress_offset.take() {
            let max_offset = self.total_lines().saturating_sub(1);
            self.offset = saved_offset.min(max_offset);
        } else if let Some(anchor) = anchor {
            if anchor.restore(self) {
                return;
            }
        }
        self.clamp_position();
    }

    /// Set the figure-preview-pane flag and keep dependent state coherent.
    ///
    /// Side effects: `text_only` is forced to match (otherwise figures
    /// would render both inline and in the preview, defeating the
    /// design); on activation, `current_figure` is seeded to `Some(0)`
    /// when figures exist so the preview opens with something visible.
    /// Re-toggling preserves whatever `current_figure` the user was on.
    pub fn set_figure_preview_active(&mut self, value: bool) {
        if self.figure_preview_active == value {
            return;
        }
        self.figure_preview_active = value;
        self.preview_state.active = value;
        self.set_text_only(value);
        self.refresh_preview_selection();
    }

    /// `i` binding in normal mode — flip the preview pane and persist
    /// the new value as the global default for next session.
    pub fn toggle_figure_preview(&mut self) {
        self.set_figure_preview_active(!self.figure_preview_active);
        let mut cfg = crate::config::load();
        cfg.figure_preview_default = self.figure_preview_active;
        crate::config::save(&cfg);
    }

    /// `]f` / `[f` step the cursor through `figure_kitty_ids` with
    /// wraparound at both ends.  Silent no-op when the preview pane is
    /// hidden or the document has no figures — matches the "predictable
    /// no-ops" decision from the design discussion.
    pub fn step_figure(&mut self, delta: i32) {
        if !self.preview_state.active {
            return;
        }
        let len = self.figure_index.entries.len();
        if len == 0 {
            return;
        }
        let current = self.preview_state.selected_index.unwrap_or(0) as i32;
        let next = (current + delta).rem_euclid(len as i32) as usize;
        self.set_preview_selection(Some(next));
    }

    /// `kitty_id` of the figure currently selected for the preview
    /// pane, or `None` when no preview is active or the document has
    /// no figures.  Used by `after_draw` to dispatch
    /// `images::place_one_figure`.
    pub fn current_figure_kitty_id(&self) -> Option<u32> {
        self.preview_state.selected_kitty_id
    }

    /// Ordered list of figure `kitty_id`s for the current document.
    /// Subfigures inside a `Block::ImageRow` are flattened: each item
    /// gets its own slot so `]f` / `[f` step through subfigures
    /// individually.  Used by hosts to drive a preview-pane navigation
    /// cursor (`current_figure` indexes into this vector).
    pub fn figure_kitty_ids(&self) -> Vec<u32> {
        self.figure_index.ordered_kitty_ids()
    }

    /// Whether the side pane has anything to show when active: the
    /// document has figures, OR it has bibliography entries (so a citation
    /// under the cursor can fill the pane).  Static (cursor-independent)
    /// so the pane's layout stays stable as the cursor moves.
    fn preview_pane_available(&self) -> bool {
        !self.figure_index.entries.is_empty() || !self.bib_entries.is_empty()
    }

    pub fn figure_preview_visible(&self) -> bool {
        self.preview_state.active && self.preview_pane_available()
    }

    fn preview_layout_active(&self) -> bool {
        self.preview_state.active && self.preview_pane_available()
    }

    pub fn figure_preview_state(&self) -> &FigurePreviewState {
        &self.preview_state
    }

    pub fn figure_count(&self) -> usize {
        self.figure_index.entries.len()
    }

    pub fn current_figure_position(&self) -> Option<(usize, usize)> {
        let selected = self.preview_state.selected_index?;
        let total = self.figure_count();
        if total == 0 {
            None
        } else {
            Some((selected + 1, total))
        }
    }

    /// The full FigureEntry for the figure currently shown in the
    /// preview, or `None` when no figure is selected.  Used by
    /// `render::draw_preview_pane` to render column headers and by
    /// `images::place_one_figure` to lay out the image grid.
    pub fn current_figure_entry(&self) -> Option<&FigureEntry> {
        let id = self.preview_state.selected_kitty_id?;
        self.figure_index.get(id)
    }

    /// The figure entry index a `\ref{fig:…}` under the cursor points at,
    /// if any.  When set, the preview pane shows this figure instead of
    /// the manually-selected one (context-first; `]f`/`[f` is the
    /// fallback).
    pub fn ref_figure_under_cursor(&self) -> Option<usize> {
        match self.link_under_cursor()? {
            doc_model::LinkTarget::Internal(label) => self.figure_labels.get(&label).copied(),
            _ => None,
        }
    }

    /// `kitty_id` of the figure the preview pane should show: the
    /// cursor's figure reference if any, else the manual selection.
    pub(crate) fn preview_figure_kitty_id(&self) -> Option<u32> {
        match self.ref_figure_under_cursor() {
            Some(idx) => self
                .figure_index
                .entries
                .get(idx)
                .map(|e| e.representative_kitty_id()),
            None => self.current_figure_kitty_id(),
        }
    }

    /// `FigureEntry` the preview pane should render (ref figure or manual).
    pub(crate) fn preview_figure_entry(&self) -> Option<&FigureEntry> {
        match self.ref_figure_under_cursor() {
            Some(idx) => self.figure_index.entries.get(idx),
            None => self.current_figure_entry(),
        }
    }

    /// `(position, total)` for the pane title — reflects the ref figure
    /// when the cursor is on a figure reference, else the manual figure.
    pub(crate) fn preview_figure_position(&self) -> Option<(usize, usize)> {
        match self.ref_figure_under_cursor() {
            Some(idx) => {
                let total = self.figure_count();
                (total > 0).then_some((idx + 1, total))
            }
            None => self.current_figure_position(),
        }
    }

    pub(crate) fn set_preview_geometry(&self, area: Option<Rect>) {
        self.preview_state
            .last_geometry
            .set(area.map(PreviewGeometry::from_rect));
    }

    /// Borrow the full `FigureEntry` for a specific part id — the
    /// preview tiler needs every row/column to lay out the whole
    /// figure, not just the matching part.
    pub(crate) fn figure_entry_for(&self, kitty_id: u32) -> Option<&FigureEntry> {
        self.figure_index.get(kitty_id)
    }

    /// Toggle inline figure rendering.  When `true`, `visual_lines` is
    /// rebuilt without `Image` / `ImageRow` rows so text reflows past
    /// where the figures would have been.  Used by hosts that draw
    /// figures in a dedicated preview pane via `images::place_one_figure`.
    ///
    /// Preserves `bib_entries` (keyed by cite-key, not visual-line
    /// index) so externally-merged bibitems aren't lost across toggles.
    pub fn set_text_only(&mut self, value: bool) {
        if self.text_only == value {
            return;
        }
        let anchor = ReflowAnchor::capture(self);
        self.text_only = value;
        self.rebuild_layout(LayoutRebuildReason::TextOnlyToggle);
        if let Some(anchor) = anchor {
            if anchor.restore(self) {
                return;
            }
        }
        self.clamp_position();
    }

    pub fn toggle_toc(&mut self) {
        let anchor = ReflowAnchor::capture(self);
        self.toc_visible = !self.toc_visible;
        self.rebuild_layout(LayoutRebuildReason::TocToggle);
        if let Some(anchor) = anchor {
            if anchor.restore(self) {
                return;
            }
        }
        self.clamp_position();
    }

    // ── Full-screen contents view (`:contents`) ───────────────────────

    /// Whether the full-screen contents view is open.
    pub fn contents_visible(&self) -> bool {
        self.contents_visible
    }

    /// Selected section index in the contents view (for the renderer).
    pub fn contents_selected(&self) -> usize {
        self.contents_selected
    }

    /// Open the contents view, seeding the selection at the section the
    /// reader is currently in.  No-op when the document has no sections.
    pub fn open_contents(&mut self) {
        if self.sections().is_empty() {
            return;
        }
        self.contents_selected = self.current_section_idx().unwrap_or(0);
        self.contents_visible = true;
    }

    /// Close the contents view without moving.
    pub fn close_contents(&mut self) {
        self.contents_visible = false;
    }

    /// Move the contents selection by `delta` rows (clamped).
    pub fn contents_move(&mut self, delta: i32) {
        let n = self.sections().len();
        if n == 0 {
            return;
        }
        let next = (self.contents_selected as i32 + delta).clamp(0, n as i32 - 1);
        self.contents_selected = next as usize;
    }

    /// Jump the selection to the first / last section.
    pub fn contents_jump_edge(&mut self, last: bool) {
        if last {
            self.contents_selected = self.sections().len().saturating_sub(1);
        } else {
            self.contents_selected = 0;
        }
    }

    /// Jump to the selected section and close the view.  Pushes a nav
    /// mark so `Ctrl+O` returns to the prior reading position.
    pub fn contents_jump_selected(&mut self) {
        let line = self.sections().get(self.contents_selected).map(|s| s.0);
        self.contents_visible = false;
        if let Some(line) = line {
            self.push_nav_mark();
            self.jump_to_line(line);
        }
    }

    /// Clamp offset and cursor_y to stay within current document bounds.
    pub fn clamp_position(&mut self) {
        let total = self.visual_lines.len();
        let ch = self.content_height();
        if total == 0 {
            self.offset = 0;
            self.cursor_y = 0;
            return;
        }
        let max_offset = total.saturating_sub(ch).max(0);
        self.offset = self.offset.min(max_offset);
        let max_cursor = ch
            .saturating_sub(1)
            .min(total.saturating_sub(1 + self.offset));
        self.cursor_y = self.cursor_y.min(max_cursor);
    }

    /// Push current position onto the back-navigation stack before a jump.
    pub fn push_nav_mark(&mut self) {
        let pos = (self.offset, self.cursor_y);
        if self.nav_history.last() != Some(&pos) {
            self.nav_history.push(pos);
            // Cap history at 50 entries to avoid unbounded growth.
            if self.nav_history.len() > 50 {
                self.nav_history.remove(0);
            }
        }
    }

    /// Return to the previous position in the back-navigation stack.
    pub fn nav_back(&mut self) {
        if let Some((offset, cursor_y)) = self.nav_history.pop() {
            self.offset = offset;
            self.cursor_y = cursor_y;
            self.clamp_cursor_after_line_change();
        }
    }

    pub fn toggle_help(&mut self) {
        let next = !self.help_visible;
        self.help_visible = next;
        if next {
            self.help_query.clear();
            self.help_selected = 0;
        }
    }

    pub fn close_help(&mut self) {
        self.help_visible = false;
    }

    pub fn move_help_selection(&mut self, delta: isize, total: usize) {
        if total == 0 {
            self.help_selected = 0;
            return;
        }
        let max = total.saturating_sub(1) as isize;
        let next = (self.help_selected as isize + delta).clamp(0, max) as usize;
        self.help_selected = next;
    }

    pub fn clamp_help_selection(&mut self, total: usize) {
        if total == 0 {
            self.help_selected = 0;
        } else {
            self.help_selected = self.help_selected.min(total.saturating_sub(1));
        }
    }

    /// Index into `sections` of the last section header at or above the current line.
    pub fn current_section_idx(&self) -> Option<usize> {
        let cur = self.current_line();
        self.sections.iter().rposition(|s| s.0 <= cur)
    }

    pub fn current_line(&self) -> usize {
        self.offset + self.cursor_y
    }

    pub fn total_lines(&self) -> usize {
        self.visual_lines.len()
    }

    pub fn content_height(&self) -> usize {
        content_height_for(
            self.height,
            self.meta.is_some(),
            matches!(self.mode, Mode::Search | Mode::Command),
        )
    }

    pub fn update_search_matches(&mut self) {
        let q = self.search_query.to_lowercase();
        self.search_matches = if q.is_empty() {
            Vec::new()
        } else {
            self.visual_lines
                .iter()
                .enumerate()
                .filter(|(_, vl)| vl.text.to_lowercase().contains(&q))
                .map(|(i, _)| i)
                .collect()
        };
        self.search_idx = 0;
    }

    pub fn jump_to_match(&mut self, idx: usize) {
        if self.search_matches.is_empty() {
            return;
        }
        let line = self.search_matches[idx];
        self.offset = line;
        self.cursor_y = 0;
        self.clamp_cursor_after_line_change();
    }
}

/// Compute the full content (structural) width given terminal width,
/// TOC visibility, and the figure-preview pane.  This is the width
/// tables, figures, display math and the draw area use.  The narrower
/// reading measure for prose is derived separately by
/// `Reader::prose_width`.
fn content_width_for(terminal_width: usize, toc_visible: bool, preview_visible: bool) -> usize {
    let content_width = if toc_visible {
        // +1 for the border column.
        terminal_width.saturating_sub(TOC_WIDTH + 1)
    } else {
        terminal_width
    };
    if preview_visible {
        content_width.saturating_mul(PREVIEW_TEXT_PERCENT) / 100
    } else {
        content_width
    }
}

fn content_height_for(terminal_height: usize, has_header: bool, has_prompt: bool) -> usize {
    let header = usize::from(has_header);
    let status = 1;
    let prompt = usize::from(has_prompt);
    let content_height = terminal_height.saturating_sub(header + status + prompt);
    content_height.saturating_sub(reader_vertical_margin(content_height) * 2)
}

fn build_sections(visual_lines: &[VisualLine]) -> Vec<(usize, u8, String)> {
    visual_lines
        .iter()
        .enumerate()
        .filter_map(|(i, vl)| {
            if let VisualLineKind::Header { level, number } = &vl.kind {
                // Bake the number into the section title so the TOC shows
                // it and `:goto 3.2` can match it — on both parser paths.
                let title = match number {
                    Some(n) => format!("{n}  {}", vl.text),
                    None => vl.text.clone(),
                };
                Some((i, *level, title))
            } else {
                None
            }
        })
        .collect()
}

/// Build the cross-reference resolution maps from `Block::Anchor`
/// markers and bibliography div ids.  Returns `(label_lines,
/// bib_entries, bib_entry_lines)`.
///
/// - `label_lines`: each Anchor associates with the first VL of the
///   *next* visible block.  For ref-targets we want the equation /
///   figure / table fully visible, so callers (`follow_link_target`)
///   subtract one when jumping.
/// - `bib_entries`: keys are cite-keys (Pandoc strips the `ref-`
///   prefix); value is the joined text of the entry block.
/// - `bib_entry_lines`: same keys, value is the first VL index of the
///   entry — used by `Enter` (jump-to-bib).
fn build_link_indexes(
    blocks: &[Block],
    visual_lines: &[VisualLine],
) -> (
    HashMap<String, usize>,
    HashMap<String, String>,
    HashMap<String, usize>,
    HashMap<String, usize>,
) {
    // Map block_idx → first VL with that block_idx.  O(n) once.
    let mut block_to_vl: HashMap<usize, usize> = HashMap::new();
    for (vl_idx, vl) in visual_lines.iter().enumerate() {
        block_to_vl.entry(vl.block_idx).or_insert(vl_idx);
    }

    // Map each `Block::Figure`'s block_idx → its figure entry index
    // (0-based, document order) — matches `FigureIndex::entries`.  Lets a
    // figure cross-reference resolve to a slot in the preview pane.
    let mut figure_entry_of_block: HashMap<usize, usize> = HashMap::new();
    let mut fig_idx = 0usize;
    for (bi, block) in blocks.iter().enumerate() {
        if matches!(block, Block::Figure { .. }) {
            figure_entry_of_block.insert(bi, fig_idx);
            fig_idx += 1;
        }
    }

    let mut label_lines = HashMap::new();
    let mut bib_entries = HashMap::new();
    let mut bib_entry_lines = HashMap::new();
    let mut figure_labels = HashMap::new();

    for (bi, block) in blocks.iter().enumerate() {
        if let Block::Anchor(label) = block {
            // Walk forward from bi+1 to find the next visible block.
            let target_block = (bi + 1..blocks.len())
                .find(|&j| !matches!(blocks[j], Block::Anchor(_) | Block::Blank));
            let target_vl = target_block.and_then(|j| block_to_vl.get(&j).copied());
            if let Some(vl) = target_vl {
                // Pandoc bib divs have id="ref-<key>".  Strip the prefix and
                // also capture the entry text for popup display.
                if let Some(key) = label.strip_prefix("ref-") {
                    bib_entry_lines.insert(key.to_string(), vl);
                    if let Some(j) = target_block {
                        let entry_text = block_text(&blocks[j]);
                        if !entry_text.is_empty() {
                            bib_entries.insert(key.to_string(), entry_text);
                        }
                    }
                } else {
                    label_lines.insert(label.clone(), vl);
                }
            }
            // Figure label: when the anchored target is a figure, also map
            // the label to its preview-pane slot (independent of whether
            // the figure had a visual line, e.g. in text-only mode).
            if let Some(entry) = target_block.and_then(|j| figure_entry_of_block.get(&j)) {
                figure_labels.insert(label.clone(), *entry);
            }
        }
    }
    (label_lines, bib_entries, bib_entry_lines, figure_labels)
}

/// Extract the rendered text of a block for bib-entry popup display.
/// Strips inline styling — popup is plain text only.
fn block_text(block: &Block) -> String {
    match block {
        Block::Line(s) => s.clone(),
        Block::StyledLine(spans) => spans.iter().map(|s| s.text.as_str()).collect(),
        Block::Header { text, .. } => text.clone(),
        Block::ListItem { content, .. } => content.iter().map(|s| s.text.as_str()).collect(),
        Block::Quote(spans) => spans.iter().map(|s| s.text.as_str()).collect(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_pane_activates_on_figureless_paper_with_citations() {
        // No figures, but a bibliography entry → the pane is available so a
        // citation under the cursor can fill it.
        let bibitems = HashMap::from([("k".to_string(), "An entry.".to_string())]);
        let mut reader =
            Reader::new_with_bibitems(vec![Block::Line("prose".into())], 80, 24, bibitems);
        reader.set_figure_preview_active(true);
        assert!(reader.figure_preview_visible());

        // Neither figures nor references → nothing to show, pane stays hidden.
        let mut bare = Reader::new(vec![Block::Line("prose".into())], 80, 24);
        bare.set_figure_preview_active(true);
        assert!(!bare.figure_preview_visible());
    }

    #[test]
    fn cursor_citation_resolves_entry_under_cursor() {
        use doc_model::{InlineSpan, LinkTarget};
        // Leading citation span so byte 0 of the wrapped line is on it.
        let blocks = vec![Block::StyledLine(vec![
            InlineSpan {
                text: "[1]".into(),
                link_target: Some(LinkTarget::Citation("smith".into())),
                ..Default::default()
            },
            InlineSpan {
                text: " and more text".into(),
                ..Default::default()
            },
        ])];
        let bibitems = HashMap::from([(
            "smith".to_string(),
            "Smith, J. A great paper. 2020.".to_string(),
        )]);
        let mut reader = Reader::new_with_bibitems(blocks, 80, 24, bibitems);

        reader.set_cursor_x(0);
        assert_eq!(
            reader.cursor_citation(),
            Some((
                "smith".to_string(),
                "Smith, J. A great paper. 2020.".to_string()
            ))
        );

        // A plain line (no link span) yields no citation context.
        let mut plain = Reader::new(vec![Block::Line("just prose".into())], 80, 24);
        plain.set_cursor_x(2);
        assert_eq!(plain.cursor_citation(), None);
    }

    #[test]
    fn figure_ref_under_cursor_resolves_to_figure_index() {
        use doc_model::{InlineSpan, LinkTarget};
        let blocks = vec![
            Block::StyledLine(vec![InlineSpan {
                text: "fig1".into(),
                link_target: Some(LinkTarget::Internal("fig:1".into())),
                ..Default::default()
            }]),
            Block::Anchor("fig:1".into()),
            Block::Figure {
                rows: vec![vec![doc_model::ImageItem {
                    path: std::path::PathBuf::from("a.png"),
                    kitty_id: 7,
                    dims: Some((100, 100)),
                }]],
                alt: "a figure".into(),
                figure_id: 1,
                column_gaps_after: Vec::new(),
                header_rows: Vec::new(),
            },
        ];
        let mut reader = Reader::new(blocks, 80, 24);

        // Cursor on the figure reference resolves to figure entry 0 and
        // the pane focuses that figure's representative image.
        reader.set_cursor_x(0);
        assert_eq!(reader.ref_figure_under_cursor(), Some(0));
        assert_eq!(reader.preview_figure_kitty_id(), Some(7));
        assert_eq!(reader.preview_figure_position(), Some((1, 1)));

        // A non-figure label doesn't resolve.
        let plain = Reader::new(vec![Block::Line("prose".into())], 80, 24);
        assert_eq!(plain.ref_figure_under_cursor(), None);
    }

    #[test]
    fn contents_view_open_move_and_jump() {
        let blocks = vec![
            Block::Header {
                level: 1,
                text: "One".into(),
                number: Some("1".into()),
            },
            Block::Line("a".into()),
            Block::Header {
                level: 1,
                text: "Two".into(),
                number: Some("2".into()),
            },
            Block::Line("b".into()),
            Block::Header {
                level: 2,
                text: "Two-One".into(),
                number: Some("2.1".into()),
            },
            Block::Line("c".into()),
        ];
        let mut reader = Reader::new(blocks, 80, 24);
        assert_eq!(reader.sections().len(), 3);

        reader.open_contents();
        assert!(reader.contents_visible());
        // Seeded at the section the reader is in (top → section 0).
        assert_eq!(reader.contents_selected(), 0);

        reader.contents_move(1);
        assert_eq!(reader.contents_selected(), 1);
        reader.contents_move(-5); // clamps at 0
        assert_eq!(reader.contents_selected(), 0);
        reader.contents_jump_edge(true);
        assert_eq!(reader.contents_selected(), 2);

        // Enter jumps and closes the view.
        reader.contents_jump_selected();
        assert!(!reader.contents_visible());
    }

    #[test]
    fn sections_bake_number_into_title_for_toc_and_goto() {
        let reader = Reader::new(
            vec![
                Block::Header {
                    level: 1,
                    text: "Background".to_string(),
                    number: Some("2".to_string()),
                },
                Block::Line("body".to_string()),
                Block::Header {
                    level: 1,
                    text: "References".to_string(),
                    number: None,
                },
            ],
            80,
            24,
        );
        let titles: Vec<&str> = reader.sections().iter().map(|s| s.2.as_str()).collect();
        // Numbered section gets "N  Title"; unnumbered stays bare.
        assert_eq!(titles, vec!["2  Background", "References"]);
    }

    fn doc_with_one_image() -> Vec<Block> {
        vec![
            Block::Line("paragraph one".to_string()),
            Block::Figure {
                rows: vec![vec![doc_model::ImageItem {
                    path: std::path::PathBuf::from("nowhere.png"),
                    kitty_id: 1,
                    dims: Some((100, 100)),
                }]],
                alt: "alt text".to_string(),
                figure_id: 1,
                column_gaps_after: Vec::new(),
                header_rows: Vec::new(),
            },
            Block::Line("paragraph two".to_string()),
        ]
    }

    fn styled_line_doc(text: &str) -> Vec<Block> {
        vec![Block::StyledLine(vec![doc_model::InlineSpan::plain(text)])]
    }

    fn line_covering_byte(reader: &Reader, block_idx: usize, byte: usize) -> Option<usize> {
        reader
            .visual_lines
            .iter()
            .enumerate()
            .find_map(|(idx, vl)| {
                if vl.block_idx == block_idx
                    && byte >= vl.block_byte_start
                    && byte < vl.block_byte_end
                {
                    Some(idx)
                } else {
                    None
                }
            })
    }

    #[test]
    fn text_only_filters_image_visual_lines() {
        let mut reader = Reader::new(doc_with_one_image(), 80, 24);
        let inline_has_image = reader.visual_lines.iter().any(|vl| {
            matches!(
                vl.kind,
                VisualLineKind::Image { .. } | VisualLineKind::ImageRow { .. },
            )
        });
        assert!(
            inline_has_image,
            "default reader should render image rows inline"
        );

        reader.set_text_only(true);
        let text_only_has_image = reader.visual_lines.iter().any(|vl| {
            matches!(
                vl.kind,
                VisualLineKind::Image { .. } | VisualLineKind::ImageRow { .. },
            )
        });
        assert!(
            !text_only_has_image,
            "text_only mode must not emit image rows"
        );
        assert!(reader.text_only);
    }

    #[test]
    fn set_text_only_round_trips() {
        let mut reader = Reader::new(doc_with_one_image(), 80, 24);
        let before = reader.visual_lines.len();
        reader.set_text_only(true);
        reader.set_text_only(false);
        assert_eq!(
            reader.visual_lines.len(),
            before,
            "toggling back restores inline figures"
        );
        assert!(!reader.text_only);
    }

    #[test]
    fn figure_kitty_ids_groups_image_rows() {
        use doc_model::ImageItem;
        let blocks = vec![
            Block::Line("intro".to_string()),
            Block::Figure {
                rows: vec![vec![ImageItem {
                    path: std::path::PathBuf::from("a.png"),
                    kitty_id: 1,
                    dims: Some((100, 100)),
                }]],
                alt: String::new(),
                figure_id: 1,
                column_gaps_after: Vec::new(),
                header_rows: Vec::new(),
            },
            Block::Line("middle".to_string()),
            Block::Figure {
                rows: vec![vec![
                    ImageItem {
                        path: std::path::PathBuf::from("b.png"),
                        kitty_id: 2,
                        dims: Some((100, 100)),
                    },
                    ImageItem {
                        path: std::path::PathBuf::from("c.png"),
                        kitty_id: 3,
                        dims: Some((100, 100)),
                    },
                ]],
                alt: String::new(),
                figure_id: 2,
                column_gaps_after: Vec::new(),
                header_rows: Vec::new(),
            },
            Block::Line("end".to_string()),
        ];
        let reader = Reader::new(blocks, 80, 24);
        // Two logical figures: the standalone image, and the row of two
        // subfigures.  Subfigures share their parent figure under the
        // representative id (the first part), so id=2 represents that
        // whole row.
        assert_eq!(reader.figure_kitty_ids(), vec![1, 2]);
        assert_eq!(reader.figure_count(), 2);
    }

    #[test]
    fn figure_index_groups_stacked_siblings_as_one_figure() {
        // The parser emits one `Block::Figure` per source figure with
        // the full 2D layout baked in.  A 3-row stacked figure (one
        // image, then a 2-subfigure row, then one image) becomes one
        // Block::Figure with rows = [[a], [b, c], [d]] and a single
        // caption — exactly one logical figure end-to-end.
        use doc_model::ImageItem;
        let blocks = vec![Block::Figure {
            rows: vec![
                vec![ImageItem {
                    path: "row1.png".into(),
                    kitty_id: 1,
                    dims: Some((100, 50)),
                }],
                vec![
                    ImageItem {
                        path: "row2a.png".into(),
                        kitty_id: 2,
                        dims: Some((100, 50)),
                    },
                    ImageItem {
                        path: "row2b.png".into(),
                        kitty_id: 3,
                        dims: Some((100, 50)),
                    },
                ],
                vec![ImageItem {
                    path: "row3.png".into(),
                    kitty_id: 4,
                    dims: Some((100, 50)),
                }],
            ],
            alt: "Fig 3 caption".to_string(),
            figure_id: 3,
            column_gaps_after: Vec::new(),
            header_rows: Vec::new(),
        }];
        let index = FigureIndex::build(&blocks);
        assert_eq!(
            index.entries().len(),
            1,
            "three stacked sibling rows should be ONE logical figure"
        );
        let entry = &index.entries()[0];
        assert_eq!(entry.rows.len(), 3, "row structure must survive");
        assert_eq!(entry.rows[1].len(), 2, "side-by-side row keeps its width");
        assert_eq!(entry.parts().count(), 4);
        assert_eq!(entry.alt, "Fig 3 caption");
        assert_eq!(index.ordered_kitty_ids(), vec![1]);
        // get(any_part_id) resolves to the same grouped entry.
        for id in [1, 2, 3, 4] {
            assert_eq!(
                index.get(id).map(|e| e.alt.as_str()),
                Some("Fig 3 caption"),
                "id={id} should resolve to the grouped figure"
            );
        }
    }

    #[test]
    fn figure_kitty_ids_survives_text_only_toggle() {
        let mut reader = Reader::new(doc_with_one_image(), 80, 24);
        let before = reader.figure_kitty_ids();
        reader.set_text_only(true);
        let after = reader.figure_kitty_ids();
        assert_eq!(
            before, after,
            "figure list is derived from blocks, not visual_lines"
        );
    }

    #[test]
    fn set_figure_preview_active_couples_text_only() {
        let mut reader = Reader::new(doc_with_one_image(), 80, 24);
        assert!(!reader.text_only);
        reader.set_figure_preview_active(true);
        assert!(reader.figure_preview_active);
        assert!(reader.text_only, "preview on should force text_only on");
        reader.set_figure_preview_active(false);
        assert!(!reader.figure_preview_active);
        assert!(!reader.text_only, "preview off should clear text_only");
    }

    #[test]
    fn set_figure_preview_active_seeds_current_figure() {
        let mut reader = Reader::new(doc_with_one_image(), 80, 24);
        assert!(reader.current_figure.is_none());
        reader.set_figure_preview_active(true);
        assert_eq!(reader.current_figure, Some(0));
    }

    #[test]
    fn reading_measure_narrows_prose_not_content_width() {
        // Wide terminal: full content width is 200 (no horizontal page
        // margin), unchanged by the measure (tables/figures use it).
        // Only the prose column is capped.
        let mut reader = Reader::new(vec![Block::Line("hi".to_string())], 200, 24);
        assert_eq!(reader.content_width(), 200);
        assert_eq!(reader.prose_width(), DEFAULT_MAX_MEASURE);

        // 0 disables the cap → prose flows the full content width.
        reader.set_max_measure(0);
        assert_eq!(reader.content_width(), 200);
        assert_eq!(reader.prose_width(), 200);

        // A custom cap below the available width takes effect on prose.
        reader.set_max_measure(100);
        assert_eq!(reader.prose_width(), 100);

        // A cap wider than what's available clamps to the content width.
        reader.set_max_measure(500);
        assert_eq!(reader.prose_width(), 200);
    }

    #[test]
    fn figure_preview_reflows_to_reader_pane_width() {
        let mut blocks = doc_with_one_image();
        blocks.insert(0, Block::Rule);
        let mut reader = Reader::new(blocks, 100, 24);
        // Full structural width: 100 (no horizontal page margin).
        assert_eq!(reader.content_width(), 100);

        reader.set_figure_preview_active(true);

        // Preview text pane is 60% of 100 = 60.
        assert_eq!(reader.content_width(), 60);
        assert_eq!(
            reader
                .visual_lines
                .iter()
                .find(|vl| matches!(vl.kind, VisualLineKind::Rule))
                .map(|vl| vl.text.chars().count()),
            Some(reader.content_width()),
            "preview mode should wrap text to the left reader pane"
        );
    }

    #[test]
    fn resize_preserves_current_source_position_after_reflow() {
        let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon";
        let target_byte = text.find("lambda").expect("target word");
        let mut reader = Reader::new(styled_line_doc(text), 30, 24);
        let target_line = line_covering_byte(&reader, 0, target_byte)
            .expect("target byte should resolve before resize");
        reader.jump_to_line(target_line);
        let line_start = reader.visual_lines[target_line].block_byte_start;
        reader.set_cursor_x(target_byte - line_start);

        reader.resize(80, 24);

        let landed = reader.visual_lines[reader.current_line()].clone();
        assert_eq!(landed.block_idx, 0);
        assert!(
            target_byte >= landed.block_byte_start && target_byte < landed.block_byte_end,
            "current line after resize should still cover source byte {target_byte}, got {:?}",
            landed
        );
    }

    #[test]
    fn preview_toggle_preserves_current_source_position_after_reflow() {
        let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon";
        let target_byte = text.find("lambda").expect("target word");
        let mut blocks = styled_line_doc(text);
        blocks.push(Block::Figure {
            rows: vec![vec![doc_model::ImageItem {
                path: std::path::PathBuf::from("preview.png"),
                kitty_id: 9,
                dims: Some((100, 100)),
            }]],
            alt: "caption".to_string(),
            figure_id: 1,
            column_gaps_after: Vec::new(),
            header_rows: Vec::new(),
        });
        let mut reader = Reader::new(blocks, 80, 24);
        let target_line = line_covering_byte(&reader, 0, target_byte)
            .expect("target byte should resolve before preview");
        reader.jump_to_line(target_line);
        let line_start = reader.visual_lines[target_line].block_byte_start;
        reader.set_cursor_x(target_byte - line_start);

        reader.set_figure_preview_active(true);

        let landed = reader.visual_lines[reader.current_line()].clone();
        assert_eq!(landed.block_idx, 0);
        assert!(
            target_byte >= landed.block_byte_start && target_byte < landed.block_byte_end,
            "current line after preview reflow should still cover source byte {target_byte}, got {:?}",
            landed
        );
    }

    #[test]
    fn content_height_reserves_command_prompt_row() {
        let mut reader = Reader::new(doc_with_one_image(), 80, 24);
        let normal_height = reader.content_height();

        reader.enter_command_mode();

        assert_eq!(reader.content_height(), normal_height - 1);
    }

    #[test]
    fn step_figure_wraps_in_both_directions() {
        use doc_model::ImageItem;
        let blocks = vec![
            Block::Figure {
                rows: vec![vec![ImageItem {
                    path: std::path::PathBuf::from("a.png"),
                    kitty_id: 1,
                    dims: Some((100, 100)),
                }]],
                alt: "first".to_string(),
                figure_id: 1,
                column_gaps_after: Vec::new(),
                header_rows: Vec::new(),
            },
            Block::Line("between".to_string()),
            Block::Figure {
                rows: vec![vec![
                    ImageItem {
                        path: "b.png".into(),
                        kitty_id: 2,
                        dims: Some((100, 100)),
                    },
                    ImageItem {
                        path: "c.png".into(),
                        kitty_id: 3,
                        dims: Some((100, 100)),
                    },
                ]],
                alt: "second".to_string(),
                figure_id: 2,
                column_gaps_after: Vec::new(),
                header_rows: Vec::new(),
            },
        ];
        let mut reader = Reader::new(blocks, 80, 24);
        reader.set_figure_preview_active(true);
        // Two distinct figures, each its own Block::Figure.
        assert_eq!(reader.figure_count(), 2);
        assert_eq!(reader.current_figure, Some(0));
        reader.step_figure(1);
        assert_eq!(reader.current_figure, Some(1));
        // Wrap forward.
        reader.step_figure(1);
        assert_eq!(reader.current_figure, Some(0));
        // Wrap backward.
        reader.step_figure(-1);
        assert_eq!(reader.current_figure, Some(1));
    }

    #[test]
    fn step_figure_is_noop_when_preview_inactive() {
        let mut reader = Reader::new(doc_with_one_image(), 80, 24);
        reader.step_figure(1);
        assert!(reader.current_figure.is_none());
    }

    #[test]
    fn current_figure_kitty_id_resolves_index_to_id() {
        let mut reader = Reader::new(doc_with_one_image(), 80, 24);
        reader.set_figure_preview_active(true);
        assert_eq!(reader.current_figure_kitty_id(), Some(1));
        assert!(reader.figure_preview_visible());
        reader.set_figure_preview_active(false);
        // Cleared preview still leaves current_figure for resumption, but
        // current_figure_kitty_id is None because the index is invalidated
        // by the preview being off — actually no, we kept the index per
        // design.  Verify the resume behaviour: id lookup still works.
        assert_eq!(reader.current_figure_kitty_id(), Some(1));
        assert!(!reader.figure_preview_visible());
    }

    #[test]
    fn set_text_only_preserves_image_paths() {
        // image_paths is keyed off blocks, not visual_lines — Phase C
        // needs it intact in text_only mode so the preview pane can
        // still look up paths by kitty_id.
        let mut reader = Reader::new(doc_with_one_image(), 80, 24);
        assert_eq!(reader.image_paths.len(), 1);
        reader.set_text_only(true);
        assert_eq!(reader.image_paths.len(), 1);
    }

    fn assert_layout_cache_mirrors_public_fields(reader: &Reader) {
        assert_eq!(
            reader.visual_lines.len(),
            reader.layout_cache.visual_lines.len()
        );
        assert_eq!(reader.sections, reader.layout_cache.sections);
        assert_eq!(reader.label_lines, reader.layout_cache.label_lines);
        assert_eq!(reader.bib_entries, reader.layout_cache.bib_entries);
        assert_eq!(reader.bib_entry_lines, reader.layout_cache.bib_entry_lines);
    }

    #[test]
    fn layout_cache_rebuilds_for_resize_toc_text_only_and_reload() {
        let mut reader = Reader::new_with_bibitems(
            vec![
                Block::Header {
                    level: 1,
                    text: "Intro".to_string(),
                    number: None,
                },
                Block::Anchor("fig:intro".to_string()),
                Block::Figure {
                    rows: vec![vec![doc_model::ImageItem {
                        path: std::path::PathBuf::from("a.png"),
                        kitty_id: 1,
                        dims: Some((100, 100)),
                    }]],
                    alt: "caption".to_string(),
                    figure_id: 1,
                    column_gaps_after: Vec::new(),
                    header_rows: Vec::new(),
                },
                Block::Anchor("sec:intro".to_string()),
                Block::Line("anchored prose".to_string()),
                Block::Anchor("ref-smith".to_string()),
                Block::Line("Smith bibliography entry".to_string()),
            ],
            80,
            24,
            HashMap::from([("smith".to_string(), "external entry".to_string())]),
        );

        assert_layout_cache_mirrors_public_fields(&reader);
        assert_eq!(
            reader.bib_entries.get("smith"),
            Some(&"external entry".to_string())
        );

        reader.resize(60, 20);
        assert_layout_cache_mirrors_public_fields(&reader);

        reader.toggle_toc();
        assert_layout_cache_mirrors_public_fields(&reader);

        reader.set_text_only(true);
        assert_layout_cache_mirrors_public_fields(&reader);
        assert!(reader.label_lines.contains_key("sec:intro"));

        reader.reload_with(
            vec![
                Block::Header {
                    level: 1,
                    text: "Reloaded".to_string(),
                    number: None,
                },
                Block::Line("body".to_string()),
            ],
            HashMap::from([("doe".to_string(), "new entry".to_string())]),
        );
        assert_layout_cache_mirrors_public_fields(&reader);
        assert_eq!(
            reader.bib_entries.get("doe"),
            Some(&"new entry".to_string())
        );
        assert!(!reader.bib_entries.contains_key("smith"));
    }

    #[test]
    fn figure_index_returns_ordered_ids_and_metadata() {
        use doc_model::ImageItem;
        let blocks = vec![
            Block::Figure {
                rows: vec![vec![ImageItem {
                    path: std::path::PathBuf::from("a.png"),
                    kitty_id: 1,
                    dims: Some((10, 20)),
                }]],
                alt: "single".to_string(),
                figure_id: 1,
                column_gaps_after: Vec::new(),
                header_rows: Vec::new(),
            },
            Block::Figure {
                rows: vec![vec![
                    ImageItem {
                        path: "b.png".into(),
                        kitty_id: 2,
                        dims: Some((30, 40)),
                    },
                    ImageItem {
                        path: "c.png".into(),
                        kitty_id: 3,
                        dims: None,
                    },
                ]],
                alt: "row".to_string(),
                figure_id: 2,
                column_gaps_after: Vec::new(),
                header_rows: Vec::new(),
            },
        ];

        let index = FigureIndex::build(&blocks);

        // Two logical figures: the standalone Image (id=1) and the row
        // of subfigures (id=2 represents that row; id=3 is its sibling
        // part and also resolves to the same entry).
        assert_eq!(index.ordered_kitty_ids(), vec![1, 2]);
        assert_eq!(index.entries().len(), 2);
        assert_eq!(index.entries()[1].parts().count(), 2);

        assert_eq!(index.get(1).map(|entry| entry.alt.as_str()), Some("single"));
        assert_eq!(index.get(2).map(|entry| entry.alt.as_str()), Some("row"));
        assert_eq!(index.get(3).map(|entry| entry.alt.as_str()), Some("row"));

        // Part-specific metadata is still addressable: id=2 keeps its own
        // (30,40) dims, id=3 keeps its None dims.  The aspect-correct
        // tiling path depends on this.
        let part2 = index
            .get(2)
            .unwrap()
            .parts()
            .find(|p| p.kitty_id == 2)
            .unwrap();
        let part3 = index
            .get(3)
            .unwrap()
            .parts()
            .find(|p| p.kitty_id == 3)
            .unwrap();
        assert_eq!(part2.path.as_path(), std::path::Path::new("b.png"));
        assert_eq!(part2.dims, Some((30, 40)));
        assert_eq!(part3.dims, None);
    }

    #[test]
    fn reload_rebuilds_figure_index_and_image_paths() {
        let mut reader = Reader::new(doc_with_one_image(), 80, 24);
        assert_eq!(reader.figure_kitty_ids(), vec![1]);

        reader.reload_with(
            vec![Block::Figure {
                rows: vec![vec![doc_model::ImageItem {
                    path: std::path::PathBuf::from("next.png"),
                    kitty_id: 42,
                    dims: Some((200, 100)),
                }]],
                alt: String::new(),
                figure_id: 1,
                column_gaps_after: Vec::new(),
                header_rows: Vec::new(),
            }],
            HashMap::new(),
        );

        assert_eq!(reader.figure_kitty_ids(), vec![42]);
        assert_eq!(
            reader.image_paths.get(&42),
            Some(&std::path::PathBuf::from("next.png"))
        );
        assert!(!reader.image_paths.contains_key(&1));
    }

    #[test]
    fn preview_state_mirrors_public_compat_fields() {
        let mut reader = Reader::new(doc_with_one_image(), 80, 24);

        reader.set_figure_preview_active(true);
        assert!(reader.figure_preview_state().active);
        assert_eq!(
            reader.figure_preview_state().selected_index,
            reader.current_figure
        );
        assert_eq!(reader.figure_preview_state().selected_kitty_id, Some(1));

        reader.set_figure_preview_active(false);
        assert!(!reader.figure_preview_state().active);
        assert_eq!(
            reader.figure_preview_state().selected_index,
            reader.current_figure
        );
        assert_eq!(reader.figure_preview_state().selected_kitty_id, Some(1));
    }

    #[test]
    fn preview_geometry_tracks_last_pane_rect() {
        let reader = Reader::new(doc_with_one_image(), 80, 24);
        assert!(reader.figure_preview_state().last_geometry().is_none());

        reader.set_preview_geometry(Some(Rect::new(10, 2, 30, 12)));
        assert_eq!(
            reader.figure_preview_state().last_geometry(),
            Some(PreviewGeometry {
                x: 10,
                y: 2,
                width: 30,
                height: 12
            }),
        );

        reader.set_preview_geometry(None);
        assert!(reader.figure_preview_state().last_geometry().is_none());
    }

    // Audit C2 regression: search → resize → `n` previously left
    // `search_matches` holding visual-line indices from the pre-resize
    // layout.  The new layout's `visual_lines` is shorter or shifted,
    // so the stale indices either pointed past the end (unsafe offset)
    // or to unrelated lines (wrong "next match" target).  rebuild_layout
    // now owns invalidation; this test exercises the full lifecycle.
    #[test]
    fn search_matches_remap_after_resize() {
        // A wide line that wraps differently at 80 vs 20 cols.  The
        // wrapping shift is what makes search_match indices stale.
        let long = "alpha beta gamma delta epsilon zeta eta theta iota foo kappa lambda mu nu xi";
        let blocks = vec![
            Block::Line(long.to_string()),
            Block::Line("middle".to_string()),
            Block::Line("end with foo too".to_string()),
        ];
        let mut reader = Reader::new(blocks, 80, 24);
        reader.search_query = "foo".into();
        reader.update_search_matches();
        assert_eq!(reader.search_matches.len(), 2);

        // Resize narrower — the long line now wraps onto more visual
        // rows, shifting the index of every later match.
        reader.resize(20, 24);

        // After rebuild, every stored match must still be in range.
        let total = reader.visual_lines.len();
        for &m in &reader.search_matches {
            assert!(
                m < total,
                "stale search_match index {m} past visual_lines.len() {total}",
            );
        }
        // Match count is content-driven (we have 2 "foo" occurrences),
        // not layout-driven — the count must survive reflow.
        assert_eq!(reader.search_matches.len(), 2);

        // `n` and `N` are the user-visible crash trigger from the audit.
        // Both must complete without panicking after the resize.
        reader.search_next();
        reader.search_next();
        reader.search_prev();

        // And the resulting offset must be a valid visual-line index.
        assert!(reader.offset < total);
    }

    // Edge: search → resize when the query is empty (e.g. user cleared
    // it).  The rebuild path must not preserve a stale match set.
    #[test]
    fn empty_query_clears_search_matches_after_resize() {
        let blocks = vec![
            Block::Line("only line with foo".to_string()),
            Block::Line("nothing else".to_string()),
        ];
        let mut reader = Reader::new(blocks, 80, 24);
        reader.search_query = "foo".into();
        reader.update_search_matches();
        assert_eq!(reader.search_matches.len(), 1);

        // User cancels search but keeps current matches around (the
        // event loop's cancel path clears search_query but rebuild
        // could still run on resize).
        reader.search_query.clear();
        reader.resize(30, 24);

        assert!(reader.search_matches.is_empty());
        assert_eq!(reader.search_idx, 0);
    }

    // Re-anchor contract: after rebuild, `search_idx` should point at
    // the first match at or after the user's prior anchor, so pressing
    // `n` continues forward instead of jumping back to match 0.
    #[test]
    fn search_idx_reanchors_to_current_position_on_rebuild() {
        let blocks = vec![
            Block::Line("foo one".to_string()),
            Block::Line("foo two".to_string()),
            Block::Line("foo three".to_string()),
            Block::Line("foo four".to_string()),
        ];
        let mut reader = Reader::new(blocks, 80, 24);
        reader.search_query = "foo".into();
        reader.update_search_matches();
        assert_eq!(reader.search_matches.len(), 4);

        // Walk to match 2 (the "foo three" line).
        reader.search_next();
        reader.search_next();
        assert_eq!(reader.search_idx, 2);
        let prior_match_line = reader.search_matches[reader.search_idx];

        // Resize.  rebuild_layout runs; search_idx must re-anchor to
        // the same line so the user keeps their place.
        reader.resize(40, 24);
        let new_match_line = reader.search_matches[reader.search_idx];
        assert_eq!(
            new_match_line, prior_match_line,
            "search_idx should re-anchor to the user's prior match line",
        );
    }

    // ── Mode-transition invariants (ADR-0004 Seam 1) ───────────────────
    //
    // These tests pin the per-method invariants documented in
    // `state/mode.rs`.  They exist so future refactors that fold modes
    // together or add new ones can't silently drop a buffer clear or
    // forget to seed an anchor.

    #[test]
    fn enter_command_mode_clears_buffers_and_error() {
        let mut reader = Reader::new(doc_with_one_image(), 80, 24);
        reader.count_buf.push_str("12");
        reader.cmd_buf.push_str("stale");
        reader.cmd_error = Some("prev".into());

        reader.enter_command_mode();

        assert_eq!(*reader.mode(), Mode::Command);
        assert!(reader.count_buf.is_empty());
        assert!(reader.cmd_buf.is_empty());
        assert!(reader.cmd_error.is_none());
    }

    #[test]
    fn return_to_normal_clears_count_and_cmd_buf_but_preserves_search() {
        let mut reader = Reader::new(doc_with_one_image(), 80, 24);
        reader.enter_search();
        reader.search_query.push_str("foo");
        reader.search_matches.push(0);
        reader.count_buf.push_str("3");
        reader.cmd_buf.push_str("set");

        reader.return_to_normal();

        assert_eq!(*reader.mode(), Mode::Normal);
        assert!(reader.count_buf.is_empty());
        assert!(reader.cmd_buf.is_empty());
        // Search state survives the round-trip so `n` / `N` keep working
        // (vim convention; explicit `cancel_search` is the way to drop it).
        assert_eq!(reader.search_query, "foo");
        assert_eq!(reader.search_matches, vec![0]);
    }

    #[test]
    fn enter_visual_mode_seeds_anchor_from_cursor() {
        let mut reader = Reader::new(doc_with_one_image(), 80, 24);
        // Move cursor down one line and forward a few columns.
        reader.cursor_y = 0;
        reader.cursor_x = 5;
        let cur_line = reader.current_line();

        reader.enter_visual_mode(false);
        assert_eq!(*reader.mode(), Mode::Visual { line_mode: false });
        assert_eq!(reader.visual_anchor, cur_line);
        assert_eq!(reader.visual_anchor_x, 5);

        // Line-mode forces horizontal anchor to 0 regardless of cursor_x.
        reader.return_to_normal();
        reader.cursor_x = 7;
        reader.enter_visual_mode(true);
        assert_eq!(*reader.mode(), Mode::Visual { line_mode: true });
        assert_eq!(reader.visual_anchor_x, 0);
    }

    // ── Cursor/scroll invariants (ADR-0004 Seam 2) ─────────────────────
    //
    // Pin the documented invariants for the new cursor-seam methods so
    // a future refactor that splits or unifies them can't silently
    // drop the cursor_x / desired_column reset (the historical cause of
    // "`j` after a jump goes to the wrong column" bugs).

    #[test]
    fn jump_to_line_resets_cursor_x_and_desired_column() {
        let mut reader = Reader::new(doc_with_one_image(), 80, 24);
        reader.cursor_x = 5;
        reader.desired_column = 5;
        reader.jump_to_line(0);
        assert_eq!(reader.cursor_x(), 0);
        assert_eq!(reader.desired_column(), 0);
    }

    #[test]
    fn center_on_line_preserves_cursor_x() {
        // Build a doc with enough lines that the viewport actually moves.
        let blocks: Vec<Block> = (0..30)
            .map(|i| Block::Line(format!("line {i:02}")))
            .collect();
        let mut reader = Reader::new(blocks, 80, 24);
        reader.cursor_x = 4;
        reader.desired_column = 4;
        // Pick a line past the half-screen mark so offset has to advance.
        let target = 20;
        reader.center_on_line(target);
        assert_eq!(reader.cursor_x(), 4, "voice auto-follow must not reset column");
        assert_eq!(reader.desired_column(), 4);
        assert_eq!(
            reader.offset() + reader.cursor_y(),
            target,
            "cursor must land on the target line",
        );
    }

    #[test]
    fn jump_to_line_with_context_above_lands_target_at_bottom() {
        // 60 lines × small terminal so the target sits comfortably
        // below the initial viewport — forces the scroll branch.
        let blocks: Vec<Block> = (0..60)
            .map(|i| Block::Line(format!("line {i:02}")))
            .collect();
        let mut reader = Reader::new(blocks, 80, 24);
        let ch = reader.content_height();
        let target = ch + 10; // safely below initial viewport
        reader.jump_to_line_with_context_above(target);
        assert_eq!(
            reader.cursor_y(),
            ch - 1,
            "target should sit at viewport bottom so context is above",
        );
        assert_eq!(reader.offset() + reader.cursor_y(), target);
        assert_eq!(reader.cursor_x(), 0);
    }

    // ── Voice invariants (ADR-0004 Seam 3) ─────────────────────────────

    // ── Bookmark invariants (ADR-0004 Seam 4) ──────────────────────────

    #[test]
    fn mark_survives_terminal_resize_reflow() {
        // A long paragraph that will wrap at 30 cols into multiple visual
        // lines, then collapse into fewer when widened to 200 cols.  The
        // bookmark must land in the same paragraph at the same byte
        // offset either way — pre-Seam-4 this would have lost track,
        // because the stored visual-line index meant a different VL
        // after the wrap rebuild.
        let long = "alpha beta gamma delta epsilon zeta eta theta iota kappa \
                    lambda mu nu xi omicron pi rho sigma tau upsilon"
            .to_string();
        let blocks = vec![Block::Line(long.clone())];
        let mut reader = Reader::new(blocks, 30, 24);

        // Position the cursor on the word "lambda" (byte ~56 in the
        // source string).  Find its visual line, set a mark there.
        let target_byte = long.find("lambda").expect("word present");
        let (target_vl, col) = reader
            .visual_lines
            .iter()
            .enumerate()
            .find(|(_, vl)| {
                target_byte >= vl.block_byte_start && target_byte < vl.block_byte_end
            })
            .map(|(idx, vl)| (idx, target_byte - vl.block_byte_start))
            .expect("byte resolves to a VL pre-resize");
        reader.jump_to_line(target_vl);
        reader.set_cursor_x(col);
        reader.set_mark('a');

        // Now widen the terminal so the paragraph wraps differently
        // (likely into a single VL at 200 cols).  Pre-resize VL
        // indices are now stale.
        reader.resize(200, 24);

        // Jump to mark 'a' — should land in the (still single) block
        // at the same source-byte position.
        reader.jump_to_mark('a');
        let landed_vl = reader.current_line();
        let landed_byte = reader.visual_lines[landed_vl].block_byte_start + reader.cursor_x();
        assert_eq!(
            landed_byte, target_byte,
            "mark should resolve to the same source byte after reflow",
        );
    }

    #[test]
    fn stop_continuous_reading_clears_only_the_continuous_flag() {
        let mut reader = Reader::new(doc_with_one_image(), 80, 24);
        reader.reading_mode = true;
        reader.continuous_reading = true;
        reader.voice_status = crate::voice::PlaybackStatus::Playing;

        reader.stop_continuous_reading();

        assert!(!reader.continuous_reading());
        // reading_mode and voice_status are intentionally PRESERVED — the
        // current chunk should keep playing through to its natural end
        // (and the status line should keep saying "READING") even after
        // continuous auto-advance is turned off.
        assert!(reader.reading_mode());
        assert!(matches!(
            reader.voice_status(),
            crate::voice::PlaybackStatus::Playing
        ));
    }
}
