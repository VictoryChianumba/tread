use arxiv_render::pandoc_parse::try_pandoc;
use doc_model::Block;

fn main() {
    let dir = std::env::args()
        .nth(1)
        .expect("usage: dump_figures <source-dir>");
    let dir = std::path::Path::new(&dir);
    let mut files: Vec<(String, String)> = Vec::new();
    fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, String)>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(root, &p, out);
            } else if p.extension().and_then(|s| s.to_str()) == Some("tex") {
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().to_string();
                if let Ok(content) = std::fs::read_to_string(&p) {
                    out.push((rel, content));
                }
            }
        }
    }
    walk(dir, dir, &mut files);
    eprintln!("found {} .tex files", files.len());
    let blocks = try_pandoc(&files).expect("pandoc failed");
    let mut idx = 0;
    for b in &blocks {
        if let Block::Figure {
            rows,
            alt,
            figure_id,
            column_gaps_after,
            header_rows,
        } = b
        {
            idx += 1;
            let total_imgs: usize = rows.iter().map(|r| r.len()).sum();
            let alt_trim: String = alt.chars().take(70).collect();
            eprintln!(
                "[{idx}] fig#{figure_id} rows={} parts={total_imgs} gaps_after={column_gaps_after:?} headers={} alt={alt_trim:?}",
                rows.len(),
                header_rows.len(),
            );
            for (hi, hrow) in header_rows.iter().enumerate() {
                let summary: String = hrow
                    .iter()
                    .map(|c| format!("{:?}×{}", c.text, c.col_span))
                    .collect::<Vec<_>>()
                    .join(" | ");
                eprintln!("   header[{hi}]: {summary}");
            }
            for (ri, row) in rows.iter().enumerate() {
                let summary: String = row
                    .iter()
                    .take(3)
                    .map(|it| {
                        it.path
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("?")
                            .to_string()
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                eprintln!(
                    "   row[{ri}]: {} items: {}{}",
                    row.len(),
                    summary,
                    if row.len() > 3 { ", ..." } else { "" }
                );
            }
        }
    }
    eprintln!("\ntotal Block::Figure entries: {idx}");
}
