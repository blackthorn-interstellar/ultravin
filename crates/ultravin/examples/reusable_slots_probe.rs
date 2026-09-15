//! Bounded ordered delivery through reusable worker-owned result slots.

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
const ROW_BUDGET: usize = 12_000;

enum Command {
    Pass(Arc<Pass>),
    Stop,
}

struct Pass {
    rows: usize,
    batch: usize,
    coordinator: Arc<Coordinator>,
    measure_bytes: bool,
}

struct State {
    next_claim: usize,
    live_batches: usize,
    live_rows: usize,
    live_bytes: usize,
    peak_batches: usize,
    peak_rows: usize,
    peak_bytes: usize,
    generation: u64,
    cancelled: bool,
}

struct Coordinator {
    state: Mutex<State>,
    changed: Condvar,
    max_batches: usize,
    max_rows: usize,
}

enum Claim {
    Work {
        id: usize,
        start: usize,
        end: usize,
        permit: CreditPermit,
    },
    Blocked(u64),
    Finished,
    Cancelled,
}

impl Coordinator {
    fn try_claim(self: &Arc<Self>, rows: usize, batch: usize) -> Claim {
        let mut state = self.state.lock().expect("coordinator lock poisoned");
        if state.cancelled {
            return Claim::Cancelled;
        }
        let start = state.next_claim * batch;
        if start >= rows {
            return Claim::Finished;
        }
        let end = (start + batch).min(rows);
        let batch_rows = end - start;
        if state.live_batches >= self.max_batches || state.live_rows + batch_rows > self.max_rows {
            return Claim::Blocked(state.generation);
        }
        let id = state.next_claim;
        state.next_claim += 1;
        state.live_batches += 1;
        state.live_rows += batch_rows;
        state.peak_batches = state.peak_batches.max(state.live_batches);
        state.peak_rows = state.peak_rows.max(state.live_rows);
        Claim::Work {
            id,
            start,
            end,
            permit: CreditPermit {
                coordinator: Arc::clone(self),
                rows: batch_rows,
                bytes: 0,
            },
        }
    }

    fn wait_for_change(&self, generation: u64) {
        let state = self.state.lock().expect("coordinator lock poisoned");
        let _guard = self
            .changed
            .wait_while(state, |state| {
                !state.cancelled && state.generation == generation
            })
            .expect("coordinator lock poisoned");
    }

    fn generation(&self) -> u64 {
        self.state
            .lock()
            .expect("coordinator lock poisoned")
            .generation
    }

    fn notify_progress(&self) {
        let mut state = self.state.lock().expect("coordinator lock poisoned");
        state.generation = state.generation.wrapping_add(1);
        self.changed.notify_all();
    }

    fn cancel(&self) {
        let mut state = self.state.lock().expect("coordinator lock poisoned");
        state.cancelled = true;
        state.generation = state.generation.wrapping_add(1);
        self.changed.notify_all();
    }

    fn is_cancelled(&self) -> bool {
        self.state
            .lock()
            .expect("coordinator lock poisoned")
            .cancelled
    }

    fn publish_bytes(&self, bytes: usize) {
        let mut state = self.state.lock().expect("coordinator lock poisoned");
        state.live_bytes += bytes;
        state.peak_bytes = state.peak_bytes.max(state.live_bytes);
    }

    fn release(&self, rows: usize, bytes: usize) {
        let mut state = self.state.lock().expect("coordinator lock poisoned");
        state.live_batches -= 1;
        state.live_rows -= rows;
        state.live_bytes -= bytes;
        state.generation = state.generation.wrapping_add(1);
        self.changed.notify_all();
    }
}

struct CreditPermit {
    coordinator: Arc<Coordinator>,
    rows: usize,
    bytes: usize,
}

impl CreditPermit {
    fn record_bytes(&mut self, bytes: usize) {
        self.bytes = bytes;
        if bytes != 0 {
            self.coordinator.publish_bytes(bytes);
        }
    }
}

impl Drop for CreditPermit {
    fn drop(&mut self) {
        self.coordinator.release(self.rows, self.bytes);
    }
}

struct CancelOnDrop {
    coordinator: Arc<Coordinator>,
    armed: bool,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.coordinator.cancel();
        }
    }
}

enum SlotState {
    Free,
    Published { len: usize, permit: CreditPermit },
    Recycle { len: usize, permit: CreditPermit },
}

struct Slot {
    results: Vec<Option<ultravin::DecodeResult<'static>>>,
    state: SlotState,
}

impl Slot {
    fn new(batch: usize) -> Self {
        Self {
            results: (0..batch).map(|_| None).collect(),
            state: SlotState::Free,
        }
    }
}

struct Published {
    id: usize,
    owner: usize,
    slot: usize,
    failed: bool,
}

struct WorkerDone {
    decode_ns: u64,
    wait_ns: u64,
    publish_ns: u64,
    cleanup_ns: u64,
    batches: usize,
    reused_batches: usize,
}

// The memory diagnostic must distinguish owned capacity from borrowed text.
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

fn slots_bytes(slot: &Slot, len: usize) -> usize {
    slot.results.capacity() * std::mem::size_of::<Option<ultravin::DecodeResult<'_>>>()
        + slot.results[..len]
            .iter()
            .flatten()
            .map(result_bytes)
            .sum::<usize>()
}

fn fill_slot(vins: &[String], start: usize, end: usize, slot: &mut Slot) {
    let db = ultravin::Db::embedded();
    fill_slot_with(vins, start, end, slot, |vin| {
        ultravin::decode_full(db, vin, NOW, YEAR, None)
    });
}

fn fill_slot_with(
    vins: &[String],
    start: usize,
    end: usize,
    slot: &mut Slot,
    mut decode: impl FnMut(&str) -> ultravin::DecodeResult<'static>,
) {
    let len = end - start;
    debug_assert!(slot.results.iter().all(Option::is_none));
    let mut order = (start..end).collect::<Vec<_>>();
    order.sort_unstable_by_key(|&index| {
        let mut key = [0_u8; 8];
        let bytes = vins[index].as_bytes();
        let len = bytes.len().min(8);
        key[..len].copy_from_slice(&bytes[..len]);
        u64::from_be_bytes(key)
    });
    for index in order {
        slot.results[index - start] = Some(decode(&vins[index]));
    }
    debug_assert!(slot.results[..len].iter().all(Option::is_some));
}

fn new_coordinator(max_batches: usize, max_rows: usize) -> Arc<Coordinator> {
    Arc::new(Coordinator {
        state: Mutex::new(State {
            next_claim: 0,
            live_batches: 0,
            live_rows: 0,
            live_bytes: 0,
            peak_batches: 0,
            peak_rows: 0,
            peak_bytes: 0,
            generation: 0,
            cancelled: false,
        }),
        changed: Condvar::new(),
        max_batches,
        max_rows,
    })
}

fn recycle_slots(
    slots: &[Arc<Mutex<Slot>>],
    free: &mut Vec<usize>,
    cancelled: bool,
) -> (usize, u64) {
    let started = Instant::now();
    let mut recycled = 0;
    for (index, slot) in slots.iter().enumerate() {
        let mut slot = slot.lock().expect("slot lock poisoned");
        let recyclable = matches!(slot.state, SlotState::Recycle { .. })
            || (cancelled && matches!(slot.state, SlotState::Published { .. }));
        if !recyclable {
            continue;
        }
        let state = std::mem::replace(&mut slot.state, SlotState::Free);
        let (len, permit) = match state {
            SlotState::Published { len, permit } | SlotState::Recycle { len, permit } => {
                (len, permit)
            }
            SlotState::Free => unreachable!(),
        };
        for result in &mut slot.results[..len] {
            drop(result.take());
        }
        drop(permit);
        free.push(index);
        recycled += 1;
    }
    (recycled, started.elapsed().as_nanos() as u64)
}

fn worker_pass(
    owner: usize,
    vins: &[String],
    pass: &Pass,
    slots: &[Arc<Mutex<Slot>>],
    published: &mpsc::Sender<Published>,
) -> WorkerDone {
    let mut free = (0..slots.len()).rev().collect::<Vec<_>>();
    let mut stats = WorkerDone {
        decode_ns: 0,
        wait_ns: 0,
        publish_ns: 0,
        cleanup_ns: 0,
        batches: 0,
        reused_batches: 0,
    };
    loop {
        let cancelled = pass.coordinator.is_cancelled();
        let (recycled, cleanup_ns) = recycle_slots(slots, &mut free, cancelled);
        stats.cleanup_ns += cleanup_ns;
        stats.reused_batches += recycled;
        if cancelled {
            break;
        }
        if free.is_empty() {
            let generation = pass.coordinator.generation();
            let (recycled, cleanup_ns) = recycle_slots(slots, &mut free, false);
            stats.cleanup_ns += cleanup_ns;
            stats.reused_batches += recycled;
            if free.is_empty() {
                let started = Instant::now();
                pass.coordinator.wait_for_change(generation);
                stats.wait_ns += started.elapsed().as_nanos() as u64;
            }
            continue;
        }
        match pass.coordinator.try_claim(pass.rows, pass.batch) {
            Claim::Work {
                id,
                start,
                end,
                mut permit,
            } => {
                let slot_index = free.pop().expect("free slot");
                let started = Instant::now();
                let mut slot = slots[slot_index].lock().expect("slot lock poisoned");
                let decoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    fill_slot(vins, start, end, &mut slot);
                }));
                if decoded.is_ok() {
                    if pass.measure_bytes {
                        permit.record_bytes(slots_bytes(&slot, end - start));
                    }
                    slot.state = SlotState::Published {
                        len: end - start,
                        permit,
                    };
                } else {
                    for result in &mut slot.results[..end - start] {
                        drop(result.take());
                    }
                    drop(permit);
                }
                drop(slot);
                stats.decode_ns += started.elapsed().as_nanos() as u64;
                if decoded.is_err() {
                    pass.coordinator.cancel();
                    let _ = published.send(Published {
                        id,
                        owner,
                        slot: slot_index,
                        failed: true,
                    });
                    continue;
                }
                let started = Instant::now();
                if published
                    .send(Published {
                        id,
                        owner,
                        slot: slot_index,
                        failed: false,
                    })
                    .is_err()
                {
                    pass.coordinator.cancel();
                }
                stats.publish_ns += started.elapsed().as_nanos() as u64;
                stats.batches += 1;
            }
            Claim::Blocked(generation) => {
                let (recycled, cleanup_ns) = recycle_slots(slots, &mut free, false);
                stats.cleanup_ns += cleanup_ns;
                stats.reused_batches += recycled;
                let started = Instant::now();
                pass.coordinator.wait_for_change(generation);
                stats.wait_ns += started.elapsed().as_nanos() as u64;
            }
            Claim::Finished => {
                if free.len() == slots.len() {
                    break;
                }
                let generation = pass.coordinator.generation();
                let (recycled, cleanup_ns) = recycle_slots(slots, &mut free, false);
                stats.cleanup_ns += cleanup_ns;
                stats.reused_batches += recycled;
                if free.len() != slots.len() {
                    let started = Instant::now();
                    pass.coordinator.wait_for_change(generation);
                    stats.wait_ns += started.elapsed().as_nanos() as u64;
                }
            }
            Claim::Cancelled => continue,
        }
    }
    stats
}

fn consume_ordered(
    batches: usize,
    published: &mpsc::Receiver<Published>,
    all_slots: &[Vec<Arc<Mutex<Slot>>>],
    coordinator: &Arc<Coordinator>,
    mut consume: impl FnMut(usize, &[Option<ultravin::DecodeResult<'static>>]),
) {
    let mut guard = CancelOnDrop {
        coordinator: Arc::clone(coordinator),
        armed: true,
    };
    let mut waiting = BTreeMap::new();
    let mut next = 0;
    while next < batches {
        let item = published.recv().expect("publisher closed");
        assert!(!item.failed, "decode worker failed");
        assert!(item.id >= next && item.id < batches, "invalid batch index");
        assert!(waiting.insert(item.id, item).is_none(), "duplicate batch");
        while let Some(item) = waiting.remove(&next) {
            let mut slot = all_slots[item.owner][item.slot]
                .lock()
                .expect("slot lock poisoned");
            let state = std::mem::replace(&mut slot.state, SlotState::Free);
            let (len, permit) = match state {
                SlotState::Published { len, permit } => (len, permit),
                _ => panic!("published descriptor referenced unavailable slot"),
            };
            let consumed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                consume(next, &slot.results[..len]);
            }));
            slot.state = SlotState::Recycle { len, permit };
            drop(slot);
            coordinator.notify_progress();
            if let Err(payload) = consumed {
                std::panic::resume_unwind(payload);
            }
            next += 1;
        }
    }
    guard.armed = false;
}

// Keep the explicit persistent-worker endpoints visible in this probe.
#[allow(clippy::too_many_arguments)]
fn run_pass(
    vins: &Arc<Vec<String>>,
    batch: usize,
    senders: &[mpsc::SyncSender<Command>],
    published: &mpsc::Receiver<Published>,
    done: &mpsc::Receiver<WorkerDone>,
    all_slots: &[Vec<Arc<Mutex<Slot>>>],
    max_batches: usize,
    measure_bytes: bool,
) -> (u64, u64, u64, u64, usize, usize, usize) {
    let batches = vins.len().div_ceil(batch);
    let coordinator = new_coordinator(max_batches, ROW_BUDGET);
    let mut cancel_guard = CancelOnDrop {
        coordinator: Arc::clone(&coordinator),
        armed: true,
    };
    let pass = Arc::new(Pass {
        rows: vins.len(),
        batch,
        coordinator: Arc::clone(&coordinator),
        measure_bytes,
    });
    for sender in senders {
        sender
            .send(Command::Pass(Arc::clone(&pass)))
            .expect("worker closed");
    }
    let started = Instant::now();
    consume_ordered(batches, published, all_slots, &coordinator, |_, results| {
        std::hint::black_box(results);
    });
    let delivery_ns = started.elapsed().as_nanos() as u64;
    let mut worker_decode = 0;
    let mut worker_wait_publish = 0;
    let mut worker_cleanup = 0;
    let mut reused = 0;
    for _ in senders {
        let stats = done.recv().expect("worker done closed");
        worker_decode += stats.decode_ns;
        worker_wait_publish += stats.wait_ns + stats.publish_ns;
        worker_cleanup += stats.cleanup_ns;
        reused += stats.reused_batches;
        assert!(stats.batches <= batches);
    }
    let state = coordinator.state.lock().expect("coordinator lock poisoned");
    assert_eq!(
        (state.live_batches, state.live_rows, state.next_claim),
        (0, 0, batches)
    );
    cancel_guard.armed = false;
    (
        delivery_ns,
        worker_decode,
        worker_wait_publish,
        worker_cleanup,
        state.peak_batches,
        state.peak_rows,
        if measure_bytes {
            state.peak_bytes
        } else {
            reused
        },
    )
}

fn shared_pass(vins: &[String], measure_bytes: bool) -> usize {
    let mut peak = 0;
    for chunk in vins.chunks(ROW_BUDGET) {
        let results = ultravin::decode_batch_managed_at(chunk, None, NOW);
        if measure_bytes {
            peak = peak.max(
                results.capacity() * std::mem::size_of::<ultravin::DecodeResult<'_>>()
                    + results.iter().map(result_bytes).sum::<usize>(),
            );
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
    let corpus = std::fs::read_to_string(path).expect("read corpus");
    let vins = Arc::new(corpus.lines().map(str::to_owned).collect::<Vec<_>>());
    drop(corpus);
    assert!(workers > 0 && batch > 0 && batch <= ROW_BUDGET && !vins.is_empty());
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
            serde_json::json!({"mode":"shared","workers":workers,"batch_size":ROW_BUDGET,"rows":vins.len(),"elapsed_seconds":elapsed,"rows_per_second":vins.len() as f64/elapsed,"max_output_owned_bytes":peak,"process_user_cpu_seconds":cpu.map(|x|x.user_seconds),"process_system_cpu_seconds":cpu.map(|x|x.system_seconds),"average_busy_cores":cpu.map(|x|x.average_busy_cores(elapsed)),"process_counters":counts,"now_micros":NOW,"ordered_delivery":true})
        );
        return;
    }
    assert_eq!(mode, "slots");
    let batches = vins.len().div_ceil(batch);
    let max_batches = ROW_BUDGET.div_ceil(batch).min(batches);
    let slots_per_owner = max_batches.div_ceil(workers).max(2);
    let physical_slots = slots_per_owner * workers;
    let admission_batch_cap = physical_slots.min(batches);
    let all_slots = (0..workers)
        .map(|_| {
            (0..slots_per_owner)
                .map(|_| Arc::new(Mutex::new(Slot::new(batch))))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let (pub_tx, pub_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::sync_channel(workers);
    let mut senders = Vec::new();
    let mut handles = Vec::new();
    for (owner, owner_slots) in all_slots.iter().cloned().enumerate() {
        let (tx, rx) = mpsc::sync_channel(0);
        senders.push(tx);
        let vins = Arc::clone(&vins);
        let pub_tx = pub_tx.clone();
        let done_tx = done_tx.clone();
        handles.push(std::thread::spawn(move || loop {
            match rx.recv().expect("command closed") {
                Command::Stop => break,
                Command::Pass(pass) => {
                    let stats = worker_pass(owner, &vins, &pass, &owner_slots, &pub_tx);
                    done_tx.send(stats).expect("done closed");
                }
            }
        }));
    }
    drop(pub_tx);
    drop(done_tx);
    run_pass(
        &vins,
        batch,
        &senders,
        &pub_rx,
        &done_rx,
        &all_slots,
        admission_batch_cap,
        false,
    );
    eprintln!("warmup complete; starting timed whole-corpus pass");
    let cpu_start = cpu::snapshot();
    let counters_start = counters::snapshot();
    let started = Instant::now();
    let (delivery_ns, decode_ns, wait_ns, cleanup_ns, peak_batches, peak_rows, final_metric) =
        run_pass(
            &vins,
            batch,
            &senders,
            &pub_rx,
            &done_rx,
            &all_slots,
            admission_batch_cap,
            measure_bytes,
        );
    let elapsed = started.elapsed().as_secs_f64();
    let cpu = cpu::elapsed(cpu_start, cpu::snapshot());
    let counts = counters::elapsed(counters_start, counters::snapshot());
    println!(
        "{}",
        serde_json::json!({"mode":"slots","workers":workers,"active_threads_including_consumer":workers+1,"batch_size":batch,"slots_per_owner":slots_per_owner,"physical_slots":physical_slots,"admission_batch_cap":admission_batch_cap,"rows":vins.len(),"elapsed_seconds":elapsed,"rows_per_second":vins.len() as f64/elapsed,"live_result_row_budget":ROW_BUDGET,"physical_slot_row_capacity":physical_slots*batch,"peak_live_result_batches":peak_batches,"peak_live_result_rows":peak_rows,"peak_completed_result_owned_bytes_excluding_construction_and_empty_preallocation":if measure_bytes {Some(final_metric)} else {None},"recycled_batches":if measure_bytes {None} else {Some(final_metric)},"aggregate_worker_decode_seconds":decode_ns as f64/1e9,"aggregate_worker_wait_and_publish_seconds":wait_ns as f64/1e9,"aggregate_worker_recycle_scan_and_cleanup_seconds":cleanup_ns as f64/1e9,"consumer_delivery_seconds":delivery_ns as f64/1e9,"process_user_cpu_seconds":cpu.map(|x|x.user_seconds),"process_system_cpu_seconds":cpu.map(|x|x.system_seconds),"average_busy_cores":cpu.map(|x|x.average_busy_cores(elapsed)),"process_counters":counts,"now_micros":NOW,"full_output_materialized":true,"ordered_delivery":true,"credits_released_after_owner_destruction":true,"unsafe_used":false})
    );
    for tx in &senders {
        tx.send(Command::Stop).expect("worker closed");
    }
    for handle in handles {
        handle.join().expect("worker panic");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_vins() -> Vec<String> {
        [
            "1HGCM82633A004352",
            "INVALID",
            "1HGCM82633A004352",
            "",
            "5YJSA1E26HF000337",
        ]
        .map(str::to_owned)
        .to_vec()
    }

    #[test]
    fn direct_slot_fill_preserves_every_field_mixed_duplicates_and_tail() {
        let vins = sample_vins();
        let mut slot = Slot::new(vins.len() + 2);
        fill_slot(&vins, 0, vins.len(), &mut slot);
        let actual = slot.results[..vins.len()]
            .iter()
            .map(|x| x.as_ref().unwrap())
            .collect::<Vec<_>>();
        let expected = vins
            .iter()
            .map(|vin| ultravin::decode_full(ultravin::Db::embedded(), vin, NOW, YEAR, None))
            .collect::<Vec<_>>();
        assert!(actual.iter().zip(&expected).all(|(a, b)| *a == b));
    }

    #[test]
    fn reverse_completion_is_consumed_in_input_order_and_recycled_by_owner() {
        let vins = sample_vins();
        let coordinator = new_coordinator(3, 5);
        let slots = vec![(0..3)
            .map(|_| Arc::new(Mutex::new(Slot::new(2))))
            .collect::<Vec<_>>()];
        for expected_id in 0..3 {
            let Claim::Work {
                id,
                start,
                end,
                permit,
            } = coordinator.try_claim(5, 2)
            else {
                panic!("claim")
            };
            assert_eq!(id, expected_id);
            let mut slot = slots[0][id].lock().unwrap();
            fill_slot(&vins, start, end, &mut slot);
            slot.state = SlotState::Published {
                len: end - start,
                permit,
            };
            drop(slot);
        }
        let mut order = Vec::new();
        let (tx, rx) = mpsc::channel();
        for id in (0..3).rev() {
            tx.send(Published {
                id,
                owner: 0,
                slot: id,
                failed: false,
            })
            .unwrap();
        }
        drop(tx);
        let mut actual = Vec::new();
        consume_ordered(3, &rx, &slots, &coordinator, |id, rows| {
            order.push(id);
            actual.extend(
                rows.iter()
                    .flatten()
                    .map(|row| serde_json::to_value(row).unwrap()),
            );
        });
        assert_eq!(order, vec![0, 1, 2]);
        let expected = vins
            .iter()
            .map(|vin| {
                serde_json::to_value(ultravin::decode_full(
                    ultravin::Db::embedded(),
                    vin,
                    NOW,
                    YEAR,
                    None,
                ))
                .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
        let mut free = Vec::new();
        recycle_slots(&slots[0], &mut free, false);
        assert_eq!(free.len(), 3);
        assert_eq!(coordinator.state.lock().unwrap().live_rows, 0);
    }

    #[test]
    fn cleanup_releases_credit_only_after_nested_result_destruction() {
        let coordinator = new_coordinator(1, 1);
        let Claim::Work { permit, .. } = coordinator.try_claim(1, 1) else {
            panic!("claim")
        };
        let slots = vec![Arc::new(Mutex::new(Slot::new(1)))];
        let mut slot = slots[0].lock().unwrap();
        fill_slot(&["1HGCM82633A004352".to_owned()], 0, 1, &mut slot);
        slot.state = SlotState::Recycle { len: 1, permit };
        drop(slot);
        assert_eq!(coordinator.state.lock().unwrap().live_rows, 1);
        recycle_slots(&slots, &mut Vec::new(), false);
        assert!(slots[0].lock().unwrap().results[0].is_none());
        assert_eq!(coordinator.state.lock().unwrap().live_rows, 0);
    }

    #[test]
    fn blocked_admission_wakes_for_owner_cleanup_without_deadlock() {
        let coordinator = new_coordinator(1, 1);
        let Claim::Work { permit, .. } = coordinator.try_claim(2, 1) else {
            panic!("claim")
        };
        let generation = match coordinator.try_claim(2, 1) {
            Claim::Blocked(g) => g,
            _ => panic!("blocked"),
        };
        let waiter = Arc::clone(&coordinator);
        let handle = std::thread::spawn(move || {
            waiter.wait_for_change(generation);
            waiter.try_claim(2, 1)
        });
        let slots = vec![Arc::new(Mutex::new(Slot::new(1)))];
        slots[0].lock().unwrap().state = SlotState::Recycle { len: 0, permit };
        recycle_slots(&slots, &mut Vec::new(), false);
        assert!(matches!(handle.join().unwrap(), Claim::Work { .. }));
    }

    #[test]
    fn cancellation_wakes_waiter_and_allows_published_cleanup() {
        let coordinator = new_coordinator(1, 1);
        let Claim::Work { permit, .. } = coordinator.try_claim(2, 1) else {
            panic!("claim")
        };
        let generation = coordinator.generation();
        let waiter = Arc::clone(&coordinator);
        let handle = std::thread::spawn(move || waiter.wait_for_change(generation));
        let slots = vec![Arc::new(Mutex::new(Slot::new(1)))];
        slots[0].lock().unwrap().state = SlotState::Published { len: 0, permit };
        coordinator.cancel();
        handle.join().unwrap();
        recycle_slots(&slots, &mut Vec::new(), true);
        assert_eq!(coordinator.state.lock().unwrap().live_rows, 0);
    }

    #[test]
    fn mid_batch_panic_drops_partial_results_without_poisoning_slot() {
        let vins = sample_vins();
        let slot = Arc::new(Mutex::new(Slot::new(vins.len())));
        let mut guard = slot.lock().unwrap();
        let mut calls = 0;
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            fill_slot_with(&vins, 0, vins.len(), &mut guard, |vin| {
                calls += 1;
                if calls == 3 {
                    panic!("injected mid-batch failure");
                }
                ultravin::decode_full(ultravin::Db::embedded(), vin, NOW, YEAR, None)
            });
        }));
        assert!(failed.is_err());
        for result in &mut guard.results {
            drop(result.take());
        }
        drop(guard);
        assert!(
            slot.lock().is_ok(),
            "caught panic must not poison owner slot"
        );
    }

    #[test]
    fn repeated_actual_pipeline_drains_final_slots_and_reuses_them() {
        let (finished_tx, finished_rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let vins = Arc::new(
                sample_vins()
                    .into_iter()
                    .cycle()
                    .take(17)
                    .collect::<Vec<_>>(),
            );
            let workers = 2;
            let batch = 2;
            let all_slots = (0..workers)
                .map(|_| {
                    (0..2)
                        .map(|_| Arc::new(Mutex::new(Slot::new(batch))))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let (pub_tx, pub_rx) = mpsc::channel();
            let (done_tx, done_rx) = mpsc::sync_channel(workers);
            let mut senders = Vec::new();
            let mut handles = Vec::new();
            for (owner, owner_slots) in all_slots.iter().cloned().enumerate() {
                let (tx, rx) = mpsc::sync_channel(0);
                senders.push(tx);
                let vins = Arc::clone(&vins);
                let pub_tx = pub_tx.clone();
                let done_tx = done_tx.clone();
                handles.push(std::thread::spawn(move || loop {
                    match rx.recv().unwrap() {
                        Command::Stop => break,
                        Command::Pass(pass) => done_tx
                            .send(worker_pass(owner, &vins, &pass, &owner_slots, &pub_tx))
                            .unwrap(),
                    }
                }));
            }
            drop(pub_tx);
            drop(done_tx);
            for _ in 0..2 {
                let result = run_pass(
                    &vins, batch, &senders, &pub_rx, &done_rx, &all_slots, 2, false,
                );
                assert!((1..=4).contains(&result.5));
                assert_eq!(result.6, vins.len().div_ceil(batch));
                assert!(all_slots.iter().flatten().all(|slot| {
                    let slot = slot.lock().unwrap();
                    matches!(slot.state, SlotState::Free)
                        && slot.results.iter().all(Option::is_none)
                }));
            }
            for tx in &senders {
                tx.send(Command::Stop).unwrap();
            }
            for handle in handles {
                handle.join().unwrap();
            }
            finished_tx.send(()).unwrap();
        });
        finished_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("actual repeated pipeline deadlocked");
        handle.join().unwrap();
    }

    #[test]
    fn consumer_panic_leaves_unpoisoned_recyclable_slot_for_owner() {
        let vins = sample_vins();
        let coordinator = new_coordinator(1, 1);
        let Claim::Work {
            id,
            start,
            end,
            permit,
        } = coordinator.try_claim(1, 1)
        else {
            panic!("claim")
        };
        let slots = vec![vec![Arc::new(Mutex::new(Slot::new(1)))]];
        let mut slot = slots[0][0].lock().unwrap();
        fill_slot(&vins, start, end, &mut slot);
        slot.state = SlotState::Published { len: 1, permit };
        drop(slot);
        let (tx, rx) = mpsc::channel();
        tx.send(Published {
            id,
            owner: 0,
            slot: 0,
            failed: false,
        })
        .unwrap();
        drop(tx);
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            consume_ordered(1, &rx, &slots, &coordinator, |_, _| {
                panic!("injected consumer failure")
            });
        }));
        assert!(failed.is_err());
        assert!(matches!(
            slots[0][0].lock().unwrap().state,
            SlotState::Recycle { .. }
        ));
        recycle_slots(&slots[0], &mut Vec::new(), true);
        assert_eq!(coordinator.state.lock().unwrap().live_rows, 0);
    }
}
