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
/// IHDR chunk directly.  PDF: read the page size via `pdfinfo` and scale
/// to the rasteriser's DPI — **without** rendering the page.  JPG/JPEG:
/// use the image crate's header reader so photo-heavy figure rows
/// preserve aspect ratio too.  Unsupported formats return `None` and the
/// caller falls back to the default cell footprint.
///
/// Why the PDF path avoids `pdftoppm` here: layout only needs each
/// figure's dimensions to reserve an aspect-correct footprint, and the
/// actual rasterisation already happens lazily on first scroll-into-view
/// (`tread::images::png::resolve_png`).  Rendering every PDF figure
/// eagerly just to learn its size cost ~20s on figure-heavy papers that
/// fall to this tarball path; `pdfinfo` returns the same aspect in
/// milliseconds.
fn read_image_dims(path: &std::path::Path) -> Option<(u32, u32)> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => kitty_graphics::png::dimensions(path),
        "jpg" | "jpeg" => image::image_dimensions(path).ok(),
        "pdf" => kitty_graphics::pdf::pdf_page_dims(path),
        _ => None,
    }
}

#[cfg(test)]
mod golden_tests {
    //! End-to-end parse + layout goldens for benchmark papers from
    //! `test-papers.txt`.  Mechanizes the smoke claims made in
    //! ADR-0005, ADR-0007, and elsewhere ("N blocks → M visual lines,
    //! Table 3 vertical rules intact, …") so a regression in any of
    //! those is a `cargo test` failure rather than "I ran it once."
    //!
    //! Each test is `#[ignore]`-gated and requires:
    //! - network access on first run (cached under
    //!   `~/.cache/tread/sources/<id>/` thereafter);
    //! - `pandoc` on `PATH` (this exercises the Pandoc fallback
    //!   path; the production code path tries ar5iv first via
    //!   `tread::paper::open_arxiv`).
    //!
    //! Run a single golden:
    //! ```bash
    //! cargo test -p arxiv-render attention_parse_and_layout_golden \
    //!   --release -- --ignored --nocapture
    //! ```
    //!
    //! Run every golden:
    //! ```bash
    //! cargo test -p arxiv-render golden --release \
    //!   -- --ignored --nocapture
    //! ```
    //!
    //! When a parser or layout change shifts a count intentionally,
    //! update the EXPECTED_* constants in the corresponding test and
    //! note the new baseline in the ADR that justifies the change.

    use doc_model::{Block, build_visual_lines};

    use crate::{degrade_images_to_captions, fetch, pandoc_parse};

    /// Shared pipeline: fetch the e-print tarball, run Pandoc, assert
    /// the pre-degrade block count, return the blocks for paper-
    /// specific structural assertions.  Caller follows up with
    /// `assert_visual_line_count` once it has finished its asserts.
    fn parse_and_check_block_count(arxiv_id: &str, expected_blocks: usize) -> Vec<Block> {
        let fetched = fetch::fetch_source(arxiv_id)
            .unwrap_or_else(|e| panic!("fetch arXiv:{arxiv_id}: {e}"));
        let blocks = pandoc_parse::try_pandoc(&fetched.tex)
            .unwrap_or_else(|e| panic!("pandoc parse arXiv:{arxiv_id}: {e}"));
        assert_eq!(
            blocks.len(),
            expected_blocks,
            "[{arxiv_id}] block count drift — if intentional, update the EXPECTED_BLOCKS constant",
        );
        blocks
    }

    /// Run the post-degrade layout the way the arxiv-render binary
    /// does (figures → captions, 80×50 area) and assert the visual-
    /// line count.
    fn assert_visual_line_count(
        arxiv_id: &str,
        mut blocks: Vec<Block>,
        expected_visual_lines: usize,
    ) {
        degrade_images_to_captions(&mut blocks);
        let visual_lines = build_visual_lines(&blocks, 80, 80, 50);
        assert_eq!(
            visual_lines.len(),
            expected_visual_lines,
            "[{arxiv_id}] visual-line count drift — if intentional, update the EXPECTED_VISUAL_LINES constant",
        );
    }

    // ── Attention Is All You Need (1706.03762) ─────────────────────
    // ML paper; stresses equations, tables with vertical rules,
    // figures, multi-section structure, external bibliography
    // (\bibliography{NIPS2017}).

    #[test]
    #[ignore]
    fn attention_parse_and_layout_golden() {
        const ID: &str = "1706.03762";
        /// Pre-degrade block count from ADR-0005 / ADR-0007 smoke.
        const EXPECTED_BLOCKS: usize = 379;
        /// Post-`build_visual_lines` count at the binary's 80×50 layout
        /// after `degrade_images_to_captions` (matches what
        /// `cargo run -p arxiv-render -- 1706.03762` produces on stdout).
        /// 675 → 678 when headings gained a leading blank line for
        /// vertical breathing room (+3 headers lacked a preceding blank).
        /// 678 → 676 when `normalize_blank_rhythm` trimmed the leading
        /// blank and collapsed doubled inter-block gaps to one.
        /// 676 → 697 when bracketed `[Table N: …]` / `[Figure N: …]`
        /// captions began wrapping to the reading measure (was one
        /// over-long line) — see the caption-wrap changelog entry.
        const EXPECTED_VISUAL_LINES: usize = 697;

        let blocks = parse_and_check_block_count(ID, EXPECTED_BLOCKS);

        // Title header.
        assert!(
            blocks.iter().any(|b| matches!(
                b,
                Block::Header { level: 1, text, .. }
                    if text == "Attention Is All You Need"
            )),
            "[{ID}] expected H1 title \"Attention Is All You Need\" not found",
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
            "[{ID}] expected figures 1..=5 in source order, got {figure_ids:?}",
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
        let table_3_rules = blocks.iter().any(|b| matches!(
            b,
            Block::Matrix { vertical_rules, .. }
                if vertical_rules == &[1, 10]
        ));
        assert!(table_3_rules, "[{ID}] Table 3's vertical_rules [1, 10] not found");

        let table_4_rules = blocks.iter().any(|b| matches!(
            b,
            Block::Matrix { vertical_rules, .. }
                if vertical_rules == &[1, 2]
        ));
        assert!(table_4_rules, "[{ID}] Table 4's vertical_rules [1, 2] not found");

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
                Block::Header { level: 1, number: Some(_), .. }
            ))
            .count();
        assert_eq!(
            numbered_section_count, 7,
            "[{ID}] expected 7 numbered top-level sections (1-7)",
        );

        // At least one numbered DisplayMath equation (e.g. the
        // scaled dot-product attention formula at (1)).
        assert!(
            blocks.iter().any(|b| matches!(
                b,
                Block::DisplayMath { num: Some(_), .. }
            )),
            "[{ID}] expected at least one numbered DisplayMath block",
        );

        assert_visual_line_count(ID, blocks, EXPECTED_VISUAL_LINES);
    }

    // ── GPT-3 (2005.14165) ─────────────────────────────────────────
    // 50+ source files via \input{}; stress for cross-file
    // resolution.  Different shape from Attention: more sections,
    // more prose, fewer tables with vertical rules.

    #[test]
    #[ignore]
    fn gpt3_parse_and_layout_golden() {
        const ID: &str = "2005.14165";
        const EXPECTED_BLOCKS: usize = 1422;
        // 3138 → 3134: normalize_blank_rhythm trimmed the leading blank +
        // collapsed doubled inter-block gaps.
        // 3134 → 3357: bracketed captions now wrap to the reading measure.
        const EXPECTED_VISUAL_LINES: usize = 3357;

        let blocks = parse_and_check_block_count(ID, EXPECTED_BLOCKS);

        // GPT-3's title is split across two lines in the source
        // ("Language Models are Few-Shot Learners").  Pandoc renders
        // it as a single H1.
        assert!(
            blocks.iter().any(|b| matches!(
                b,
                Block::Header { level: 1, text, .. }
                    if text.contains("Language Models")
                       && text.contains("Few-Shot Learners")
            )),
            "[{ID}] expected H1 title containing 'Language Models … Few-Shot Learners'",
        );

        // At least one figure (figure 1.1 is the canonical few-shot
        // accuracy plot).
        assert!(
            blocks.iter().any(|b| matches!(b, Block::Figure { .. })),
            "[{ID}] expected at least one Block::Figure",
        );

        // Multiple numbered DisplayMath entries — the paper has
        // explicit numbered equations in §2.
        let numbered_math_count = blocks
            .iter()
            .filter(|b| matches!(b, Block::DisplayMath { num: Some(_), .. }))
            .count();
        assert!(
            numbered_math_count >= 1,
            "[{ID}] expected ≥1 numbered DisplayMath block, got {numbered_math_count}",
        );

        // Heavy multi-section structure — at least 6 numbered top
        // level sections (Intro, Approach, Results, Measuring &
        // Preventing, Related Work, Discussion).
        let numbered_sections: Vec<&str> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Header { level: 1, text, number: Some(_) } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            numbered_sections.len() >= 6,
            "[{ID}] expected ≥6 numbered top-level sections, got {}: {:?}",
            numbered_sections.len(),
            numbered_sections,
        );

        assert_visual_line_count(ID, blocks, EXPECTED_VISUAL_LINES);
    }

    // ── Differential Algebra (1707.09763) ──────────────────────────
    // Dense math, multiline equations, differential operators.
    // Different stress: heavy DisplayMath payload, fewer prose
    // paragraphs.

    #[test]
    #[ignore]
    fn differential_algebra_parse_and_layout_golden() {
        const ID: &str = "1707.09763";
        const EXPECTED_BLOCKS: usize = 530;
        // 1679 → 1677: normalize_blank_rhythm leading-trim + gap collapse.
        // 1677 → 1733: over-wide display equations now wrap at the relation
        // instead of overflowing (this paper is math-heavy).
        const EXPECTED_VISUAL_LINES: usize = 1733;

        let blocks = parse_and_check_block_count(ID, EXPECTED_BLOCKS);

        // Math-heavy paper: expect a significant fraction of blocks
        // to be DisplayMath.  Not pinning a tight ratio (paper-style
        // dependent), but ≥30 numbered display equations is the
        // empirical floor.
        let numbered_math_count = blocks
            .iter()
            .filter(|b| matches!(b, Block::DisplayMath { num: Some(_), .. }))
            .count();
        assert!(
            numbered_math_count >= 30,
            "[{ID}] expected ≥30 numbered DisplayMath blocks (math-heavy paper), got {numbered_math_count}",
        );

        // Any DisplayMath at all (sanity).
        let display_math_count = blocks
            .iter()
            .filter(|b| matches!(b, Block::DisplayMath { .. }))
            .count();
        assert!(
            display_math_count >= 50,
            "[{ID}] expected ≥50 DisplayMath blocks total, got {display_math_count}",
        );

        assert_visual_line_count(ID, blocks, EXPECTED_VISUAL_LINES);
    }

    // ── 3D Gaussian Head Reconstruction (2605.04035) ───────────────
    // Pins the external-`\bibliography{file.bib}` path that B3a was
    // originally about.  This paper SHIPS its .bib in the e-print
    // tarball (referenced via `\bibliography{...}`), so the
    // `bibitems::extract_bibitems_ordered` → `bibtex::extract_bibtex_entries`
    // wiring reads the entries, the auto-append in `try_pandoc`
    // synthesizes a References section, and citations resolve.
    //
    // Compare with Attention: it ALSO uses `\bibliography{NIPS2017}`
    // but doesn't ship NIPS2017.bib in the tarball, so the Pandoc
    // fallback can't reconstruct the bibliography from the local
    // sources alone.  Production users hit the ar5iv primary path
    // for that case (ar5iv runs bibtex itself, so the rendered HTML
    // carries the bibliography).

    #[test]
    #[ignore]
    fn gaussian_head_parse_and_layout_golden() {
        const ID: &str = "2605.04035";
        const EXPECTED_BLOCKS: usize = 789;
        // 1496 → 1500 when headings gained a leading blank line for
        // vertical breathing room (+4 headers lacked a preceding blank).
        // 1500 → 1494: normalize_blank_rhythm leading-trim + gap collapse.
        // 1494 → 1495: one over-wide display equation now wraps at the relation.
        // 1495 → 1553: bracketed captions now wrap to the reading measure.
        const EXPECTED_VISUAL_LINES: usize = 1553;

        let blocks = parse_and_check_block_count(ID, EXPECTED_BLOCKS);

        // Title.
        assert!(
            blocks.iter().any(|b| matches!(
                b,
                Block::Header { level: 1, text, .. }
                    if text.contains("Gaussian Head Reconstruction")
            )),
            "[{ID}] expected H1 title containing 'Gaussian Head Reconstruction'",
        );

        // References header — the load-bearing B3a assertion.  This
        // paper ships its .bib in the tarball; without the
        // `bibitems` → `bibtex` wiring, the auto-append path in
        // `try_pandoc` would never fire and this header would be
        // missing.
        assert!(
            blocks.iter().any(|b| matches!(
                b,
                Block::Header { text, .. } if text == "References"
            )),
            "[{ID}] expected References header (B3a regression — bib parsing broken?)",
        );

        // ≥1 figure (the paper has ~9 in source).
        let figure_count = blocks
            .iter()
            .filter(|b| matches!(b, Block::Figure { .. }))
            .count();
        assert!(
            figure_count >= 5,
            "[{ID}] expected ≥5 figures, got {figure_count}",
        );

        // Numbered top-level sections present (Introduction through
        // the appendix sections — 13 in the live parse: 7 in the
        // main paper + 6 in the supplementary material).
        let numbered_section_count = blocks
            .iter()
            .filter(|b| matches!(
                b,
                Block::Header { level: 1, number: Some(_), .. }
            ))
            .count();
        assert!(
            numbered_section_count >= 7,
            "[{ID}] expected ≥7 numbered top-level sections, got {numbered_section_count}",
        );

        assert_visual_line_count(ID, blocks, EXPECTED_VISUAL_LINES);
    }

    // Diffusion Geometry (2602.06006) used to fail the Pandoc parse on
    // `\newcolumntype{C}[1]{>{\centering\arraybackslash}p{#1}}`; the
    // `strip_newcolumntype` preprocess pass now drops those preamble
    // definitions, so the Pandoc path parses it (~2138 blocks).  A
    // Pandoc-path golden for it could be pinned here following the
    // existing helpers; not yet wired up.
}
