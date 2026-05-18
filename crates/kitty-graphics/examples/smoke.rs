//! Standalone smoke test for the Kitty graphics pipeline.
//!
//! Reads a PNG from argv[1], exercises the exact transmit/place path
//! used by tread (including tmux passthrough wrapping), then
//! sleeps a few seconds so the image stays visible before the shell
//! redraws.  Run inside the same terminal you're seeing the bug in.
//!
//!   cargo run --release -p kitty-graphics --example smoke -- <path-to.png>

use std::io::Write;

fn main() {
    let path = std::env::args().nth(1).expect("usage: smoke <path-to.png>");
    let png = std::fs::read(&path).expect("read png");

    println!("kitty-graphics smoke test");
    println!("  capability: {:?}", kitty_graphics::detect());
    println!("  in_tmux:    {}", kitty_graphics::in_tmux());
    println!("  png bytes:  {}", png.len());
    println!();

    // Reserve some blank rows so the image has somewhere to land.  Without
    // these, the next prompt redraw immediately scrolls over our image.
    for _ in 0..14 {
        println!();
    }

    // Transmit-and-place at row 5 (so the image lands inside the visible
    // window above the prompt's eventual return).  Cursor positioning is
    // bundled into the escape, so no separate `move_cursor` is needed.
    let id = 1u32;
    kitty_graphics::transmit::transmit_and_place(id, &png, 60, 12, 5, 1)
        .expect("transmit_and_place");
    std::io::stdout().flush().unwrap();

    // Hold visible long enough for the user to see anything that rendered.
    std::thread::sleep(std::time::Duration::from_secs(4));

    // Move below the image area so the shell doesn't paint over it
    // immediately.  Then delete the placement so it doesn't linger after
    // exit (clean handoff back to the prompt).
    print!("\x1b[14B\n");
    let _ = kitty_graphics::transmit::delete_placement(id);
    std::io::stdout().flush().unwrap();
}
