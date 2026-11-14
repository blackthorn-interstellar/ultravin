//! Bounded, ordered native-thread streaming with reusable result slots.

use std::any::Any;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};

use crate::{current_year_at, decode_full_cached, Db, DecodeResult, DecodeWorkspace};

const MAX_RETAINED_VIN_BYTES: usize = 64;

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

/// The caller model year of each input row.
#[derive(Clone, Copy)]
pub(crate) enum Years<'a> {
    /// The same year, or none, for every row.
    All(Option<i32>),
    /// Entry `i` for row `i`; a `None` or missing entry means none.
    Each(&'a [Option<i32>]),
}

impl<'a> Years<'a> {
    pub(crate) fn each(years: Option<&'a [Option<i32>]>) -> Self {
        years.map_or(Self::All(None), Self::Each)
    }

    pub(crate) fn at(self, row: usize) -> Option<i32> {
        match self {
            Self::All(year) => year,
            Self::Each(years) => years.get(row).copied().flatten(),
        }
    }
}

/// A complete batch borrowed from a worker-owned result slot, delivered in
/// stream order: input order, unless the stream visits the inputs in another
/// order (see [`NativeBatch::input_indices`]).
pub struct NativeBatch<'batch, 'db> {
    batch_index: usize,
    start_index: usize,
    results: &'batch mut [Option<DecodeResult<'db>>],
    /// The input index of each row, when the stream reorders its inputs.
    order: Option<&'batch [u32]>,
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

    /// The position in the stream's inputs of each result, in [`Self::iter`]
    /// order: `start_index..start_index + len` unless the stream reorders.
    pub fn input_indices(&self) -> impl ExactSizeIterator<Item = usize> + '_ {
        let (start, order) = (self.start_index, self.order);
        (0..self.len()).map(move |row| order.map_or(start + row, |order| order[row] as usize))
    }

    /// All of a call's results, in input order, as one batch.
    pub(crate) fn whole(results: &'batch mut [Option<DecodeResult<'db>>]) -> Self {
        Self {
            batch_index: 0,
            start_index: 0,
            results,
            order: None,
        }
    }

    /// Move the results out with their input indices; their rows decode into
    /// new storage next time.
    pub(crate) fn take(&mut self) -> impl Iterator<Item = (usize, DecodeResult<'db>)> + '_ {
        let (start, order) = (self.start_index, self.order);
        self.results
            .iter_mut()
            .enumerate()
            .map(move |(row, result)| {
                let index = order.map_or(start + row, |order| order[row] as usize);
                (
                    index,
                    result.take().expect("published native slot is complete"),
                )
            })
    }
}

/// Failure to configure or finish native streaming.
#[derive(Debug)]
pub enum NativeStreamError {
    InvalidConfig(&'static str),
    WorkerStopped,
}

impl fmt::Display for NativeStreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(message) => write!(f, "invalid native stream config: {message}"),
            Self::WorkerStopped => f.write_str("native decode worker stopped before completion"),
        }
    }
}

impl std::error::Error for NativeStreamError {}

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

struct Slot<'db, T> {
    results: Vec<Option<DecodeResult<'db>>>,
    /// What the converter made of the published batch.
    output: Option<T>,
    state: SlotState,
}

impl<'db, T> Slot<'db, T> {
    fn new(batch_size: usize) -> Self {
        Self {
            results: (0..batch_size).map(|_| None).collect(),
            output: None,
            state: SlotState::Free,
        }
    }
}

/// The next batch to claim and the rows that claims hold until recycled.
/// Rows are only added under the `next_batch` lock, so claims reserve them in
/// batch order and an earlier batch never waits on rows a later one holds.
struct Coordinator {
    next_batch: Mutex<usize>,
    inflight_rows: AtomicUsize,
    max_inflight_rows: usize,
    /// Workers waiting for `inflight_rows` to fall.
    row_waiters: AtomicUsize,
    cancelled: AtomicBool,
    /// One per worker: everything a worker can wait on wakes only it.
    wakes: Vec<Wake>,
}

/// A worker's wake-up token, like `thread::park`: `wake` leaves it set and
/// `wait` takes it, so a worker that looks, finds nothing to do, and then
/// waits cannot miss a wake sent after it looked. Each worker waits only on
/// its own, and only when it has nothing to do, so the consumer marking a
/// slot reusable wakes that slot's owner and no one else.
#[repr(align(128))]
#[derive(Default)]
struct Wake {
    set: Mutex<bool>,
    changed: Condvar,
}

impl Wake {
    fn wake(&self) {
        let mut set = lock(&self.set);
        if !*set {
            *set = true;
            self.changed.notify_one();
        }
    }

    fn wait(&self) {
        let set = lock(&self.set);
        let mut set = self
            .changed
            .wait_while(set, |set| !*set)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *set = false;
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Coordinator {
    fn claim(self: &Arc<Self>, rows: usize, batch_size: usize) -> Claim {
        let mut next_batch = lock(&self.next_batch);
        if self.cancelled() {
            return Claim::Cancelled;
        }
        let start = match next_batch.checked_mul(batch_size) {
            Some(start) if start < rows => start,
            _ => return Claim::Finished,
        };
        let end = start.saturating_add(batch_size).min(rows);
        let len = end - start;
        // Other threads only subtract, so the cap still holds after the add.
        if self.inflight_rows.load(SeqCst) > self.max_inflight_rows - len {
            return Claim::Blocked(len);
        }
        let batch = *next_batch;
        *next_batch += 1;
        self.inflight_rows.fetch_add(len, SeqCst);
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

    /// Wait until `rows` more fit under the cap, or anything else wakes `owner`.
    fn wait_for_rows(&self, owner: usize, rows: usize) {
        self.row_waiters.fetch_add(1, SeqCst);
        // Look again now that a dropped permit will wake us.
        if !self.cancelled() && self.inflight_rows.load(SeqCst) > self.max_inflight_rows - rows {
            self.wakes[owner].wait();
        }
        self.row_waiters.fetch_sub(1, SeqCst);
    }

    fn cancel(&self) {
        self.cancelled.store(true, SeqCst);
        for wake in &self.wakes {
            wake.wake();
        }
    }

    fn cancelled(&self) -> bool {
        self.cancelled.load(SeqCst)
    }
}

struct Permit {
    coordinator: Arc<Coordinator>,
    rows: usize,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let coordinator = &self.coordinator;
        coordinator.inflight_rows.fetch_sub(self.rows, SeqCst);
        if coordinator.row_waiters.load(SeqCst) > 0 {
            for wake in &coordinator.wakes {
                wake.wake();
            }
        }
    }
}

enum Claim {
    Work {
        batch: usize,
        start: usize,
        end: usize,
        permit: Permit,
    },
    Blocked(usize),
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

fn clear_slot<T>(slot: &mut Slot<'_, T>, len: usize) {
    for result in &mut slot.results[..len] {
        drop(result.take());
    }
    slot.output = None;
}

/// Empty each row but keep its buffers. A row sharing a result-table
/// entry's elements keeps that result too: a hit on the same entry then
/// rewrites only `vin`, and any other decode resets every field itself.
fn prepare_slot_reuse<T>(slot: &mut Slot<'_, T>, len: usize) {
    for result in slot.results[..len].iter_mut().flatten() {
        result.vin.clear();
        if result.vin.capacity() > MAX_RETAINED_VIN_BYTES {
            result.vin = String::new();
        }
        if result.elements.is_shared() {
            continue;
        }
        result.wmi.clear();
        result.descriptor.clear();
        result.error_codes = Vec::new();
        result.corrected_vin = String::new();
        let elements = result.elements.to_mut();
        crate::clear_elements(elements);
        if elements.capacity() > crate::MAX_RETAINED_ELEMENTS {
            *elements = Vec::new();
        }
    }
}

fn clear_all_slots<T>(slots: &[Mutex<Slot<'_, T>>]) {
    for mutex in slots {
        let mut slot = lock(mutex);
        let len = slot.results.len();
        clear_slot(&mut slot, len);
        slot.state = SlotState::Free;
    }
}

fn recycle<T>(
    slots: &[Mutex<Slot<'_, T>>],
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

/// What every worker of one stream shares.
struct Job<'a, 'db, C> {
    db: &'db Db,
    inputs: &'a [String],
    /// The input index of each stream row, or input order.
    order: Option<&'a [u32]>,
    years: Years<'a>,
    now_micros: i64,
    current_year: i32,
    batch_size: usize,
    retain_results: bool,
    result_table: &'a crate::result_cache::Table<'db>,
    convert: &'a C,
}

fn worker<'db, T, C>(
    owner: usize,
    job: &Job<'_, 'db, C>,
    slots: &[Mutex<Slot<'db, T>>],
    coordinator: &Arc<Coordinator>,
    published: &mpsc::Sender<Published>,
) where
    C: Fn(&NativeBatch<'_, 'db>) -> T,
{
    let inputs = job.inputs;
    let mut free = (0..slots.len()).rev().collect::<Vec<_>>();
    let mut order = Vec::<(u64, usize)>::with_capacity(job.batch_size.min(inputs.len()));
    let mut decode_workspace = DecodeWorkspace::default();
    loop {
        let cancelled = coordinator.cancelled();
        recycle(slots, &mut free, cancelled, job.retain_results);
        if cancelled {
            clear_all_slots(slots);
            return;
        }
        if free.is_empty() {
            coordinator.wakes[owner].wait();
            continue;
        }
        match coordinator.claim(inputs.len(), job.batch_size) {
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
                    let input = |row: usize| job.order.map_or(row, |order| order[row] as usize);
                    order.clear();
                    order.extend(
                        (start..end).map(|row| (crate::locality_key(&inputs[input(row)]), row)),
                    );
                    order.sort_unstable_by_key(|&(key, _)| key);
                    let Slot {
                        results, output, ..
                    } = &mut *slot;
                    for (_, row) in order.iter().copied() {
                        let index = input(row);
                        let result_slot = &mut results[row - start];
                        *result_slot = Some(decode_full_cached(
                            job.db,
                            &inputs[index],
                            job.now_micros,
                            job.current_year,
                            job.years.at(index),
                            result_slot.take(),
                            job.result_table,
                            &mut decode_workspace,
                        ));
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
                    *output = Some((job.convert)(&NativeBatch {
                        batch_index: batch,
                        start_index: start,
                        results: &mut results[..end - start],
                        order: job.order.map(|order| &order[start..end]),
                    }));
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
            Claim::Blocked(rows) => coordinator.wait_for_rows(owner, rows),
            Claim::Finished => {
                if free.len() == slots.len() {
                    clear_all_slots(slots);
                    return;
                }
                coordinator.wakes[owner].wait();
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
        self.stream_at(
            inputs,
            None,
            Years::All(model_year),
            now_micros,
            config,
            crate::result_cache::configured_entries(),
            |_| {},
            |batch, ()| consume(batch),
        )
    }

    /// The stream itself: workers on the decoder pool decode the inputs in
    /// batches, visiting row `r` of the stream at input `order[r]` (or `r`),
    /// share a result table of `result_cache_entries`, and pass each batch
    /// to `convert`; `consume` then gets the batches in stream order on the
    /// calling thread, each with what `convert` made of it.
    // The decode inputs, the cache size, then the two per-batch steps.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn stream_at<'db, T, C, F>(
        &'db self,
        inputs: &[String],
        order: Option<&[u32]>,
        years: Years<'_>,
        now_micros: i64,
        config: NativeStreamConfig,
        result_cache_entries: usize,
        convert: C,
        mut consume: F,
    ) -> Result<(), NativeStreamError>
    where
        T: Send,
        C: Fn(&NativeBatch<'_, 'db>) -> T + Sync,
        F: for<'batch> FnMut(NativeBatch<'batch, 'db>, T),
    {
        debug_assert!(order.is_none_or(|order| order.len() == inputs.len()));
        // Workers are jobs on the decoder pool: a caller that is one of its
        // threads would wait on workers that may need that very thread.
        debug_assert!(
            crate::batch_pool().current_thread_index().is_none(),
            "a native stream must not be started from a decoder-pool thread"
        );
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
            next_batch: Mutex::new(0),
            inflight_rows: AtomicUsize::new(0),
            max_inflight_rows: config.max_inflight_rows,
            row_waiters: AtomicUsize::new(0),
            cancelled: AtomicBool::new(false),
            wakes: (0..active_workers).map(|_| Wake::default()).collect(),
        });
        let all_slots = (0..active_workers)
            .map(|_| {
                (0..config.slots_per_worker)
                    .map(|_| Mutex::new(Slot::new(slot_capacity)))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        // A stream stores at most one entry per input; a bigger table only
        // costs its setup and teardown, which a short batch call would feel.
        let result_table = crate::result_cache::Table::new(result_cache_entries.min(inputs.len()));
        let job = Job {
            db: self,
            inputs,
            order,
            years,
            now_micros,
            current_year: current_year_at(now_micros),
            batch_size: config.batch_size,
            retain_results,
            result_table: &result_table,
            convert: &convert,
        };
        let (published_tx, published_rx) = mpsc::channel();
        let mut panic_payload = None;
        let mut stream_error = None;
        // Workers run on the decoder pool's long-lived threads rather than
        // threads of their own: results that outlive the stream (a batch
        // call's `Vec`) are then freed into heaps that keep their pages, not
        // into the abandoned heaps of exited threads.
        crate::batch_pool().in_place_scope(|scope| {
            for (owner, owner_slots) in all_slots.iter().enumerate() {
                let tx = published_tx.clone();
                let coordinator_ref = Arc::clone(&coordinator);
                let job = &job;
                scope.spawn(move |_| {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        worker(owner, job, owner_slots, &coordinator_ref, &tx)
                    }));
                    if let Err(payload) = result {
                        coordinator_ref.cancel();
                        recycle(owner_slots, &mut Vec::new(), true, false);
                        clear_all_slots(owner_slots);
                        let _ = tx.send(Published::Panicked(payload));
                    }
                });
            }
            drop(published_tx);
            {
                let consumed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut waiting = BTreeMap::new();
                    let mut next = 0;
                    'consume: while next < batches {
                        match published_rx.recv() {
                            Ok(Published::Ready { batch, owner, slot }) => {
                                waiting.insert(batch, (owner, slot));
                                while let Some((owner, slot_index)) = waiting.remove(&next) {
                                    let mut slot = lock(&all_slots[owner][slot_index]);
                                    let state = std::mem::replace(&mut slot.state, SlotState::Free);
                                    let (len, permit) = match state {
                                        SlotState::Published { len, permit } => (len, permit),
                                        // Cancelled workers clear slots they already
                                        // published, so a panicking worker's `Panicked`
                                        // can trail this `Ready`. Stop; the drain after
                                        // the join reports that panic, not this symptom.
                                        _ if coordinator.cancelled() => {
                                            stream_error = Some(NativeStreamError::WorkerStopped);
                                            break 'consume;
                                        }
                                        _ => panic!("native worker published an unavailable slot"),
                                    };
                                    let consumed = std::panic::catch_unwind(
                                        std::panic::AssertUnwindSafe(|| {
                                            let start = next * config.batch_size;
                                            let output = slot.output.take();
                                            consume(
                                                NativeBatch {
                                                    batch_index: next,
                                                    start_index: start,
                                                    results: &mut slot.results[..len],
                                                    order: order
                                                        .map(|order| &order[start..start + len]),
                                                },
                                                output.expect("a published slot holds its output"),
                                            );
                                        }),
                                    );
                                    slot.state = SlotState::Recycle { len, permit };
                                    drop(slot);
                                    coordinator.wakes[owner].wake();
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
        });
        // The scope has joined every worker.
        {
            if panic_payload.is_none() {
                panic_payload = published_rx.try_iter().find_map(|message| match message {
                    Published::Panicked(payload) => Some(payload),
                    Published::Ready { .. } => None,
                });
            }
            debug_assert_eq!(coordinator.inflight_rows.load(SeqCst), 0);
            debug_assert!(all_slots.iter().flatten().all(|mutex| {
                let slot = lock(mutex);
                matches!(slot.state, SlotState::Free) && slot.results.iter().all(Option::is_none)
            }));
        }
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

    /// `PANIC_DECODE_*` are process-wide, so tests that arm them take turns.
    static DECODE_PANIC: Mutex<()> = Mutex::new(());

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
        if crate::Db::try_embedded().is_none() {
            eprintln!("skip: artifact not built");
            return;
        }
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
        if crate::Db::try_embedded().is_none() {
            eprintln!("skip: artifact not built");
            return;
        }
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
        if crate::Db::try_embedded().is_none() {
            eprintln!("skip: artifact not built");
            return;
        }
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
        if crate::Db::try_embedded().is_none() {
            eprintln!("skip: artifact not built");
            return;
        }
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
        if crate::Db::try_embedded().is_none() {
            eprintln!("skip: artifact not built");
            return;
        }
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
        if crate::Db::try_embedded().is_none() {
            eprintln!("skip: artifact not built");
            return;
        }
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
    fn consumer_panic_wakes_every_waiting_worker() {
        if crate::Db::try_embedded().is_none() {
            eprintln!("skip: artifact not built");
            return;
        }
        // The slow consumer lets the other workers fill every slot (a wide
        // cap) or block on the cap (one batch in flight) before it panics;
        // the stream must still wake and join them all.
        let inputs: Vec<_> = inputs().into_iter().cycle().take(31).collect();
        for max_inflight_rows in [2, 64] {
            let config = NativeStreamConfig {
                workers: 4,
                batch_size: 2,
                slots_per_worker: 2,
                max_inflight_rows,
            };
            let panic = std::panic::catch_unwind(|| {
                decode_native_stream_at(&inputs, None, NOW, config, |batch| {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    if batch.batch_index() == 1 {
                        panic!("consumer stopped");
                    }
                })
                .unwrap();
            });
            assert!(panic.is_err());
        }
    }

    #[test]
    fn unexpected_worker_panic_is_cancelled_joined_and_propagated() {
        if crate::Db::try_embedded().is_none() {
            eprintln!("skip: artifact not built");
            return;
        }
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
    fn decode_panic_is_reported_even_when_a_cancelled_peer_cleared_its_published_slot() {
        if crate::Db::try_embedded().is_none() {
            eprintln!("skip: artifact not built");
            return;
        }
        // The slow first callback lets a peer publish batch 1 while batch 2's
        // decode panics; the peer then clears batch 1 on cancellation, and the
        // consumer used to report that cleared slot instead of the real panic.
        let _turn = lock(&DECODE_PANIC);
        let inputs: Vec<_> = inputs().into_iter().cycle().take(31).collect();
        let config = NativeStreamConfig {
            workers: 2,
            batch_size: 2,
            slots_per_worker: 2,
            max_inflight_rows: 64,
        };
        PANIC_DECODE_INPUT.store(
            inputs.as_ptr() as usize,
            std::sync::atomic::Ordering::SeqCst,
        );
        // The interleaving is timing-dependent, so give it many chances.
        for _ in 0..25 {
            PANIC_DECODE_BATCH.store(2, std::sync::atomic::Ordering::SeqCst);
            let panic = std::panic::catch_unwind(|| {
                decode_native_stream_at(&inputs, None, NOW, config, |batch| {
                    if batch.batch_index == 0 {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                })
                .unwrap();
            });
            let payload = panic.expect_err("the injected decode panic propagates");
            let message = payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(String::as_str));
            assert_eq!(message, Some("injected partial native decode panic"));
        }
        PANIC_DECODE_BATCH.store(usize::MAX, std::sync::atomic::Ordering::SeqCst);
        PANIC_DECODE_INPUT.store(usize::MAX, std::sync::atomic::Ordering::SeqCst);
    }

    /// Stream the inputs in reverse order with a converter that joins each
    /// batch's VINs; returns each batch's input indices, text and years.
    fn converted(
        inputs: &[String],
        years: &[Option<i32>],
        convert_panics_at: Option<usize>,
    ) -> Vec<(Vec<usize>, String, Vec<Option<i32>>)> {
        let order: Vec<u32> = (0..inputs.len() as u32).rev().collect();
        let mut seen = Vec::new();
        Db::embedded()
            .stream_at(
                inputs,
                Some(&order),
                Years::Each(years),
                NOW,
                recycling_config(),
                16,
                |batch| {
                    if convert_panics_at == Some(batch.batch_index()) {
                        panic!("injected converter panic");
                    }
                    batch
                        .iter()
                        .map(|r| r.vin.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                },
                |batch, text| {
                    let years = batch.iter().map(|r| r.model_year).collect();
                    seen.push((batch.input_indices().collect(), text, years));
                },
            )
            .unwrap();
        seen
    }

    #[test]
    fn converter_output_and_per_row_years_follow_the_stream_order() {
        if crate::Db::try_embedded().is_none() {
            eprintln!("skip: artifact not built");
            return;
        }
        let inputs: Vec<_> = inputs().into_iter().cycle().take(31).collect();
        let years: Vec<_> = (0..inputs.len())
            .map(|row| (row % 2 == 0).then_some(2004))
            .collect();
        let expected = crate::decode_batch_at(&inputs, Some(&years), NOW);
        let seen = converted(&inputs, &years, None);
        assert_eq!(seen.len(), inputs.len().div_ceil(2));
        let mut visited = Vec::new();
        for (indices, text, model_years) in seen {
            let rows: Vec<_> = indices.iter().map(|&index| &expected[index]).collect();
            let vins: Vec<_> = rows.iter().map(|r| r.vin.as_str()).collect();
            assert_eq!(text, vins.join(","));
            let decoded: Vec<_> = rows.iter().map(|r| r.model_year).collect();
            assert_eq!(model_years, decoded);
            visited.extend(indices);
        }
        assert_eq!(visited, (0..inputs.len()).rev().collect::<Vec<_>>());
    }

    #[test]
    fn converter_panic_is_cancelled_joined_and_propagated() {
        if crate::Db::try_embedded().is_none() {
            eprintln!("skip: artifact not built");
            return;
        }
        let inputs: Vec<_> = inputs().into_iter().cycle().take(31).collect();
        for batch in [0, 3, 15] {
            let panic = std::panic::catch_unwind(|| converted(&inputs, &[], Some(batch)));
            let payload = panic.expect_err("the converter panic propagates");
            assert_eq!(
                payload.downcast_ref::<&str>(),
                Some(&"injected converter panic")
            );
        }
        assert_eq!(converted(&inputs, &[], None).len(), 16);
    }

    #[test]
    fn partial_decode_panic_clears_slot_and_releases_credit() {
        let _turn = lock(&DECODE_PANIC);
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
