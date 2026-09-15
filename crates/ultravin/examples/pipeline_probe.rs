//! Diagnostic for overlapping managed decode and cleanup on one decoder pool.
//!
//! This does not change the public API. The pipeline hands complete, ordered
//! `BatchResults` from a producer to the consuming thread through a rendezvous
//! channel. While the consumer destroys batch N, the producer may decode batch
//! N+1. Both operations use ultravin's process decoder pool; this example does
//! not initialize or submit work to Rayon's global pool.
//!
//! `pipeline_probe CORPUS SECONDS WORKERS LIVE_ROW_BUDGET MODE`
//!
//! MODE is `sequential_budget`, `sequential_batch`, or `pipeline`. The pipeline
//! uses half the row budget per batch because at most two batches can be live.
//! `sequential_budget` uses the full budget in one batch for an equal-memory
//! comparison. `sequential_batch` uses the pipeline batch size as a control.

#[path = "support/counters.rs"]
mod counters;

use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const FROZEN_NOW_MICROS: i64 = 1_788_220_800_000_000; // 2026-09-01T00:00:00Z

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    SequentialBudget,
    SequentialBatch,
    Pipeline,
}

impl Mode {
    fn parse(value: &str) -> Self {
        match value {
            "sequential_budget" => Self::SequentialBudget,
            "sequential_batch" => Self::SequentialBatch,
            "pipeline" => Self::Pipeline,
            _ => panic!("mode must be sequential_budget, sequential_batch, or pipeline"),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::SequentialBudget => "sequential_budget",
            Self::SequentialBatch => "sequential_batch",
            Self::Pipeline => "pipeline",
        }
    }
}

#[derive(Default)]
struct Timings {
    decode: Duration,
    drop_results: Duration,
    pass: Duration,
    maximum_observed_live_rows: usize,
    completed_rows: usize,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let corpus_path = args.next().expect("corpus path is required");
    let seconds = parse_positive::<f64>(args.next(), "seconds");
    assert!(seconds.is_finite(), "seconds must be finite");
    let workers = parse_positive::<usize>(args.next(), "workers");
    let live_row_budget = parse_positive::<usize>(args.next(), "live-row-budget");
    let mode = Mode::parse(&args.next().expect("mode is required"));
    assert!(args.next().is_none(), "unexpected extra argument");
    assert_eq!(
        std::env::var("RAYON_NUM_THREADS").ok().as_deref(),
        Some(workers.to_string().as_str()),
        "RAYON_NUM_THREADS must equal the workers argument"
    );
    if matches!(mode, Mode::Pipeline | Mode::SequentialBatch) {
        assert!(
            live_row_budget >= 2,
            "live-row-budget must be at least two for this mode"
        );
    }

    let corpus = std::fs::read_to_string(&corpus_path).expect("read corpus");
    let inputs: Vec<String> = corpus
        .lines()
        .filter(|line| line.len() == 17)
        .map(str::to_owned)
        .collect();
    drop(corpus);
    assert!(!inputs.is_empty(), "empty corpus: {corpus_path}");

    let pipeline_batch_rows = (live_row_budget / 2).max(1);
    let batch_rows = match mode {
        Mode::SequentialBudget => live_row_budget,
        Mode::SequentialBatch | Mode::Pipeline => pipeline_batch_rows,
    };
    let maximum_live_batches = usize::from(mode == Mode::Pipeline) + 1;
    let maximum_live_rows = batch_rows.saturating_mul(maximum_live_batches);
    assert!(
        maximum_live_rows <= live_row_budget,
        "selected mode exceeds live-row-budget"
    );

    // Warm the exact mode over every input. Loading and warmup precede the
    // marker and do not contribute to reported throughput or phase durations.
    run_pass(mode, &inputs, batch_rows);
    eprintln!("pipeline probe: warmup complete; starting timed whole passes");

    #[cfg(feature = "stage-trace")]
    ultravin::stage_trace::reset();

    let budget = Duration::from_secs_f64(seconds);
    let cpu_started = process_cpu_seconds();
    let counters_started = counters::snapshot();
    let started = Instant::now();
    let mut timings = Timings::default();
    let mut rows = 0_u64;
    let mut whole_passes = 0_u64;
    while started.elapsed() < budget {
        let pass = run_pass(mode, &inputs, batch_rows);
        timings.decode += pass.decode;
        timings.drop_results += pass.drop_results;
        timings.pass += pass.pass;
        timings.maximum_observed_live_rows = timings
            .maximum_observed_live_rows
            .max(pass.maximum_observed_live_rows);
        assert_eq!(pass.completed_rows, inputs.len(), "incomplete input pass");
        rows += inputs.len() as u64;
        whole_passes += 1;
    }
    let elapsed = started.elapsed();
    let counter_usage = counters::elapsed(counters_started, counters::snapshot());
    let cpu_finished = process_cpu_seconds();
    let process_user_cpu_seconds = cpu_finished.0 - cpu_started.0;
    let process_system_cpu_seconds = cpu_finished.1 - cpu_started.1;
    let rate = rows as f64 / elapsed.as_secs_f64();
    #[cfg(feature = "stage-trace")]
    let stage_trace = Some(ultravin::stage_trace::snapshot());
    #[cfg(not(feature = "stage-trace"))]
    let stage_trace: Option<serde_json::Value> = None;

    eprintln!(
        "{}: {} VINs in {:.6}s = {:.0} VIN/s ({} workers)",
        mode.name(),
        rows,
        elapsed.as_secs_f64(),
        rate,
        workers
    );
    println!(
        "{}",
        serde_json::json!({
            "benchmark": "pipeline_probe",
            "mode": mode.name(),
            "semantics": "ordered full managed results; synchronous cleanup included",
            "phase_time_basis": if mode == Mode::Pipeline {
                "decode producer time and consumer drop time overlap; both are summed separately"
            } else {
                "decode and drop wall durations are accumulated serially"
            },
            "decoder_pool": "one process-owned ultravin pool shared by decode and cleanup",
            "queue_capacity_batches": if mode == Mode::Pipeline { Some(0) } else { None },
            "backpressure": if mode == Mode::Pipeline {
                Some("rendezvous handoff prevents producer from starting N+2 before consumer receives N+1")
            } else {
                None
            },
            "frozen_now_micros": FROZEN_NOW_MICROS,
            "workers": workers,
            "corpus_rows": inputs.len(),
            "live_row_budget": live_row_budget,
            "batch_rows": batch_rows,
            "maximum_live_batches": maximum_live_batches,
            "maximum_live_rows_limit": maximum_live_rows,
            "maximum_observed_live_rows": timings.maximum_observed_live_rows,
            "memory_comparison": "bounds live or under-construction batch rows; transient decode, container, worklist, allocator, resident input, and database memory are not asserted equal",
            "whole_passes": whole_passes,
            "rows": rows,
            "elapsed_seconds": elapsed.as_secs_f64(),
            "process_counters": counter_usage,
            "process_user_cpu_seconds": process_user_cpu_seconds,
            "process_system_cpu_seconds": process_system_cpu_seconds,
            "average_busy_cores": (process_user_cpu_seconds + process_system_cpu_seconds) / elapsed.as_secs_f64(),
            "actual_rows_per_second": rate,
            "phase_seconds": {
                "decode": timings.decode.as_secs_f64(),
                "drop_results": timings.drop_results.as_secs_f64(),
                "whole_pass_wall": timings.pass.as_secs_f64(),
            },
            "stage_trace": stage_trace,
        })
    );
}

#[cfg(unix)]
fn process_cpu_seconds() -> (f64, f64) {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage initializes the pointed-to rusage on success.
    let status = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    assert_eq!(status, 0, "getrusage failed");
    // SAFETY: the successful call above initialized the structure.
    let usage = unsafe { usage.assume_init() };
    let seconds = |time: libc::timeval| time.tv_sec as f64 + time.tv_usec as f64 / 1_000_000.0;
    (seconds(usage.ru_utime), seconds(usage.ru_stime))
}

#[cfg(not(unix))]
fn process_cpu_seconds() -> (f64, f64) {
    (0.0, 0.0)
}

fn parse_positive<T>(value: Option<String>, name: &str) -> T
where
    T: std::str::FromStr + PartialOrd + Default,
    T::Err: std::fmt::Debug,
{
    let parsed = value
        .unwrap_or_else(|| panic!("{name} is required"))
        .parse::<T>()
        .unwrap_or_else(|error| panic!("invalid {name}: {error:?}"));
    assert!(parsed > T::default(), "{name} must be positive");
    parsed
}

fn run_pass(mode: Mode, inputs: &[String], batch_rows: usize) -> Timings {
    match mode {
        Mode::SequentialBudget | Mode::SequentialBatch => sequential_pass(inputs, batch_rows),
        Mode::Pipeline => pipeline_pass(inputs, batch_rows),
    }
}

fn sequential_pass(inputs: &[String], batch_rows: usize) -> Timings {
    let pass_started = Instant::now();
    let mut timings = Timings::default();
    for chunk in inputs.chunks(batch_rows) {
        timings.maximum_observed_live_rows = timings.maximum_observed_live_rows.max(chunk.len());
        let decode_started = Instant::now();
        let results = ultravin::decode_batch_managed_at(chunk, None, FROZEN_NOW_MICROS);
        timings.decode += decode_started.elapsed();
        black_box(&results);

        let drop_started = Instant::now();
        drop(results);
        timings.drop_results += drop_started.elapsed();
        timings.completed_rows += chunk.len();
    }
    timings.pass = pass_started.elapsed();
    timings
}

fn pipeline_pass(inputs: &[String], batch_rows: usize) -> Timings {
    let pass_started = Instant::now();
    let (sender, receiver) = sync_channel(0);
    let live_rows = Arc::new(AtomicUsize::new(0));
    let maximum_live_rows = Arc::new(AtomicUsize::new(0));
    let (decode, drop_results, completed_rows) = std::thread::scope(|scope| {
        // Drop the receiver before scoped threads are joined if the consumer panics,
        // waking a producer blocked in the zero-capacity send.
        let receiver = receiver;
        let producer_live_rows = Arc::clone(&live_rows);
        let producer_maximum_live_rows = Arc::clone(&maximum_live_rows);
        let producer = scope.spawn(move || {
            let mut decode = Duration::ZERO;
            for (index, chunk) in inputs.chunks(batch_rows).enumerate() {
                let now_live =
                    producer_live_rows.fetch_add(chunk.len(), Ordering::Relaxed) + chunk.len();
                update_maximum(&producer_maximum_live_rows, now_live);
                let decode_started = Instant::now();
                let results = ultravin::decode_batch_managed_at(chunk, None, FROZEN_NOW_MICROS);
                decode += decode_started.elapsed();
                if sender.send((index, chunk.len(), results)).is_err() {
                    break;
                }
            }
            decode
        });

        let mut expected_index = 0_usize;
        let mut drop_results = Duration::ZERO;
        let mut completed_rows = 0_usize;
        while let Ok((index, chunk_rows, results)) = receiver.recv() {
            assert_eq!(index, expected_index, "pipeline result order changed");
            assert!(!results.is_empty(), "pipeline returned an empty batch");
            black_box(&results);

            let drop_started = Instant::now();
            drop(results);
            drop_results += drop_started.elapsed();
            live_rows.fetch_sub(chunk_rows, Ordering::Relaxed);
            completed_rows += chunk_rows;
            expected_index += 1;
        }
        let decode = producer.join().expect("pipeline producer panicked");
        assert_eq!(
            expected_index,
            inputs.len().div_ceil(batch_rows),
            "pipeline producer stopped before the complete input pass"
        );
        (decode, drop_results, completed_rows)
    });

    Timings {
        decode,
        drop_results,
        pass: pass_started.elapsed(),
        maximum_observed_live_rows: maximum_live_rows.load(Ordering::Relaxed),
        completed_rows,
    }
}

fn update_maximum(maximum: &AtomicUsize, candidate: usize) {
    maximum.fetch_max(candidate, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_completes_ordered_batches_and_tail_within_row_bound() {
        let inputs = vec!["1HGCM82633A004352".to_owned(); 5];

        let timings = pipeline_pass(&inputs, 2);

        assert_eq!(timings.completed_rows, inputs.len());
        assert!(timings.maximum_observed_live_rows >= 2);
        assert!(timings.maximum_observed_live_rows <= 4);
    }
}
