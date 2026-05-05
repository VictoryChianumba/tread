pub mod fetch;
pub mod pandoc_parse;
pub mod parse;
pub mod pdf_anchors;
pub mod placement;

pub use parse::{extract_bibitems, to_blocks};
pub use placement::lift_tables;

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
    match b {
      doc_model::Block::Image { path, .. } => resolve(path),
      doc_model::Block::ImageRow { items, .. } => {
        for item in items.iter_mut() {
          resolve(&mut item.path);
        }
      }
      _ => {}
    }
  }
}
