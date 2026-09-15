//! Diagnostic comparison of independent sequential worker batches with the
//! existing shared Rayon batch implementation.

use std::collections::BTreeMap;
use std::sync::{mpsc, Arc};
use std::time::Instant;

#[path = "support/counters.rs"]
mod counters;
#[path = "support/cpu.rs"]
mod cpu;

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const NOW_MICROS: i64 = 1_788_220_800_000_000;
const CURRENT_YEAR: i32 = 2026;

enum Command {
    Decode {
        batch_id: usize,
        start: usize,
        end: usize,
    },
    Release {
        results: Vec<ultravin::DecodeResult<'static>>,
    },
    Flush,
    Stop,
}

enum Event {
    Completed {
        worker: usize,
        batch_id: usize,
        results: Vec<ultravin::DecodeResult<'static>>,
    },
    Flushed {
        decode_ns: u64,
        cleanup_ns: u64,
    },
}

fn locality_key(input: &str) -> u64 {
    let bytes = input.as_bytes();
    let mut key = [0_u8; 8];
    let len = bytes.len().min(8);
    key[..len].copy_from_slice(&bytes[..len]);
    u64::from_be_bytes(key)
}

fn decode_independent_batch(
    vins: &[String],
    start: usize,
    end: usize,
) -> Vec<ultravin::DecodeResult<'static>> {
    let db = ultravin::Db::embedded();
    let mut order: Vec<usize> = (start..end).collect();
    order.sort_unstable_by_key(|&index| locality_key(&vins[index]));
    let mut slots: Vec<Option<ultravin::DecodeResult<'static>>> =
        (start..end).map(|_| None).collect();
    for index in order {
        slots[index - start] = Some(ultravin::decode_full(
            db,
            &vins[index],
            NOW_MICROS,
            CURRENT_YEAR,
            None,
        ));
    }
    slots
        .into_iter()
        .map(|result| result.expect("every output slot is initialized"))
        .collect()
}

fn independent_pass(
    rows: usize,
    batch_size: usize,
    senders: &[mpsc::SyncSender<Command>],
    completed: &mpsc::Receiver<Event>,
    mut capture: Option<&mut Vec<ultravin::DecodeResult<'static>>>,
) -> (u64, u64) {
    let batches = rows.div_ceil(batch_size);
    let mut next_dispatch = 0;
    for (worker, sender) in senders.iter().enumerate().take(batches) {
        dispatch(sender, next_dispatch, batch_size, rows);
        next_dispatch += 1;
        debug_assert!(worker < senders.len());
    }

    let mut next_consume = 0;
    let mut waiting = BTreeMap::<usize, (usize, Vec<ultravin::DecodeResult<'static>>)>::new();
    while next_consume < batches {
        match completed.recv().expect("worker completion channel closed") {
            Event::Completed {
                worker,
                batch_id,
                results,
            } => {
                assert!(waiting.insert(batch_id, (worker, results)).is_none());
            }
            Event::Flushed { .. } => panic!("unexpected flush acknowledgement"),
        }
        while let Some((worker, results)) = waiting.remove(&next_consume) {
            assert_eq!(results.len(), batch_len(next_consume, batch_size, rows));
            std::hint::black_box(&results);
            if let Some(output) = capture.as_deref_mut() {
                output.extend(results.iter().cloned());
            }
            senders[worker]
                .send(Command::Release { results })
                .expect("worker command channel closed");
            if next_dispatch < batches {
                dispatch(&senders[worker], next_dispatch, batch_size, rows);
                next_dispatch += 1;
            }
            next_consume += 1;
        }
    }
    for sender in senders {
        sender
            .send(Command::Flush)
            .expect("worker command channel closed");
    }
    let mut decode_ns = 0;
    let mut cleanup_ns = 0;
    for _ in senders {
        match completed.recv().expect("worker completion channel closed") {
            Event::Flushed {
                decode_ns: decode,
                cleanup_ns: cleanup,
            } => {
                decode_ns += decode;
                cleanup_ns += cleanup;
            }
            Event::Completed { .. } => panic!("received a late decode completion during flush"),
        }
    }
    (decode_ns, cleanup_ns)
}

fn batch_len(batch_id: usize, batch_size: usize, rows: usize) -> usize {
    batch_size.min(rows - batch_id * batch_size)
}

fn dispatch(sender: &mpsc::SyncSender<Command>, batch_id: usize, batch_size: usize, rows: usize) {
    let start = batch_id * batch_size;
    sender
        .send(Command::Decode {
            batch_id,
            start,
            end: (start + batch_size).min(rows),
        })
        .expect("worker command channel closed");
}

fn shared_pass(vins: &[String], batch_size: usize) {
    for chunk in vins.chunks(batch_size) {
        let results = ultravin::decode_batch_managed_at(chunk, None, NOW_MICROS);
        std::hint::black_box(&results);
        drop(results);
    }
}

fn parity_check(vins: &[String], batch_size: usize) {
    let sample_count = vins.len().min(batch_size.clamp(17, 1_000));
    let actual = decode_independent_batch(vins, 0, sample_count);
    let expected: Vec<_> = vins[..sample_count]
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
        .collect();
    assert_eq!(
        actual, expected,
        "independent output order/every-field parity"
    );
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: independent_batch_probe CORPUS MODE WORKERS BATCH_SIZE");
    let mode = args.next().expect("missing mode: independent or shared");
    let workers: usize = args
        .next()
        .expect("missing workers")
        .parse()
        .expect("workers integer");
    let batch_size: usize = args
        .next()
        .expect("missing batch size")
        .parse()
        .expect("batch size integer");
    assert!(workers > 0 && batch_size > 0);
    assert!(matches!(mode.as_str(), "independent" | "shared"));

    let corpus = std::fs::read_to_string(&path).expect("read corpus");
    let vins = Arc::new(
        corpus
            .lines()
            .filter(|line| line.len() == 17)
            .map(str::to_owned)
            .collect::<Vec<_>>(),
    );
    drop(corpus);
    assert!(!vins.is_empty(), "empty corpus");
    parity_check(&vins, batch_size);

    if mode == "shared" {
        assert_eq!(
            ultravin::predictor::worker_count(),
            workers,
            "RAYON_NUM_THREADS must match workers"
        );
        shared_pass(&vins, batch_size);
        eprintln!("warmup complete; starting timed whole-corpus pass");
        let cpu_start = cpu::snapshot();
        let counter_start = counters::snapshot();
        let started = Instant::now();
        shared_pass(&vins, batch_size);
        report(
            &mode,
            workers,
            batch_size,
            vins.len(),
            started,
            (cpu_start, counter_start),
        );
        return;
    }

    let (completed_tx, completed_rx) = mpsc::sync_channel::<Event>(workers);
    let mut senders = Vec::with_capacity(workers);
    let mut handles = Vec::with_capacity(workers);
    for worker in 0..workers {
        let (command_tx, command_rx) = mpsc::sync_channel::<Command>(1);
        senders.push(command_tx);
        let worker_vins = Arc::clone(&vins);
        let worker_completed = completed_tx.clone();
        handles.push(std::thread::spawn(move || {
            let mut decode_ns = 0_u64;
            let mut cleanup_ns = 0_u64;
            loop {
                match command_rx
                    .recv()
                    .expect("coordinator command channel closed")
                {
                    Command::Decode {
                        batch_id,
                        start,
                        end,
                    } => {
                        let started = Instant::now();
                        let results = decode_independent_batch(&worker_vins, start, end);
                        decode_ns += started.elapsed().as_nanos() as u64;
                        worker_completed
                            .send(Event::Completed {
                                worker,
                                batch_id,
                                results,
                            })
                            .expect("completion channel closed");
                    }
                    Command::Release { results } => {
                        let started = Instant::now();
                        drop(results);
                        cleanup_ns += started.elapsed().as_nanos() as u64;
                    }
                    Command::Flush => {
                        worker_completed
                            .send(Event::Flushed {
                                decode_ns,
                                cleanup_ns,
                            })
                            .expect("completion channel closed");
                        decode_ns = 0;
                        cleanup_ns = 0;
                    }
                    Command::Stop => break,
                }
            }
        }));
    }
    drop(completed_tx);

    let parity_rows = vins
        .len()
        .min(batch_size.saturating_mul(2).saturating_add(3));
    let expected: Vec<_> = vins[..parity_rows]
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
        .collect();
    let mut transported = Vec::with_capacity(parity_rows);
    independent_pass(
        parity_rows,
        batch_size,
        &senders,
        &completed_rx,
        Some(&mut transported),
    );
    assert_eq!(
        transported, expected,
        "coordinator transport/order/every-field parity"
    );
    drop(transported);
    drop(expected);

    independent_pass(vins.len(), batch_size, &senders, &completed_rx, None);
    eprintln!("warmup complete; starting timed whole-corpus pass");
    let cpu_start = cpu::snapshot();
    let counter_start = counters::snapshot();
    let started = Instant::now();
    let worker_ns = independent_pass(vins.len(), batch_size, &senders, &completed_rx, None);
    let elapsed = started.elapsed();
    report_elapsed(
        &mode,
        workers,
        batch_size,
        vins.len(),
        elapsed.as_secs_f64(),
        (cpu_start, counter_start),
        Some(worker_ns),
    );
    for sender in &senders {
        sender
            .send(Command::Stop)
            .expect("worker command channel closed");
    }
    for handle in handles {
        handle.join().expect("worker panicked");
    }
}

fn report(
    mode: &str,
    workers: usize,
    batch_size: usize,
    rows: usize,
    started: Instant,
    starts: (Option<cpu::Snapshot>, Option<counters::Snapshot>),
) {
    report_elapsed(
        mode,
        workers,
        batch_size,
        rows,
        started.elapsed().as_secs_f64(),
        starts,
        None,
    );
}

fn report_elapsed(
    mode: &str,
    workers: usize,
    batch_size: usize,
    rows: usize,
    elapsed: f64,
    starts: (Option<cpu::Snapshot>, Option<counters::Snapshot>),
    worker_ns: Option<(u64, u64)>,
) {
    let cpu_usage = cpu::elapsed(starts.0, cpu::snapshot());
    let counter_usage = counters::elapsed(starts.1, counters::snapshot());
    println!(
        "{}",
        serde_json::json!({
            "mode": mode,
            "workers": workers,
            "batch_size": batch_size,
            "max_live_result_batches": if mode == "independent" { workers } else { 1 },
            "max_live_result_rows": if mode == "independent" { workers * batch_size } else { batch_size },
            "rows": rows,
            "elapsed_seconds": elapsed,
            "rows_per_second": rows as f64 / elapsed,
            "process_user_cpu_seconds": cpu_usage.map(|usage| usage.user_seconds),
            "process_system_cpu_seconds": cpu_usage.map(|usage| usage.system_seconds),
            "average_busy_cores": cpu_usage.map(|usage| usage.average_busy_cores(elapsed)),
            "process_counters": counter_usage,
            "now_micros": NOW_MICROS,
            "current_year": CURRENT_YEAR,
            "full_output_materialized": true,
            "ordered_consumption": true,
            "worker_local_cleanup": mode == "independent",
            "aggregate_worker_decode_seconds": worker_ns.map(|value| value.0 as f64 / 1e9),
            "aggregate_worker_cleanup_seconds": worker_ns.map(|value| value.1 as f64 / 1e9),
            "aggregate_worker_other_or_wait_seconds": worker_ns.map(|value| {
                (elapsed * workers as f64 - (value.0 + value.1) as f64 / 1e9).max(0.0)
            }),
        })
    );
}
