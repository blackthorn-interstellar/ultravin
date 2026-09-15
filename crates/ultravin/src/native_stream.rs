//! Bounded, ordered native-thread streaming with reusable result slots.

use std::any::Any;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};

use crate::{
    current_year_at, decode_full_reusing, decode_full_with_workspace, Db, DecodeResult,
    DecodeWorkspace,
};

const MAX_RETAINED_VIN_BYTES: usize = 64;
const MAX_RETAINED_ELEMENTS: usize = 256;

#[cfg(test)]
static PANIC_WORKER_BATCH: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(usize::MAX);
#[cfg(test)]
static PANIC_WORKER_INPUT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(usize::MAX);
#[cfg(test)]
static PANIC_DECODE_BATCH: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(usize::MAX);
#[cfg(test)]
static PANIC_DECODE_INPUT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(usize::MAX);

/// Resource limits for native-thread streaming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeStreamConfig {
    pub workers: usize,
    pub batch_size: usize,
    pub slots_per_worker: usize,
    pub max_inflight_rows: usize,
}

impl NativeStreamConfig {
    /// Check the configuration before any worker or result-slot allocation.
    pub fn validate(self) -> Result<Self, NativeStreamError> {
        if self.workers == 0 {
            return Err(NativeStreamError::InvalidConfig(
                "workers must be greater than zero",
            ));
        }
        if self.batch_size == 0 {
            return Err(NativeStreamError::InvalidConfig(
                "batch_size must be greater than zero",
            ));
        }
        if self.slots_per_worker == 0 {
            return Err(NativeStreamError::InvalidConfig(
                "slots_per_worker must be greater than zero",
            ));
        }
        if self.max_inflight_rows == 0 {
            return Err(NativeStreamError::InvalidConfig(
                "max_inflight_rows must be greater than zero",
            ));
        }
        if self.batch_size > self.max_inflight_rows {
            return Err(NativeStreamError::InvalidConfig(
                "batch_size must not exceed max_inflight_rows",
            ));
        }
        let slots = self.workers.checked_mul(self.slots_per_worker).ok_or(
            NativeStreamError::InvalidConfig("workers * slots_per_worker overflows usize"),
        )?;
        let cells = slots
            .checked_mul(self.batch_size)
            .ok_or(NativeStreamError::InvalidConfig(
                "total result-slot size overflows usize",
            ))?;
        if cells > isize::MAX as usize / std::mem::size_of::<Option<DecodeResult<'static>>>() {
            return Err(NativeStreamError::InvalidConfig(
                "total result-slot allocation exceeds platform limits",
            ));
        }
        Ok(self)
    }
}

/// A complete input-order batch borrowed from a worker-owned result slot.
pub struct NativeBatch<'batch, 'db> {
    batch_index: usize,
    start_index: usize,
    results: &'batch [Option<DecodeResult<'db>>],
}

impl<'batch, 'db> NativeBatch<'batch, 'db> {
    pub fn batch_index(&self) -> usize {
        self.batch_index
    }

    pub fn start_index(&self) -> usize {
        self.start_index
    }

    pub fn len(&self) -> usize {
        self.results.len()
    }

    pub fn is_empty(&self) -> bool {
        self.results.is_empty()
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &DecodeResult<'db>> + '_ {
        self.results
            .iter()
            .map(|result| result.as_ref().expect("published native slot is complete"))
    }
}

/// Failure to configure or start native streaming.
#[derive(Debug)]
pub enum NativeStreamError {
    InvalidConfig(&'static str),
    Spawn(std::io::Error),
    WorkerStopped,
}

impl fmt::Display for NativeStreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(message) => write!(f, "invalid native stream config: {message}"),
            Self::Spawn(error) => write!(f, "failed to spawn native decode worker: {error}"),
            Self::WorkerStopped => f.write_str("native decode worker stopped before completion"),
        }
    }
}

impl std::error::Error for NativeStreamError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(error) => Some(error),
            _ => None,
        }
    }
}

/// Stream decoded batches from the embedded database in input order.
pub fn decode_native_stream_at<F>(
    inputs: &[String],
    model_year: Option<i32>,
    now_micros: i64,
    config: NativeStreamConfig,
    consume: F,
) -> Result<(), NativeStreamError>
where
    F: for<'batch> FnMut(NativeBatch<'batch, 'static>),
{
    Db::embedded().decode_native_stream_at(inputs, model_year, now_micros, config, consume)
}

enum SlotState {
    Free,
    Published { len: usize, permit: Permit },
    Recycle { len: usize, permit: Permit },
}

struct Slot<'db> {
    results: Vec<Option<DecodeResult<'db>>>,
    state: SlotState,
}

impl<'db> Slot<'db> {
    fn new(batch_size: usize) -> Self {
        Self {
            results: (0..batch_size).map(|_| None).collect(),
            state: SlotState::Free,
        }
    }
}

struct State {
    next_batch: usize,
    inflight_rows: usize,
    generation: u64,
    cancelled: bool,
}

struct Coordinator {
    state: Mutex<State>,
    changed: Condvar,
    max_inflight_rows: usize,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Coordinator {
    fn claim(self: &Arc<Self>, rows: usize, batch_size: usize) -> Claim {
        let mut state = lock(&self.state);
        if state.cancelled {
            return Claim::Cancelled;
        }
        let start = match state.next_batch.checked_mul(batch_size) {
            Some(start) if start < rows => start,
            _ => return Claim::Finished,
        };
        let end = start.saturating_add(batch_size).min(rows);
        let len = end - start;
        if state.inflight_rows > self.max_inflight_rows - len {
            return Claim::Blocked(state.generation);
        }
        let batch = state.next_batch;
        state.next_batch += 1;
        state.inflight_rows += len;
        Claim::Work {
            batch,
            start,
            end,
            permit: Permit {
                coordinator: Arc::clone(self),
                rows: len,
            },
        }
    }

    fn generation(&self) -> u64 {
        lock(&self.state).generation
    }

    fn wait(&self, generation: u64) {
        let state = lock(&self.state);
        drop(
            self.changed
                .wait_while(state, |state| {
                    !state.cancelled && state.generation == generation
                })
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
    }

    fn notify(&self) {
        let mut state = lock(&self.state);
        state.generation = state.generation.wrapping_add(1);
        self.changed.notify_all();
    }

    fn cancel(&self) {
        let mut state = lock(&self.state);
        state.cancelled = true;
        state.generation = state.generation.wrapping_add(1);
        self.changed.notify_all();
    }

    fn cancelled(&self) -> bool {
        lock(&self.state).cancelled
    }
}

struct Permit {
    coordinator: Arc<Coordinator>,
    rows: usize,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut state = lock(&self.coordinator.state);
        state.inflight_rows -= self.rows;
        state.generation = state.generation.wrapping_add(1);
        self.coordinator.changed.notify_all();
    }
}

enum Claim {
    Work {
        batch: usize,
        start: usize,
        end: usize,
        permit: Permit,
    },
    Blocked(u64),
    Finished,
    Cancelled,
}

enum Published {
    Ready {
        batch: usize,
        owner: usize,
        slot: usize,
    },
    Panicked(Box<dyn Any + Send>),
}

fn clear_slot(slot: &mut Slot<'_>, len: usize) {
    for result in &mut slot.results[..len] {
        drop(result.take());
    }
}

fn prepare_slot_reuse(slot: &mut Slot<'_>, len: usize) {
    for result in slot.results[..len].iter_mut().flatten() {
        result.vin.clear();
        if result.vin.capacity() > MAX_RETAINED_VIN_BYTES {
            result.vin = String::new();
        }
        result.wmi.clear();
        result.descriptor.clear();
        result.error_codes = Vec::new();
        result.corrected_vin = String::new();
        result.elements.clear();
        if result.elements.capacity() > MAX_RETAINED_ELEMENTS {
            result.elements = Vec::new();
        }
    }
}

fn clear_all_slots(slots: &[Mutex<Slot<'_>>]) {
    for mutex in slots {
        let mut slot = lock(mutex);
        let len = slot.results.len();
        clear_slot(&mut slot, len);
        slot.state = SlotState::Free;
    }
}

fn recycle(
    slots: &[Mutex<Slot<'_>>],
    free: &mut Vec<usize>,
    cancelled: bool,
    retain_results: bool,
) {
    for (index, mutex) in slots.iter().enumerate() {
        let mut slot = lock(mutex);
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
        if retain_results && !cancelled {
            prepare_slot_reuse(&mut slot, len);
        } else {
            clear_slot(&mut slot, len);
        }
        drop(permit);
        free.push(index);
    }
}

// Keep the immutable job inputs and the worker's transport/slot endpoints explicit.
#[allow(clippy::too_many_arguments)]
fn worker<'db>(
    owner: usize,
    db: &'db Db,
    inputs: &[String],
    model_year: Option<i32>,
    now_micros: i64,
    current_year: i32,
    batch_size: usize,
    retain_results: bool,
    slots: &[Mutex<Slot<'db>>],
    coordinator: &Arc<Coordinator>,
    published: &mpsc::Sender<Published>,
) {
    let mut free = (0..slots.len()).rev().collect::<Vec<_>>();
    let mut order = Vec::<(u64, usize)>::with_capacity(batch_size.min(inputs.len()));
    let mut decode_workspace = DecodeWorkspace::default();
    loop {
        let cancelled = coordinator.cancelled();
        recycle(slots, &mut free, cancelled, retain_results);
        if cancelled {
            clear_all_slots(slots);
            return;
        }
        if free.is_empty() {
            let generation = coordinator.generation();
            recycle(slots, &mut free, false, retain_results);
            if free.is_empty() {
                coordinator.wait(generation);
            }
            continue;
        }
        match coordinator.claim(inputs.len(), batch_size) {
            Claim::Work {
                batch,
                start,
                end,
                permit,
            } => {
                #[cfg(test)]
                if PANIC_WORKER_INPUT.load(std::sync::atomic::Ordering::SeqCst)
                    == inputs.as_ptr() as usize
                    && PANIC_WORKER_BATCH
                        .compare_exchange(
                            batch,
                            usize::MAX,
                            std::sync::atomic::Ordering::SeqCst,
                            std::sync::atomic::Ordering::SeqCst,
                        )
                        .is_ok()
                {
                    panic!("injected native worker panic");
                }
                let slot_index = free.pop().expect("checked nonempty free slots");
                let mut slot = lock(&slots[slot_index]);
                let decoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    order.clear();
                    order.extend(
                        (start..end).map(|index| (crate::locality_key(&inputs[index]), index)),
                    );
                    order.sort_unstable_by_key(|&(key, _)| key);
                    for (_, index) in order.iter().copied() {
                        let result_slot = &mut slot.results[index - start];
                        let decoded = match result_slot.take() {
                            Some(previous) => decode_full_reusing(
                                db,
                                &inputs[index],
                                now_micros,
                                current_year,
                                model_year,
                                previous,
                                &mut decode_workspace,
                            ),
                            None => decode_full_with_workspace(
                                db,
                                &inputs[index],
                                now_micros,
                                current_year,
                                model_year,
                                &mut decode_workspace,
                            ),
                        };
                        *result_slot = Some(decoded);
                        #[cfg(test)]
                        if PANIC_DECODE_INPUT.load(std::sync::atomic::Ordering::SeqCst)
                            == inputs.as_ptr() as usize
                            && PANIC_DECODE_BATCH
                                .compare_exchange(
                                    batch,
                                    usize::MAX,
                                    std::sync::atomic::Ordering::SeqCst,
                                    std::sync::atomic::Ordering::SeqCst,
                                )
                                .is_ok()
                        {
                            panic!("injected partial native decode panic");
                        }
                    }
                }));
                match decoded {
                    Ok(()) => {
                        slot.state = SlotState::Published {
                            len: end - start,
                            permit,
                        };
                        drop(slot);
                        if published
                            .send(Published::Ready {
                                batch,
                                owner,
                                slot: slot_index,
                            })
                            .is_err()
                        {
                            coordinator.cancel();
                        }
                    }
                    Err(payload) => {
                        clear_slot(&mut slot, end - start);
                        drop(permit);
                        drop(slot);
                        coordinator.cancel();
                        let _ = published.send(Published::Panicked(payload));
                    }
                }
            }
            Claim::Blocked(generation) => {
                recycle(slots, &mut free, false, retain_results);
                coordinator.wait(generation);
            }
            Claim::Finished => {
                if free.len() == slots.len() {
                    clear_all_slots(slots);
                    return;
                }
                let generation = coordinator.generation();
                recycle(slots, &mut free, false, retain_results);
                if free.len() != slots.len() {
                    coordinator.wait(generation);
                } else {
                    clear_all_slots(slots);
                    return;
                }
            }
            Claim::Cancelled => {}
        }
    }
}

impl Db {
    /// Decode on scoped native workers and lend each ordered batch to `consume`.
    pub fn decode_native_stream_at<'db, F>(
        &'db self,
        inputs: &[String],
        model_year: Option<i32>,
        now_micros: i64,
        config: NativeStreamConfig,
        mut consume: F,
    ) -> Result<(), NativeStreamError>
    where
        F: for<'batch> FnMut(NativeBatch<'batch, 'db>),
    {
        let config = config.validate()?;
        if inputs.is_empty() {
            return Ok(());
        }
        let batches = inputs.len().div_ceil(config.batch_size);
        let active_workers = config.workers.min(batches);
        let slot_capacity = config.batch_size.min(inputs.len());
        let retain_results = active_workers
            .checked_mul(config.slots_per_worker)
            .and_then(|slots| slots.checked_mul(slot_capacity))
            .is_some_and(|physical_rows| physical_rows <= config.max_inflight_rows);
        let coordinator = Arc::new(Coordinator {
            state: Mutex::new(State {
                next_batch: 0,
                inflight_rows: 0,
                generation: 0,
                cancelled: false,
            }),
            changed: Condvar::new(),
            max_inflight_rows: config.max_inflight_rows,
        });
        let all_slots = (0..active_workers)
            .map(|_| {
                (0..config.slots_per_worker)
                    .map(|_| Mutex::new(Slot::new(slot_capacity)))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let (published_tx, published_rx) = mpsc::channel();
        let mut panic_payload = None;
        let mut stream_error = None;
        std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(active_workers);
            for (owner, owner_slots) in all_slots.iter().enumerate() {
                let tx = published_tx.clone();
                let coordinator_ref = Arc::clone(&coordinator);
                let spawned = std::thread::Builder::new()
                    .name(format!("ultravin-native-{owner}"))
                    .spawn_scoped(scope, move || {
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            worker(
                                owner,
                                self,
                                inputs,
                                model_year,
                                now_micros,
                                current_year_at(now_micros),
                                config.batch_size,
                                retain_results,
                                owner_slots,
                                &coordinator_ref,
                                &tx,
                            )
                        }));
                        if let Err(payload) = result {
                            coordinator_ref.cancel();
                            recycle(owner_slots, &mut Vec::new(), true, false);
                            clear_all_slots(owner_slots);
                            let _ = tx.send(Published::Panicked(payload));
                        }
                    });
                match spawned {
                    Ok(handle) => handles.push(handle),
                    Err(error) => {
                        coordinator.cancel();
                        stream_error = Some(NativeStreamError::Spawn(error));
                        break;
                    }
                }
            }
            drop(published_tx);
            if stream_error.is_none() {
                let consumed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut waiting = BTreeMap::new();
                    let mut next = 0;
                    while next < batches {
                        match published_rx.recv() {
                            Ok(Published::Ready { batch, owner, slot }) => {
                                waiting.insert(batch, (owner, slot));
                                while let Some((owner, slot_index)) = waiting.remove(&next) {
                                    let mut slot = lock(&all_slots[owner][slot_index]);
                                    let state = std::mem::replace(&mut slot.state, SlotState::Free);
                                    let (len, permit) = match state {
                                        SlotState::Published { len, permit } => (len, permit),
                                        _ => panic!("native worker published an unavailable slot"),
                                    };
                                    let consumed = std::panic::catch_unwind(
                                        std::panic::AssertUnwindSafe(|| {
                                            consume(NativeBatch {
                                                batch_index: next,
                                                start_index: next * config.batch_size,
                                                results: &slot.results[..len],
                                            });
                                        }),
                                    );
                                    slot.state = SlotState::Recycle { len, permit };
                                    drop(slot);
                                    coordinator.notify();
                                    if let Err(payload) = consumed {
                                        std::panic::resume_unwind(payload);
                                    }
                                    next += 1;
                                }
                            }
                            Ok(Published::Panicked(payload)) => {
                                panic_payload = Some(payload);
                                coordinator.cancel();
                                break;
                            }
                            Err(_) => {
                                stream_error = Some(NativeStreamError::WorkerStopped);
                                coordinator.cancel();
                                break;
                            }
                        }
                    }
                }));
                if let Err(payload) = consumed {
                    panic_payload = Some(payload);
                    coordinator.cancel();
                }
            }
            for handle in handles {
                if let Err(payload) = handle.join() {
                    coordinator.cancel();
                    if panic_payload.is_none() {
                        panic_payload = Some(payload);
                    }
                }
            }
            debug_assert_eq!(lock(&coordinator.state).inflight_rows, 0);
            debug_assert!(all_slots.iter().flatten().all(|mutex| {
                let slot = lock(mutex);
                matches!(slot.state, SlotState::Free) && slot.results.iter().all(Option::is_none)
            }));
        });
        if let Some(payload) = panic_payload {
            std::panic::resume_unwind(payload);
        }
        match stream_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_767_225_600_000_000;

    fn inputs() -> Vec<String> {
        [
            "1M8GDM9AXKP042788",
            "1HGCM82633A004352",
            "5YJ3E1EA7KF317000",
            "JH4KA8260MC000000",
            "WP0ZZZ99ZTS392124",
            "INVALID",
            "1FTFW1ET1EFA00001",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    fn recycling_config() -> NativeStreamConfig {
        NativeStreamConfig {
            workers: 2,
            batch_size: 2,
            slots_per_worker: 1,
            max_inflight_rows: 2,
        }
    }

    #[test]
    fn repeated_streams_recycle_slots_and_preserve_full_order() {
        let inputs = inputs();
        let expected = crate::decode_batch_at(&inputs, None, NOW);
        for _ in 0..3 {
            let mut actual = Vec::new();
            decode_native_stream_at(&inputs, None, NOW, recycling_config(), |batch| {
                assert_eq!(batch.start_index(), actual.len());
                actual.extend(batch.iter().cloned());
            })
            .unwrap();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn recycled_slot_retains_header_and_element_buffers() {
        let inputs: Vec<_> = ["1HGCM82633A004352", "1HGCM82633A004352"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let config = NativeStreamConfig {
            workers: 1,
            batch_size: 1,
            slots_per_worker: 1,
            max_inflight_rows: 1,
        };
        let mut buffers = Vec::new();
        decode_native_stream_at(&inputs, Some(2004), NOW, config, |batch| {
            let result = batch.iter().next().unwrap();
            buffers.push((result.vin.as_ptr(), result.elements.as_ptr()));
        })
        .unwrap();
        assert_eq!(buffers[0], buffers[1]);
    }

    #[test]
    fn reuse_preserves_whitespace_unicode_and_low_volume_headers() {
        let inputs = [
            "  1hgcm82633a004352  ",
            "1F9TC25FTAB123456",
            "1HGCM8263Ł3A00435",
            "\u{00a0}AB\u{00a0}",
        ]
        .into_iter()
        .cycle()
        .take(12)
        .map(str::to_owned)
        .collect::<Vec<_>>();
        let expected = crate::decode_batch_at(&inputs, Some(&vec![Some(2004); inputs.len()]), NOW);
        let config = NativeStreamConfig {
            workers: 1,
            batch_size: 1,
            slots_per_worker: 1,
            max_inflight_rows: 1,
        };
        let mut actual = Vec::new();
        decode_native_stream_at(&inputs, Some(2004), NOW, config, |batch| {
            actual.extend(batch.iter().cloned());
        })
        .unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn empty_input_does_not_call_consumer() {
        let mut called = false;
        decode_native_stream_at(&[], None, NOW, recycling_config(), |_| called = true).unwrap();
        assert!(!called);
    }

    #[test]
    fn invalid_configs_are_rejected() {
        let base = recycling_config();
        for config in [
            NativeStreamConfig { workers: 0, ..base },
            NativeStreamConfig {
                batch_size: 0,
                ..base
            },
            NativeStreamConfig {
                slots_per_worker: 0,
                ..base
            },
            NativeStreamConfig {
                max_inflight_rows: 0,
                ..base
            },
            NativeStreamConfig {
                batch_size: 3,
                max_inflight_rows: 2,
                ..base
            },
            NativeStreamConfig {
                workers: usize::MAX,
                slots_per_worker: 2,
                ..base
            },
        ] {
            assert!(matches!(
                config.validate(),
                Err(NativeStreamError::InvalidConfig(_))
            ));
        }
    }

    #[test]
    fn consumer_panic_cancels_and_joins_workers() {
        let inputs = inputs();
        let panic = std::panic::catch_unwind(|| {
            decode_native_stream_at(&inputs, None, NOW, recycling_config(), |_| {
                panic!("consumer stopped")
            })
            .unwrap();
        });
        assert!(panic.is_err());

        let mut count = 0;
        decode_native_stream_at(&inputs, None, NOW, recycling_config(), |batch| {
            count += batch.len();
        })
        .unwrap();
        assert_eq!(count, inputs.len());
    }

    #[test]
    fn tight_row_cap_with_extra_physical_slots_does_not_stall() {
        let inputs: Vec<_> = inputs().into_iter().cycle().take(31).collect();
        let config = NativeStreamConfig {
            workers: 3,
            batch_size: 2,
            slots_per_worker: 3,
            max_inflight_rows: 2,
        };
        let mut indices = Vec::new();
        decode_native_stream_at(&inputs, None, NOW, config, |batch| {
            indices.extend(batch.start_index()..batch.start_index() + batch.len());
        })
        .unwrap();
        assert_eq!(indices, (0..inputs.len()).collect::<Vec<_>>());
    }

    #[test]
    fn unexpected_worker_panic_is_cancelled_joined_and_propagated() {
        let inputs: Vec<_> = inputs().into_iter().cycle().take(31).collect();
        PANIC_WORKER_INPUT.store(
            inputs.as_ptr() as usize,
            std::sync::atomic::Ordering::SeqCst,
        );
        PANIC_WORKER_BATCH.store(3, std::sync::atomic::Ordering::SeqCst);
        let panic = std::panic::catch_unwind(|| {
            decode_native_stream_at(&inputs, None, NOW, recycling_config(), |_| {}).unwrap();
        });
        PANIC_WORKER_BATCH.store(usize::MAX, std::sync::atomic::Ordering::SeqCst);
        PANIC_WORKER_INPUT.store(usize::MAX, std::sync::atomic::Ordering::SeqCst);
        assert!(panic.is_err());

        let mut count = 0;
        Db::embedded()
            .decode_native_stream_at(&inputs, None, NOW, recycling_config(), |batch| {
                count += batch.len();
            })
            .unwrap();
        assert_eq!(count, inputs.len());
    }

    #[test]
    fn partial_decode_panic_clears_slot_and_releases_credit() {
        let inputs: Vec<_> = inputs().into_iter().cycle().take(31).collect();
        PANIC_DECODE_INPUT.store(
            inputs.as_ptr() as usize,
            std::sync::atomic::Ordering::SeqCst,
        );
        PANIC_DECODE_BATCH.store(2, std::sync::atomic::Ordering::SeqCst);
        let panic = std::panic::catch_unwind(|| {
            decode_native_stream_at(&inputs, None, NOW, recycling_config(), |_| {}).unwrap();
        });
        PANIC_DECODE_BATCH.store(usize::MAX, std::sync::atomic::Ordering::SeqCst);
        PANIC_DECODE_INPUT.store(usize::MAX, std::sync::atomic::Ordering::SeqCst);
        assert!(panic.is_err());
    }
}
