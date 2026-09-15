//! Fixed managed-batch grid for native predictor model data.
//!
//! One process owns one worker count. It loads the corpus once, warms every
//! unique input once, measures production two-sample calibration independently,
//! then emits one JSON line after every completed timed cell.
//! The Python driver validates that the supplied corpus contains unique VINs.
//!
//! `native_grid CORPUS SECONDS WORKERS BATCH_SIZES_CSV ROUNDS`

use std::hint::black_box;
use std::io::Write as _;
use std::time::{Duration, Instant};

mod support;
use support::native_output_bytes;

#[path = "support/cpu.rs"]
mod cpu;

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const FROZEN_NOW_MICROS: i64 = 1_788_220_800_000_000; // 2026-09-01T00:00:00Z
const MEMORY_BYTES: usize = 512 * 1024 * 1024;
const CALIBRATION_ROWS: usize = 256;
const CALIBRATION_OFFSETS: usize = 5;

#[derive(Default)]
struct CellTimings {
    decode: Duration,
    output_sample: Duration,
    drop_results: Duration,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let corpus_path = args.next().expect("corpus path is required");
    let seconds = parse_positive::<f64>(args.next(), "seconds");
    assert!(seconds.is_finite(), "seconds must be finite");
    let workers = parse_positive::<usize>(args.next(), "workers");
    let batch_sizes = parse_batch_sizes(&args.next().expect("batch-sizes CSV is required"));
    let rounds = parse_positive::<usize>(args.next(), "rounds");
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
    assert!(
        inputs.len() >= CALIBRATION_ROWS * 2,
        "corpus must have at least two unique calibration batches"
    );

    // One full unique-input warm pass, outside calibration and every timed cell.
    run_whole_pass(&inputs, CALIBRATION_ROWS);
    eprintln!("native grid: warmup complete; measuring calibration");
    let calibration_rates = calibration_rates(&inputs, workers);
    let mut sorted_rates = calibration_rates.clone();
    sorted_rates.sort_by(f64::total_cmp);
    let calibration_median = sorted_rates[sorted_rates.len() / 2];
    println!(
        "{}",
        serde_json::json!({
            "record": "calibration",
            "workers": workers,
            "sample_rows": CALIBRATION_ROWS,
            "offset_samples": calibration_rates,
            "median_single_core_rows_per_second": calibration_median,
            "memory_bytes": MEMORY_BYTES,
        })
    );
    std::io::stdout().flush().expect("flush calibration record");

    let budget = Duration::from_secs_f64(seconds);
    for (round, batch_size) in grid_order(&batch_sizes, rounds) {
        let cpu_started = cpu::snapshot();
        let started = Instant::now();
        let mut rows = 0_u64;
        let mut cursor = 0_usize;
        let mut estimated_peak_returned_output_bytes = 0_usize;
        let mut timings = CellTimings::default();
        while started.elapsed() < budget {
            assert!(
                cursor < inputs.len(),
                "corpus exhausted before the requested duration; use a larger unique corpus"
            );
            let end = cursor.saturating_add(batch_size).min(inputs.len());
            let measured = measured_batch(
                &inputs[cursor..end],
                &mut estimated_peak_returned_output_bytes,
            );
            timings.decode += measured.decode;
            timings.output_sample += measured.output_sample;
            timings.drop_results += measured.drop_results;
            rows += (end - cursor) as u64;
            cursor = end;
        }
        let elapsed = started.elapsed();
        let cpu_usage = cpu::elapsed(cpu_started, cpu::snapshot());
        let rate = rows as f64 / elapsed.as_secs_f64();
        let record = serde_json::json!({
            "record": "cell",
            "workers": workers,
            "batch_size": batch_size,
            "round": round,
            "corpus_rows": inputs.len(),
            "timing_policy": "unique_prefix_complete_batches",
            "unique_rows": rows,
            "completed_corpus_passes": u8::from(cursor == inputs.len()),
            "rows": rows,
            "elapsed_seconds": elapsed.as_secs_f64(),
            "actual_rows_per_second": rate,
            "process_user_cpu_seconds": cpu_usage.map(|usage| usage.user_seconds),
            "process_system_cpu_seconds": cpu_usage.map(|usage| usage.system_seconds),
            "average_busy_cores": cpu_usage.map(|usage| usage.average_busy_cores(elapsed.as_secs_f64())),
            "decode_seconds": timings.decode.as_secs_f64(),
            "output_sample_seconds": timings.output_sample.as_secs_f64(),
            "drop_results_seconds": timings.drop_results.as_secs_f64(),
            "estimated_peak_returned_output_bytes": estimated_peak_returned_output_bytes,
            "estimated_peak_working_bytes": estimated_peak_returned_output_bytes.saturating_mul(2),
            "output_estimator_sample_rows": 16,
            "calibration_median_single_core_rows_per_second": calibration_median,
            "frozen_now_micros": FROZEN_NOW_MICROS,
            "result_owner": "BatchResults",
        });
        println!("{record}");
        std::io::stdout().flush().expect("flush completed cell");
        eprintln!(
            "grid: B={batch_size} round {round}/{rounds}: {rows} VINs in {:.6}s = {rate:.0} VIN/s",
            elapsed.as_secs_f64()
        );
    }
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

fn parse_batch_sizes(csv: &str) -> Vec<usize> {
    let sizes: Vec<usize> = csv
        .split(',')
        .map(|value| {
            value
                .parse::<usize>()
                .unwrap_or_else(|error| panic!("invalid batch size {value:?}: {error}"))
        })
        .collect();
    assert!(!sizes.is_empty(), "at least one batch size is required");
    assert!(
        sizes.iter().all(|&size| size > 0),
        "batch sizes must be positive"
    );
    let mut distinct = sizes.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), sizes.len(), "batch sizes must be distinct");
    sizes
}

fn grid_order(batch_sizes: &[usize], rounds: usize) -> Vec<(usize, usize)> {
    let mut order = Vec::with_capacity(batch_sizes.len() * rounds);
    for round in 1..=rounds {
        let mut sizes = batch_sizes.to_vec();
        // Reverse each adjacent pair exactly; rotate only between pairs so
        // the largest batch is not always measured last in a two-round grid.
        let offset = ((round - 1) / 2) % sizes.len();
        sizes.rotate_left(offset);
        if round % 2 == 0 {
            sizes.reverse();
        }
        order.extend(sizes.into_iter().map(|size| (round, size)));
    }
    order
}

// This historical shared-batch grid remains useful for explicit comparisons.
// Measure its serial reference directly; native auto now owns worker slots and
// must never route its per-worker plan through the old shared-batch controller.
fn calibration_rates(inputs: &[String], _workers: usize) -> Vec<f64> {
    (0..CALIBRATION_OFFSETS)
        .map(|sample| {
            let maximum_offset = inputs.len() - CALIBRATION_ROWS;
            let offset = sample * maximum_offset / (CALIBRATION_OFFSETS - 1);
            let chunk = &inputs[offset..offset + CALIBRATION_ROWS];
            for vin in chunk {
                drop(black_box(ultravin::decode_at(vin, None, FROZEN_NOW_MICROS)));
            }
            let started = Instant::now();
            for vin in chunk {
                drop(black_box(ultravin::decode_at(vin, None, FROZEN_NOW_MICROS)));
            }
            CALIBRATION_ROWS as f64 / started.elapsed().as_secs_f64()
        })
        .collect()
}

fn run_whole_pass(inputs: &[String], batch_size: usize) {
    for chunk in inputs.chunks(batch_size) {
        let results = ultravin::decode_batch_managed_at(chunk, None, FROZEN_NOW_MICROS);
        drop(black_box(results));
    }
}

fn measured_batch(
    chunk: &[String],
    estimated_peak_returned_output_bytes: &mut usize,
) -> CellTimings {
    let mut timings = CellTimings::default();
    let decode_started = Instant::now();
    let results = ultravin::decode_batch_managed_at(chunk, None, FROZEN_NOW_MICROS);
    timings.decode = decode_started.elapsed();

    let sample_started = Instant::now();
    *estimated_peak_returned_output_bytes = (*estimated_peak_returned_output_bytes)
        .max(native_output_bytes(&results, results.capacity()));
    timings.output_sample = sample_started.elapsed();
    black_box(&results);

    let drop_started = Instant::now();
    drop(results);
    timings.drop_results = drop_started.elapsed();
    timings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_order_reverses_pairs_and_rotates_between_pairs() {
        assert_eq!(
            grid_order(&[1500, 6000, 12000], 4),
            [
                (1, 1500),
                (1, 6000),
                (1, 12000),
                (2, 12000),
                (2, 6000),
                (2, 1500),
                (3, 6000),
                (3, 12000),
                (3, 1500),
                (4, 1500),
                (4, 12000),
                (4, 6000),
            ]
        );
    }
}
