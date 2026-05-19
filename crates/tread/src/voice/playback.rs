use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use super::{VoicePlayingInfo, chunk_paragraphs, provider::TtsProvider};

/// Lock acquisition that recovers from poisoning AND clears the
/// poison flag.  The pre-C7 pattern (`lock().unwrap_or_else(|e|
/// e.into_inner())`) recovers the guard but leaves the poison set,
/// so every subsequent `lock()` returns `Err(PoisonError)` again
/// and every call site has to repeat the dance.  Worse, the poison
/// signal — designed to surface "your data may be inconsistent
/// because a panic happened mid-write" — never propagates anywhere.
///
/// `clear_poison()` (stable since Rust 1.74) tells the Mutex the
/// caller has acknowledged the panic and is choosing to continue
/// with whatever value is in there.  For the voice subsystem that's
/// the right call: each field (status, playing_info, error,
/// session) is a small POD or `Option`, and the worst that can
/// happen is the next read sees a torn write that gets corrected
/// on the next sync_voice_status tick.
trait MutexExt<T> {
    fn lock_clearing_poison(&self) -> MutexGuard<'_, T>;
}

impl<T> MutexExt<T> for Mutex<T> {
    fn lock_clearing_poison(&self) -> MutexGuard<'_, T> {
        match self.lock() {
            Ok(guard) => guard,
            Err(poison) => {
                self.clear_poison();
                poison.into_inner()
            }
        }
    }
}

pub enum PlaybackCommand {
    Start {
        text: String,
        doc_start_line: usize,
        doc_end_line: usize,
        /// Monotonic session id assigned by `PlaybackController::start`.
        /// The playback loop publishes this to `PlaybackController::session`
        /// so a Reader can detect when another Reader preempted it.
        session_id: u64,
    },
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
    /// Monotonic counter feeding `start()`'s return value.  Each call
    /// produces a unique session id that the calling Reader records on
    /// its `voice_started_session` field — and checks each tick against
    /// `session()` to detect cross-tab preemption.
    session_counter: AtomicU64,
    /// Current owner's session id, or None when nothing is playing.
    /// Updated immediately by `start()` (before the playback thread sees
    /// the command) so a follow-up Reader's `start()` observation isn't
    /// racing the chunk loop.
    pub session: Arc<Mutex<Option<u64>>>,
}

impl PlaybackController {
    pub fn new(provider: Box<dyn TtsProvider>) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<PlaybackCommand>();
        let status = Arc::new(Mutex::new(PlaybackStatus::Idle));
        let voice_error = Arc::new(Mutex::new(None::<String>));
        let playing_info = Arc::new(Mutex::new(None::<VoicePlayingInfo>));
        let session = Arc::new(Mutex::new(None::<u64>));

        let status_clone = Arc::clone(&status);
        let error_clone = Arc::clone(&voice_error);
        let info_clone = Arc::clone(&playing_info);
        let session_clone = Arc::clone(&session);

        thread::spawn(move || {
            playback_loop(
                provider,
                cmd_rx,
                status_clone,
                error_clone,
                info_clone,
                session_clone,
            );
        });

        Self {
            cmd_tx,
            status,
            voice_error,
            playing_info,
            session_counter: AtomicU64::new(0),
            session,
        }
    }

    /// Start playback of `text`.  Returns the monotonic session id stamped
    /// on this request — callers should record it (e.g. on
    /// `Reader::voice_started_session`) and compare against
    /// `session_id()` each tick to detect when another caller preempted
    /// them.  The current playing-session field is updated synchronously
    /// here, before the command is even consumed by the playback thread,
    /// so a near-simultaneous follow-up `start()` from a different caller
    /// observes the new id without any race.
    pub fn start(&self, text: String, doc_start_line: usize, doc_end_line: usize) -> u64 {
        let id = self.session_counter.fetch_add(1, Ordering::SeqCst) + 1;
        *self.session.lock_clearing_poison() = Some(id);
        let _ = self.cmd_tx.send(PlaybackCommand::Start {
            text,
            doc_start_line,
            doc_end_line,
            session_id: id,
        });
        id
    }

    /// Currently-playing session id, or None when nothing is playing or
    /// the most recent session ended naturally.  Compare against your
    /// recorded `voice_started_session`: if `Some(other) != Some(yours)`,
    /// you were preempted by another caller and should exit reading mode
    /// quietly.
    pub fn session_id(&self) -> Option<u64> {
        *self.session.lock_clearing_poison()
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
        self.status.lock_clearing_poison().clone()
    }

    /// Take the pending error message (clears it after reading).
    pub fn take_error(&self) -> Option<String> {
        self.voice_error.lock_clearing_poison().take()
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

#[cfg(feature = "voice")]
fn playback_loop(
    provider: Box<dyn TtsProvider>,
    cmd_rx: Receiver<PlaybackCommand>,
    status: Arc<Mutex<PlaybackStatus>>,
    error: Arc<Mutex<Option<String>>>,
    playing_info: Arc<Mutex<Option<VoicePlayingInfo>>>,
    session: Arc<Mutex<Option<u64>>>,
) {
    // Lazy audio init.  Creating an `OutputStream` spins up a cpal
    // playback thread (a real OS thread that wakes for buffer fills)
    // and opens the default audio device — work that's pure overhead
    // for the common case where the user never invokes voice mode.
    // Build it on the first Start command instead, reuse for the
    // remaining lifetime of the playback thread.  If init fails we
    // surface the error and `continue` rather than tearing down the
    // whole loop, so a later Start with the audio system fixed can
    // still succeed.
    let mut output: Option<(rodio::OutputStream, rodio::OutputStreamHandle)> = None;

    for cmd in cmd_rx.iter() {
        match cmd {
            // ------------------------------------------------------------------ //
            PlaybackCommand::Start {
                text,
                doc_start_line,
                doc_end_line,
                session_id: my_session,
            } => {
                if output.is_none() {
                    match rodio::OutputStream::try_default() {
                        Ok(r) => output = Some(r),
                        Err(e) => {
                            *error.lock_clearing_poison() =
                                Some(format!("Audio init failed: {e}"));
                            continue;
                        }
                    }
                }
                let handle = &output.as_ref().expect("output just set").1;
                let sink = match rodio::Sink::try_new(handle) {
                    Ok(s) => s,
                    Err(e) => {
                        *error.lock_clearing_poison() =
                            Some(format!("Audio sink error: {e}"));
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
                                *status.lock_clearing_poison() =
                                    PlaybackStatus::Paused;
                            }
                            PlaybackCommand::Resume => {
                                sink.play();
                                *status.lock_clearing_poison() =
                                    PlaybackStatus::Playing;
                            }
                            PlaybackCommand::Start { .. } => {
                                was_stopped = true;
                                break 'chunks;
                            }
                        }
                    }

                    let buf = match provider.stream(&chunk_text) {
                        Err(msg) => {
                            *error.lock_clearing_poison() = Some(msg);
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
                        if let Ok(interrupt) = cmd_rx.try_recv()
                            && matches!(interrupt, PlaybackCommand::Stop) {
                                was_stopped = true;
                                break 'chunks;
                            }
                        thread::sleep(Duration::from_millis(20));
                    }

                    let chunk_len = chunk_text.len();
                    match rodio::Decoder::new(buf) {
                        Ok(source) => {
                            *playing_info.lock_clearing_poison() =
                                Some(VoicePlayingInfo {
                                    doc_start_line,
                                    doc_end_line,
                                    started_at: Instant::now(),
                                    chars_before_chunk: chars_before,
                                });
                            *status.lock_clearing_poison() =
                                PlaybackStatus::Playing;
                            sink.append(source);
                        }
                        Err(e) => {
                            *error.lock_clearing_poison() =
                                Some(format!("Audio decode error: {e}"));
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
                                    *status.lock_clearing_poison() =
                                        PlaybackStatus::Paused;
                                    'paused: for pcmd in cmd_rx.iter() {
                                        match pcmd {
                                            PlaybackCommand::Resume => {
                                                sink.play();
                                                *status.lock_clearing_poison() =
                                                    PlaybackStatus::Playing;
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
                *playing_info.lock_clearing_poison() = None;
                *status.lock_clearing_poison() = PlaybackStatus::Idle;
                // Clear our session id only if we still own it.  If a follow-up
                // `start()` arrived and bumped the session counter while we were
                // mid-chunk (we received a Start interrupt and broke out), the
                // `session` field already holds the new id — leaving it alone
                // means the new caller's `session_id()` query reflects reality.
                let mut s = session.lock_clearing_poison();
                if *s == Some(my_session) {
                    *s = None;
                }
            }

            // ------------------------------------------------------------------ //
            PlaybackCommand::Stop => {
                *playing_info.lock_clearing_poison() = None;
                *status.lock_clearing_poison() = PlaybackStatus::Idle;
                *session.lock_clearing_poison() = None;
            }
            PlaybackCommand::Pause | PlaybackCommand::Resume => {}
        }
    }
}

/// No-rodio playback loop used when the `voice` feature is off.
/// Accepts and drains commands so the channel doesn't fill, but never
/// touches an audio backend.  A Start surfaces a user-visible error
/// once per request rather than panicking or silently doing nothing.
#[cfg(not(feature = "voice"))]
fn playback_loop(
  _provider: Box<dyn TtsProvider>,
  cmd_rx: Receiver<PlaybackCommand>,
  _status: Arc<Mutex<PlaybackStatus>>,
  error: Arc<Mutex<Option<String>>>,
  _playing_info: Arc<Mutex<Option<VoicePlayingInfo>>>,
  _session: Arc<Mutex<Option<u64>>>,
) {
  for cmd in cmd_rx.iter() {
    if matches!(cmd, PlaybackCommand::Start { .. }) {
      *error.lock_clearing_poison() =
        Some("voice not compiled in (build with --features voice)".to_string());
    }
  }
}

#[cfg(test)]
mod tests {
    use super::super::provider::TtsProvider;
    use super::super::stream_buffer::StreamBuffer;
    use super::*;

    /// A no-op provider that always errors.  Lets us construct a real
    /// `PlaybackController` without depending on the audio device — the
    /// chunk loop runs, errors out at `provider.stream`, and falls
    /// through.  Sufficient for testing the synchronous parts of the
    /// API (session counter, session field updates).
    struct ErrProvider;
    impl TtsProvider for ErrProvider {
        fn stream(&self, _text: &str) -> Result<StreamBuffer, String> {
            Err("test mock".to_string())
        }
    }

    #[test]
    fn session_counter_is_monotonic() {
        let pc = PlaybackController::new(Box::new(ErrProvider));
        let id1 = pc.start("a".to_string(), 0, 0);
        let id2 = pc.start("b".to_string(), 0, 0);
        let id3 = pc.start("c".to_string(), 0, 0);
        assert!(id1 > 0);
        assert!(id2 > id1);
        assert!(id3 > id2);
    }

    #[test]
    fn start_synchronously_updates_session() {
        // start() writes to `session` BEFORE sending the command on the
        // channel, so a second caller observes the new id immediately —
        // no race with the playback thread's processing of the first
        // Start command.  This is what makes cross-tab preemption work
        // without subscribing to thread events.
        let pc = PlaybackController::new(Box::new(ErrProvider));
        let _id1 = pc.start("a".to_string(), 0, 0);
        let id2 = pc.start("b".to_string(), 0, 0);
        // Right after the second start, session is Some(id2).  (After the
        // playback thread eventually processes both Stops it clears, but
        // we read here before yielding.)
        assert_eq!(pc.session_id(), Some(id2));
    }
}
