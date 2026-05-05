use std::io::Read;
use std::thread;

use super::provider::TtsProvider;
use super::stream_buffer::{StreamBuffer, StreamWriter};

pub struct ElevenLabsService {
  api_key: String,
  voice_id: String,
}

impl ElevenLabsService {
  pub fn new(api_key: String, voice_id: String) -> Self {
    Self { api_key, voice_id }
  }

  fn fetch_audio(&self, text: &str) -> Result<StreamBuffer, String> {
    let url = format!(
      "https://api.elevenlabs.io/v1/text-to-speech/{}/stream",
      self.voice_id
    );

    let body = serde_json::json!({
      "text": text,
      "model_id": "eleven_monolingual_v1",
      "voice_settings": {
        "stability": 0.5,
        "similarity_boost": 0.75
      }
    });

    let response = ureq::post(&url)
      .set("xi-api-key", &self.api_key)
      .set("Content-Type", "application/json")
      .set("Accept", "audio/mpeg")
      .send_json(body)
      .map_err(|e| match e {
        ureq::Error::Status(401, _) => "Invalid API key".to_string(),
        ureq::Error::Status(429, _) => "Rate limited".to_string(),
        ureq::Error::Status(code, _) => format!("HTTP {code}"),
        ureq::Error::Transport(t) => t.to_string(),
      })?;

    let (buf, writer) = StreamBuffer::new();
    thread::spawn(move || fill_buffer(response.into_reader(), writer));
    Ok(buf)
  }
}

impl TtsProvider for ElevenLabsService {
  fn stream(&self, text: &str) -> Result<StreamBuffer, String> {
    self.fetch_audio(text)
  }
}

fn fill_buffer(mut reader: impl Read, writer: StreamWriter) {
  let mut chunk = [0u8; 8192];
  loop {
    match reader.read(&mut chunk) {
      Ok(0) => break,
      Ok(n) => writer.push(&chunk[..n]),
      Err(_) => break,
    }
  }
  writer.finish();
}
