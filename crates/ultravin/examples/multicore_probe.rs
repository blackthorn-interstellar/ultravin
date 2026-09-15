//! Diagnostic comparison of fixed managed batches and independently consumed chunks.
//!
//! This is not an API equivalence benchmark. `managed` calls the public batch API,
//! which restores input order and returns one globally ordered batch. `streaming`
//! partitions a pass into independent chunks, fully materializes each chunk's full
//! results, black-boxes them, and drops them on the worker that decoded them. It
//! therefore measures a streaming consumer that does not return a global batch.
//!
//! Run with `RAYON_NUM_THREADS` equal to the workers argument:
//! `multicore_probe CORPUS SECONDS WORKERS LIVE_ROW_BUDGET managed|streaming`

use std::hint::black_box;
use std::time::{Duration, Instant};

use rayon::prelude::*;

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const FROZEN_NOW_MICROS: i64 = 1_788_220_800_000_000; // 2026-09-01T00:00:00Z

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Managed,
    Streaming,
}

impl Mode {
    fn parse(value: &str) -> Self {
        match value {
            "managed" => Self::Managed,
            "streaming" => Self::Streaming,
            _ => panic!("mode must be managed or streaming"),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Managed => "managed",
            Self::Streaming => "streaming",
        }
    }
}

#[derive(Default)]
struct Timings {
    decode: Duration,
    drop_results: Duration,
    pass: Duration,
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

    let corpus = std::fs::read_to_string(&corpus_path).expect("read corpus");
    let inputs: Vec<String> = corpus
        .lines()
        .filter(|line| line.len() == 17)
        .map(str::to_owned)
        .collect();
    drop(corpus);
    assert!(!inputs.is_empty(), "empty corpus: {corpus_path}");

    let local_rows = (live_row_budget / workers).max(1);
    if mode == Mode::Streaming {
        assert!(
            live_row_budget >= workers,
            "streaming live-row-budget must be at least workers"
        );
        let chunks = inputs.len().div_ceil(local_rows);
        assert!(
            chunks >= workers * 8,
            "streaming requires at least workers * 8 chunks for load balance"
        );
    }

    // Warm the exact selected workload with every unique input. Nothing before
    // the marker contributes to the reported phase or throughput measurements.
    run_pass(mode, &inputs, workers, live_row_budget, local_rows);
    eprintln!("multicore probe: warmup complete; starting timed whole passes");

    let budget = Duration::from_secs_f64(seconds);
    let started = Instant::now();
    let mut rows = 0_u64;
    let mut passes = 0_u64;
    let mut timings = Timings::default();
    while started.elapsed() < budget {
        let pass = run_pass(mode, &inputs, workers, live_row_budget, local_rows);
        timings.decode += pass.decode;
        timings.drop_results += pass.drop_results;
        timings.pass += pass.pass;
        rows += inputs.len() as u64;
        passes += 1;
    }
    let elapsed = started.elapsed();
    let rate = rows as f64 / elapsed.as_secs_f64();
    let semantics = match mode {
        Mode::Managed => "ordered public managed batch API; one returned batch per live-row budget",
        Mode::Streaming => {
            "independent streaming chunks; full results consumed and dropped per worker; no global batch return"
        }
    };
    let phase_basis = match mode {
        Mode::Managed => "wall time accumulated across serial API calls",
        Mode::Streaming => "worker time summed across concurrently executing chunks",
    };

    eprintln!(
        "{}: {} VINs in {:.6}s = {:.0} VIN/s ({} core(s))",
        mode.name(),
        rows,
        elapsed.as_secs_f64(),
        rate,
        workers
    );
    println!(
        "{}",
        serde_json::json!({
            "benchmark": "multicore_probe",
            "mode": mode.name(),
            "semantics": semantics,
            "phase_time_basis": phase_basis,
            "frozen_now_micros": FROZEN_NOW_MICROS,
            "workers": workers,
            "live_row_budget": live_row_budget,
            "local_rows": if mode == Mode::Streaming { Some(local_rows) } else { None },
            "maximum_active_streaming_rows": if mode == Mode::Streaming {
                Some(local_rows.saturating_mul(workers))
            } else {
                None
            },
            "corpus_rows": inputs.len(),
            "whole_passes": passes,
            "rows": rows,
            "elapsed_seconds": elapsed.as_secs_f64(),
            "actual_rows_per_second": rate,
            "phase_seconds": {
                "decode_and_local_sort": timings.decode.as_secs_f64(),
                "drop_results": timings.drop_results.as_secs_f64(),
                "whole_pass_wall": timings.pass.as_secs_f64(),
            },
        })
    );
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

fn run_pass(
    mode: Mode,
    inputs: &[String],
    workers: usize,
    live_row_budget: usize,
    local_rows: usize,
) -> Timings {
    match mode {
        Mode::Managed => managed_pass(inputs, live_row_budget),
        Mode::Streaming => streaming_pass(inputs, workers, local_rows),
    }
}

fn managed_pass(inputs: &[String], live_row_budget: usize) -> Timings {
    let pass_started = Instant::now();
    let mut timings = Timings::default();
    for chunk in inputs.chunks(live_row_budget) {
        let decode_started = Instant::now();
        let results = ultravin::decode_batch_managed_at(chunk, None, FROZEN_NOW_MICROS);
        timings.decode += decode_started.elapsed();
        black_box(&results);

        let drop_started = Instant::now();
        drop(results);
        timings.drop_results += drop_started.elapsed();
    }
    timings.pass = pass_started.elapsed();
    timings
}

fn streaming_pass(inputs: &[String], workers: usize, local_rows: usize) -> Timings {
    let pass_started = Instant::now();

    // This initializes only Rayon's global pool. Unlike managed mode, each item
    // calls the sequential API, so no second decoder pool is active.
    let chunk_timings = inputs
        .par_chunks(local_rows)
        .map(|chunk| {
            let decode_started = Instant::now();
            let mut order: Vec<usize> = (0..chunk.len()).collect();
            order.sort_unstable_by_key(|&index| descriptor_key(&chunk[index]));
            let results: Vec<_> = order
                .iter()
                .map(|&index| ultravin::decode_at(&chunk[index], None, FROZEN_NOW_MICROS))
                .collect();
            let decode = decode_started.elapsed();
            black_box(&results);

            let drop_started = Instant::now();
            drop(results);
            Timings {
                decode,
                drop_results: drop_started.elapsed(),
                pass: Duration::ZERO,
            }
        })
        .reduce(Timings::default, |left, right| Timings {
            decode: left.decode + right.decode,
            drop_results: left.drop_results + right.drop_results,
            pass: Duration::ZERO,
        });
    assert_eq!(rayon::current_num_threads(), workers);

    Timings {
        decode: chunk_timings.decode,
        drop_results: chunk_timings.drop_results,
        pass: pass_started.elapsed(),
    }
}

fn descriptor_key(input: &str) -> u64 {
    let bytes = input.as_bytes();
    let mut key = [0_u8; 8];
    let len = bytes.len().min(key.len());
    key[..len].copy_from_slice(&bytes[..len]);
    u64::from_be_bytes(key)
}
