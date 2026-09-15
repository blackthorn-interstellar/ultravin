//! Diagnostic-only stage and Rayon-worker spans.
//!
//! Decode and cleanup IDs are independent phase ordinals. They align in the
//! diagnostic probe, which drops one managed result per decode in FIFO order;
//! they are not correlation IDs for arbitrary concurrent API use or `into_vec`.
//! Reset and snapshot only while the decoder pool is quiescent.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

const MAX_WORKERS: usize = 128;
const EVENTS_PER_BUCKET: usize = 8_192;
const SERIAL_BUCKET: usize = MAX_WORKERS;

#[derive(Debug, Clone, serde::Serialize)]
pub struct StageEvent {
    pub stage: &'static str,
    pub batch_id: usize,
    pub worker: Option<usize>,
    pub rows: usize,
    pub start_ns: u64,
    pub end_ns: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StageTrace {
    pub clock: &'static str,
    pub sample_every_batches: usize,
    pub events_per_bucket_limit: usize,
    pub dropped_events: usize,
    pub events: Vec<StageEvent>,
}

#[derive(Clone)]
struct RawEvent {
    stage: &'static str,
    batch_id: usize,
    worker: Option<usize>,
    rows: usize,
    start: Instant,
    end: Instant,
}

struct Recorder {
    origin: Mutex<Instant>,
    buckets: [Mutex<Vec<RawEvent>>; MAX_WORKERS + 1],
    dropped: AtomicUsize,
    decode_batches: AtomicUsize,
    cleanup_batches: AtomicUsize,
    sample_every: usize,
}

fn recorder() -> &'static Recorder {
    static RECORDER: OnceLock<Recorder> = OnceLock::new();
    RECORDER.get_or_init(|| Recorder {
        origin: Mutex::new(Instant::now()),
        buckets: std::array::from_fn(|_| Mutex::new(Vec::new())),
        dropped: AtomicUsize::new(0),
        decode_batches: AtomicUsize::new(0),
        cleanup_batches: AtomicUsize::new(0),
        sample_every: std::env::var("ULTRAVIN_STAGE_TRACE_EVERY")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(64),
    })
}

fn nanos(origin: Instant, instant: Instant) -> u64 {
    instant
        .saturating_duration_since(origin)
        .as_nanos()
        .min(u64::MAX as u128) as u64
}

fn record(event: RawEvent) {
    let index = event
        .worker
        .filter(|index| *index < MAX_WORKERS)
        .unwrap_or(SERIAL_BUCKET);
    let mut bucket = recorder().buckets[index]
        .lock()
        .expect("stage trace worker bucket poisoned");
    if bucket.len() == EVENTS_PER_BUCKET {
        recorder().dropped.fetch_add(1, Ordering::Relaxed);
    } else {
        bucket.push(event);
    }
}

fn sample(counter: &AtomicUsize) -> Option<usize> {
    let every = recorder().sample_every;
    if every == 0 {
        return None;
    }
    let batch_id = counter.fetch_add(1, Ordering::Relaxed);
    batch_id.is_multiple_of(every).then_some(batch_id)
}

pub(crate) fn sample_decode_batch() -> Option<usize> {
    sample(&recorder().decode_batches)
}

pub(crate) fn sample_cleanup_batch() -> Option<usize> {
    sample(&recorder().cleanup_batches)
}

pub(crate) struct WorkerSpan {
    stage: &'static str,
    batch_id: Option<usize>,
    worker: Option<usize>,
    rows: usize,
    start: Instant,
}

impl WorkerSpan {
    pub(crate) fn new(stage: &'static str, batch_id: Option<usize>) -> Self {
        Self {
            stage,
            batch_id,
            worker: rayon::current_thread_index(),
            rows: 0,
            start: Instant::now(),
        }
    }

    pub(crate) fn row(&mut self) {
        self.rows += 1;
    }

    pub(crate) fn rows(&mut self, rows: usize) {
        self.rows += rows;
    }
}

impl Drop for WorkerSpan {
    fn drop(&mut self) {
        if let Some(batch_id) = self.batch_id.filter(|_| self.rows > 0) {
            record(RawEvent {
                stage: self.stage,
                batch_id,
                worker: self.worker,
                rows: self.rows,
                start: self.start,
                end: Instant::now(),
            });
        }
    }
}

pub(crate) fn serial<T>(
    stage: &'static str,
    batch_id: Option<usize>,
    rows: usize,
    run: impl FnOnce() -> T,
) -> T {
    let Some(batch_id) = batch_id else {
        return run();
    };
    let start = Instant::now();
    let value = run();
    record(RawEvent {
        stage,
        batch_id,
        worker: rayon::current_thread_index(),
        rows,
        start,
        end: Instant::now(),
    });
    value
}

/// Clear all events and start a new monotonic trace epoch.
pub fn reset() {
    let recorder = recorder();
    *recorder.origin.lock().expect("stage trace origin poisoned") = Instant::now();
    for bucket in &recorder.buckets {
        bucket
            .lock()
            .expect("stage trace worker bucket poisoned")
            .clear();
    }
    recorder.dropped.store(0, Ordering::Relaxed);
    recorder.decode_batches.store(0, Ordering::Relaxed);
    recorder.cleanup_batches.store(0, Ordering::Relaxed);
}

/// Copy the bounded event stream for diagnostic serialization.
pub fn snapshot() -> StageTrace {
    let recorder = recorder();
    let origin = *recorder.origin.lock().expect("stage trace origin poisoned");
    let mut events: Vec<_> = recorder
        .buckets
        .iter()
        .flat_map(|bucket| {
            bucket
                .lock()
                .expect("stage trace worker bucket poisoned")
                .clone()
        })
        .map(|event| StageEvent {
            stage: event.stage,
            batch_id: event.batch_id,
            worker: event.worker,
            rows: event.rows,
            start_ns: nanos(origin, event.start),
            end_ns: nanos(origin, event.end),
        })
        .collect();
    events.sort_unstable_by_key(|event| event.start_ns);
    StageTrace {
        clock: "monotonic nanoseconds since reset",
        sample_every_batches: recorder.sample_every,
        events_per_bucket_limit: EVENTS_PER_BUCKET,
        dropped_events: recorder.dropped.load(Ordering::Relaxed),
        events,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_sampled_spans_and_skips_unsampled_spans() {
        const SERIAL_STAGE: &str = "stage_trace_test_serial_unique";
        const WORKER_STAGE: &str = "stage_trace_test_worker_unique";
        const SKIPPED_STAGE: &str = "stage_trace_test_skipped_unique";
        serial(SERIAL_STAGE, Some(usize::MAX), 7, || {});
        let mut span = WorkerSpan::new(WORKER_STAGE, Some(usize::MAX));
        span.row();
        span.row();
        drop(span);
        serial(SKIPPED_STAGE, None, 100, || {});
        let trace = snapshot();
        let own: Vec<_> = trace
            .events
            .iter()
            .filter(|event| matches!(event.stage, SERIAL_STAGE | WORKER_STAGE | SKIPPED_STAGE))
            .collect();
        assert_eq!(own.len(), 2);
        assert_eq!(own.iter().map(|event| event.rows).sum::<usize>(), 9);
        assert!(own.iter().all(|event| event.end_ns >= event.start_ns));
        assert!(own.iter().all(|event| event.stage != SKIPPED_STAGE));
    }
}
