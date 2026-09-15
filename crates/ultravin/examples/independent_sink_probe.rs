//! Compute/dispatch ceiling for persistent workers processing independent batches.
//! Results are fully materialized and destroyed locally; this deliberately has
//! no ordered downstream delivery.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Instant;

#[path = "support/counters.rs"]
mod counters;
#[path = "support/cpu.rs"]
mod cpu;

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const NOW: i64 = 1_788_220_800_000_000;
const YEAR: i32 = 2026;

enum Command {
    Pass,
    Stop,
}

struct Done {
    decode_ns: u64,
    cleanup_ns: u64,
    batches: usize,
    rows: usize,
}

fn locality_key(input: &str) -> u64 {
    let bytes = input.as_bytes();
    let mut key = [0_u8; 8];
    let len = bytes.len().min(8);
    key[..len].copy_from_slice(&bytes[..len]);
    u64::from_be_bytes(key)
}

fn decode_batch(vins: &[String], start: usize, end: usize) -> Vec<ultravin::DecodeResult<'static>> {
    let mut order: Vec<usize> = (start..end).collect();
    order.sort_unstable_by_key(|&index| locality_key(&vins[index]));
    let mut slots: Vec<Option<ultravin::DecodeResult<'static>>> =
        (start..end).map(|_| None).collect();
    let db = ultravin::Db::embedded();
    for index in order {
        slots[index - start] = Some(ultravin::decode_full(db, &vins[index], NOW, YEAR, None));
    }
    slots
        .into_iter()
        .map(|result| result.expect("every output slot initialized"))
        .collect()
}

fn run_pass(
    senders: &[mpsc::SyncSender<Command>],
    done: &mpsc::Receiver<Done>,
    cursor: &AtomicUsize,
) -> (u64, u64, usize, usize) {
    cursor.store(0, Ordering::Release);
    for sender in senders {
        sender.send(Command::Pass).expect("worker channel closed");
    }
    let mut totals = (0, 0, 0, 0);
    for _ in senders {
        let worker = done.recv().expect("completion channel closed");
        totals.0 += worker.decode_ns;
        totals.1 += worker.cleanup_ns;
        totals.2 += worker.batches;
        totals.3 += worker.rows;
    }
    totals
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("CORPUS required");
    let workers: usize = args
        .next()
        .expect("WORKERS required")
        .parse()
        .expect("WORKERS integer");
    let batch_size: usize = args
        .next()
        .expect("BATCH_SIZE required")
        .parse()
        .expect("BATCH_SIZE integer");
    assert!(workers > 0 && batch_size > 0);
    let corpus = std::fs::read_to_string(path).expect("read corpus");
    let vins = Arc::new(
        corpus
            .lines()
            .filter(|line| line.len() == 17)
            .map(str::to_owned)
            .collect::<Vec<_>>(),
    );
    drop(corpus);
    assert!(!vins.is_empty());

    let parity_rows = vins.len().min(batch_size.clamp(17, 1_000));
    let actual = decode_batch(&vins, 0, parity_rows);
    let expected: Vec<_> = vins[..parity_rows]
        .iter()
        .map(|vin| ultravin::decode_full(ultravin::Db::embedded(), vin, NOW, YEAR, None))
        .collect();
    assert_eq!(
        actual, expected,
        "local batch input-order/every-field parity"
    );
    drop(actual);
    drop(expected);

    let cursor = Arc::new(AtomicUsize::new(0));
    let (done_tx, done_rx) = mpsc::sync_channel(workers);
    let mut senders = Vec::with_capacity(workers);
    let mut handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let (tx, rx) = mpsc::sync_channel(0);
        senders.push(tx);
        let worker_vins = Arc::clone(&vins);
        let worker_cursor = Arc::clone(&cursor);
        let worker_done = done_tx.clone();
        handles.push(std::thread::spawn(move || {
            while let Command::Pass = rx.recv().expect("coordinator channel closed") {
                let mut decode_ns = 0;
                let mut cleanup_ns = 0;
                let mut batches = 0;
                let mut rows = 0;
                loop {
                    let start = worker_cursor.fetch_add(batch_size, Ordering::Relaxed);
                    if start >= worker_vins.len() {
                        break;
                    }
                    let decode_started = Instant::now();
                    let results = decode_batch(
                        &worker_vins,
                        start,
                        (start + batch_size).min(worker_vins.len()),
                    );
                    decode_ns += decode_started.elapsed().as_nanos() as u64;
                    std::hint::black_box(&results);
                    let cleanup_started = Instant::now();
                    drop(results);
                    cleanup_ns += cleanup_started.elapsed().as_nanos() as u64;
                    batches += 1;
                    rows += (start + batch_size).min(worker_vins.len()) - start;
                }
                worker_done
                    .send(Done {
                        decode_ns,
                        cleanup_ns,
                        batches,
                        rows,
                    })
                    .expect("completion channel closed");
            }
        }));
    }
    drop(done_tx);
    run_pass(&senders, &done_rx, &cursor);
    eprintln!("warmup complete; starting timed whole-corpus pass");
    let cpu_start = cpu::snapshot();
    let counters_start = counters::snapshot();
    let started = Instant::now();
    let stats = run_pass(&senders, &done_rx, &cursor);
    assert_eq!(stats.2, vins.len().div_ceil(batch_size));
    assert_eq!(stats.3, vins.len());
    let elapsed = started.elapsed().as_secs_f64();
    let cpu = cpu::elapsed(cpu_start, cpu::snapshot());
    let process_counters = counters::elapsed(counters_start, counters::snapshot());
    println!(
        "{}",
        serde_json::json!({
            "mode": "independent_local_sink",
            "workers": workers,
            "batch_size": batch_size,
            "rows": vins.len(),
            "elapsed_seconds": elapsed,
            "rows_per_second": vins.len() as f64 / elapsed,
            "batches": stats.2,
            "aggregate_worker_decode_seconds": stats.0 as f64 / 1e9,
            "aggregate_worker_cleanup_seconds": stats.1 as f64 / 1e9,
            "aggregate_worker_other_or_wait_seconds": (elapsed * workers as f64 - (stats.0 + stats.1) as f64 / 1e9).max(0.0),
            "process_user_cpu_seconds": cpu.map(|value| value.user_seconds),
            "process_system_cpu_seconds": cpu.map(|value| value.system_seconds),
            "average_busy_cores": cpu.map(|value| value.average_busy_cores(elapsed)),
            "process_counters": process_counters,
            "now_micros": NOW,
            "full_output_materialized": true,
            "ordered_local_batch_results": true,
            "ordered_downstream_delivery": false,
            "worker_local_cleanup": true,
        })
    );
    for sender in &senders {
        sender.send(Command::Stop).expect("worker channel closed");
    }
    for handle in handles {
        handle.join().expect("worker panicked");
    }
}
