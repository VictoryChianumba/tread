//! Image preparation worker.
//!
//! Image byte loads (file read, optional PDF rasterisation, PNG
//! re-encoding, downscale-to-budget) can take 50–200 ms for a complex
//! figure.  Doing that on the reader thread would freeze the UI mid-
//! scroll.  We push every load to a worker thread; placement reads
//! results off a channel each frame and keeps the previous frame's
//! placement stable until bytes arrive.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

use super::ImageState;
use super::png::resolve_png;

#[derive(Debug, Clone)]
pub(crate) struct ImageJob {
    pub(crate) kitty_id: u32,
    pub(crate) path: PathBuf,
}

impl ImageJob {
    pub(crate) fn resolve_png(kitty_id: u32, path: PathBuf) -> Self {
        Self { kitty_id, path }
    }
}

#[derive(Debug)]
pub(crate) struct ImageResult {
    kitty_id: u32,
    png_bytes: Result<Vec<u8>, String>,
}

pub(crate) struct ImageWorker {
    jobs: Sender<ImageJob>,
    results: Receiver<ImageResult>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ImageLoadContext {
    Inline,
    Preview,
}

pub(crate) fn run_image_job(job: ImageJob) -> ImageResult {
    ImageResult {
        kitty_id: job.kitty_id,
        png_bytes: resolve_png(&job.path).map_err(|err| err.to_string()),
    }
}

fn spawn_image_worker() -> Option<ImageWorker> {
    let (job_tx, job_rx) = mpsc::channel::<ImageJob>();
    let (result_tx, result_rx) = mpsc::channel::<ImageResult>();
    let spawn_result = thread::Builder::new()
        .name("tread-image-worker".to_string())
        .spawn(move || {
            while let Ok(job) = job_rx.recv() {
                let result = run_image_job(job);
                if result_tx.send(result).is_err() {
                    break;
                }
            }
        });

    if spawn_result.is_ok() {
        Some(ImageWorker {
            jobs: job_tx,
            results: result_rx,
        })
    } else {
        None
    }
}

fn apply_image_result(state: &mut ImageState, result: ImageResult) {
    state.pending_jobs.remove(&result.kitty_id);
    match result.png_bytes {
        Ok(bytes) => {
            state.negative_loads.remove(&result.kitty_id);
            state.bytes.insert(result.kitty_id, Some(bytes));
        }
        Err(_) => {
            state.bytes.insert(result.kitty_id, None);
        }
    }
}

pub(crate) fn poll_ready(state: &mut ImageState) -> bool {
    let Some(worker) = state.worker.as_ref() else {
        return false;
    };

    let mut disconnected = false;
    let mut results = Vec::new();
    loop {
        match worker.results.try_recv() {
            Ok(result) => results.push(result),
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                disconnected = true;
                break;
            }
        }
    }

    let changed = !results.is_empty();
    for result in results {
        apply_image_result(state, result);
    }
    if disconnected {
        state.worker = None;
        state.pending_jobs.clear();
    }
    changed
}

pub(crate) fn has_pending_jobs(state: &ImageState) -> bool {
    !state.pending_jobs.is_empty()
}

pub(crate) fn schedule_image_job(
    state: &mut ImageState,
    job: ImageJob,
    trace: bool,
    context: ImageLoadContext,
) {
    let id = job.kitty_id;
    if state.bytes.contains_key(&id) || state.pending_jobs.contains(&id) {
        return;
    }

    if state.worker.is_none() {
        state.worker = spawn_image_worker();
    }

    if let Some(worker) = &state.worker {
        match worker.jobs.send(job) {
            Ok(()) => {
                state.pending_jobs.insert(id);
                if trace {
                    match context {
                        ImageLoadContext::Inline => eprintln!("  schedule image job id={id}"),
                        ImageLoadContext::Preview => {
                            eprintln!("preview: schedule image job id={id}")
                        }
                    }
                }
            }
            Err(err) => {
                ensure_image_bytes(state, err.0, trace, context);
            }
        }
    } else {
        ensure_image_bytes(state, job, trace, context);
    }
}

pub(crate) fn ensure_image_bytes(
    state: &mut ImageState,
    job: ImageJob,
    trace: bool,
    context: ImageLoadContext,
) {
    if state.bytes.contains_key(&job.kitty_id) {
        return;
    }

    let id = job.kitty_id;
    let path = job.path.clone();
    let result = run_image_job(job);
    if trace {
        match &result.png_bytes {
            Ok(bytes) => match context {
                ImageLoadContext::Inline => {
                    eprintln!(
                        "  load id={} path={:?} ok ({} bytes)",
                        id,
                        path,
                        bytes.len()
                    )
                }
                ImageLoadContext::Preview => {
                    eprintln!("preview: load id={id} ok ({} bytes)", bytes.len())
                }
            },
            Err(err) => match context {
                ImageLoadContext::Inline => {
                    eprintln!("  load id={} path={:?} ERR: {}", id, path, err)
                }
                ImageLoadContext::Preview => eprintln!("preview: load id={id} ERR: {err}"),
            },
        }
    }
    apply_image_result(state, result);
}
