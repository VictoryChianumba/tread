// Performance instrumentation sink.
//
// Activated by env: TREAD_BENCH=<path>.  When set, scoped Span timers
// and explicit emit() calls append JSON-Lines records to the file.
// When unset, all helpers are zero-cost beyond a single OnceLock load
// and an Option check.
//
// Records have shape: {"ts_us": <abs micros since unix epoch>,
//                      "event": "<name>",
//                      ... extra fields ...}
//
// Frame-latency samples are also accumulated in-memory so we can dump
// a percentile summary at shutdown without re-parsing the JSONL.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

struct State {
    file: Mutex<File>,
    samples: Mutex<Samples>,
}

#[derive(Default)]
struct Samples {
    event_to_frame_us: Vec<u32>,
    frame_us: Vec<u32>,
}

static STATE: OnceLock<Option<State>> = OnceLock::new();

pub fn init() {
    let _ = STATE.set(build());
}

fn build() -> Option<State> {
    let path = std::env::var("TREAD_BENCH").ok()?;
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    Some(State {
        file: Mutex::new(file),
        samples: Mutex::new(Samples::default()),
    })
}

fn state() -> Option<&'static State> {
    STATE.get().and_then(|s| s.as_ref())
}

pub fn is_active() -> bool {
    state().is_some()
}

fn now_us() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0)
}

fn write_line(line: &str) {
    if let Some(st) = state()
        && let Ok(mut f) = st.file.lock()
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Emit a single record with an integer `us` field.
pub fn emit_us(event: &str, us: u128) {
    if !is_active() {
        return;
    }
    let line = format!(
        "{{\"ts_us\":{},\"event\":\"{}\",\"us\":{}}}\n",
        now_us(),
        event,
        us
    );
    write_line(&line);
}

/// Emit a single record with arbitrary numeric fields.
pub fn emit_fields(event: &str, fields: &[(&str, i64)]) {
    if !is_active() {
        return;
    }
    let mut line = format!("{{\"ts_us\":{},\"event\":\"{}\"", now_us(), event);
    for (k, v) in fields {
        line.push_str(&format!(",\"{}\":{}", k, v));
    }
    line.push_str("}\n");
    write_line(&line);
}

/// Time a closure and emit `name` with `us = elapsed`.
pub fn time<T>(name: &str, f: impl FnOnce() -> T) -> T {
    if !is_active() {
        return f();
    }
    let start = Instant::now();
    let out = f();
    emit_us(name, start.elapsed().as_micros());
    out
}

/// RAII scoped timer: emits on drop.
pub struct Span {
    name: &'static str,
    start: Instant,
    armed: bool,
}

impl Span {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            start: Instant::now(),
            armed: is_active(),
        }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        if self.armed {
            emit_us(self.name, self.start.elapsed().as_micros());
        }
    }
}

/// Record an event→frame latency sample (per-event responsiveness).
pub fn record_event_to_frame(us: u32) {
    if let Some(st) = state()
        && let Ok(mut s) = st.samples.lock()
    {
        s.event_to_frame_us.push(us);
    }
    emit_fields("event_to_frame", &[("us", us as i64)]);
}

/// Frames over this threshold also emit a separate `slow_frame` event
/// so a future regression that pushes a frame past the TUI budget shows
/// up in the JSONL stream immediately, without waiting for the
/// shutdown summary.  16ms matches the 60 FPS budget; below this the
/// percentile summary catches drift.
const SLOW_FRAME_THRESHOLD_US: u32 = 16_000;

/// Record a single frame duration (terminal.draw + after_draw).
pub fn record_frame(us: u32) {
    if let Some(st) = state()
        && let Ok(mut s) = st.samples.lock()
    {
        s.frame_us.push(us);
    }
    emit_fields("frame", &[("us", us as i64)]);
    if us >= SLOW_FRAME_THRESHOLD_US {
        emit_fields("slow_frame", &[("us", us as i64)]);
    }
}

fn percentile(sorted: &[u32], p: f64) -> u32 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Dump aggregate percentile summary lines for collected samples.
/// Call once near shutdown.
pub fn dump_summary() {
    let Some(st) = state() else {
        return;
    };
    let Ok(samples) = st.samples.lock() else {
        return;
    };

    for (name, data) in [
        ("event_to_frame_summary", &samples.event_to_frame_us),
        ("frame_summary", &samples.frame_us),
    ] {
        if data.is_empty() {
            continue;
        }
        let mut sorted = data.clone();
        sorted.sort_unstable();
        let p50 = percentile(&sorted, 0.50);
        let p95 = percentile(&sorted, 0.95);
        let p99 = percentile(&sorted, 0.99);
        let max = *sorted.last().unwrap();
        let n = sorted.len() as i64;
        emit_fields(
            name,
            &[
                ("n", n),
                ("p50_us", p50 as i64),
                ("p95_us", p95 as i64),
                ("p99_us", p99 as i64),
                ("max_us", max as i64),
            ],
        );
    }
}
