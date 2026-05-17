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

    // reqwest doesn't fail on non-2xx, so we split error reporting
    // into a transport branch (network failure, no response) and a
    // status branch (got a response with a non-success code).
    let response = reqwest::blocking::Client::new()
      .post(&url)
      .header("xi-api-key", &self.api_key)
      .header(reqwest::header::CONTENT_TYPE, "application/json")
      .header(reqwest::header::ACCEPT, "audio/mpeg")
      .json(&body)
      .send()
      .map_err(|e| e.to_string())?;

    let status = response.status();
    if !status.is_success() {
      return Err(match status.as_u16() {
        401 => "Invalid API key".to_string(),
        429 => "Rate limited".to_string(),
        code => format!("HTTP {code}"),
      });
    }

    // reqwest::blocking::Response implements `std::io::Read`, so
    // we pass it straight to the streaming fill_buffer thread.
    let (buf, writer) = StreamBuffer::new();
    thread::spawn(move || fill_buffer(response, writer));
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
