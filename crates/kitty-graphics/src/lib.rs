//! Kitty graphics protocol support for the block-reader.
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
  if std::env::var("KITTY_WINDOW_ID").is_ok() {
    return Capability::Supported;
  }
  if std::env::var("WEZTERM_PANE").is_ok() || std::env::var("WEZTERM_EXECUTABLE").is_ok() {
    return Capability::Supported;
  }
  if std::env::var("GHOSTTY_RESOURCES_DIR").is_ok() {
    return Capability::Supported;
  }
  if let Ok(term) = std::env::var("LC_TERMINAL") {
    if term.eq_ignore_ascii_case("iTerm2") && iterm2_at_least_3_5() {
      return Capability::Supported;
    }
  }
  if let Ok(term) = std::env::var("TERM") {
    if term.contains("kitty") || term.contains("ghostty") {
      return Capability::Supported;
    }
  }
  Capability::Unsupported
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
  /// don't leak state.
  struct EnvLock {
    snapshot: Vec<(&'static str, Option<String>)>,
  }
  impl EnvLock {
    fn new(keys: &[&'static str]) -> Self {
      let snapshot = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
      for k in keys { unsafe { std::env::remove_var(k); } }
      Self { snapshot }
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
}
