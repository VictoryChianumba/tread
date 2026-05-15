//! Kitty graphics protocol support for tread.
//!
//! Two responsibilities for now:
//! - **Capability detection** — figure out at startup whether the host
//!   terminal speaks the protocol, so the reader can gate inline-image
//!   rendering accordingly and fall back to text placeholders elsewhere.
//! - (Future stages: PNG transmission, image placement, PDF→PNG.)
//!
//! ## Detection strategy
//!
//! Two viable approaches for detecting Kitty graphics support:
//!
//! 1. **Protocol query** — send `\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\`
//!    and read the response with a timeout.  Most accurate but fiddly:
//!    requires raw stdin reading, a thread or non-blocking I/O, and
//!    bracketing with a known-safe query (`\x1b[c`) so non-supporting
//!    terminals don't just hang us out.
//!
//! 2. **Environment-variable sniffing** — Kitty/WezTerm/Ghostty/iTerm2
//!    each set distinguishing env vars when launching child processes.
//!    Less precise (a user could set `KITTY_WINDOW_ID` while running
//!    inside a non-Kitty wrapper), but robust, instantaneous, no I/O,
//!    and aligns with how viuer / chafa / similar tools detect support.
//!
//! v1 uses approach #2.  If we ever need precision, we can layer
//! approach #1 on top behind a feature flag.

pub mod pdf;
pub mod png;
pub mod transmit;

/// Whether the host terminal supports the Kitty graphics protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
  Supported,
  Unsupported,
}

/// Detect Kitty graphics support via environment-variable sniffing.
///
/// Recognised terminals:
/// - **Kitty**: sets `KITTY_WINDOW_ID`.
/// - **WezTerm**: sets `WEZTERM_PANE` and/or `WEZTERM_EXECUTABLE`.
/// - **Ghostty**: sets `GHOSTTY_RESOURCES_DIR`.
/// - **iTerm2 ≥ 3.5**: sets `LC_TERMINAL=iTerm2`.  (Older iTerm2 does
///   not speak the Kitty protocol; if `LC_TERMINAL_VERSION` is parseable
///   we check the major version, else we conservatively treat any
///   `LC_TERMINAL=iTerm2` as supported and let the user disable via
///   `TREAD_DISABLE_KITTY_GRAPHICS=1` if it misbehaves.)
/// - **TERM** containing `kitty` or `ghostty` as a fallback.
///
/// Override: setting `TREAD_DISABLE_KITTY_GRAPHICS=1` forces
/// `Unsupported` regardless of detection.
pub fn detect() -> Capability {
  if std::env::var("TREAD_DISABLE_KITTY_GRAPHICS").is_ok() {
    return Capability::Unsupported;
  }
  // Manual force-enable.  Useful inside tmux when the host terminal
  // supports Kitty graphics but no env-var hint survives the multiplexer.
  if std::env::var("TREAD_FORCE_KITTY_GRAPHICS").is_ok() {
    return Capability::Supported;
  }
  if std::env::var("KITTY_WINDOW_ID").is_ok() {
    return Capability::Supported;
  }
  if std::env::var("WEZTERM_PANE").is_ok() || std::env::var("WEZTERM_EXECUTABLE").is_ok() {
    return Capability::Supported;
  }
  if std::env::var("GHOSTTY_RESOURCES_DIR").is_ok() {
    return Capability::Supported;
  }
  if let Ok(term) = std::env::var("LC_TERMINAL")
    && term.eq_ignore_ascii_case("iTerm2")
    && iterm2_at_least_3_5()
  {
    return Capability::Supported;
  }
  // `TERM_PROGRAM` survives tmux on most setups (it's set by the host
  // terminal, not by the shell), so we use it as the tmux fallback —
  // when iTerm2 launches tmux, `TERM_PROGRAM=iTerm.app` propagates into
  // pane shells even though `KITTY_WINDOW_ID` / `LC_TERMINAL` typically
  // don't.  The version check is best-effort: missing/unparseable means
  // we trust the user (over-enable on iTerm2 ≥ 3.5).
  if let Ok(prog) = std::env::var("TERM_PROGRAM") {
    let p = prog.to_ascii_lowercase();
    if p.contains("iterm") && iterm2_term_program_at_least_3_5() {
      return Capability::Supported;
    }
    if p.contains("kitty") || p.contains("ghostty") || p.contains("wezterm") {
      return Capability::Supported;
    }
  }
  if let Ok(term) = std::env::var("TERM")
    && (term.contains("kitty") || term.contains("ghostty"))
  {
    return Capability::Supported;
  }
  Capability::Unsupported
}

/// True when running inside tmux.  `transmit` knows how to wrap
/// graphics escapes in tmux's DCS passthrough envelope, but the user
/// must additionally have `set -g allow-passthrough on` in their tmux
/// config — otherwise tmux silently drops the wrapped sequence.
pub fn in_tmux() -> bool {
  std::env::var_os("TMUX").is_some()
}

/// Probe the active tmux server for the `allow-passthrough` setting.
///
/// Returns:
/// - `None` if we're not in tmux, or the probe failed (older tmux
///   without the option, no `tmux` binary on PATH, server unreachable,
///   etc.) — i.e. "unknown".
/// - `Some(true)` if the option is explicitly `on`.
/// - `Some(false)` if it's explicitly `off`.
///
/// `Some(false)` is the silent-failure trap this is meant to catch:
/// every DCS passthrough envelope tread emits will be consumed by tmux
/// before reaching the host terminal, so callers should surface a loud
/// warning (or refuse to enable graphics) when that's the case.
///
/// Cost: one `fork`/`exec` of the `tmux` binary at the call site.
/// Cheap relative to the rest of startup, so safe to call from `main`.
pub fn tmux_passthrough_enabled() -> Option<bool> {
  if !in_tmux() {
    return None;
  }
  let out = std::process::Command::new("tmux")
    .args(["show", "-gv", "allow-passthrough"])
    .output()
    .ok()?;
  if !out.status.success() {
    return None;
  }
  parse_tmux_passthrough_output(&out.stdout)
}

/// Split out for unit-testing without spawning processes.  `tmux show
/// -gv allow-passthrough` prints `on\n` / `off\n` on tmux ≥ 3.3, an
/// empty string when the option is unset, or fails entirely on older
/// versions.  We treat anything other than the literal `on`/`off`
/// tokens as `None` (unknown) so the caller can choose its messaging.
fn parse_tmux_passthrough_output(stdout: &[u8]) -> Option<bool> {
  let s = std::str::from_utf8(stdout).ok()?.trim();
  match s {
    "on" => Some(true),
    "off" => Some(false),
    _ => None,
  }
}

/// True when running inside Zellij.  Unlike tmux, Zellij has no
/// documented passthrough envelope for APC sequences — recent
/// versions intercept Kitty graphics directly and re-render via the
/// host terminal's image protocol.  That re-emission works for some
/// host pairs (Ghostty, Kitty, WezTerm) but is known to fail
/// silently for iTerm2's Kitty fork.  Callers use this together with
/// `is_iterm2` to print an actionable startup warning so the user
/// knows the broken-figures pane isn't a tread bug.
pub fn in_zellij() -> bool {
  std::env::var_os("ZELLIJ").is_some()
    || std::env::var_os("ZELLIJ_SESSION_NAME").is_some()
}

/// True when the host terminal program is iTerm2 (regardless of
/// version).  Checks `LC_TERMINAL` first (set directly by iTerm2)
/// and falls back to `TERM_PROGRAM` (which propagates through both
/// tmux and Zellij).  Returns false for Ghostty / Kitty / WezTerm
/// even though those also support Kitty graphics — this predicate
/// exists specifically to scope the Zellij-over-iTerm2 warning, not
/// to gate graphics emission generally.
pub fn is_iterm2() -> bool {
  if let Ok(term) = std::env::var("LC_TERMINAL")
    && term.eq_ignore_ascii_case("iTerm2")
  {
    return true;
  }
  if let Ok(prog) = std::env::var("TERM_PROGRAM")
    && prog.to_ascii_lowercase().contains("iterm")
  {
    return true;
  }
  false
}

/// True when the host terminal persists transmitted image data across
/// frames — i.e. supports the cached `a=p` (place-by-id) flow without
/// re-sending bytes on every placement.  Native Kitty does; iTerm2's
/// Kitty implementation does not (its cache is per-frame, so `a=p` for
/// an image not transmitted in the same frame silently no-ops).
///
/// Used by image-emit callers to pick between:
/// - `transmit_and_place` (`a=T`, ~400 KB base64 per scroll line) — safe
///   everywhere, slow under continuous scroll.
/// - `delete_placement` + `place_by_id` (`a=d` + `a=p`, ~100 bytes per
///   scroll line) — only safe on persistent-cache terminals.
///
/// Detection matches `detect()`'s native-Kitty path (KITTY_WINDOW_ID,
/// or TERM/TERM_PROGRAM with `kitty`).  We intentionally don't include
/// WezTerm / Ghostty here even though they have real image caches —
/// neither has been validated against the cached path yet, and a wrong
/// answer manifests as silently-missing figures.
pub fn has_persistent_image_cache() -> bool {
  if std::env::var_os("KITTY_WINDOW_ID").is_some() {
    return true;
  }
  if let Ok(prog) = std::env::var("TERM_PROGRAM")
    && prog.to_ascii_lowercase().contains("kitty")
  {
    return true;
  }
  if let Ok(term) = std::env::var("TERM")
    && term.contains("kitty")
  {
    return true;
  }
  false
}

/// Maximum *raw PNG* bytes we'll feed into a single `transmit_and_place`
/// call.  The normalizer downscales anything larger to fit.
///
/// The cap is iTerm2-shaped, not Kitty-shaped: iTerm2's protocol
/// implementation silently drops continuation chunks of an APC sequence
/// (see `transmit.rs`), so the whole payload has to fit in one
/// unchunked APC.  Empirically that's good for ~150–500 KB encoded
/// (≈300 KB raw PNG after base64 expansion + tmux DCS overhead).
///
/// Native Kitty's parser has no equivalent single-APC restriction, so
/// when we know we're on the unwrapped native path — `KITTY_WINDOW_ID`
/// set and **not** inside tmux — we raise the cap so larger paper
/// figures (the 850–930 KB cases noted in `images.rs`) render at their
/// source fidelity instead of being downscaled.
///
/// Override: `TREAD_DISABLE_KITTY_GRAPHICS` already forces the path off
/// entirely; this function deliberately doesn't take an override of its
/// own — if the larger cap ever misbehaves on a specific Kitty build,
/// the user can drop into tmux to fall back to the conservative cap.
pub fn transmit_byte_cap() -> usize {
  const DEFAULT_CAP: usize = 300_000;
  const NATIVE_KITTY_CAP: usize = 1_000_000;
  if in_tmux() {
    return DEFAULT_CAP;
  }
  if std::env::var_os("KITTY_WINDOW_ID").is_some() {
    return NATIVE_KITTY_CAP;
  }
  DEFAULT_CAP
}

/// Like `iterm2_at_least_3_5` but reads `TERM_PROGRAM_VERSION` instead
/// of `LC_TERMINAL_VERSION`.  iTerm2 sets both, but `LC_*` doesn't
/// propagate through tmux while `TERM_PROGRAM*` does (tmux's
/// `update-environment` includes it on most distros).
fn iterm2_term_program_at_least_3_5() -> bool {
  let Ok(v) = std::env::var("TERM_PROGRAM_VERSION") else { return true };
  let mut parts = v.split('.');
  let major: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
  let minor: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
  major > 3 || (major == 3 && minor >= 5)
}

/// Parse `LC_TERMINAL_VERSION` (typically `"3.5.0"` or similar) and
/// return whether it's at least 3.5.  Returns `true` if the version
/// string is missing or unparseable — better to over-enable on iTerm2
/// than to under-enable, since `TREAD_DISABLE_KITTY_GRAPHICS=1` is
/// always available as the manual escape hatch.
fn iterm2_at_least_3_5() -> bool {
  let Ok(v) = std::env::var("LC_TERMINAL_VERSION") else { return true };
  let mut parts = v.split('.');
  let major: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
  let minor: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
  major > 3 || (major == 3 && minor >= 5)
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Helper that snapshots and restores env vars across tests so they
  /// don't leak state.  Holds a process-wide mutex for the duration of
  /// its lifetime so concurrent tests can't stomp on each other's env
  /// vars — `std::env` is a shared, process-global table.
  use std::sync::{Mutex, MutexGuard};
  static ENV_MUTEX: Mutex<()> = Mutex::new(());

  struct EnvLock {
    snapshot: Vec<(&'static str, Option<String>)>,
    _guard: MutexGuard<'static, ()>,
  }
  impl EnvLock {
    fn new(keys: &[&'static str]) -> Self {
      let guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
      let snapshot = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
      for k in keys { unsafe { std::env::remove_var(k); } }
      Self { snapshot, _guard: guard }
    }
  }
  impl Drop for EnvLock {
    fn drop(&mut self) {
      for (k, v) in &self.snapshot {
        unsafe {
          match v {
            Some(val) => std::env::set_var(k, val),
            None => std::env::remove_var(k),
          }
        }
      }
    }
  }

  const ENV_KEYS: &[&str] = &[
    "TREAD_DISABLE_KITTY_GRAPHICS",
    "KITTY_WINDOW_ID",
    "WEZTERM_PANE",
    "WEZTERM_EXECUTABLE",
    "GHOSTTY_RESOURCES_DIR",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "TERM",
    "TERM_PROGRAM",
    "TMUX",
    "ZELLIJ",
    "ZELLIJ_SESSION_NAME",
  ];

  #[test]
  fn no_env_vars_means_unsupported() {
    let _lock = EnvLock::new(ENV_KEYS);
    assert_eq!(detect(), Capability::Unsupported);
  }

  #[test]
  fn kitty_window_id_means_supported() {
    let _lock = EnvLock::new(ENV_KEYS);
    unsafe { std::env::set_var("KITTY_WINDOW_ID", "1"); }
    assert_eq!(detect(), Capability::Supported);
  }

  #[test]
  fn override_forces_unsupported() {
    let _lock = EnvLock::new(ENV_KEYS);
    unsafe {
      std::env::set_var("KITTY_WINDOW_ID", "1");
      std::env::set_var("TREAD_DISABLE_KITTY_GRAPHICS", "1");
    }
    assert_eq!(detect(), Capability::Unsupported);
  }

  #[test]
  fn iterm2_old_version_unsupported() {
    let _lock = EnvLock::new(ENV_KEYS);
    unsafe {
      std::env::set_var("LC_TERMINAL", "iTerm2");
      std::env::set_var("LC_TERMINAL_VERSION", "3.4.16");
    }
    assert_eq!(detect(), Capability::Unsupported);
  }

  #[test]
  fn iterm2_new_version_supported() {
    let _lock = EnvLock::new(ENV_KEYS);
    unsafe {
      std::env::set_var("LC_TERMINAL", "iTerm2");
      std::env::set_var("LC_TERMINAL_VERSION", "3.5.0");
    }
    assert_eq!(detect(), Capability::Supported);
  }

  #[test]
  fn term_kitty_supported() {
    let _lock = EnvLock::new(ENV_KEYS);
    unsafe { std::env::set_var("TERM", "xterm-kitty"); }
    assert_eq!(detect(), Capability::Supported);
  }

  #[test]
  fn term_xterm_unsupported() {
    let _lock = EnvLock::new(ENV_KEYS);
    unsafe { std::env::set_var("TERM", "xterm-256color"); }
    assert_eq!(detect(), Capability::Unsupported);
  }

  #[test]
  fn transmit_cap_native_kitty_no_tmux_is_large() {
    let _lock = EnvLock::new(ENV_KEYS);
    unsafe { std::env::set_var("KITTY_WINDOW_ID", "1"); }
    assert_eq!(transmit_byte_cap(), 1_000_000);
  }

  #[test]
  fn transmit_cap_kitty_inside_tmux_falls_back() {
    let _lock = EnvLock::new(ENV_KEYS);
    unsafe {
      std::env::set_var("KITTY_WINDOW_ID", "1");
      std::env::set_var("TMUX", "/tmp/tmux-1000/default,1,0");
    }
    assert_eq!(transmit_byte_cap(), 300_000);
  }

  #[test]
  fn transmit_cap_other_terminal_is_default() {
    let _lock = EnvLock::new(ENV_KEYS);
    unsafe { std::env::set_var("LC_TERMINAL", "iTerm2"); }
    assert_eq!(transmit_byte_cap(), 300_000);
  }

  #[test]
  fn persistent_cache_kitty_window_id() {
    let _lock = EnvLock::new(ENV_KEYS);
    unsafe { std::env::set_var("KITTY_WINDOW_ID", "1"); }
    assert!(has_persistent_image_cache());
  }

  #[test]
  fn persistent_cache_iterm2_is_false() {
    let _lock = EnvLock::new(ENV_KEYS);
    unsafe {
      std::env::set_var("LC_TERMINAL", "iTerm2");
      std::env::set_var("LC_TERMINAL_VERSION", "3.5.0");
    }
    assert!(!has_persistent_image_cache());
  }

  #[test]
  fn persistent_cache_no_env_is_false() {
    let _lock = EnvLock::new(ENV_KEYS);
    assert!(!has_persistent_image_cache());
  }

  #[test]
  fn in_zellij_detects_session_name_var() {
    let _lock = EnvLock::new(ENV_KEYS);
    assert!(!in_zellij());
    unsafe { std::env::set_var("ZELLIJ_SESSION_NAME", "default"); }
    assert!(in_zellij());
  }

  #[test]
  fn in_zellij_detects_zellij_var() {
    let _lock = EnvLock::new(ENV_KEYS);
    unsafe { std::env::set_var("ZELLIJ", "0"); }
    assert!(in_zellij());
  }

  #[test]
  fn is_iterm2_via_lc_terminal() {
    let _lock = EnvLock::new(ENV_KEYS);
    assert!(!is_iterm2());
    unsafe { std::env::set_var("LC_TERMINAL", "iTerm2"); }
    assert!(is_iterm2());
  }

  #[test]
  fn is_iterm2_via_term_program() {
    let _lock = EnvLock::new(ENV_KEYS);
    unsafe { std::env::set_var("TERM_PROGRAM", "iTerm.app"); }
    assert!(is_iterm2());
  }

  #[test]
  fn is_iterm2_false_for_ghostty() {
    let _lock = EnvLock::new(ENV_KEYS);
    unsafe { std::env::set_var("TERM_PROGRAM", "ghostty"); }
    assert!(!is_iterm2());
  }

  // Parser-only coverage.  The full `tmux_passthrough_enabled()` would
  // require an actual tmux server to probe; the parsing logic is what
  // we own, so that's what we lock down.  The mapping here is the
  // public contract: only the literal `on`/`off` tokens count, anything
  // else (empty, garbage, non-utf8) collapses to `None`.

  #[test]
  fn parse_tmux_passthrough_on_token() {
    assert_eq!(parse_tmux_passthrough_output(b"on\n"), Some(true));
    assert_eq!(parse_tmux_passthrough_output(b"on"), Some(true));
  }

  #[test]
  fn parse_tmux_passthrough_off_token() {
    assert_eq!(parse_tmux_passthrough_output(b"off\n"), Some(false));
    assert_eq!(parse_tmux_passthrough_output(b"off"), Some(false));
  }

  #[test]
  fn parse_tmux_passthrough_empty_is_unknown() {
    assert_eq!(parse_tmux_passthrough_output(b""), None);
    assert_eq!(parse_tmux_passthrough_output(b"\n"), None);
  }

  #[test]
  fn parse_tmux_passthrough_unrecognized_is_unknown() {
    // Future-proofing: if tmux ever adds a third state (e.g. "all"
    // already exists on some versions), we treat it as unknown rather
    // than guessing.
    assert_eq!(parse_tmux_passthrough_output(b"all\n"), None);
    assert_eq!(parse_tmux_passthrough_output(b"yes\n"), None);
  }

  #[test]
  fn parse_tmux_passthrough_non_utf8_is_unknown() {
    assert_eq!(parse_tmux_passthrough_output(&[0xff, 0xfe, 0xfd]), None);
  }
}
