//! Cursor / scroll seam for `Reader`.
//!
//! `offset`, `cursor_y`, `cursor_x`, and `desired_column` are private
//! to `state/`; every external write lands here.  The invariants the
//! methods own:
//!
//! - `jump_to_line` (and the variants below) always reset `cursor_x`
//!   and `desired_column` to 0.  Forgetting this is the historical
//!   source of "`j` after a jump goes to the wrong column" bugs —
//!   `desired_column` lingered from before the jump.
//! - `center_on_line` preserves `cursor_x` / `desired_column`: the
//!   user is being auto-scrolled (voice playback), not jumping, so
//!   their column should follow them.
//! - `line` is always clamped to `[0, total_lines)`.  Out-of-range
//!   jumps are silently floored at `total - 1`, matching vim's `G N`.
//!
//! Read-side access goes through `offset()`, `cursor_y()`,
//! `cursor_x()`, and `desired_column()` — the fields stay private so
//! the compiler enforces routing through these helpers.
//!
//! `clamp_cursor_after_line_change` / `remember_column` (defined in
//! `state/nav.rs`) are deliberately NOT here — they're internal to
//! the nav module and shouldn't be called from outside the
//! reader-internal motion code.

use super::Reader;

#[cfg(test)]
impl Reader {
    /// Test-only: set `cursor_y` and `cursor_x` directly, leaving
    /// `offset` untouched.  Mirrors the pre-Seam-2 idiom
    /// `r.cursor_y = …; r.cursor_x = …;` that unit tests use to land
    /// on a specific (relative line, column) before exercising
    /// downstream code.  Not exposed to production callers.
    pub(crate) fn cursor_set_for_test(&mut self, cursor_y: usize, x: usize) {
        self.cursor_y = cursor_y;
        self.cursor_x = x;
        self.desired_column = x;
    }
}

impl Reader {
    /// Active viewport top in absolute-line indices.
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Active cursor row within the viewport (0 = topmost visible line).
    pub fn cursor_y(&self) -> usize {
        self.cursor_y
    }

    /// Active cursor byte column on the current visual line.
    pub fn cursor_x(&self) -> usize {
        self.cursor_x
    }

    /// Vim's `curswant`: the column to restore when a vertical motion
    /// crosses through a short line back onto a long one.
    pub fn desired_column(&self) -> usize {
        self.desired_column
    }

    /// Jump the cursor to absolute line `line`, top-aligning the
    /// viewport if a scroll is needed (in either direction).  Resets
    /// `cursor_x` and `desired_column` to 0.
    ///
    /// Use for `Enter` (follow link), `Shift+Enter` (citation),
    /// citation popups, `:goto`, `:abstract`, `:references`,
    /// `:cite`, and `G N` (vim's count-prefixed bottom-jump).  Pair
    /// with `push_nav_mark()` at the call site if the jump should be
    /// reversible by `Ctrl+O`.
    pub fn jump_to_line(&mut self, line: usize) {
        let total = self.total_lines();
        if total == 0 {
            return;
        }
        let line = line.min(total - 1);
        let ch = self.content_height();
        if line >= self.offset && line < self.offset + ch {
            self.cursor_y = line - self.offset;
        } else {
            self.offset = line;
            self.cursor_y = 0;
        }
        self.cursor_x = 0;
        self.desired_column = 0;
    }

    /// Jump to `line` but bottom-align the viewport when scrolling —
    /// i.e. the target sits at the LAST visible row, with the
    /// preceding lines as context above.  Resets `cursor_x` and
    /// `desired_column`.
    ///
    /// Use for `:N` (numeric goto): the user typed a specific line
    /// because they want to see what's there *and* what came before
    /// it.  Distinguished from `jump_to_line` by intent — link
    /// follows want context BELOW the target; numeric jumps want
    /// context ABOVE.
    pub fn jump_to_line_with_context_above(&mut self, line: usize) {
        let total = self.total_lines();
        if total == 0 {
            return;
        }
        let line = line.min(total - 1);
        let ch = self.content_height();
        if line < self.offset || line >= self.offset + ch {
            let max_offset = total.saturating_sub(ch);
            self.offset = (line + 1).saturating_sub(ch).min(max_offset);
        }
        self.cursor_y = line.saturating_sub(self.offset);
        self.cursor_x = 0;
        self.desired_column = 0;
    }

    /// Set the cursor's byte column on the current line and remember
    /// it as the desired column for subsequent vertical motions.
    /// Used by intra-line motions that originate outside `state/nav.rs`
    /// — currently just `f` / `F` / `t` / `T` (find-char-in-line).
    pub fn set_cursor_x(&mut self, x: usize) {
        self.cursor_x = x;
        self.desired_column = x;
    }

    /// Recenter the viewport so `line` sits at vertical midpoint,
    /// preserving `cursor_x` and `desired_column`.  When `line` is
    /// closer to the document top than half a screen, the viewport
    /// clamps at offset 0 and the cursor row tracks the absolute
    /// line instead.
    ///
    /// Use for auto-scroll while voice playback follows a paragraph
    /// across the document — the reader is being moved with the
    /// audio, not jumping, so their column shouldn't reset.
    pub fn center_on_line(&mut self, line: usize) {
        let total = self.total_lines();
        if total == 0 {
            return;
        }
        let line = line.min(total - 1);
        let half = self.content_height() / 2;
        if line >= half {
            self.offset = line - half;
            self.cursor_y = half;
        } else {
            self.offset = 0;
            self.cursor_y = line;
        }
    }
}
