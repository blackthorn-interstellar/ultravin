//! 60-second throughput probe for the in-process engine.
//!
//! Decodes the shared benchmark corpus (scripts/bench/corpus.txt, passed as
//! argv[1]) on repeat until the wall-clock budget is spent, reporting VIN/s for
//! the single-stream path and the parallel `decode_batch` path. This is the
//! engine ceiling the SQL oracles in scripts/bench/throughput.py are measured
//! against. Run:
//! `cargo run -p ultravin --example throughput --release -- scripts/bench/corpus.txt [secs] [both|single|batch] [full|json] [batch-size|auto]`
//! `json` includes full-provenance JSON encoding; `full` returns the Rust structs.
//! `auto` is the default with `full` and exercises the native worker-slot predictor.
//! Select one path to measure its CPU time and memory with `/usr/bin/time -l`.

use std::time::{Duration, Instant};

#[path = "support/counters.rs"]
mod counters;
#[path = "support/cpu.rs"]
mod cpu;

// Match the shipped wheel: a sharded allocator so the parallel batch path doesn't
// serialize on the global heap lock (see crates/ultravin-py/src/lib.rs). Gated to
// the same arches that carry the dep.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .unwrap_or_else(|| "scripts/bench/corpus.txt".into());
    let secs: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(60.0);
    let mode = args.next().unwrap_or_else(|| "both".into());
    assert!(
        matches!(mode.as_str(), "both" | "single" | "batch"),
        "mode must be both, single or batch"
    );
    let format = args.next().unwrap_or_else(|| "full".into());
    let batch_arg = args.next();
    let auto_batch =
        batch_arg.as_deref() == Some("auto") || (batch_arg.is_none() && format == "full");
    let batch_size = batch_arg.as_deref().and_then(|s| {
        if s == "auto" {
            None
        } else {
            Some(
                s.parse::<usize>()
                    .expect("batch-size must be an integer or auto"),
            )
        }
    });
    let now_micros = std::env::var("ULTRAVIN_NOW_MICROS").ok().map(|value| {
        value
            .parse::<i64>()
            .expect("ULTRAVIN_NOW_MICROS must be an integer")
    });
    assert!(
        matches!(format.as_str(), "full" | "json"),
        "format must be full or json"
    );
    assert!(!auto_batch || format == "full", "auto requires format full");
    let decode = |vin: &str| {
        if format == "json" {
            std::hint::black_box(match now_micros {
                Some(now) => ultravin::decode_json_at(vin, None, now),
                None => ultravin::decode_json(vin, None),
            });
        } else {
            std::hint::black_box(match now_micros {
                Some(now) => ultravin::decode_at(vin, None, now),
                None => ultravin::decode(vin, None),
            });
        }
    };
    let decode_batch = |vins: &[String]| {
        if format == "json" {
            std::hint::black_box(match now_micros {
                Some(now) => ultravin::decode_batch_json_at(vins, None, now),
                None => ultravin::decode_batch_json(vins, None),
            });
        } else {
            std::hint::black_box(match now_micros {
                Some(now) => ultravin::decode_batch_at(vins, None, now),
                None => ultravin::decode_batch(vins, None),
            });
        }
    };
    let budget = Duration::from_secs_f64(secs);

    let corpus = std::fs::read_to_string(&path).expect("read corpus");
    let vins: Vec<&str> = corpus.lines().filter(|l| l.len() == 17).collect();
    assert!(!vins.is_empty(), "empty corpus: {path}");

    // Single-stream: one sequential caller, system-clock path (what a caller sees).
    if mode != "batch" {
        for vin in &vins {
            decode(vin);
        }
        let t = Instant::now();
        let mut n: u64 = 0;
        while t.elapsed() < budget {
            // Decode a full pass so we never check the clock more than per-corpus.
            for v in &vins {
                decode(v);
            }
            n += vins.len() as u64;
        }
        let dt = t.elapsed().as_secs_f64();
        report("single", n, dt, 1);
    }

    // Native auto assigns whole batches to workers; explicit sizes retain the
    // shared Rayon batch API for comparisons.
    if mode != "single" {
        let owned: Vec<String> = vins.iter().map(|s| s.to_string()).collect();
        drop(vins);
        drop(corpus);
        if auto_batch {
            benchmark_auto_native(
                &owned,
                budget,
                now_micros.unwrap_or_else(ultravin::now_micros),
            );
            return;
        }
        let chunks: Vec<&[String]> = match batch_size {
            Some(size) => {
                assert!(size > 0, "batch-size must be positive");
                owned.chunks(size).collect()
            }
            None => vec![&owned],
        };
        for chunk in &chunks {
            decode_batch(chunk);
        }
        let t = Instant::now();
        let mut n: u64 = 0;
        while t.elapsed() < budget {
            // Walk every corpus chunk in order. Small-batch measurements therefore
            // cover the same VIN distribution as large batches instead of repeatedly
            // decoding only the first N rows.
            for chunk in &chunks {
                decode_batch(chunk);
                n += chunk.len() as u64;
            }
        }
        let dt = t.elapsed().as_secs_f64();
        report("batch", n, dt, ultravin::predictor::worker_count());
    }
}

fn benchmark_auto_native(owned: &[String], budget: Duration, job_now_micros: i64) {
    use std::collections::BTreeMap;

    let options = ultravin::NativeAutoOptions::default();
    // Use the same production path for warmup and timing. Each complete timed
    // job includes calibration, ordered consumption, and owner-worker cleanup.
    ultravin::decode_native_stream_auto_at(owned, None, job_now_micros, options, |batch| {
        std::hint::black_box(batch);
    })
    .expect("native warmup");

    eprintln!("native auto: warmup complete; starting timed passes");
    let workers = ultravin::predictor::worker_count();
    let cpu_started = cpu::snapshot();
    let counters_started = counters::snapshot();
    let started = Instant::now();
    let mut rows = 0_u64;
    let mut selected_batches = BTreeMap::<usize, u64>::new();
    let mut selected_rows = BTreeMap::<usize, u64>::new();
    let mut initial_prediction = None;

    while started.elapsed() < budget {
        let prediction =
            ultravin::decode_native_stream_auto_at(owned, None, job_now_micros, options, |batch| {
                let count = batch.len();
                *selected_batches.entry(count).or_default() += 1;
                *selected_rows.entry(count).or_default() += count as u64;
                rows += count as u64;
                std::hint::black_box(batch);
            })
            .expect("native automatic stream");
        initial_prediction = initial_prediction.or(prediction);
    }

    let elapsed = started.elapsed().as_secs_f64();
    let counter_usage = counters::elapsed(counters_started, counters::snapshot());
    let cpu_usage = cpu::elapsed(cpu_started, cpu::snapshot());
    report("batch", rows, elapsed, workers);
    println!(
        "{}",
        serde_json::json!({
            "benchmark": "batch",
            "format": "full",
            "result_owner": "worker_slots",
            "batch_size": "auto",
            "process_counters": counter_usage,
            "process_user_cpu_seconds": cpu_usage.map(|usage| usage.user_seconds),
            "process_system_cpu_seconds": cpu_usage.map(|usage| usage.system_seconds),
            "average_busy_cores": cpu_usage.map(|usage| usage.average_busy_cores(elapsed)),
            "memory_bytes": options.memory_bytes,
            "workers": workers,
            "predictor": initial_prediction,
            "actual_rows_per_second": rows as f64 / elapsed,
            "selected_size_batches": selected_batches,
            "selected_size_rows": selected_rows,
            "rows": rows,
            "elapsed_seconds": elapsed,
        })
    );
}

fn report(label: &str, n: u64, dt: f64, cores: usize) {
    let per_s = n as f64 / dt;
    eprintln!(
        "{label}: {n} VINs in {dt:.1}s = {per_s:.0} VIN/s ({:.0} in 60s, {cores} core(s))",
        per_s * 60.0,
    );
}
