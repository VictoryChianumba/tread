//! Synthetic redraw benchmark for tread.
//!
//! Drives `tread::draw` against ratatui's TestBackend in a tight loop —
//! no terminal I/O, no crossterm event poll, no idle waits.  Measures
//! the pure per-frame render cost that the wall-clock sampling
//! profiler couldn't isolate (samply was 94% idle on `kevent`).
//!
//! Three scenarios, each over `ITERATIONS` frames after `WARMUP`:
//! - **idle redraw**: state unchanged frame to frame.  Floor for the
//!   widget tree + ratatui buffer-diff cost.
//! - **scrolling**: `offset++` per frame — different VL slice rendered
//!   each frame, buffer diff sees mostly-changed cells.
//! - **TOC toggle**: alternates `toggle_toc()` per frame, which
//!   triggers `build_visual_lines` to re-wrap the whole document to
//!   the new content width.  Hot reflow path.
//!
//! Usage: `cargo run --release --example redraw_bench`.

use doc_model::{Block, InlineSpan};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use std::time::Instant;
use tread::{Reader, Theme};

const WIDTH: u16 = 120;
const HEIGHT: u16 = 40;
const WARMUP: usize = 50;
const ITERATIONS: usize = 2000;

fn synth_blocks() -> Vec<Block> {
    let mut out = Vec::with_capacity(3000);
    for sec in 0..40 {
        out.push(Block::Header {
            level: 1,
            text: format!("Section {sec} — Synthetic Content for Redraw Benchmarking"),
        });
        out.push(Block::Blank);
        for para in 0..20 {
            if para % 3 == 0 {
                out.push(Block::StyledLine(vec![
                    InlineSpan::plain(format!("Paragraph {para} starts plain, ")),
                    InlineSpan::bold("then goes bold,"),
                    InlineSpan::plain(" then "),
                    InlineSpan::italic("italic,"),
                    InlineSpan::plain(" then back to "),
                    InlineSpan::monospace("monospace tokens"),
                    InlineSpan::plain(
                        " — exercising the per-span style switch in the ratatui line builder.",
                    ),
                ]));
            } else {
                out.push(Block::Line(format!(
          "Section {sec}, paragraph {para}: filler prose long enough to wrap across at least one \
           visual line at the target terminal width.  The renderer's per-frame cost depends \
           heavily on how many visual lines fall inside the viewport, so a paragraph that wraps \
           twice has roughly twice the redraw cost of one that wraps once."
        )));
            }
            out.push(Block::Blank);
        }
    }
    out
}

fn percentile(sorted: &[u128], p: f64) -> u128 {
    let idx = (((sorted.len() - 1) as f64) * p).round() as usize;
    sorted[idx]
}

fn run_scenario<F: FnMut(&mut Reader, usize)>(label: &str, mut step: F) {
    let mut reader = Reader::new(synth_blocks(), WIDTH as usize, HEIGHT as usize);
    let theme = Theme::dark();
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
    let area = Rect::new(0, 0, WIDTH, HEIGHT);

    for i in 0..WARMUP {
        step(&mut reader, i);
        terminal
            .draw(|f| tread::draw(f, area, &reader, &theme))
            .unwrap();
    }

    let mut times_ns: Vec<u128> = Vec::with_capacity(ITERATIONS);
    for i in 0..ITERATIONS {
        step(&mut reader, WARMUP + i);
        let start = Instant::now();
        terminal
            .draw(|f| tread::draw(f, area, &reader, &theme))
            .unwrap();
        times_ns.push(start.elapsed().as_nanos());
    }

    times_ns.sort_unstable();
    let n = times_ns.len();
    let mean_us = (times_ns.iter().sum::<u128>() as f64 / n as f64) / 1000.0;
    let p50_us = percentile(&times_ns, 0.50) as f64 / 1000.0;
    let p95_us = percentile(&times_ns, 0.95) as f64 / 1000.0;
    let p99_us = percentile(&times_ns, 0.99) as f64 / 1000.0;
    let fps = 1_000_000.0 / mean_us;

    println!(
        "{label:<24}  mean={mean_us:>7.1}µs  p50={p50_us:>7.1}µs  p95={p95_us:>7.1}µs  \
     p99={p99_us:>7.1}µs  ({fps:>6.0} fps eq.)",
    );
}

fn main() {
    println!(
        "\ntread redraw benchmark — {ITERATIONS} iterations after {WARMUP} warmup, \
     {WIDTH}×{HEIGHT} cells\n"
    );

    run_scenario("idle redraw", |_, _| {});

    run_scenario("scrolling (offset++)", |r, i| {
        let max_off = r.visual_lines.len().saturating_sub(HEIGHT as usize).max(1);
        r.jump_to_line(i % max_off);
    });

    run_scenario("TOC toggle (reflow)", |r, _| {
        r.toggle_toc();
    });

    println!();
}
