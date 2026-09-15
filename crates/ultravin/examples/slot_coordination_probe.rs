//! Atomic admission and fixed-ring experiments for reusable worker-owned slots.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

fn partition_slots(physical_slots: usize, workers: usize) -> Vec<usize> {
    assert!(workers > 0, "at least one worker is required");
    let base_slots = physical_slots / workers;
    let extra_slots = physical_slots % workers;
    (0..workers)
        .map(|owner| base_slots + usize::from(owner < extra_slots))
        .collect()
}

enum Command {
    Pass(Arc<Pass>),
    Stop,
}

#[cfg(test)]
mod atomic_tests {
    use super::*;

    fn sample_vins(rows: usize) -> Arc<Vec<String>> {
        Arc::new(
            [
                "1HGCM82633A004352",
                "INVALID",
                "1HGCM82633A004352",
                "",
                "5YJSA1E26HF000337",
            ]
            .into_iter()
            .cycle()
            .take(rows)
            .map(str::to_owned)
            .collect(),
        )
    }

    fn actual_pass(ring: bool) {
        let vins = sample_vins(37);
        let workers = 2;
        let batch = 3;
        let quotas = [2, 2];
        let all_slots = quotas
            .iter()
            .map(|&quota| {
                (0..quota)
                    .map(|_| Arc::new(Mutex::new(Slot::new(batch))))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let wakes = (0..workers)
            .map(|_| Arc::new(OwnerWake::new()))
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
            let wake = Arc::clone(&wakes[owner]);
            handles.push(std::thread::spawn(move || loop {
                match rx.recv().expect("command") {
                    Command::Stop => break,
                    Command::Pass(pass) => done_tx
                        .send(worker_pass(
                            owner,
                            &vins,
                            &pass,
                            &owner_slots,
                            &wake,
                            &pub_tx,
                        ))
                        .expect("done"),
                }
            }));
        }
        drop(pub_tx);
        drop(done_tx);
        for _ in 0..3 {
            let result = run_pass(
                &vins, batch, &senders, &pub_rx, &done_rx, &all_slots, &wakes, 4, ring, false,
            );
            assert_eq!(result.6, vins.len().div_ceil(batch));
            assert!(all_slots.iter().flatten().all(|slot| {
                let slot = slot.lock().expect("slot");
                matches!(slot.state, SlotState::Free) && slot.results.iter().all(Option::is_none)
            }));
        }
        for tx in &senders {
            tx.send(Command::Stop).expect("stop");
        }
        for handle in handles {
            handle.join().expect("worker");
        }
    }

    fn assert_completes(action: impl FnOnce() + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            action();
            tx.send(()).expect("completion");
        });
        rx.recv_timeout(std::time::Duration::from_secs(10))
            .expect("worker path deadlocked");
        handle.join().expect("worker path panicked");
    }

    #[test]
    fn repeated_actual_worker_path_wraps_map_beyond_capacity() {
        assert_completes(|| actual_pass(false));
    }

    #[test]
    fn repeated_actual_worker_path_wraps_ring_beyond_capacity() {
        assert_completes(|| actual_pass(true));
    }

    #[test]
    fn full_fields_match_for_invalid_duplicates_and_tail() {
        let vins = sample_vins(17);
        let mut slot = Slot::new(20);
        fill_slot(&vins, 0, vins.len(), &mut slot);
        let expected = vins
            .iter()
            .map(|vin| ultravin::decode_full(ultravin::Db::embedded(), vin, NOW, YEAR, None))
            .collect::<Vec<_>>();
        let actual = slot.results[..vins.len()]
            .iter()
            .map(|result| result.as_ref().expect("filled result"))
            .collect::<Vec<_>>();
        assert_eq!(actual.len(), expected.len());
        assert!(actual
            .into_iter()
            .zip(expected)
            .all(|(actual, expected)| *actual == expected));
    }

    #[test]
    fn quotas_are_exact_for_b200() {
        let quotas12 = partition_slots(60, 12);
        let quotas8 = partition_slots(60, 8);
        assert_eq!(quotas12, vec![5; 12]);
        assert_eq!(quotas8, vec![8, 8, 8, 8, 7, 7, 7, 7]);
        assert_eq!(quotas8.iter().sum::<usize>() * 200, ROW_BUDGET);
        assert_eq!(partition_slots(2, 2), vec![1, 1]);
        assert_eq!(partition_slots(1, 3), vec![1, 0, 0]);
        let uneven = partition_slots(ROW_BUDGET / 700, 8);
        assert_eq!(uneven, vec![3, 2, 2, 2, 2, 2, 2, 2]);
        assert!(uneven.iter().sum::<usize>() * 700 <= ROW_BUDGET);
    }

    #[test]
    fn zero_quota_owner_finishes_without_waiting() {
        let pass = Pass {
            rows: 1,
            batch: 1,
            coordinator: new_coordinator(1, 1),
            wakes: vec![Arc::new(OwnerWake::new())],
            measure_bytes: false,
        };
        let (tx, _rx) = mpsc::channel();
        let stats = worker_pass(0, &sample_vins(1), &pass, &[], &pass.wakes[0], &tx);
        assert_eq!((stats.batches, stats.wait_ns), (0, 0));
    }

    #[test]
    fn caught_mid_batch_failure_clears_partial_slot_without_poisoning() {
        let vins = sample_vins(9);
        let slot = Arc::new(Mutex::new(Slot::new(vins.len())));
        let mut guard = slot.lock().expect("slot");
        let mut calls = 0;
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            fill_slot_with(&vins, 0, vins.len(), &mut guard, |vin| {
                calls += 1;
                assert_ne!(calls, 4, "injected decode failure");
                ultravin::decode_full(ultravin::Db::embedded(), vin, NOW, YEAR, None)
            });
        }));
        assert!(failed.is_err());
        for result in &mut guard.results {
            drop(result.take());
        }
        drop(guard);
        assert!(slot
            .lock()
            .expect("unpoisoned caught failure")
            .results
            .iter()
            .all(Option::is_none));
    }

    #[test]
    fn forced_reverse_ring_completion_preserves_full_fields() {
        let vins = sample_vins(5);
        let coordinator = new_coordinator(3, 5);
        let wakes = vec![Arc::new(OwnerWake::new())];
        let pass = Arc::new(Pass {
            rows: 5,
            batch: 2,
            coordinator: Arc::clone(&coordinator),
            wakes,
            measure_bytes: false,
        });
        let slots = vec![(0..3)
            .map(|_| Arc::new(Mutex::new(Slot::new(2))))
            .collect::<Vec<_>>()];
        let (tx, rx) = mpsc::channel();
        let mut published = Vec::new();
        for slot_index in 0..3 {
            let Claim::Work {
                id,
                start,
                end,
                permit,
            } = coordinator.try_claim(5, 2, false)
            else {
                panic!("claim")
            };
            let mut slot = slots[0][slot_index].lock().expect("slot");
            fill_slot(&vins, start, end, &mut slot);
            slot.state = SlotState::Published {
                len: end - start,
                permit,
            };
            published.push(Published {
                id,
                owner: 0,
                slot: slot_index,
                failed: false,
            });
        }
        for item in published.into_iter().rev() {
            tx.send(item).expect("publish");
        }
        drop(tx);
        let mut actual = Vec::new();
        consume_ordered(3, &rx, &slots, &pass, Some(3), |_, rows| {
            actual.extend(
                rows.iter()
                    .map(|row| serde_json::to_value(row.as_ref().expect("result")).expect("json")),
            )
        });
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
                .expect("json")
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn consumer_panic_cancels_and_wakes_every_owner() {
        let coordinator = new_coordinator(1, 1);
        let wakes = (0..2)
            .map(|_| Arc::new(OwnerWake::new()))
            .collect::<Vec<_>>();
        let pass = Arc::new(Pass {
            rows: 1,
            batch: 1,
            coordinator: Arc::clone(&coordinator),
            wakes: wakes.clone(),
            measure_bytes: false,
        });
        let vins = sample_vins(1);
        let slots = vec![vec![Arc::new(Mutex::new(Slot::new(1)))], vec![]];
        let Claim::Work {
            id,
            start,
            end,
            permit,
        } = coordinator.try_claim(1, 1, false)
        else {
            panic!("claim")
        };
        let mut slot = slots[0][0].lock().expect("slot");
        fill_slot(&vins, start, end, &mut slot);
        slot.state = SlotState::Published { len: 1, permit };
        drop(slot);
        let (wake_tx, wake_rx) = mpsc::channel();
        let waiters = wakes
            .iter()
            .cloned()
            .map(|wake| {
                let coordinator = Arc::clone(&coordinator);
                let wake_tx = wake_tx.clone();
                std::thread::spawn(move || {
                    let seen = wake.snapshot();
                    wake.wait(seen, &coordinator.cancelled);
                    wake_tx.send(()).expect("wake");
                })
            })
            .collect::<Vec<_>>();
        let (tx, rx) = mpsc::channel();
        tx.send(Published {
            id,
            owner: 0,
            slot: 0,
            failed: false,
        })
        .expect("publish");
        drop(tx);
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            consume_ordered(1, &rx, &slots, &pass, Some(1), |_, _| {
                panic!("consumer failure")
            })
        }));
        assert!(failed.is_err());
        for _ in &waiters {
            wake_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("owner not woken");
        }
        for waiter in waiters {
            waiter.join().expect("waiter");
        }
    }
}

struct Pass {
    rows: usize,
    batch: usize,
    coordinator: Arc<Coordinator>,
    wakes: Vec<Arc<OwnerWake>>,
    measure_bytes: bool,
}

struct State {
    live_bytes: usize,
    peak_bytes: usize,
}

struct Coordinator {
    state: Mutex<State>,
    next_claim: AtomicUsize,
    cancelled: AtomicBool,
}

struct OwnerWake {
    generation: Mutex<u64>,
    changed: Condvar,
}

enum Claim {
    Work {
        id: usize,
        start: usize,
        end: usize,
        permit: CreditPermit,
    },
    Finished,
    Cancelled,
}

impl Coordinator {
    fn try_claim(self: &Arc<Self>, rows: usize, batch: usize, measure_bytes: bool) -> Claim {
        if self.cancelled.load(Ordering::Acquire) {
            return Claim::Cancelled;
        }
        let id = self.next_claim.fetch_add(1, Ordering::Relaxed);
        if id >= rows.div_ceil(batch) {
            return Claim::Finished;
        }
        let start = id * batch;
        let end = (start + batch).min(rows);
        Claim::Work {
            id,
            start,
            end,
            permit: CreditPermit {
                coordinator: measure_bytes.then(|| Arc::clone(self)),
                bytes: 0,
            },
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn publish_bytes(&self, bytes: usize) {
        let mut state = self.state.lock().expect("coordinator lock poisoned");
        state.live_bytes += bytes;
        state.peak_bytes = state.peak_bytes.max(state.live_bytes);
    }

    fn release(&self, bytes: usize) {
        if bytes != 0 {
            self.state
                .lock()
                .expect("coordinator metrics lock poisoned")
                .live_bytes -= bytes;
        }
    }
}

impl OwnerWake {
    fn new() -> Self {
        Self {
            generation: Mutex::new(0),
            changed: Condvar::new(),
        }
    }
    fn snapshot(&self) -> u64 {
        *self.generation.lock().expect("owner wake poisoned")
    }
    fn notify(&self) {
        let mut g = self.generation.lock().expect("owner wake poisoned");
        *g = g.wrapping_add(1);
        self.changed.notify_one();
    }
    fn notify_all(&self) {
        let mut g = self.generation.lock().expect("owner wake poisoned");
        *g = g.wrapping_add(1);
        self.changed.notify_all();
    }
    fn wait(&self, seen: u64, cancelled: &AtomicBool) {
        let g = self.generation.lock().expect("owner wake poisoned");
        let _guard = self
            .changed
            .wait_while(g, |g| *g == seen && !cancelled.load(Ordering::Acquire))
            .expect("owner wake poisoned");
    }
}

impl Pass {
    fn cancel(&self) {
        self.coordinator.cancel();
        for wake in &self.wakes {
            wake.notify_all();
        }
    }
}

struct CreditPermit {
    coordinator: Option<Arc<Coordinator>>,
    bytes: usize,
}

impl CreditPermit {
    fn record_bytes(&mut self, bytes: usize) {
        self.bytes = bytes;
        if bytes != 0 {
            self.coordinator
                .as_ref()
                .expect("byte accounting coordinator")
                .publish_bytes(bytes);
        }
    }
}

impl Drop for CreditPermit {
    fn drop(&mut self) {
        if let Some(coordinator) = &self.coordinator {
            coordinator.release(self.bytes);
        }
    }
}

struct CancelOnDrop {
    pass: Arc<Pass>,
    armed: bool,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.pass.cancel();
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

// Ownership and String capacity are needed to exclude borrowed database text.
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

fn new_coordinator(_max_batches: usize, _max_rows: usize) -> Arc<Coordinator> {
    Arc::new(Coordinator {
        state: Mutex::new(State {
            live_bytes: 0,
            peak_bytes: 0,
        }),
        next_claim: AtomicUsize::new(0),
        cancelled: AtomicBool::new(false),
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
    wake: &OwnerWake,
    published: &mpsc::Sender<Published>,
) -> WorkerDone {
    if slots.is_empty() {
        return WorkerDone {
            decode_ns: 0,
            wait_ns: 0,
            publish_ns: 0,
            cleanup_ns: 0,
            batches: 0,
            reused_batches: 0,
        };
    }
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
            let generation = wake.snapshot();
            let (recycled, cleanup_ns) = recycle_slots(slots, &mut free, false);
            stats.cleanup_ns += cleanup_ns;
            stats.reused_batches += recycled;
            if free.is_empty() {
                let started = Instant::now();
                wake.wait(generation, &pass.coordinator.cancelled);
                stats.wait_ns += started.elapsed().as_nanos() as u64;
            }
            continue;
        }
        match pass
            .coordinator
            .try_claim(pass.rows, pass.batch, pass.measure_bytes)
        {
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
                    pass.cancel();
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
                    pass.cancel();
                }
                stats.publish_ns += started.elapsed().as_nanos() as u64;
                stats.batches += 1;
            }
            Claim::Finished => {
                if free.len() == slots.len() {
                    break;
                }
                let generation = wake.snapshot();
                let (recycled, cleanup_ns) = recycle_slots(slots, &mut free, false);
                stats.cleanup_ns += cleanup_ns;
                stats.reused_batches += recycled;
                if free.len() != slots.len() {
                    let started = Instant::now();
                    wake.wait(generation, &pass.coordinator.cancelled);
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
    pass: &Arc<Pass>,
    ring_capacity: Option<usize>,
    mut consume: impl FnMut(usize, &[Option<ultravin::DecodeResult<'static>>]),
) {
    let mut guard = CancelOnDrop {
        pass: Arc::clone(pass),
        armed: true,
    };
    let mut waiting = BTreeMap::new();
    let mut ring = ring_capacity.map(|capacity| (0..capacity).map(|_| None).collect::<Vec<_>>());
    let mut next = 0;
    while next < batches {
        let item = published.recv().expect("publisher closed");
        assert!(!item.failed, "decode worker failed");
        assert!(item.id >= next && item.id < batches, "invalid batch index");
        if let Some(ring) = &mut ring {
            assert!(
                item.id - next < ring.len(),
                "sequence window exceeded ring capacity"
            );
            let index = item.id % ring.len();
            assert!(
                ring[index].is_none(),
                "duplicate or ring generation collision"
            );
            ring[index] = Some(item);
        } else {
            assert!(waiting.insert(item.id, item).is_none(), "duplicate batch");
        }
        loop {
            let item = if let Some(ring) = &mut ring {
                let index = next % ring.len();
                match ring[index].as_ref() {
                    Some(item) if item.id == next => ring[index].take(),
                    Some(_) => panic!("stale ring generation"),
                    None => None,
                }
            } else {
                waiting.remove(&next)
            };
            let Some(item) = item else { break };
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
            pass.wakes[item.owner].notify();
            if let Err(payload) = consumed {
                std::panic::resume_unwind(payload);
            }
            next += 1;
        }
    }
    guard.armed = false;
}

// Keep diagnostic transport endpoints and capacity controls explicit.
#[allow(clippy::too_many_arguments)]
fn run_pass(
    vins: &Arc<Vec<String>>,
    batch: usize,
    senders: &[mpsc::SyncSender<Command>],
    published: &mpsc::Receiver<Published>,
    done: &mpsc::Receiver<WorkerDone>,
    all_slots: &[Vec<Arc<Mutex<Slot>>>],
    wakes: &[Arc<OwnerWake>],
    max_batches: usize,
    ring: bool,
    measure_bytes: bool,
) -> (u64, u64, u64, u64, usize, usize, usize) {
    let batches = vins.len().div_ceil(batch);
    let coordinator = new_coordinator(max_batches, ROW_BUDGET);
    let pass = Arc::new(Pass {
        rows: vins.len(),
        batch,
        coordinator: Arc::clone(&coordinator),
        wakes: wakes.to_vec(),
        measure_bytes,
    });
    let mut cancel_guard = CancelOnDrop {
        pass: Arc::clone(&pass),
        armed: true,
    };
    for sender in senders {
        sender
            .send(Command::Pass(Arc::clone(&pass)))
            .expect("worker closed");
    }
    let started = Instant::now();
    consume_ordered(
        batches,
        published,
        all_slots,
        &pass,
        ring.then_some(max_batches),
        |_, results| {
            std::hint::black_box(results);
        },
    );
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
    assert!(coordinator.next_claim.load(Ordering::Relaxed) >= batches);
    let state = coordinator.state.lock().expect("coordinator lock poisoned");
    cancel_guard.armed = false;
    (
        delivery_ns,
        worker_decode,
        worker_wait_publish,
        worker_cleanup,
        max_batches,
        max_batches * batch,
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
    assert!(matches!(mode.as_str(), "atomic-map" | "atomic-ring"));
    let ring = mode == "atomic-ring";
    let batches = vins.len().div_ceil(batch);
    let physical_slots = (ROW_BUDGET / batch).min(batches);
    assert!(
        physical_slots >= workers,
        "worker count exceeds physical slot quota"
    );
    let owner_slot_quotas = partition_slots(physical_slots, workers);
    let admission_batch_cap = physical_slots.min(batches);
    let all_slots = owner_slot_quotas
        .iter()
        .map(|&quota| {
            (0..quota)
                .map(|_| Arc::new(Mutex::new(Slot::new(batch))))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let wakes = (0..workers)
        .map(|_| Arc::new(OwnerWake::new()))
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
        let wake = Arc::clone(&wakes[owner]);
        handles.push(std::thread::spawn(move || loop {
            match rx.recv().expect("command closed") {
                Command::Stop => break,
                Command::Pass(pass) => {
                    let stats = worker_pass(owner, &vins, &pass, &owner_slots, &wake, &pub_tx);
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
        &wakes,
        admission_batch_cap,
        ring,
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
            &wakes,
            admission_batch_cap,
            ring,
            measure_bytes,
        );
    let elapsed = started.elapsed().as_secs_f64();
    let cpu = cpu::elapsed(cpu_start, cpu::snapshot());
    let counts = counters::elapsed(counters_start, counters::snapshot());
    println!(
        "{}",
        serde_json::json!({"mode":mode,"workers":workers,"active_threads_including_consumer":workers+1,"batch_size":batch,"owner_slot_quotas":owner_slot_quotas,"physical_slots":physical_slots,"admission_batch_cap":admission_batch_cap,"rows":vins.len(),"elapsed_seconds":elapsed,"rows_per_second":vins.len() as f64/elapsed,"live_result_row_budget":ROW_BUDGET,"physical_slot_row_capacity":physical_slots*batch,"live_result_batch_capacity":peak_batches,"live_result_row_capacity":peak_rows,"peak_completed_result_owned_bytes_excluding_construction_and_empty_preallocation":if measure_bytes {Some(final_metric)} else {None},"recycled_batches":if measure_bytes {None} else {Some(final_metric)},"aggregate_worker_decode_seconds":decode_ns as f64/1e9,"aggregate_worker_wait_and_publish_seconds":wait_ns as f64/1e9,"aggregate_worker_owner_local_recycle_scan_and_cleanup_seconds":cleanup_ns as f64/1e9,"consumer_delivery_seconds":delivery_ns as f64/1e9,"ready_descriptor_capacity":if ring {Some(physical_slots)} else {None},"process_user_cpu_seconds":cpu.map(|x|x.user_seconds),"process_system_cpu_seconds":cpu.map(|x|x.system_seconds),"average_busy_cores":cpu.map(|x|x.average_busy_cores(elapsed)),"process_counters":counts,"now_micros":NOW,"year":YEAR,"full_output_materialized":true,"ordered_delivery":true,"credits_released_after_owner_destruction":true,"unsafe_used":false})
    );
    for tx in &senders {
        tx.send(Command::Stop).expect("worker closed");
    }
    for handle in handles {
        handle.join().expect("worker panic");
    }
}
