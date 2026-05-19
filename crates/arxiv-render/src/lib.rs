pub mod ar5iv_parse;
pub mod bibitems;
pub mod bibtex;
pub mod fetch;
pub mod pandoc_parse;
pub mod pdf_anchors;
pub mod placement;

pub use bibitems::extract_bibitems;
pub use placement::lift_tables;

/// Replace every `Block::Figure` with a plain caption line.  The
/// fallback for terminals that don't speak any inline-graphics protocol
/// — instead of reserving 16 blank rows per figure (where pixels would
/// land on a graphics-capable terminal), users see a single
/// `[Figure N: caption]` line in document flow.
///
/// Call this in `main.rs` *before* `absolutize_image_paths` when
/// `kitty_graphics::detect()` returns `Unsupported`.  After this pass
/// there are no `Figure` blocks left in the tree, so the graphics
/// capability flag effectively becomes a no-op for the reader's hot
/// path.
pub fn degrade_images_to_captions(blocks: &mut Vec<doc_model::Block>) {
    for b in blocks.iter_mut() {
        if let doc_model::Block::Figure { alt, .. } = b {
            *b = doc_model::Block::Line(format!("[{alt}]"));
        }
    }
}

/// Rewrite every `Block::Image::path` from a tarball-relative form to an
/// absolute path under `asset_dir`, and recover the file extension when
/// LaTeX's `\includegraphics{name}` form omitted it.  Idempotent: paths
/// already absolute and resolvable are left alone.  Called from the
/// binary entry points right after `to_blocks` so the reader sees
/// ready-to-load paths.
///
/// **Why extension recovery matters**: LaTeX's `graphicx` package treats
/// `\includegraphics{Figures/ModalNet-19}` as "search `\graphicspath`
/// for `ModalNet-19.png`, `ModalNet-19.jpg`, `ModalNet-19.pdf`, etc."
/// Pandoc faithfully preserves the source, so paths without extensions
/// reach us as-is and `std::fs::read` returns ENOENT.  We probe the
/// usual image suffixes in priority order until one matches a real
/// file in the asset directory.
pub fn absolutize_image_paths(blocks: &mut [doc_model::Block], asset_dir: &std::path::Path) {
    const PROBE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "pdf"];
    let resolve = |path: &mut std::path::PathBuf| {
        if path.is_relative() {
            *path = asset_dir.join(&*path);
        }
        if path.exists() {
            return;
        }
        for ext in PROBE_EXTS {
            let candidate = path.with_extension(ext);
            if candidate.exists() {
                *path = candidate;
                return;
            }
        }
    };
    for b in blocks {
        if let doc_model::Block::Figure { rows, .. } = b {
            for row in rows.iter_mut() {
                for item in row.iter_mut() {
                    resolve(&mut item.path);
                    item.dims = read_image_dims(&item.path);
                }
            }
        }
    }
}

/// Read pixel `(width, height)` for an image at `path`.  PNG: parse the
/// IHDR chunk directly.  PDF: rasterise via `pdftoppm` (cached), then
/// read the resulting PNG's header.  JPG/JPEG: use the image crate's
/// header reader so photo-heavy figure rows preserve aspect ratio too.
/// Unsupported formats return `None` and the caller falls back to the
/// default cell footprint.
fn read_image_dims(path: &std::path::Path) -> Option<(u32, u32)> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => kitty_graphics::png::dimensions(path),
        "jpg" | "jpeg" => image::image_dimensions(path).ok(),
        "pdf" => {
            // Eager rasterisation so build_visual_lines has dims.  pdf_to_png
            // is cached by FNV-1a of canonical path, so subsequent runs and
            // retries pay zero conversion cost.
            let cache = pdf_cache_dir();
            let png = kitty_graphics::pdf::pdf_to_png(path, &cache).ok()?;
            kitty_graphics::png::dimensions(&png)
        }
        _ => None,
    }
}

fn pdf_cache_dir() -> std::path::PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return std::path::PathBuf::from(home)
            .join(".cache")
            .join("tread")
            .join("figures");
    }
    std::env::temp_dir().join("tread-figures")
}

#[cfg(test)]
mod attention_golden_tests {
    //! End-to-end parse + layout golden for the Attention paper
    //! (`1706.03762`).  Mechanizes the smoke claims made in ADR-0005,
    //! ADR-0007, and elsewhere ("379 blocks → 675 visual lines, Table
    //! 3 with `c|cccccc|ccc` vertical rules intact, all 5 figures
    //! render with captions") so a regression in any of those is a
    //! compile-time test failure rather than a "I ran it last week."
    //!
    //! Gated by `#[ignore]` and requires:
    //! - network access on first run (cached under
    //!   `~/.cache/tread/sources/1706.03762/` thereafter);
    //! - `pandoc` on `PATH`.
    //!
    //! Run with:
    //! ```bash
    //! cargo test -p arxiv-render attention_golden --release \
    //!     -- --ignored --nocapture
    //! ```
    //!
    //! If a parser or layout change shifts the counts, this test
    //! fails by design.  When the change is intentional, update the
    //! `EXPECTED_BLOCKS` / `EXPECTED_VISUAL_LINES` constants below
    //! and note the new baseline in the corresponding ADR.

    use doc_model::{Block, build_visual_lines};

    use crate::{degrade_images_to_captions, fetch, pandoc_parse};

    /// Pre-degrade block count from ADR-0005 / ADR-0007 smoke.
    const EXPECTED_BLOCKS: usize = 379;
    /// Post-`build_visual_lines` count at the binary's 80×50 layout
    /// after `degrade_images_to_captions` (matches what
    /// `cargo run -p arxiv-render -- 1706.03762` produces on stdout).
    const EXPECTED_VISUAL_LINES: usize = 675;

    #[test]
    #[ignore]
    fn attention_parse_and_layout_golden() {
        let fetched = fetch::fetch_source("1706.03762").expect("fetch arXiv:1706.03762");
        let mut blocks = pandoc_parse::try_pandoc(&fetched.tex).expect("pandoc parse");

        // Structural assertions on the pre-degrade block stream.
        // These cover what ADRs 0005/0007 claim survives the parser.

        // Title header.
        assert!(
            blocks.iter().any(|b| matches!(
                b,
                Block::Header { level: 1, text }
                    if text == "Attention Is All You Need"
            )),
            "expected H1 title \"Attention Is All You Need\" not found",
        );

        // Five figures (Fig 1..=5; ar5iv/pandoc emit them as Block::Figure
        // with `figure_id` 1..=5).
        let figure_ids: Vec<u32> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Figure { figure_id, .. } => Some(*figure_id),
                _ => None,
            })
            .collect();
        assert_eq!(
            figure_ids,
            vec![1, 2, 3, 4, 5],
            "expected figures 1..=5 in source order, got {:?}",
            figure_ids,
        );

        // Attention has two tables that carry vertical rules through
        // the parser: Table 3 (the model-variations table, with rows
        // "base", "(A)", "(B)", … "(E)", "big") and Table 4 (the
        // parser benchmark).
        //
        // Table 3's source spec is `c|ccccccccc|ccc` (results.tex:54
        // of arXiv:1706.03762 — NINE inner c's plus 1+3 outer cols =
        // 13 total) → vertical_rules == [1, 10].  Pinned at the unit
        // level by `pandoc_parse::spec::tests::attention_table_3_spec`.
        // The neighbouring `two_rules` unit test asserts the synthetic
        // `c|cccccc|ccc` pattern (6 inner c's, 10 cols total → [1, 7])
        // — that's a smaller minimal case, not Table 3.  An earlier
        // draft of ADR-0007 and several inline comments confused the
        // two; closed in commit-fix-divergence.
        let table_3_rules = blocks.iter().any(|b| matches!(
            b,
            Block::Matrix { vertical_rules, .. }
                if vertical_rules == &[1, 10]
        ));
        assert!(table_3_rules, "Table 3's vertical_rules [1, 10] not found");

        let table_4_rules = blocks.iter().any(|b| matches!(
            b,
            Block::Matrix { vertical_rules, .. }
                if vertical_rules == &[1, 2]
        ));
        assert!(table_4_rules, "Table 4's vertical_rules [1, 2] not found");

        // Section structure: Attention has 7 numbered top-level
        // sections (Introduction, Background, Model Architecture, Why
        // Self-Attention, Training, Results, Conclusion) plus the H1
        // title and the "Attention Visualizations" appendix.  No
        // "References" header — the paper uses `\bibliography{...}`
        // (external bib file) and Pandoc without `--citeproc` emits
        // no References Div, so the auto-append path in
        // `try_pandoc` only fires when bibitems were also extracted
        // from `thebibliography` (not the case here).
        let numbered_section_count = blocks
            .iter()
            .filter(|b| matches!(
                b,
                Block::Header { level: 1, text }
                    if text.chars().next().map_or(false, |c| c.is_ascii_digit())
            ))
            .count();
        assert_eq!(
            numbered_section_count, 7,
            "expected 7 numbered top-level sections (1-7)",
        );

        // At least one numbered DisplayMath equation (e.g. the
        // scaled dot-product attention formula at (1)).
        assert!(
            blocks.iter().any(|b| matches!(
                b,
                Block::DisplayMath { num: Some(_), .. }
            )),
            "expected at least one numbered DisplayMath block",
        );

        // Strict block-count golden.
        assert_eq!(
            blocks.len(),
            EXPECTED_BLOCKS,
            "block count drift — if intentional, update EXPECTED_BLOCKS",
        );

        // Now run the layout the way the arxiv-render binary does:
        // degrade figures to captions (no inline graphics in a text
        // dump), then build visual lines at the binary's 80×50.
        degrade_images_to_captions(&mut blocks);
        let visual_lines = build_visual_lines(&blocks, 80, 50);

        assert_eq!(
            visual_lines.len(),
            EXPECTED_VISUAL_LINES,
            "visual-line count drift — if intentional, update EXPECTED_VISUAL_LINES",
        );
    }
}
