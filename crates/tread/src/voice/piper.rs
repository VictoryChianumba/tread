use std::io::Write;
use std::process::{Command, Stdio};

use super::provider::TtsProvider;
use super::stream_buffer::StreamBuffer;

pub struct PiperProvider {
  pub binary: String,
  pub model: String,
}

impl TtsProvider for PiperProvider {
  fn stream(&self, text: &str) -> Result<StreamBuffer, String> {
    let tmp = std::env::temp_dir()
      .join(format!("hygg_piper_{}.wav", std::process::id()));

    let mut child = Command::new(&self.binary)
      .args([
        "--model",
        &self.model,
        "--output_file",
        tmp.to_str().unwrap_or("/tmp/hygg_piper.wav"),
      ])
      .stdin(Stdio::piped())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .spawn()
      .map_err(|e| format!("piper: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
      stdin
        .write_all(text.as_bytes())
        .map_err(|e| format!("piper stdin: {e}"))?;
    }

    let status = child.wait().map_err(|e| format!("piper: {e}"))?;
    if !status.success() {
      return Err("piper: synthesis failed".to_string());
    }

    let bytes =
      std::fs::read(&tmp).map_err(|e| format!("piper: read output: {e}"))?;
    let _ = std::fs::remove_file(&tmp);

    let (buf, writer) = StreamBuffer::new();
    writer.push(&bytes);
    writer.finish();
    Ok(buf)
  }
}
