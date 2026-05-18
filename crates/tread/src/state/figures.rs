//! Figure types and the preview-pane layout machinery.  Extracted from
//! `state/mod.rs`: every type that participates in figure ingestion
//! (`FigureEntry`, `FigureIndex`) or in turning a figure into a preview-
//! pane placement (`FigureLayout` and its row / cell / caption parts)
//! lives here.  The Reader struct keeps a `figure_index` and a
//! `preview_state`; the geometry is computed on demand by
//! `FigureEntry::layout`.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;

use doc_model::Block;
use ratatui::layout::Rect;

/// One renderable image inside a figure.  `Block::Image` produces a
/// single-part figure; `Block::ImageRow` produces a multi-part figure
/// where each `\includegraphics` becomes one part.  Parts share the
/// containing figure's `alt` text and step together under `]f` / `[f`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FigurePart {
  pub kitty_id: u32,
  pub path: PathBuf,
  pub dims: Option<(u32, u32)>,
}

/// One logical figure as the document author intended it.  Mirrors the
/// source's 2D layout: `rows[i]` is one stacked panel row, and each row
/// is a Vec of subfigures rendered side-by-side.  A simple figure with
/// one image is `rows = [[part]]`.
///
/// Always non-empty: the parser drops empty figures rather than emit
/// them, so consumers can safely index `rows[0][0]` for the
/// representative part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FigureHeaderCell {
  pub text: String,
  pub col_span: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FigureEntry {
  pub rows: Vec<Vec<FigurePart>>,
  pub alt: String,
  /// Column indices (in flat row-item space) AFTER which the source
  /// tabular requested a visible gap (typically `@{\hspace{1mm}}`).
  /// Empty for figures whose source doesn't use column-group spacing.
  pub column_gaps_after: Vec<usize>,
  /// Column-label rows pulled from the figure's tabular header (e.g.
  /// "N=4 | N=6 | N=16" then "Input | Avat3r | Ours ..." for the
  /// Ava-256 figure).  The preview tiler draws these above the image
  /// grid using terminal-default styling.  Empty for figures whose
  /// source has no textual header.
  pub header_rows: Vec<Vec<FigureHeaderCell>>,
}

impl FigureEntry {
  /// Representative kitty_id for this figure — the first part's id.
  /// Used by status-bar / navigation code that wants a single token to
  /// identify the figure even when it has multiple parts.
  pub fn representative_kitty_id(&self) -> u32 {
    self.rows[0][0].kitty_id
  }

  /// Iterate every part across all rows.  Order is row-major,
  /// matching the source's reading order.
  pub fn parts(&self) -> impl Iterator<Item = &FigurePart> {
    self.rows.iter().flat_map(|row| row.iter())
  }
}

/// One part's placement inside the preview pane — both the Kitty image
/// emission and the per-row layout math read this.
#[derive(Debug, Clone, Copy)]
pub struct PartPlacement {
  pub kitty_id: u32,
  pub abs_col: u16,
  pub abs_row: u16,
  pub cols: u16,
  pub rows: u16,
}

/// Geometry for one image row inside the preview.
#[derive(Debug, Clone)]
pub struct ImageRowPlacement {
  /// Top y (terminal rows, 0-indexed) of this row's strip.
  pub y: u16,
  /// Height of this row's image cells (cell rows, identical for all
  /// items in the row).
  pub height: u16,
  /// One placement per part, in source order.
  pub items: Vec<PartPlacement>,
}

/// Header text positioned for one row of the figure's tabular header.
#[derive(Debug, Clone)]
pub struct HeaderRowPlacement {
  /// Screen row (absolute, terminal-y) where this header text sits.
  pub y: u16,
  /// One label per header cell in source order, with its centered
  /// `abs_col` and the cell width (in screen cells) it should
  /// occupy.  The renderer truncates `text` to `width` and centers
  /// it visually within the span.
  pub cells: Vec<HeaderLabelPlacement>,
}

#[derive(Debug, Clone)]
pub struct HeaderLabelPlacement {
  pub text: String,
  pub abs_col: u16,
  pub width: u16,
}

/// Caption placement — `lines` are pre-wrapped to fit `width`, and the
/// renderer writes them starting at `(x, y)` row by row.
#[derive(Debug, Clone)]
pub struct CaptionPlacement {
  pub x: u16,
  pub y: u16,
  pub width: u16,
  pub lines: Vec<String>,
}

/// Complete figure layout — computed once, consumed by both the text
/// renderer (`render::draw_preview_pane`) and the image tiler
/// (`images::place_one_figure`).  Keeping them in sync via a shared
/// helper avoids the off-by-one drift that would otherwise creep in
/// when the two callers compute column positions independently.
///
/// Layout order from top to bottom:
///   1. `headers` (column labels)
///   2. `image_rows` (the figure body)
///   3. `caption` (one row of vertical breathing room, then wrapped text)
/// The block is vertically centered inside the input `area`; everything
/// inside is contiguous (no inter-section padding beyond the caption
/// gap), so the figure reads as one visual unit rather than three
/// floating components.
#[derive(Debug, Clone)]
pub struct FigureLayout {
  pub headers: Vec<HeaderRowPlacement>,
  pub image_rows: Vec<ImageRowPlacement>,
  pub caption: Option<CaptionPlacement>,
}

impl FigureEntry {
  /// Lay this figure out inside `area`.  The same area is used by
  /// `place_one_figure` (Kitty image placement) and
  /// `draw_preview_pane` (header text drawing); both callers get the
  /// identical geometry from one call to this function.
  pub fn layout(&self, area: Rect) -> FigureLayout {
    // Caption block — wrap the alt text to the area width so we can
    // budget vertical space for it before laying out the image grid.
    // Capped at `MAX_CAPTION_ROWS` so a long caption doesn't crowd
    // out the images; the renderer truncates the last visible line
    // with an ellipsis if necessary.
    const MAX_CAPTION_ROWS: u16 = 8;
    const CAPTION_GAP_ROWS: u16 = 1;
    let caption_lines: Vec<String> = if self.alt.is_empty() || area.width == 0 {
      Vec::new()
    } else {
      wrap_caption(&self.alt, area.width as usize)
        .into_iter()
        .take(MAX_CAPTION_ROWS as usize)
        .collect()
    };
    let caption_block_rows = if caption_lines.is_empty() {
      0
    } else {
      (caption_lines.len() as u16).saturating_add(CAPTION_GAP_ROWS)
    };

    // Headers sit immediately above the image rows; we count one
    // screen row per source header row.
    let header_height = self.header_rows.len() as u16;

    // Available image height = area minus header band minus caption
    // band.  Saturating math keeps a tiny pane safely empty.
    let row_count = self.rows.len() as u16;
    let reserved = header_height.saturating_add(caption_block_rows);
    let image_height = area.height.saturating_sub(reserved);
    if row_count == 0 || image_height == 0 || area.width == 0 {
      return FigureLayout {
        headers: Vec::new(),
        image_rows: Vec::new(),
        caption: None,
      };
    }
    let max_row_height = image_height / row_count;
    if max_row_height == 0 {
      return FigureLayout {
        headers: Vec::new(),
        image_rows: Vec::new(),
        caption: None,
      };
    }
    // Per-row image layout — same algorithm as the tiler used in
    // place_one_figure.  Returns (row_height, cols, total_cols).
    let row_layouts: Vec<(u16, Vec<u16>, u16)> = self
      .rows
      .iter()
      .map(|row| {
        if row.is_empty() {
          return (0, Vec::new(), 0);
        }
        let row_gap_count = self
          .column_gaps_after
          .iter()
          .filter(|&&g| g < row.len())
          .count() as u16;
        let gap_cells_total = row_gap_count * PREVIEW_COLUMN_GAP_CELLS;
        let item_count = row.len() as u16;
        let usable_width = area.width.saturating_sub(gap_cells_total);
        let cell_width_budget = usable_width / item_count;
        if cell_width_budget == 0 {
          return (0, Vec::new(), 0);
        }
        let mut row_height: u16 = 1;
        for part in row {
          let (_c, r) = doc_model::compute_cell_footprint(
            part.dims,
            cell_width_budget as usize,
            max_row_height,
          );
          if r > row_height {
            row_height = r;
          }
        }
        row_height = row_height.min(max_row_height);
        let cols: Vec<u16> = row
          .iter()
          .map(|part| match part.dims {
            Some((w, h)) if h > 0 && w > 0 => {
              let derived = ((row_height as u32 * w * 2) / h) as u16;
              derived.min(cell_width_budget).max(1)
            }
            _ => cell_width_budget,
          })
          .collect();
        let total_cols: u16 = cols.iter().sum();
        (row_height, cols, total_cols)
      })
      .collect();

    // Vertical packing: stack rows tight, then center the WHOLE
    // header+gap+images+gap+caption block within `area`.  One row
    // of breathing room between headers and the image grid keeps
    // glyph descenders from kissing the image top.  Caption sits
    // one row below the image grid for the same reason.  This
    // matches the source paper where everything in a figure reads
    // as one visual unit without touching.
    const HEADER_GAP_ROWS: u16 = 1;
    let images_total_height: u16 = row_layouts.iter().map(|(h, _, _)| *h).sum();
    let header_gap = if header_height > 0 {
      HEADER_GAP_ROWS
    } else {
      0
    };
    let block_height = header_height
      .saturating_add(header_gap)
      .saturating_add(images_total_height)
      .saturating_add(caption_block_rows);
    let top_pad = area.height.saturating_sub(block_height) / 2;
    let block_origin_y = area.y.saturating_add(top_pad);
    let mut cur_y = block_origin_y
      .saturating_add(header_height)
      .saturating_add(header_gap);
    let mut image_rows = Vec::with_capacity(self.rows.len());
    // Track per-image-column x positions of row 0 (the reference
    // row for header alignment).  Index = item index in row 0.
    let mut col0_positions: Vec<(u16, u16)> = Vec::new();
    for (idx, (row, (row_height, cols, total_cols))) in
      self.rows.iter().zip(row_layouts.iter()).enumerate()
    {
      if row.is_empty() || *row_height == 0 {
        image_rows.push(ImageRowPlacement {
          y: cur_y,
          height: 0,
          items: Vec::new(),
        });
        continue;
      }
      let row_gap_count = self
        .column_gaps_after
        .iter()
        .filter(|&&g| g < row.len())
        .count() as u16;
      let gap_cells_total = row_gap_count * PREVIEW_COLUMN_GAP_CELLS;
      let row_total_with_gaps = total_cols.saturating_add(gap_cells_total);
      let row_start_x = area
        .x
        .saturating_add(area.width.saturating_sub(row_total_with_gaps) / 2);
      let mut cur_x = row_start_x;
      let mut items = Vec::with_capacity(row.len());
      for (item_idx, (part, cols_v)) in row.iter().zip(cols.iter()).enumerate() {
        let abs_col = cur_x;
        if idx == 0 {
          col0_positions.push((abs_col, *cols_v));
        }
        items.push(PartPlacement {
          kitty_id: part.kitty_id,
          abs_col,
          abs_row: cur_y,
          cols: *cols_v,
          rows: *row_height,
        });
        cur_x = cur_x.saturating_add(*cols_v);
        if self.column_gaps_after.contains(&(item_idx + 1)) {
          cur_x = cur_x.saturating_add(PREVIEW_COLUMN_GAP_CELLS);
        }
      }
      image_rows.push(ImageRowPlacement {
        y: cur_y,
        height: *row_height,
        items,
      });
      cur_y = cur_y.saturating_add(*row_height);
    }

    // Headers: each cell spans `col_span` image columns of row 0.
    // Resolve each cell's pixel range from `col0_positions`.  Y
    // anchored to the row directly above the image grid so labels
    // hug the figure body instead of floating at the pane top.
    let headers = self
      .header_rows
      .iter()
      .enumerate()
      .map(|(hi, hrow)| {
        let y = block_origin_y.saturating_add(hi as u16);
        let mut col_cursor = 0usize;
        let cells: Vec<HeaderLabelPlacement> = hrow
          .iter()
          .map(|hcell| {
            let span = hcell.col_span.max(1) as usize;
            let last_col = (col_cursor + span - 1).min(col0_positions.len().saturating_sub(1));
            let placement = if col_cursor < col0_positions.len() {
              let (start_x, _) = col0_positions[col_cursor];
              let (last_x, last_w) = col0_positions[last_col];
              let end_x = last_x.saturating_add(last_w);
              let width = end_x.saturating_sub(start_x);
              HeaderLabelPlacement {
                text: hcell.text.clone(),
                abs_col: start_x,
                width,
              }
            } else {
              HeaderLabelPlacement {
                text: hcell.text.clone(),
                abs_col: area.x,
                width: 0,
              }
            };
            col_cursor += span;
            placement
          })
          .collect();
        HeaderRowPlacement { y, cells }
      })
      .collect();

    // Caption sits one row below the image grid, wrapped to fit the
    // area width.  `cur_y` already points at the row immediately
    // after the last image row, so we add `CAPTION_GAP_ROWS` to
    // separate visually from the figure body.
    let caption = if caption_lines.is_empty() {
      None
    } else {
      Some(CaptionPlacement {
        x: area.x,
        y: cur_y.saturating_add(CAPTION_GAP_ROWS),
        width: area.width,
        lines: caption_lines,
      })
    };

    FigureLayout {
      headers,
      image_rows,
      caption,
    }
  }
}

/// Greedy word-wrap for figure captions.  `width` is the target column
/// count; words longer than `width` are kept on their own line (better
/// to overflow than to break mid-word with an ellipsis).  Empty input
/// returns an empty Vec so callers can use `.is_empty()` as the
/// "no caption" branch.
fn wrap_caption(text: &str, width: usize) -> Vec<String> {
  if width == 0 || text.is_empty() {
    return Vec::new();
  }
  let mut out = Vec::new();
  let mut cur = String::new();
  for word in text.split_whitespace() {
    if cur.is_empty() {
      cur.push_str(word);
      continue;
    }
    if cur.chars().count() + 1 + word.chars().count() <= width {
      cur.push(' ');
      cur.push_str(word);
    } else {
      out.push(std::mem::take(&mut cur));
      cur.push_str(word);
    }
  }
  if !cur.is_empty() {
    out.push(cur);
  }
  out
}

/// Gap width (in terminal cells) inserted between column groups in the
/// preview pane.  Mirrors the LaTeX `@{\hspace{1mm}}` request without
/// committing to a millimetre conversion (terminals don't know about
/// physical units).
pub const PREVIEW_COLUMN_GAP_CELLS: u16 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FigureIndex {
  pub(super) entries: Vec<FigureEntry>,
  paths: HashMap<u32, PathBuf>,
}

impl FigureIndex {
  pub fn build(blocks: &[Block]) -> Self {
    // The parser emits one `Block::Figure` per source figure with
    // the full 2D layout already in place, so the index is a
    // straight projection — no heuristics to reconstruct grouping.
    let mut entries = Vec::new();
    let mut paths = HashMap::new();
    for block in blocks {
      let Block::Figure {
        rows,
        alt,
        column_gaps_after,
        header_rows,
        ..
      } = block
      else {
        continue;
      };
      if rows.iter().all(|row| row.is_empty()) {
        continue;
      }
      let entry_rows: Vec<Vec<FigurePart>> = rows
        .iter()
        .filter(|row| !row.is_empty())
        .map(|row| {
          row
            .iter()
            .map(|item| {
              paths.insert(item.kitty_id, item.path.clone());
              FigurePart {
                kitty_id: item.kitty_id,
                path: item.path.clone(),
                dims: item.dims,
              }
            })
            .collect()
        })
        .collect();
      let entry_headers: Vec<Vec<FigureHeaderCell>> = header_rows
        .iter()
        .map(|row| {
          row
            .iter()
            .map(|c| FigureHeaderCell {
              text: c.text.clone(),
              col_span: c.col_span,
            })
            .collect()
        })
        .collect();
      entries.push(FigureEntry {
        rows: entry_rows,
        alt: alt.clone(),
        column_gaps_after: column_gaps_after.clone(),
        header_rows: entry_headers,
      });
    }
    Self { entries, paths }
  }

  /// Representative kitty_id per logical figure.  An ImageRow with
  /// N subfigures still contributes ONE id here — the first part's —
  /// so the count matches what the user sees on the page.
  pub fn ordered_kitty_ids(&self) -> Vec<u32> {
    self
      .entries
      .iter()
      .map(FigureEntry::representative_kitty_id)
      .collect()
  }

  pub fn path_map(&self) -> HashMap<u32, PathBuf> {
    self.paths.clone()
  }

  /// Look up a figure by any of its parts' kitty_id.  Returns the
  /// whole `FigureEntry` (all rows), not just the matching part —
  /// callers that want the specific part should walk `rows`.
  pub fn get(&self, kitty_id: u32) -> Option<&FigureEntry> {
    self
      .entries
      .iter()
      .find(|entry| entry.parts().any(|p| p.kitty_id == kitty_id))
  }

  /// Borrow the entries slice for callers that need to walk grouped
  /// figures (e.g. tiled preview rendering).
  #[allow(dead_code)]
  pub fn entries(&self) -> &[FigureEntry] {
    &self.entries
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewGeometry {
  pub x: u16,
  pub y: u16,
  pub width: u16,
  pub height: u16,
}

impl PreviewGeometry {
  pub(super) fn from_rect(rect: Rect) -> Self {
    Self {
      x: rect.x,
      y: rect.y,
      width: rect.width,
      height: rect.height,
    }
  }
}

#[derive(Debug)]
pub struct FigurePreviewState {
  pub active: bool,
  pub selected_index: Option<usize>,
  pub selected_kitty_id: Option<u32>,
  pub(super) last_geometry: Cell<Option<PreviewGeometry>>,
}

impl FigurePreviewState {
  pub(super) fn inactive() -> Self {
    Self {
      active: false,
      selected_index: None,
      selected_kitty_id: None,
      last_geometry: Cell::new(None),
    }
  }

  pub fn last_geometry(&self) -> Option<PreviewGeometry> {
    self.last_geometry.get()
  }
}
