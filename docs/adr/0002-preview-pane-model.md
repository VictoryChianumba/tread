# ADR-0002 — Preview pane model

- **Status:** Accepted (2026-05-18)
- **Crate:** `tread` (`state.rs`, `render.rs`, `images.rs`)
- **Relates to:** [ADR-0001 — Figure model](0001-figure-model.md),
  [ADR-0003 — Terminal image strategy](0003-terminal-image-strategy.md)

## Context

Inline figures are useful but cost vertical real estate: a half-page
multi-panel figure pushes the surrounding prose off-screen. For long
sessions the user wants:

- The figure visible while reading the paragraph that references it.
- The text reflowed past the figure so a row of prose isn't sandwiched
  between two figure rows.
- `]f` / `[f` to step through figures *in the preview*, not jump the
  reader cursor.

The reader needs two layout modes — figures inline, or figures hidden
from the main flow and tiled in a side pane — without:

- Reflowing the entire document twice and choosing one.
- Splitting the figure-layout math between the inline path and the
  preview path. Off-by-one drift between the two callers had bitten us
  before (column position computed two different ways in
  `images::place_one_figure` vs. `render::draw_image_band`).

## Decision

### `text_only` is a layout-cache input, not a render-time flag

`LayoutCache::rebuild_layout` takes `text_only: bool`. When true,
`build_visual_lines` keeps every block in the document but
`build_lines_for` then drops `VisualLineKind::Image` /
`VisualLineKind::ImageRow` rows from the resulting `Vec<VisualLine>`.
The reader's offset / cursor coordinates index into that filtered
list, so nothing else in the reader has to know image VLs ever existed.

Captions stay because they live as separate `Block::Line` / `StyledLine`
blocks — the user keeps a textual landing spot for `]f` / `[f` jumps.

`Reader::set_text_only` is the only legitimate writer; it triggers
`rebuild_layout(LayoutRebuildReason::TextOnlyToggle, …)`. Writing
the field directly desyncs `visual_lines`.

### Figure layout is one function, two consumers

`doc_model::FigureEntry::layout(area: Rect) -> FigureLayout` computes
the complete geometry for one figure inside an area:

```rust
struct FigureLayout {
    headers: Vec<HeaderRowPlacement>,
    image_rows: Vec<ImageRowPlacement>, // per row: y, height, items[PartPlacement]
    caption: Option<CaptionPlacement>,
}
```

Both `render::draw_preview_pane` (writes header text and caption into
the ratatui buffer) and `images::place_one_figure` (emits Kitty `a=p`
placements for the image cells) call `FigureEntry::layout` against the
same `Rect` and read out their respective fields. Column positions and
row heights can drift in only one place.

The layout algorithm in brief:

1. Wrap the caption to area width, cap at `MAX_CAPTION_ROWS`, reserve
   that band plus `CAPTION_GAP_ROWS`.
2. Reserve `header_rows.len()` rows for header labels above the image
   grid.
3. Per row, split remaining width evenly across `row.len()` panels
   minus accumulated `column_gaps_after` gaps; cap per-row height by
   the most demanding panel's aspect ratio.
4. Vertically centre the figure block inside the area so headers /
   images / caption read as one unit.

### Preview state lives on `Reader`

```rust
pub struct FigurePreviewState {
    pub active: bool,
    pub selected_index: Option<usize>,
    pub selected_kitty_id: Option<u32>,
    last_geometry: Cell<Option<PreviewGeometry>>,
}
```

- `active` — `i` in normal mode toggles. Drives the layout split in
  `render::split_content_for_preview`.
- `selected_index` — index into `FigureIndex::entries()`.
- `selected_kitty_id` — the representative kitty id of the selected
  entry. Carried so `ImageState` can clean up the previous preview
  placement when selection changes without reaching back into the
  index.
- `last_geometry` — `Cell`-wrapped so the renderer can update it
  through `&self`; consulted by `ImageState` to detect a moved /
  resized preview pane and force a re-place.

`set_figure_preview_active` / `toggle_figure_preview` are the writers.
Direct field writes will desync the image side because cleanup hinges
on the previous `selected_kitty_id`.

### Layout split

`render::split_content_for_preview` carves the content area into
`(reader_pane, preview_pane)` at `PREVIEW_TEXT_PERCENT` (60% reader,
40% preview). When `preview_state.active`, `rebuild_layout` is invoked
against `reader_pane.width` so wrap matches the actual draw width —
otherwise long lines wrap past the pane edge until the next event
forces a rebuild.

## Consequences

**Good:**
- The figure-layout math has one home. Off-by-one drift between text
  and pixel placements is structurally impossible.
- `text_only` is a property of the cache build, not a flag every
  renderer checks. The reader's offset/cursor logic is identical in
  both modes.
- The preview-state seam is small (one struct, three writers) and
  testable without spinning up the full TUI.

**Costs:**
- Two reflows on toggle: leaving preview mode rebuilds layout with
  image VLs reinstated and the wider reader width. Acceptable —
  rebuild is ~300 µs idle and the toggle is rare.
- `FigureEntry::layout` returns a moderately large struct
  (`FigureLayout` with three vectors). Cheap to allocate and per-frame,
  but a hot path to be aware of if preview FPS becomes a concern.
- `PreviewGeometry` is a small mutable cell on `Reader`, breaking the
  rule that `Reader` is otherwise mutated through `&mut self`. The
  trade is that the renderer can publish the geometry it actually drew
  without an extra plumbing pass.

## Validation

Acceptance tests in `crates/tread/`:

- Preview toggle, scroll, resize, and figure-step navigation.
- Preview-pane layout dimensions match the figure layout's headers
  + image rows + caption.
- Preview-width reflow: enabling preview reflows the reader pane to
  the actual left-pane width.

## Open follow-up

- Preview placement still re-emits the full `a=T` payload on iTerm2
  because that terminal does not persist image bytes between frames.
  See ADR-0003 for the cache strategy and constraints.
- ~~`FigurePreviewState::last_geometry` invalidation is currently a
  manual call-site discipline.~~ Tightened 2026-05-18:
  `Reader::rebuild_layout` now clears `last_geometry` unconditionally,
  so any rebuild (resize, reload, text-only toggle, TOC toggle)
  auto-invalidates the cached preview area. The next `after_draw`
  recomputes against the new content area instead of mis-placing
  the image for one frame.
