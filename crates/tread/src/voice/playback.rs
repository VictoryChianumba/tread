use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::{VoicePlayingInfo, chunk_paragraphs, provider::TtsProvider};

pub enum PlaybackCommand {
  Start { text: String, doc_start_line: usize, doc_end_line: usize },
  Pause,
  Resume,
  Stop,
}

#[derive(Clone, PartialEq)]
pub enum PlaybackStatus {
  Idle,
  Loading,
  Playing,
  Paused,
}

pub struct PlaybackController {
  cmd_tx: Sender<PlaybackCommand>,
  pub status: Arc<Mutex<PlaybackStatus>>,
  pub voice_error: Arc<Mutex<Option<String>>>,
  pub playing_info: Arc<Mutex<Option<VoicePlayingInfo>>>,
}

impl PlaybackController {
  pub fn new(provider: Box<dyn TtsProvider>) -> Self {
    let (cmd_tx, cmd_rx) = mpsc::channel::<PlaybackCommand>();
    let status = Arc::new(Mutex::new(PlaybackStatus::Idle));
    let voice_error = Arc::new(Mutex::new(None::<String>));
    let playing_info = Arc::new(Mutex::new(None::<VoicePlayingInfo>));

    let status_clone = Arc::clone(&status);
    let error_clone = Arc::clone(&voice_error);
    let info_clone = Arc::clone(&playing_info);

    thread::spawn(move || {
      playback_loop(provider, cmd_rx, status_clone, error_clone, info_clone);
    });

    Self { cmd_tx, status, voice_error, playing_info }
  }

  pub fn start(
    &self,
    text: String,
    doc_start_line: usize,
    doc_end_line: usize,
  ) {
    let _ = self.cmd_tx.send(PlaybackCommand::Start {
      text,
      doc_start_line,
      doc_end_line,
    });
  }

  pub fn pause(&self) {
    let _ = self.cmd_tx.send(PlaybackCommand::Pause);
  }

  pub fn resume(&self) {
    let _ = self.cmd_tx.send(PlaybackCommand::Resume);
  }

  pub fn stop(&self) {
    let _ = self.cmd_tx.send(PlaybackCommand::Stop);
  }

  pub fn status(&self) -> PlaybackStatus {
    self.status.lock().unwrap_or_else(|e| e.into_inner()).clone()
  }

  /// Take the pending error message (clears it after reading).
  pub fn take_error(&self) -> Option<String> {
    self.voice_error.lock().unwrap_or_else(|e| e.into_inner()).take()
  }
}

/// When the controller is dropped (because the owning Editor closed), make
/// sure the playback loop receives a Stop *before* its channel is hung up.
/// Without this, audio queued in rodio keeps playing after the reader closes,
/// since the loop only checks for interrupts between chunks.
impl Drop for PlaybackController {
  fn drop(&mut self) {
    let _ = self.cmd_tx.send(PlaybackCommand::Stop);
  }
}

// ---------------------------------------------------------------------------
// Background playback loop
// ---------------------------------------------------------------------------

fn playback_loop(
  provider: Box<dyn TtsProvider>,
  cmd_rx: Receiver<PlaybackCommand>,
  status: Arc<Mutex<PlaybackStatus>>,
  error: Arc<Mutex<Option<String>>>,
  playing_info: Arc<Mutex<Option<VoicePlayingInfo>>>,
) {
  let (_stream, handle) = match rodio::OutputStream::try_default() {
    Ok(r) => r,
    Err(e) => {
      *error.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!("Audio init failed: {e}"));
      return;
    }
  };

  for cmd in cmd_rx.iter() {
    match cmd {
      // ------------------------------------------------------------------ //
      PlaybackCommand::Start { text, doc_start_line, doc_end_line } => {
        let sink = match rodio::Sink::try_new(&handle) {
          Ok(s) => s,
          Err(e) => {
            *error.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!("Audio sink error: {e}"));
            continue;
          }
        };
        let mut was_stopped = false;
        let mut chars_before: usize = 0;

        'chunks: for chunk_text in chunk_paragraphs(&text) {
          // Check for interrupt before starting the next synthesis request
          while let Ok(interrupt) = cmd_rx.try_recv() {
            match interrupt {
              PlaybackCommand::Stop => {
                was_stopped = true;
                break 'chunks;
              }
              PlaybackCommand::Pause => {
                sink.pause();
                *status.lock().unwrap_or_else(|e| e.into_inner()) = PlaybackStatus::Paused;
              }
              PlaybackCommand::Resume => {
                sink.play();
                *status.lock().unwrap_or_else(|e| e.into_inner()) = PlaybackStatus::Playing;
              }
              PlaybackCommand::Start { .. } => {
                was_stopped = true;
                break 'chunks;
              }
            }
          }

          let buf = match provider.stream(&chunk_text) {
            Err(msg) => {
              *error.lock().unwrap_or_else(|e| e.into_inner()) = Some(msg);
              was_stopped = true;
              break 'chunks;
            }
            Ok(b) => b,
          };

          // Wait for enough bytes for the decoder to parse the audio header
          const PRE_BUFFER: usize = 16 * 1024;
          loop {
            if buf.buffered_len() >= PRE_BUFFER || buf.is_done() {
              break;
            }
            if let Ok(interrupt) = cmd_rx.try_recv() {
              if matches!(interrupt, PlaybackCommand::Stop) {
                was_stopped = true;
                break 'chunks;
              }
            }
            thread::sleep(Duration::from_millis(20));
          }

          let chunk_len = chunk_text.len();
          match rodio::Decoder::new(buf) {
            Ok(source) => {
              *playing_info.lock().unwrap_or_else(|e| e.into_inner()) = Some(VoicePlayingInfo {
                doc_start_line,
                doc_end_line,
                started_at: Instant::now(),
                chars_before_chunk: chars_before,
              });
              *status.lock().unwrap_or_else(|e| e.into_inner()) = PlaybackStatus::Playing;
              sink.append(source);
            }
            Err(e) => {
              *error.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!("Audio decode error: {e}"));
              was_stopped = true;
              break 'chunks;
            }
          }

          // Poll until rodio finishes playing this chunk, responding to cmds
          while sink.len() > 0 {
            if let Ok(interrupt) = cmd_rx.try_recv() {
              match interrupt {
                PlaybackCommand::Stop => {
                  sink.stop();
                  was_stopped = true;
                  break 'chunks;
                }
                PlaybackCommand::Pause => {
                  sink.pause();
                  *status.lock().unwrap_or_else(|e| e.into_inner()) = PlaybackStatus::Paused;
                  'paused: for pcmd in cmd_rx.iter() {
                    match pcmd {
                      PlaybackCommand::Resume => {
                        sink.play();
                        *status.lock().unwrap_or_else(|e| e.into_inner()) = PlaybackStatus::Playing;
                        break 'paused;
                      }
                      PlaybackCommand::Stop => {
                        sink.stop();
                        was_stopped = true;
                        break 'chunks;
                      }
                      PlaybackCommand::Start { .. } => {
                        sink.stop();
                        was_stopped = true;
                        break 'chunks;
                      }
                      PlaybackCommand::Pause => {}
                    }
                  }
                }
                PlaybackCommand::Resume => {}
                PlaybackCommand::Start { .. } => {
                  sink.stop();
                  was_stopped = true;
                  break 'chunks;
                }
              }
            }
            thread::sleep(Duration::from_millis(50));
          }

          chars_before += chunk_len;
        }

        let _ = was_stopped;
        *playing_info.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *status.lock().unwrap_or_else(|e| e.into_inner()) = PlaybackStatus::Idle;
      }

      // ------------------------------------------------------------------ //
      PlaybackCommand::Stop => {
        *playing_info.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *status.lock().unwrap_or_else(|e| e.into_inner()) = PlaybackStatus::Idle;
      }
      PlaybackCommand::Pause | PlaybackCommand::Resume => {}
    }
  }
}
