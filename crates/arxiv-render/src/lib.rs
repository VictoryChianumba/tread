pub mod bibtex;
pub mod fetch;
pub mod pandoc_parse;
pub mod parse;
pub mod pdf_anchors;
pub mod placement;

pub use parse::{extract_bibitems, to_blocks};
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
