//! Alt-screen smoke test.
//!
//! Block-reader works the moment we enter `EnterAlternateScreen`; the
//! plain `smoke` example does not.  This binary does the minimum to
//! enter alt-screen mode (no ratatui, no mouse capture, no keyboard
//! enhancement flags) and emit one Kitty graphics escape — so we can
//! tell whether alt-screen alone is enough to break iTerm2 image
//! rendering, or whether some other piece of the reader's setup is
//! the actual culprit.
//!
//!   cargo run --release -p kitty-graphics --example altscreen_smoke -- <png>
//!
//! Press any key to exit.

use std::io::{self, Read, Write};

fn main() {
  let path = std::env::args().nth(1).expect("usage: altscreen_smoke <path-to.png>");
  let png = std::fs::read(&path).expect("read png");

  // Enter alt screen via raw ANSI — same DECSET as ratatui uses.
  print!("\x1b[?1049h");
  // Clear so our image isn't competing with leftover cells.
  print!("\x1b[2J");
  io::stdout().flush().unwrap();

  // Emit the same a=T escape tread would use.  Cursor positioning
  // is bundled inside the escape so it survives tmux passthrough.
  let id = 1u32;
  if let Err(e) = kitty_graphics::transmit::transmit_and_place(id, &png, 60, 12, 5, 5) {
    eprintln!("transmit failed: {e}");
  }
  io::stdout().flush().unwrap();

  // Wait for any byte on stdin so we can hold the alt screen open.
  let mut buf = [0u8; 1];
  let _ = io::stdin().read(&mut buf);

  // Leave alt screen.
  print!("\x1b[?1049l");
  io::stdout().flush().unwrap();
}
