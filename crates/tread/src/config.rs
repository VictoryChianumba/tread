//! Block-reader's own persistent settings.  Tiny scope today — just the
//! theme override — but designed to grow without churn.
//!
//! ## Theme resolution model
//!
//! Block-reader has three theme sources, in priority order:
//!
//! 1. **`theme_override` set in our own config** (`~/.config/trench/block_reader.json`).
//!    Set via `:set theme=<name>`.  Highest priority.
//! 2. **Trench's theme** read from `~/.config/trench/config.json`.
//!    When `theme_override` is `None`, follow whatever the trench feed UI
//!    is using.  Equivalent to `:set theme=trench`.
//! 3. **Built-in default** (`ThemeId::Dark`) when neither file is present.
//!
//! Reading trench's config is done via `serde_json::Value` rather than
//! importing trench's `Config` struct — that would create a circular
//! crate dependency.  The schema's `theme` field is a kebab-case string
//! that `ThemeId::from_id` accepts.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use ui_theme::{Theme, ThemeId};

#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct ReaderConfig {
  /// `None` = follow trench's theme.  `Some(id)` = use this theme
  /// regardless of trench (id is a `ThemeId` kebab-case label).
  #[serde(default)]
  pub theme_override: Option<String>,
  /// Voice / TTS settings.  `None` (or missing in JSON) means use
  /// defaults (macOS `say` with the "Samantha" voice).  The
  /// `ELEVENLABS_API_KEY` is **environment-only** and never lands here.
  #[serde(default)]
  pub voice: Option<VoiceConfig>,
}

/// Voice / TTS configuration, persisted alongside the reader's other
/// settings.  All fields default to the empty string / 1.0 so the JSON
/// only needs to specify what differs from the auto-fallback chain
/// (ElevenLabs if env API key set → macOS `say`).
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct VoiceConfig {
  /// ElevenLabs voice id; required only when using the ElevenLabs
  /// provider.  Look up at <https://elevenlabs.io/app/voice-library>.
  #[serde(default)]
  pub voice_id: String,
  /// Force a specific provider — "elevenlabs", "say", "piper".  Empty
  /// string lets `make_provider` pick: ElevenLabs if API key in env,
  /// else `say`.
  #[serde(default)]
  pub tts_provider: String,
  /// macOS `say` voice name (run `say -v ?` to list).  Empty defaults
  /// to "Samantha".
  #[serde(default)]
  pub say_voice: String,
  /// Path to the Piper binary for offline TTS.
  #[serde(default)]
  pub piper_binary: String,
  /// Path to the Piper voice model file.
  #[serde(default)]
  pub piper_model: String,
  /// Playback speed multiplier (1.0 = normal).  Currently only honoured
  /// on a per-provider basis where supported; v2 will plumb through
  /// uniformly.
  #[serde(default = "default_speed")]
  pub playback_speed: f32,
}

fn default_speed() -> f32 { 1.0 }

fn config_path() -> Option<PathBuf> {
  dirs::config_dir().map(|p| p.join("trench").join("block_reader.json"))
}

pub fn load() -> ReaderConfig {
  let Some(p) = config_path() else { return ReaderConfig::default() };
  let Ok(data) = std::fs::read_to_string(&p) else { return ReaderConfig::default() };
  serde_json::from_str(&data).unwrap_or_default()
}

pub fn save(c: &ReaderConfig) {
  let Some(p) = config_path() else { return };
  if let Some(dir) = p.parent() {
    let _ = std::fs::create_dir_all(dir);
  }
  if let Ok(data) = serde_json::to_string(c) {
    let _ = std::fs::write(&p, data);
  }
}

/// Read trench's theme selection from `~/.config/trench/config.json`.
/// Returns `None` if the file is missing, malformed, or the theme name
/// doesn't resolve to a known `ThemeId`.
fn trench_theme_id() -> Option<ThemeId> {
  let p = dirs::config_dir()?.join("trench").join("config.json");
  let data = std::fs::read_to_string(&p).ok()?;
  let v: serde_json::Value = serde_json::from_str(&data).ok()?;
  let theme_str = v.get("theme")?.as_str()?;
  ThemeId::from_id(theme_str)
}

/// Resolve the actual `Theme` to use at startup.  Honours the priority
/// chain documented at the top of this module.
pub fn resolve_theme() -> Theme {
  let cfg = load();
  if let Some(id) = cfg.theme_override.as_deref() {
    if let Some(tid) = ThemeId::from_id(id) {
      return tid.theme();
    }
  }
  if let Some(tid) = trench_theme_id() {
    return tid.theme();
  }
  ThemeId::Dark.theme()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn config_round_trip() {
    let c = ReaderConfig { theme_override: Some("light".to_string()), voice: None };
    let json = serde_json::to_string(&c).unwrap();
    let back: ReaderConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(back.theme_override.as_deref(), Some("light"));
  }

  #[test]
  fn empty_json_loads_default() {
    let back: ReaderConfig = serde_json::from_str("{}").unwrap();
    assert!(back.theme_override.is_none());
  }

  #[test]
  fn missing_field_defaults() {
    // Forwards-compat: extra fields ignored, missing field defaults.
    let back: ReaderConfig = serde_json::from_str(r#"{"future_field": 42}"#).unwrap();
    assert!(back.theme_override.is_none());
    assert!(back.voice.is_none());
  }

  #[test]
  fn voice_round_trip() {
    let c = ReaderConfig {
      theme_override: None,
      voice: Some(VoiceConfig {
        voice_id: "abc123".to_string(),
        tts_provider: "elevenlabs".to_string(),
        say_voice: "Samantha".to_string(),
        piper_binary: String::new(),
        piper_model: String::new(),
        playback_speed: 1.25,
      }),
    };
    let json = serde_json::to_string(&c).unwrap();
    let back: ReaderConfig = serde_json::from_str(&json).unwrap();
    let v = back.voice.as_ref().unwrap();
    assert_eq!(v.voice_id, "abc123");
    assert_eq!(v.tts_provider, "elevenlabs");
    assert!((v.playback_speed - 1.25).abs() < 1e-6);
  }

  #[test]
  fn voice_speed_defaults_to_one() {
    let json = r#"{"voice": {"voice_id": "x"}}"#;
    let back: ReaderConfig = serde_json::from_str(json).unwrap();
    let v = back.voice.unwrap();
    assert!((v.playback_speed - 1.0).abs() < 1e-6);
  }
}
