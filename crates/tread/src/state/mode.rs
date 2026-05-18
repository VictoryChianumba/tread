//! Mode-transition seam for `Reader`.
//!
//! `Reader.mode` is private (see `state.rs`); every transition lands
//! here.  The invariants that the methods own:
//!
//! - Entering Command mode clears `cmd_buf` and `cmd_error` so the
//!   prompt always starts empty.
//! - Entering Search mode clears `search_query` and `search_matches`
//!   so the bar starts empty and stale match indices can't leak.
//! - Entering Visual mode seeds `visual_anchor` and `visual_anchor_x`
//!   from the current cursor — callers no longer have to remember to
//!   set them in lockstep with the mode write.
//! - Returning to Normal clears `count_buf` and `cmd_buf`.  Returning
//!   to Normal does NOT clear `search_query` / `search_matches`:
//!   `n` / `N` after a `/foo<Enter>` rely on those persisting (vim
//!   convention).  Search-cancel (Esc) clears them explicitly in
//!   `nav::cancel_search`.
//!
//! Read-side access goes through `Reader::mode()` — the field stays
//! private so the compiler enforces routing through these helpers.

use super::{FindKind, Mode, Operator, Reader};

impl Reader {
    /// Read-only view of the active mode.  Used by `render.rs` and
    /// tests; writers go through the transition helpers below.
    pub fn mode(&self) -> &Mode {
        &self.mode
    }

    /// Return to Normal mode, clearing the count and command buffers.
    /// Used by every one-shot mode (`AwaitingChar`, `AwaitingG`, …)
    /// after it consumes its follow-up keystroke, and by Command /
    /// Search on Esc.  Does not touch `search_query` / `search_matches`
    /// — those survive normal-mode round-trips so `n` / `N` work.
    pub fn return_to_normal(&mut self) {
        self.mode = Mode::Normal;
        self.count_buf.clear();
        self.cmd_buf.clear();
    }

    /// Enter Command mode (`:`-prefixed Ex-command input).  Resets
    /// the prompt buffer and clears any stale error so the bar
    /// renders empty.
    pub fn enter_command_mode(&mut self) {
        self.count_buf.clear();
        self.cmd_buf.clear();
        self.cmd_error = None;
        self.mode = Mode::Command;
    }

    /// Enter Visual mode (char-wise or line-wise), seeding the
    /// selection anchor from the current cursor.  In line mode the
    /// horizontal anchor is forced to column 0 — line-wise selections
    /// always span whole lines regardless of where the user pressed V.
    pub fn enter_visual_mode(&mut self, line_mode: bool) {
        self.count_buf.clear();
        self.visual_anchor = self.current_line();
        self.visual_anchor_x = if line_mode { 0 } else { self.cursor_x };
        self.mode = Mode::Visual { line_mode };
    }

    /// Enter the one-shot `AwaitingG` state (vim's `g`-prefix).
    pub fn enter_awaiting_g(&mut self) {
        self.count_buf.clear();
        self.mode = Mode::AwaitingG;
    }

    /// Enter `AwaitingChar` for the `f` / `F` / `t` / `T` family.
    pub fn enter_awaiting_char(&mut self, kind: FindKind) {
        self.count_buf.clear();
        self.mode = Mode::AwaitingChar { kind };
    }

    /// Enter `AwaitingBracket` after `]` or `[`.  `forward = true`
    /// records the `]` flavor.
    pub fn enter_awaiting_bracket(&mut self, forward: bool) {
        self.count_buf.clear();
        self.mode = Mode::AwaitingBracket { forward };
    }

    /// Enter `AwaitingMarkName` after `m` (set) or `'` / `` ` `` (jump).
    pub fn enter_awaiting_mark_name(&mut self, for_set: bool) {
        self.count_buf.clear();
        self.mode = Mode::AwaitingMarkName { for_set };
    }

    /// Enter `AwaitingOperator` after an operator key (currently `y`).
    pub fn enter_awaiting_operator(&mut self, op: Operator) {
        self.count_buf.clear();
        self.mode = Mode::AwaitingOperator { op };
    }

    /// Enter `AwaitingTextObject` after `yi` / `ya`.
    pub fn enter_awaiting_text_object(&mut self, op: Operator, around: bool) {
        self.mode = Mode::AwaitingTextObject { op, around };
    }

    /// Enter Search mode (`/`-prefixed query input).  Owns two extra
    /// invariants beyond the mode write: the in-progress query buffer
    /// and the live-match list both start empty so the bar doesn't
    /// show stale text and `n` can't jump to a stale match before the
    /// user types a character.
    pub fn enter_search(&mut self) {
        self.mode = Mode::Search;
        self.search_query.clear();
        self.search_matches.clear();
    }
}
