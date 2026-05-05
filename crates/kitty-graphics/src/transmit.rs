//! Kitty graphics protocol — transmit and place operations.
//!
//! Wire format reference: <https://sw.kovidgoyal.net/kitty/graphics-protocol/>
//!
//! Per-image lifecycle:
//! 1. **Transmit** (`a=t`) — send the image bytes once, keyed by id.
//!    Subsequent placements reference the cached image by id.
//! 2. **Place** (`a=p`) — render at the cursor's current row/col, with
//!    a target size in cells (`c=<cols>,r=<rows>`).
//! 3. **Delete** (`a=d`) — clear the placement (and optionally the cached
//!    image data) when the image scrolls out of view.
//!
//! All escapes are written directly to stdout, bypassing ratatui's
//! buffer.  The render loop is responsible for ensuring this happens
//! AFTER `terminal.draw()` returns so ratatui's character output
//! doesn't paint over the image area on the same frame.

use std::io::{self, Write};

/// True when running inside tmux.  Read once per call (cheap — `getenv`
/// is constant-time on every modern libc) so a re-attach mid-session
/// (which sets/unsets `$TMUX`) doesn't need a process restart.
fn in_tmux() -> bool {
  std::env::var_os("TMUX").is_some()
}

/// Wrap `payload` (an APC graphics escape including its `\x1b\\`
/// terminator) in tmux's DCS passthrough envelope.  tmux passes the
/// inner bytes verbatim to the underlying terminal *iff* the user has
/// `set -g allow-passthrough on` in their config; otherwise the whole
/// sequence is silently consumed.  Either way, no garbage on screen.
///
/// Wire shape: `\x1bPtmux;` + (payload with every `\x1b` doubled) +
/// `\x1b\\`.  The Kitty docs render this as "`\033Ptmux;\033 ...
/// \033\\`" — that lone `\033` after `Ptmux;` is the first ESC of the
/// payload-already-doubled, *not* a separate lead-in.  Adding an extra
/// lead-in would cause tmux to treat it as an early ST terminator and
/// drop the wrapped sequence.
fn tmux_wrap(payload: &[u8]) -> Vec<u8> {
  let mut out = Vec::with_capacity(payload.len() * 2 + 8);
  out.extend_from_slice(b"\x1bPtmux;");
  for &b in payload {
    if b == 0x1b {
      out.push(0x1b);
    }
    out.push(b);
  }
  out.extend_from_slice(b"\x1b\\");
  out
}

/// Write a single APC graphics escape to stdout, applying the tmux
/// passthrough wrap when needed.  All Kitty `_G…\x1b\\` sequences flow
/// through here so the wrapping logic lives in exactly one place.
fn emit_apc<W: Write>(handle: &mut W, payload: &[u8]) -> io::Result<()> {
  if in_tmux() {
    handle.write_all(&tmux_wrap(payload))
  } else {
    handle.write_all(payload)
  }
}

/// Transmit the PNG bytes AND display them at `(abs_row, abs_col)` in
/// one shot (`a=T`).  This is the most-compatible variant of the Kitty
/// graphics protocol: native Kitty supports it, and so do iTerm2 and
/// WezTerm — whereas the split `a=t` (transmit-cache) + `a=p` (place
/// by id) flow only works on terminals with a real image cache, which
/// iTerm2 lacks.
///
/// **Cursor positioning lives inside the same passthrough envelope as
/// the image escape.**  Inside tmux, a bare CSI `\x1b[<r>;<c>H` is
/// interpreted by tmux for its own pane buffer but *not* forwarded to
/// the underlying terminal — so iTerm2 keeps its stale cursor (usually
/// at the alt-screen origin) and `a=T` lands the image at top-left
/// regardless of what we set.  Combining the two escapes into one
/// `\033Ptmux;...\033\\` block forwards both to iTerm2 atomically:
/// it moves its cursor, then renders the image there.
///
/// Sent as a **single APC sequence**, no chunking.  iTerm2's Kitty
/// implementation handles the first metadata chunk correctly but
/// silently discards continuation chunks, so we put the entire payload
/// in one escape.  tmux 3.x holds the wrapped DCS body in a
/// dynamically-grown buffer with no practical size cap, so this works
/// for typical paper figures (~150–500 KB encoded).
///
/// `cols` × `rows` is the cell-grid footprint; the terminal scales the
/// PNG to fit.  `q=2` keeps the terminal silent on success/error so
/// crossterm's stdin reader doesn't get fed status replies.  `C=1`
/// asks the terminal not to move the cursor after rendering, so the
/// next ratatui frame starts from a sane position.
pub fn transmit_and_place(
  id: u32,
  png_bytes: &[u8],
  cols: u16,
  rows: u16,
  abs_row: u16,
  abs_col: u16,
) -> io::Result<()> {
  let b64 = base64_encode(png_bytes);
  let stdout = io::stdout();
  let mut handle = stdout.lock();
  let mut payload: Vec<u8> = Vec::with_capacity(96 + b64.len() + 16);
  // Cursor positioning first — bundled into the same payload so tmux
  // passthrough forwards it to iTerm2 in lockstep with the APC.
  write!(payload, "\x1b[{abs_row};{abs_col}H")?;
  write!(
    payload,
    "\x1b_Ga=T,t=d,f=100,i={id},c={cols},r={rows},C=1,q=2;"
  )?;
  payload.extend_from_slice(b64.as_bytes());
  payload.extend_from_slice(b"\x1b\\");
  emit_apc(&mut handle, &payload)?;
  handle.flush()
}

/// Delete a placement of an image (by id) without removing the cached
/// image data — i.e. clear the visible image but keep the bytes so we
/// can re-place quickly when scrolled back into view.
pub fn delete_placement(id: u32) -> io::Result<()> {
  let stdout = io::stdout();
  let mut handle = stdout.lock();
  // a=d (delete), d=i (by id, free=keep), q=2 (silent).
  let mut payload = Vec::with_capacity(32);
  write!(payload, "\x1b_Ga=d,d=i,i={id},q=2\x1b\\")?;
  emit_apc(&mut handle, &payload)?;
  handle.flush()
}

/// Position the terminal cursor at (row, col) — both 1-indexed,
/// matching the ANSI CUP convention.
pub fn move_cursor(row: u16, col: u16) -> io::Result<()> {
  let stdout = io::stdout();
  let mut handle = stdout.lock();
  write!(handle, "\x1b[{row};{col}H")?;
  handle.flush()
}

/// Standard base64 encoder.  Hand-rolled to avoid pulling in the
/// `base64` crate just for this — the OSC 52 yank in tread uses
/// the same trick.
fn base64_encode(data: &[u8]) -> String {
  const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
  for chunk in data.chunks(3) {
    let b0 = chunk[0] as usize;
    let b1 = chunk.get(1).copied().unwrap_or(0) as usize;
    let b2 = chunk.get(2).copied().unwrap_or(0) as usize;
    out.push(T[b0 >> 2] as char);
    out.push(T[((b0 & 0x03) << 4) | (b1 >> 4)] as char);
    if chunk.len() > 1 {
      out.push(T[((b1 & 0x0f) << 2) | (b2 >> 6)] as char);
    } else {
      out.push('=');
    }
    if chunk.len() > 2 {
      out.push(T[b2 & 0x3f] as char);
    } else {
      out.push('=');
    }
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn base64_roundtrip_known_values() {
    // Known answers from RFC 4648.
    assert_eq!(base64_encode(b""), "");
    assert_eq!(base64_encode(b"f"), "Zg==");
    assert_eq!(base64_encode(b"fo"), "Zm8=");
    assert_eq!(base64_encode(b"foo"), "Zm9v");
    assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
    assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
    assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
  }

  #[test]
  fn base64_handles_binary() {
    // Non-ASCII bytes should encode unchanged.
    let bytes = vec![0u8, 1, 2, 3, 0xff, 0xfe, 0xfd];
    let out = base64_encode(&bytes);
    // Just check we don't panic and produce something pad-aligned.
    assert_eq!(out.len() % 4, 0);
    assert!(!out.is_empty());
  }
}
