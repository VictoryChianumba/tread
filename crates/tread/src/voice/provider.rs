use super::stream_buffer::StreamBuffer;

pub trait TtsProvider: Send {
  fn stream(&self, text: &str) -> Result<StreamBuffer, String>;
}
