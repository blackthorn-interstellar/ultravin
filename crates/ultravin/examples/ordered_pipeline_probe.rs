//! Bounded ordered delivery with independently claiming persistent workers.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::{mpsc, Arc, Condvar, Mutex};
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
    Pass(Arc<Pass>),
    Stop,
}

enum CleanupCommand {
    Drop {
        results: Vec<ultravin::DecodeResult<'static>>,
        permit: CreditPermit,
    },
    Flush,
    Stop,
}

struct Pass {
    rows: usize,
    batch: usize,
    credits: Arc<Credits>,
    measure_bytes: bool,
}

struct CreditState {
    next_claim: usize,
    live_batches: usize,
    live_rows: usize,
    live_bytes: usize,
    peak_batches: usize,
    peak_bytes: usize,
    cancelled: bool,
}

struct Credits {
    state: Mutex<CreditState>,
    changed: Condvar,
    max_batches: usize,
    max_rows: usize,
}

impl Credits {
    fn claim(
        self: &Arc<Self>,
        rows: usize,
        batch: usize,
    ) -> Option<(usize, usize, usize, CreditPermit)> {
        let mut state = self.state.lock().expect("credit lock poisoned");
        loop {
            if state.cancelled {
                return None;
            }
            let start = state.next_claim * batch;
            if start >= rows {
                return None;
            }
            let end = (start + batch).min(rows);
            let batch_rows = end - start;
            if state.live_batches < self.max_batches
                && state.live_rows + batch_rows <= self.max_rows
            {
                let id = state.next_claim;
                state.next_claim += 1;
                state.live_batches += 1;
                state.live_rows += batch_rows;
                state.peak_batches = state.peak_batches.max(state.live_batches);
                return Some((
                    id,
                    start,
                    end,
                    CreditPermit {
                        credits: Arc::clone(self),
                        rows: batch_rows,
                        bytes: 0,
                    },
                ));
            }
            state = self.changed.wait(state).expect("credit lock poisoned");
        }
    }

    fn cancel(&self) {
        let mut state = self.state.lock().expect("credit lock poisoned");
        state.cancelled = true;
        self.changed.notify_all();
    }

    fn publish(&self, bytes: usize) {
        let mut state = self.state.lock().expect("credit lock poisoned");
        state.live_bytes += bytes;
        state.peak_bytes = state.peak_bytes.max(state.live_bytes);
    }

    fn release(&self, rows: usize, bytes: usize) {
        let mut state = self.state.lock().expect("credit lock poisoned");
        state.live_batches -= 1;
        state.live_rows -= rows;
        state.live_bytes -= bytes;
        self.changed.notify_all();
    }
}

struct CreditPermit {
    credits: Arc<Credits>,
    rows: usize,
    bytes: usize,
}

impl CreditPermit {
    fn record_bytes(&mut self, bytes: usize) {
        self.bytes = bytes;
        if bytes != 0 {
            self.credits.publish(bytes);
        }
    }
}

impl Drop for CreditPermit {
    fn drop(&mut self) {
        self.credits.release(self.rows, self.bytes);
    }
}

struct CancelOnDrop {
    credits: Arc<Credits>,
    armed: bool,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.credits.cancel();
        }
    }
}

struct Published {
    id: usize,
    results: Result<(Vec<ultravin::DecodeResult<'static>>, CreditPermit), ()>,
}
struct WorkerDone {
    decode_ns: u64,
    admission_ns: u64,
    publish_ns: u64,
    batches: usize,
}
struct CleanupDone {
    cleanup_ns: u64,
}

// Inspecting Cow ownership is necessary to count only heap-owned storage.
#[allow(clippy::ptr_arg)]
fn cow_bytes(value: &Cow<'_, str>) -> usize {
    match value {
        Cow::Borrowed(_) => 0,
        Cow::Owned(text) => text.capacity(),
    }
}

fn result_bytes(value: &ultravin::DecodeResult<'_>) -> usize {
    value.vin.capacity()
        + value.wmi.capacity()
        + value.descriptor.capacity()
        + value.error_codes.capacity() * std::mem::size_of::<i32>()
        + value.corrected_vin.capacity()
        + value.elements.capacity() * std::mem::size_of::<ultravin::DecodedElement<'_>>()
        + value
            .elements
            .iter()
            .map(|element| {
                cow_bytes(&element.value)
                    + cow_bytes(&element.attribute_id)
                    + cow_bytes(&element.source)
                    + cow_bytes(&element.keys)
            })
            .sum::<usize>()
}

fn result_slice_bytes(results: &[ultravin::DecodeResult<'_>], capacity: usize) -> usize {
    capacity * std::mem::size_of::<ultravin::DecodeResult<'_>>()
        + results.iter().map(result_bytes).sum::<usize>()
}

fn decode(vins: &[String], start: usize, end: usize) -> Vec<ultravin::DecodeResult<'static>> {
    let db = ultravin::Db::embedded();
    let mut order = (start..end).collect::<Vec<_>>();
    order.sort_unstable_by_key(|&index| {
        let mut key = [0_u8; 8];
        let bytes = vins[index].as_bytes();
        let len = bytes.len().min(8);
        key[..len].copy_from_slice(&bytes[..len]);
        u64::from_be_bytes(key)
    });
    let mut slots = (start..end).map(|_| None).collect::<Vec<_>>();
    for index in order {
        slots[index - start] = Some(ultravin::decode_full(db, &vins[index], NOW, YEAR, None));
    }
    slots
        .into_iter()
        .map(|value| value.expect("slot initialized"))
        .collect()
}

fn new_credits(max_batches: usize, max_rows: usize) -> Arc<Credits> {
    Arc::new(Credits {
        state: Mutex::new(CreditState {
            next_claim: 0,
            live_batches: 0,
            live_rows: 0,
            live_bytes: 0,
            peak_batches: 0,
            peak_bytes: 0,
            cancelled: false,
        }),
        changed: Condvar::new(),
        max_batches,
        max_rows,
    })
}

fn worker_pass(
    pass: &Pass,
    published: &mpsc::Sender<Published>,
    mut decode_batch: impl FnMut(usize, usize) -> Vec<ultravin::DecodeResult<'static>>,
) -> WorkerDone {
    let mut stats = WorkerDone {
        decode_ns: 0,
        admission_ns: 0,
        publish_ns: 0,
        batches: 0,
    };
    loop {
        let t = Instant::now();
        let Some((id, start, end, mut permit)) = pass.credits.claim(pass.rows, pass.batch) else {
            break;
        };
        stats.admission_ns += t.elapsed().as_nanos() as u64;
        let t = Instant::now();
        let decoded =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decode_batch(start, end)));
        stats.decode_ns += t.elapsed().as_nanos() as u64;
        let Ok(results) = decoded else {
            pass.credits.cancel();
            drop(permit);
            let _ = published.send(Published {
                id,
                results: Err(()),
            });
            break;
        };
        let bytes = if pass.measure_bytes {
            result_slice_bytes(&results, results.capacity())
        } else {
            0
        };
        permit.record_bytes(bytes);
        let t = Instant::now();
        if published
            .send(Published {
                id,
                results: Ok((results, permit)),
            })
            .is_err()
        {
            pass.credits.cancel();
            break;
        }
        stats.publish_ns += t.elapsed().as_nanos() as u64;
        stats.batches += 1;
    }
    stats
}

// The tuple drops results before returning their capacity credit, including on unwind.
type Completed = (Vec<ultravin::DecodeResult<'static>>, CreditPermit);

fn consume_ordered(
    batches: usize,
    published: &mpsc::Receiver<Published>,
    credits: &Arc<Credits>,
    mut consume: impl FnMut(Completed),
) {
    let mut guard = CancelOnDrop {
        credits: Arc::clone(credits),
        armed: true,
    };
    let mut waiting = BTreeMap::new();
    let mut next = 0;
    while next < batches {
        let item = published.recv().expect("publisher closed");
        assert!(item.results.is_ok(), "decode worker failed");
        assert!(item.id >= next && item.id < batches, "invalid batch index");
        assert!(waiting.insert(item.id, item).is_none(), "duplicate batch");
        while let Some(item) = waiting.remove(&next) {
            consume(item.results.expect("decode worker failed"));
            next += 1;
        }
    }
    guard.armed = false;
}

// This diagnostic keeps each transport endpoint explicit for timing and testing.
#[allow(clippy::too_many_arguments)]
fn run_pass(
    vins: &Arc<Vec<String>>,
    batch: usize,
    senders: &[mpsc::SyncSender<Command>],
    published: &mpsc::Receiver<Published>,
    done: &mpsc::Receiver<WorkerDone>,
    cleaners: &[mpsc::Sender<CleanupCommand>],
    cleanup_done: &mpsc::Receiver<CleanupDone>,
    max_batches: usize,
    max_rows: usize,
    measure_bytes: bool,
) -> (u64, u64, u64, u64, usize, usize) {
    let batches = vins.len().div_ceil(batch);
    let credits = new_credits(max_batches, max_rows);
    let mut cancel_guard = CancelOnDrop {
        credits: Arc::clone(&credits),
        armed: true,
    };
    let pass = Arc::new(Pass {
        rows: vins.len(),
        batch,
        credits: Arc::clone(&credits),
        measure_bytes,
    });
    for tx in senders {
        tx.send(Command::Pass(Arc::clone(&pass)))
            .expect("worker closed");
    }
    let mut delivery_ns = 0_u64;
    let mut cleanup_cursor = 0;
    consume_ordered(batches, published, &credits, |(results, permit)| {
        let started = Instant::now();
        std::hint::black_box(&results);
        delivery_ns += started.elapsed().as_nanos() as u64;
        cleaners[cleanup_cursor % cleaners.len()]
            .send(CleanupCommand::Drop { results, permit })
            .expect("cleanup worker closed");
        cleanup_cursor += 1;
    });
    for cleaner in cleaners {
        cleaner
            .send(CleanupCommand::Flush)
            .expect("cleanup worker closed");
    }
    let mut cleanup_ns = 0;
    for _ in cleaners {
        cleanup_ns += cleanup_done.recv().expect("cleanup done closed").cleanup_ns;
    }
    let mut worker = (0, 0, 0);
    for _ in senders {
        let stats = done.recv().expect("worker done closed");
        worker.0 += stats.decode_ns;
        worker.1 += stats.admission_ns;
        worker.2 += stats.publish_ns;
        assert!(stats.batches <= batches);
    }
    let state = credits.state.lock().expect("credit lock poisoned");
    assert_eq!(
        (
            state.live_batches,
            state.live_rows,
            state.live_bytes,
            state.next_claim
        ),
        (0, 0, 0, batches)
    );
    cancel_guard.armed = false;
    (
        delivery_ns,
        cleanup_ns,
        worker.0,
        worker.1 + worker.2,
        state.peak_batches,
        state.peak_bytes,
    )
}

fn shared_pass(vins: &[String], measure_bytes: bool) -> usize {
    let mut peak = 0;
    for chunk in vins.chunks(12_000) {
        let results = ultravin::decode_batch_managed_at(chunk, None, NOW);
        if measure_bytes {
            peak = peak.max(result_slice_bytes(&results, results.capacity()));
        }
        std::hint::black_box(&results);
        drop(results);
    }
    peak
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("CORPUS");
    let mode = args.next().expect("MODE");
    let workers: usize = args.next().expect("WORKERS").parse().expect("integer");
    let batch: usize = args.next().expect("BATCH").parse().expect("integer");
    let measure_bytes: bool = args
        .next()
        .unwrap_or_else(|| "false".to_owned())
        .parse()
        .expect("boolean");
    let requested_cleaners: usize = args
        .next()
        .unwrap_or_else(|| "1".to_owned())
        .parse()
        .expect("integer");
    let corpus = std::fs::read_to_string(path).expect("read corpus");
    let vins = Arc::new(corpus.lines().map(str::to_owned).collect::<Vec<_>>());
    drop(corpus);
    assert!(workers > 0 && batch > 0 && !vins.is_empty());
    assert!(batch <= 12_000, "batch exceeds the live-result row budget");

    if mode == "shared" {
        shared_pass(&vins, false);
        eprintln!("warmup complete; starting timed whole-corpus pass");
        let cpu_start = cpu::snapshot();
        let counters_start = counters::snapshot();
        let started = Instant::now();
        let peak = shared_pass(&vins, measure_bytes);
        let elapsed = started.elapsed().as_secs_f64();
        let cpu = cpu::elapsed(cpu_start, cpu::snapshot());
        let counts = counters::elapsed(counters_start, counters::snapshot());
        println!(
            "{}",
            serde_json::json!({"mode":"shared","workers":workers,"batch_size":12000,"rows":vins.len(),"elapsed_seconds":elapsed,"rows_per_second":vins.len() as f64/elapsed,"max_output_owned_bytes":peak,"process_user_cpu_seconds":cpu.map(|x|x.user_seconds),"process_system_cpu_seconds":cpu.map(|x|x.system_seconds),"average_busy_cores":cpu.map(|x|x.average_busy_cores(elapsed)),"process_counters":counts,"now_micros":NOW,"ordered_delivery":true})
        );
        return;
    }
    assert_eq!(mode, "ordered");
    let (pub_tx, pub_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::sync_channel(workers);
    let cleaner_count = requested_cleaners.clamp(1, workers);
    let (cleanup_done_tx, cleanup_done_rx) = mpsc::sync_channel(cleaner_count);
    let mut cleaners = Vec::new();
    let mut cleaner_handles = Vec::new();
    for _ in 0..cleaner_count {
        let (tx, rx) = mpsc::channel();
        cleaners.push(tx);
        let done = cleanup_done_tx.clone();
        cleaner_handles.push(std::thread::spawn(move || {
            let mut cleanup_ns = 0;
            loop {
                match rx.recv().expect("cleanup command closed") {
                    CleanupCommand::Drop { results, permit } => {
                        let started = Instant::now();
                        drop(results);
                        cleanup_ns += started.elapsed().as_nanos() as u64;
                        drop(permit);
                    }
                    CleanupCommand::Flush => {
                        done.send(CleanupDone { cleanup_ns })
                            .expect("cleanup done closed");
                        cleanup_ns = 0;
                    }
                    CleanupCommand::Stop => break,
                }
            }
        }));
    }
    drop(cleanup_done_tx);
    let mut senders = Vec::new();
    let mut handles = Vec::new();
    for _ in 0..workers {
        let (tx, rx) = mpsc::sync_channel(0);
        senders.push(tx);
        let vins = Arc::clone(&vins);
        let pub_tx = pub_tx.clone();
        let done_tx = done_tx.clone();
        handles.push(std::thread::spawn(move || loop {
            match rx.recv().expect("command closed") {
                Command::Stop => break,
                Command::Pass(pass) => {
                    let stats = worker_pass(&pass, &pub_tx, |start, end| decode(&vins, start, end));
                    done_tx.send(stats).expect("done closed");
                }
            }
        }));
    }
    drop(pub_tx);
    drop(done_tx);
    let batches = vins.len().div_ceil(batch);
    let max_rows: usize = 12_000;
    let slots = max_rows.div_ceil(batch).max(workers).min(batches);
    run_pass(
        &vins,
        batch,
        &senders,
        &pub_rx,
        &done_rx,
        &cleaners,
        &cleanup_done_rx,
        slots,
        max_rows,
        false,
    );
    eprintln!("warmup complete; starting timed whole-corpus pass");
    let cpu_start = cpu::snapshot();
    let count_start = counters::snapshot();
    let started = Instant::now();
    let (delivery_ns, cleanup_ns, decode_ns, worker_other_ns, peak_batches, peak_bytes) = run_pass(
        &vins,
        batch,
        &senders,
        &pub_rx,
        &done_rx,
        &cleaners,
        &cleanup_done_rx,
        slots,
        max_rows,
        measure_bytes,
    );
    let elapsed = started.elapsed().as_secs_f64();
    let cpu = cpu::elapsed(cpu_start, cpu::snapshot());
    let counts = counters::elapsed(count_start, counters::snapshot());
    println!(
        "{}",
        serde_json::json!({"mode":"ordered","workers":workers,"cleanup_workers":cleaner_count,"active_threads_including_consumer":workers+cleaner_count+1,"batch_size":batch,"rows":vins.len(),"elapsed_seconds":elapsed,"rows_per_second":vins.len() as f64/elapsed,"live_result_row_budget":max_rows,"max_live_result_batches":slots,"peak_live_result_batches":peak_batches,"owned_byte_diagnostic_enabled":measure_bytes,"peak_completed_result_owned_bytes":if measure_bytes {Some(peak_bytes)} else {None},"aggregate_worker_decode_seconds":decode_ns as f64/1e9,"aggregate_worker_admission_and_publish_seconds":worker_other_ns as f64/1e9,"aggregate_cleanup_worker_seconds":cleanup_ns as f64/1e9,"consumer_delivery_seconds":delivery_ns as f64/1e9,"process_user_cpu_seconds":cpu.map(|x|x.user_seconds),"process_system_cpu_seconds":cpu.map(|x|x.system_seconds),"average_busy_cores":cpu.map(|x|x.average_busy_cores(elapsed)),"process_counters":counts,"now_micros":NOW,"full_output_materialized":true,"ordered_delivery":true,"credits_released_after_destruction":true})
    );
    for tx in &senders {
        tx.send(Command::Stop).expect("worker closed");
    }
    for h in handles {
        h.join().expect("worker panic");
    }
    for tx in &cleaners {
        tx.send(CleanupCommand::Stop)
            .expect("cleanup worker closed");
    }
    for h in cleaner_handles {
        h.join().expect("cleanup worker panic");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_preserves_every_field_and_input_order_for_mixed_tail() {
        let vins = vec![
            "1HGCM82633A004352".to_owned(),
            "INVALID-VIN".to_owned(),
            "1HGCM82633A004352".to_owned(),
            "5YJSA1E26HF000337".to_owned(),
            "JH4KA9650MC012345".to_owned(),
        ];
        let actual = decode(&vins, 0, vins.len());
        let expected = vins
            .iter()
            .map(|vin| ultravin::decode_full(ultravin::Db::embedded(), vin, NOW, YEAR, None))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn actual_consumer_orders_reverse_completion_and_retains_cleanup_credits() {
        let vins = [
            "1HGCM82633A004352",
            "INVALID",
            "1HGCM82633A004352",
            "5YJSA1E26HF000337",
            "",
        ]
        .map(str::to_owned)
        .to_vec();
        let credits = new_credits(3, vins.len());
        let (tx, rx) = mpsc::channel();
        let mut completed = Vec::new();
        while let Some((id, start, end, permit)) = credits.claim(vins.len(), 2) {
            completed.push(Published {
                id,
                results: Ok((decode(&vins, start, end), permit)),
            });
        }
        for item in completed.into_iter().rev() {
            tx.send(item).unwrap_or_else(|_| panic!("consumer gone"));
        }
        drop(tx);
        let expected = decode(&vins, 0, vins.len());
        let mut offset = 0;
        let mut pending_cleanup = Vec::new();
        consume_ordered(3, &rx, &credits, |item| {
            assert_eq!(item.0, expected[offset..offset + item.0.len()]);
            offset += item.0.len();
            pending_cleanup.push(item);
        });
        assert_eq!(offset, vins.len());
        assert_eq!(credits.state.lock().unwrap().live_rows, vins.len());
        drop(pending_cleanup);
        assert_eq!(credits.state.lock().unwrap().live_rows, 0);
    }

    #[test]
    fn worker_panic_cancels_admission_and_unblocks_actual_consumer() {
        let (finished_tx, finished_rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let credits = new_credits(1, 1);
            let pass = Pass {
                rows: 3,
                batch: 1,
                credits: Arc::clone(&credits),
                measure_bytes: false,
            };
            let (tx, rx) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                worker_pass(&pass, &tx, |_, _| panic!("injected decode failure"))
            });
            let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                consume_ordered(3, &rx, &credits, |_| {
                    panic!("failed batch must not be emitted")
                });
            }));
            assert!(failed.is_err());
            assert_eq!(worker.join().unwrap().batches, 0);
            assert!(credits.claim(3, 1).is_none());
            assert_eq!(credits.state.lock().unwrap().live_rows, 0);
            finished_tx.send(()).unwrap();
        });
        finished_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("pipeline deadlocked after worker failure");
        handle.join().unwrap();
    }

    #[test]
    fn consumer_panic_cancels_waiting_worker_and_drops_pending_outputs() {
        let credits = new_credits(1, 1);
        let (id, _, _, permit) = credits.claim(2, 1).expect("first claim");
        let (tx, rx) = mpsc::channel();
        tx.send(Published {
            id,
            results: Ok((Vec::new(), permit)),
        })
        .unwrap_or_else(|_| panic!("consumer gone"));
        let waiter = Arc::clone(&credits);
        let (finished_tx, finished_rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            // Even if releasing the failed result wins the wake-up race, the next
            // admission must stop once the consumer's cancellation is visible.
            while let Some(claim) = waiter.claim(2, 1) {
                drop(claim);
            }
            finished_tx.send(()).unwrap();
        });
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            consume_ordered(2, &rx, &credits, |_| panic!("injected consumer failure"));
        }));
        assert!(failed.is_err());
        finished_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("admission stayed blocked after consumer failure");
        handle.join().unwrap();
        assert!(credits.state.lock().unwrap().cancelled);
        assert_eq!(credits.state.lock().unwrap().live_rows, 0);
    }

    #[test]
    fn row_credits_bound_partial_tail_and_terminate() {
        let credits = new_credits(3, 10);
        let first = credits.claim(17, 4).expect("first");
        let second = credits.claim(17, 4).expect("second");
        assert_eq!((first.1, first.2, second.1, second.2), (0, 4, 4, 8));
        drop(first.3);
        let third = credits.claim(17, 4).expect("third");
        assert_eq!((third.1, third.2), (8, 12));
        drop(second.3);
        drop(third.3);
        let fourth = credits.claim(17, 4).expect("fourth");
        let tail = credits.claim(17, 4).expect("tail");
        assert_eq!((fourth.1, fourth.2, tail.1, tail.2), (12, 16, 16, 17));
        drop(fourth.3);
        drop(tail.3);
        assert!(credits.claim(17, 4).is_none());
    }

    #[test]
    fn cancellation_wakes_blocked_claimant() {
        let credits = new_credits(1, 1);
        let held = credits.claim(2, 1).expect("first");
        let waiter = Arc::clone(&credits);
        let handle = std::thread::spawn(move || waiter.claim(2, 1));
        credits.cancel();
        assert!(handle.join().expect("claimant panicked").is_none());
        drop(held.3);
    }
}
