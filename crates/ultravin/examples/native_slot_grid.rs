//! Measure explicit worker-slot plans through the production native stream.
//!
//! This probe excludes automatic calibration and measures only an explicit
//! `NativeStreamConfig`; the production auto benchmark remains the acceptance
//! test for automatic selection. Run:
//! `native_slot_grid CORPUS WORKERS BATCH SLOTS MAX_ROWS NOW_MICROS`.

use std::time::Instant;

#[path = "support/counters.rs"]
mod counters;
#[path = "support/cpu.rs"]
mod cpu;

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn run_pass(
    db: &ultravin::Db,
    vins: &[String],
    now_micros: i64,
    config: ultravin::NativeStreamConfig,
) -> usize {
    let mut next_batch = 0;
    let mut next_row = 0;
    db.decode_native_stream_at(vins, None, now_micros, config, |batch| {
        assert_eq!(batch.batch_index(), next_batch, "batch delivery order");
        assert_eq!(batch.start_index(), next_row, "row delivery order");
        let len = batch.len();
        std::hint::black_box(batch);
        next_batch += 1;
        next_row += len;
    })
    .expect("production native stream");
    assert_eq!(next_row, vins.len(), "full corpus delivery");
    next_row
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("CORPUS");
    let workers: usize = args.next().expect("WORKERS").parse().expect("integer");
    let batch_size: usize = args.next().expect("BATCH").parse().expect("integer");
    let slots_per_worker: usize = args.next().expect("SLOTS").parse().expect("integer");
    let max_inflight_rows: usize = args.next().expect("MAX_ROWS").parse().expect("integer");
    let now_micros: i64 = args.next().expect("NOW_MICROS").parse().expect("integer");
    assert!(args.next().is_none(), "unexpected argument");
    let expected_rows = workers
        .checked_mul(batch_size)
        .and_then(|value| value.checked_mul(slots_per_worker))
        .expect("slot row capacity overflow");
    assert_eq!(
        max_inflight_rows, expected_rows,
        "max rows must equal W * B * S"
    );

    let corpus = std::fs::read_to_string(path).expect("read corpus");
    let vins = corpus.lines().map(str::to_owned).collect::<Vec<_>>();
    drop(corpus);
    assert!(!vins.is_empty(), "empty corpus");
    let config = ultravin::NativeStreamConfig {
        workers,
        batch_size,
        slots_per_worker,
        max_inflight_rows,
    }
    .validate()
    .expect("valid native stream configuration");
    let db = ultravin::Db::embedded();

    run_pass(db, &vins, now_micros, config);
    eprintln!("native slot grid: warmup complete; starting timed whole-corpus pass");
    let cpu_started = cpu::snapshot();
    let counters_started = counters::snapshot();
    let started = Instant::now();
    let rows = run_pass(db, &vins, now_micros, config);
    let elapsed = started.elapsed().as_secs_f64();
    let cpu_usage = cpu::elapsed(cpu_started, cpu::snapshot());
    let counter_usage = counters::elapsed(counters_started, counters::snapshot());
    println!(
        "{}",
        serde_json::json!({
            "benchmark": "production_native_slot_grid",
            "result_owner": "worker_slots",
            "workers": workers,
            "batch_size": batch_size,
            "slots_per_worker": slots_per_worker,
            "max_inflight_rows": max_inflight_rows,
            "rows": rows,
            "elapsed_seconds": elapsed,
            "rows_per_second": rows as f64 / elapsed,
            "process_user_cpu_seconds": cpu_usage.map(|usage| usage.user_seconds),
            "process_system_cpu_seconds": cpu_usage.map(|usage| usage.system_seconds),
            "average_busy_cores": cpu_usage.map(|usage| usage.average_busy_cores(elapsed)),
            "process_counters": counter_usage,
            "now_micros": now_micros,
            "full_output_materialized": true,
            "ordered_delivery": true,
            "owner_cleanup_complete": true,
        })
    );
}
