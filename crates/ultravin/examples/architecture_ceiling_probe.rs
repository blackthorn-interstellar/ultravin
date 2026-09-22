//! Diagnostic architecture ceilings for ordered delivery and decode stages.

use std::time::Instant;

use ultravin::diagnostic_ceiling::{self, BATCH_SIZE, SLOTS_PER_WORKER};

#[path = "support/counters.rs"]
mod counters;
#[path = "support/cpu.rs"]
mod cpu;

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const NOW_MICROS: i64 = 1_788_220_800_000_000;
const CURRENT_YEAR: i32 = 2026;
const DEFAULT_STAGE_ROWS: usize = 10_000;

fn parity_check(inputs: &[String], workers: usize) -> u64 {
    let parity_rows = workers
        .saturating_mul(BATCH_SIZE * SLOTS_PER_WORKER)
        .saturating_add(203);
    let sample = &inputs[..inputs.len().min(parity_rows)];
    let expected = sample
        .iter()
        .map(|vin| {
            ultravin::decode_full(
                ultravin::Db::embedded(),
                vin,
                NOW_MICROS,
                CURRENT_YEAR,
                None,
            )
        })
        .collect::<Vec<_>>();
    let mut actual = Vec::with_capacity(sample.len());
    ultravin::decode_native_stream_at(
        sample,
        None,
        NOW_MICROS,
        ultravin::NativeStreamConfig {
            workers,
            batch_size: BATCH_SIZE,
            slots_per_worker: SLOTS_PER_WORKER,
            max_inflight_rows: workers * BATCH_SIZE * SLOTS_PER_WORKER,
        },
        |batch| actual.extend(batch.iter().cloned()),
    )
    .expect("ordered parity stream");
    assert_eq!(actual, expected, "ordered output full-field/order parity");
    let local = diagnostic_ceiling::worker_local_outputs(sample, workers, NOW_MICROS);
    assert_eq!(
        local, expected,
        "worker-local output full-field/order parity"
    );
    let checksum = diagnostic_ceiling::full_checksum(actual.iter());
    assert_eq!(checksum, diagnostic_ceiling::full_checksum(expected.iter()));
    assert_eq!(checksum, diagnostic_ceiling::full_checksum(local.iter()));
    checksum
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect(
        "usage: architecture_ceiling_probe CORPUS (ordered|local|stages) WORKERS [STAGE_ROWS]",
    );
    let mode = args.next().expect("missing mode");
    let workers = args
        .next()
        .expect("missing workers")
        .parse::<usize>()
        .expect("workers integer");
    assert!([1, 8, 12].contains(&workers), "workers must be 1, 8, or 12");
    assert!(matches!(mode.as_str(), "ordered" | "local" | "stages"));

    let corpus = std::fs::read_to_string(path).expect("read corpus");
    let inputs = corpus
        .lines()
        .filter(|line| line.len() == 17)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    drop(corpus);
    assert!(!inputs.is_empty(), "empty VIN corpus");
    let parity_checksum = parity_check(&inputs, workers);

    if mode == "stages" {
        let sample_rows = args
            .next()
            .map(|value| value.parse::<usize>().expect("stage rows integer"))
            .unwrap_or(DEFAULT_STAGE_ROWS)
            .min(inputs.len());
        let _ = diagnostic_ceiling::measure_stages(&inputs, sample_rows, NOW_MICROS);
        eprintln!("sampled stage warm pass complete; starting instrumented sample");
        let report = diagnostic_ceiling::measure_stages(&inputs, sample_rows, NOW_MICROS);
        println!(
            "{}",
            serde_json::json!({
                "report": report,
                "parity_checksum": parity_checksum,
                "fixed_now_micros": NOW_MICROS,
                "batch_size": BATCH_SIZE,
                "requested_workers_argument": workers,
                "stage_workers": 1,
                "stage_slots": 1,
                "full_results_materialized": true,
                "full_warm_pass": false,
                "sampled_warm_pass": true,
                "timing_kind": "diagnostic ceiling; never a benchmark gain",
            })
        );
        return;
    }

    match mode.as_str() {
        "ordered" => drop(diagnostic_ceiling::run_ordered(
            &inputs, workers, NOW_MICROS,
        )),
        "local" => drop(diagnostic_ceiling::run_worker_local(
            &inputs, workers, NOW_MICROS,
        )),
        _ => unreachable!(),
    }
    eprintln!("full warm pass complete; starting timed whole-corpus pass");
    let cpu_start = cpu::snapshot();
    let counter_start = counters::snapshot();
    let started = Instant::now();
    let report = match mode.as_str() {
        "ordered" => diagnostic_ceiling::run_ordered(&inputs, workers, NOW_MICROS),
        "local" => diagnostic_ceiling::run_worker_local(&inputs, workers, NOW_MICROS),
        _ => unreachable!(),
    };
    let elapsed = started.elapsed().as_secs_f64();
    let cpu_usage = cpu::elapsed(cpu_start, cpu::snapshot());
    let counter_usage = counters::elapsed(counter_start, counters::snapshot());
    println!(
        "{}",
        serde_json::json!({
            "report": report,
            "workers": workers,
            "batch_size": BATCH_SIZE,
            "slots_per_worker": SLOTS_PER_WORKER,
            "rows": inputs.len(),
            "elapsed_seconds": elapsed,
            "rows_per_second": inputs.len() as f64 / elapsed,
            "process_user_cpu_seconds": cpu_usage.map(|usage| usage.user_seconds),
            "process_system_cpu_seconds": cpu_usage.map(|usage| usage.system_seconds),
            "average_busy_cores": cpu_usage.map(|usage| usage.average_busy_cores(elapsed)),
            "process_counters": counter_usage,
            "parity_checksum": parity_checksum,
            "fixed_now_micros": NOW_MICROS,
            "full_results_materialized_and_cleaned": true,
            "full_warm_pass": true,
            "timed_consumer": "black_box batch plus scalar row count, matching throughput probe",
            "per_worker_counters": if mode == "local" {
                "worker-private batch/row counts and active wall time"
            } else {
                "omitted to keep the production ordered control uninstrumented"
            },
            "interpretation": if mode == "local" {
                "diagnostic coordination ceiling: local consumption changes result lifetime/backpressure"
            } else {
                "current production ordered native worker pipeline"
            },
        })
    );
}
