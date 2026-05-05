use std::process::Command;

use super::provider::TtsProvider;
use super::stream_buffer::StreamBuffer;

pub struct MacOsSayProvider {
  pub voice: String,
}

impl TtsProvider for MacOsSayProvider {
  fn stream(&self, text: &str) -> Result<StreamBuffer, String> {
    let tmp = std::env::temp_dir()
      .join(format!("hygg_tts_{}.wav", std::process::id()));

    let status = Command::new("say")
      .args([
        "-v",
        &self.voice,
        "--file-format=WAVE",
        "--data-format=LEI16@22050",
        "-o",
        tmp.to_str().unwrap_or("/tmp/hygg_tts.wav"),
        text,
      ])
      .status()
      .map_err(|e| format!("say: {e}"))?;

    if !status.success() {
      return Err("say: synthesis failed".to_string());
    }

    let bytes =
      std::fs::read(&tmp).map_err(|e| format!("say: read output: {e}"))?;
    let _ = std::fs::remove_file(&tmp);

    let (buf, writer) = StreamBuffer::new();
    writer.push(&bytes);
    writer.finish();
    Ok(buf)
  }
}
