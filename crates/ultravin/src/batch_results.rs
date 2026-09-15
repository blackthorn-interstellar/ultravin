//! Cleanup-aware storage for engine-produced native batch results.
//!
//! Construction stays crate-private so arbitrary user element destructors are not silently
//! given parallel drop semantics.

use std::fmt;
use std::ops::{Deref, DerefMut};

const PARALLEL_DROP_ROWS: usize = 512;

/// A batch whose large owned result set can be destroyed across decoder workers.
///
/// Consuming iteration and [`BatchResults::into_vec`] transfer the values and
/// cleanup responsibility to the caller. Dropping this container directly uses
/// parallel cleanup only for large batches on a pool with multiple workers.
pub struct BatchResults<T: Send> {
    values: Option<Vec<T>>,
}

impl<T: Send> BatchResults<T> {
    pub(crate) fn new(values: Vec<T>) -> Self {
        Self {
            values: Some(values),
        }
    }

    pub fn len(&self) -> usize {
        self.values().len()
    }

    pub fn is_empty(&self) -> bool {
        self.values().is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.values.as_ref().map_or(0, Vec::capacity)
    }

    /// Return the underlying vector and transfer cleanup responsibility.
    pub fn into_vec(mut self) -> Vec<T> {
        self.values.take().unwrap_or_default()
    }

    fn values(&self) -> &[T] {
        self.values.as_deref().unwrap_or_default()
    }

    fn values_mut(&mut self) -> &mut [T] {
        self.values.as_deref_mut().unwrap_or_default()
    }
}

impl<T: Send> Drop for BatchResults<T> {
    fn drop(&mut self) {
        let Some(values) = self.values.take() else {
            return;
        };
        #[cfg(feature = "stage-trace")]
        let batch_id = crate::stage_trace::sample_cleanup_batch();
        if values.len() < PARALLEL_DROP_ROWS {
            #[cfg(feature = "stage-trace")]
            crate::stage_trace::serial("cleanup_serial", batch_id, values.len(), || drop(values));
            #[cfg(not(feature = "stage-trace"))]
            drop(values);
            return;
        }
        crate::install_batch_work(|| {
            #[cfg(feature = "stage-trace")]
            let mut total_span = crate::stage_trace::WorkerSpan::new("cleanup_total", batch_id);
            #[cfg(feature = "stage-trace")]
            total_span.rows(values.len());
            if rayon::current_num_threads() > 1 {
                use rayon::prelude::*;
                #[cfg(feature = "stage-trace")]
                if batch_id.is_some() {
                    let mut parallel_span =
                        crate::stage_trace::WorkerSpan::new("cleanup_parallel", batch_id);
                    parallel_span.rows(values.len());
                    values.into_par_iter().for_each_init(
                        || crate::stage_trace::WorkerSpan::new("cleanup_worker", batch_id),
                        |span, value| {
                            span.row();
                            drop(value);
                        },
                    );
                } else {
                    values.into_par_iter().for_each(drop);
                }
                #[cfg(not(feature = "stage-trace"))]
                values.into_par_iter().for_each(drop);
            } else {
                #[cfg(feature = "stage-trace")]
                crate::stage_trace::serial("cleanup_serial", batch_id, values.len(), || {
                    drop(values)
                });
                #[cfg(not(feature = "stage-trace"))]
                drop(values);
            }
        });
    }
}

impl<T: Send> Deref for BatchResults<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.values()
    }
}

impl<T: Send> DerefMut for BatchResults<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.values_mut()
    }
}

impl<T: Send> AsRef<[T]> for BatchResults<T> {
    fn as_ref(&self) -> &[T] {
        self.values()
    }
}

impl<T: Send + fmt::Debug> fmt::Debug for BatchResults<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_list().entries(self.values()).finish()
    }
}

impl<T: Send + PartialEq> PartialEq for BatchResults<T> {
    fn eq(&self, other: &Self) -> bool {
        self.values() == other.values()
    }
}

impl<T: Send + Eq> Eq for BatchResults<T> {}

impl<T: Send + PartialEq> PartialEq<Vec<T>> for BatchResults<T> {
    fn eq(&self, other: &Vec<T>) -> bool {
        self.values() == other.as_slice()
    }
}

impl<T: Send + serde::Serialize> serde::Serialize for BatchResults<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(self.values(), serializer)
    }
}

impl<T: Send> IntoIterator for BatchResults<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(mut self) -> Self::IntoIter {
        self.values.take().unwrap_or_default().into_iter()
    }
}

impl<'a, T: Send> IntoIterator for &'a BatchResults<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.values().iter()
    }
}

impl<'a, T: Send> IntoIterator for &'a mut BatchResults<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.values_mut().iter_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex};

    struct Tracked {
        drops: Arc<AtomicUsize>,
        threads: Arc<Mutex<HashSet<std::thread::ThreadId>>>,
        gate: Option<(Arc<AtomicUsize>, Arc<Barrier>)>,
    }

    impl Drop for Tracked {
        fn drop(&mut self) {
            if let Some((entered, barrier)) = &self.gate {
                if entered.fetch_add(1, Ordering::Relaxed) < 4 {
                    barrier.wait();
                }
            }
            self.drops.fetch_add(1, Ordering::Relaxed);
            self.threads
                .lock()
                .expect("tracked drop threads")
                .insert(std::thread::current().id());
        }
    }

    fn tracked(
        rows: usize,
        gate_workers: bool,
    ) -> (
        BatchResults<Tracked>,
        Arc<AtomicUsize>,
        Arc<Mutex<HashSet<std::thread::ThreadId>>>,
    ) {
        let drops = Arc::new(AtomicUsize::new(0));
        let threads = Arc::new(Mutex::new(HashSet::new()));
        let gate = gate_workers.then(|| (Arc::new(AtomicUsize::new(0)), Arc::new(Barrier::new(4))));
        let values = (0..rows)
            .map(|_| Tracked {
                drops: Arc::clone(&drops),
                threads: Arc::clone(&threads),
                gate: gate.clone(),
            })
            .collect();
        (BatchResults::new(values), drops, threads)
    }

    #[test]
    fn small_batches_drop_once_on_the_caller() {
        let caller = std::thread::current().id();
        let (batch, drops, threads) = tracked(PARALLEL_DROP_ROWS - 1, false);
        drop(batch);
        assert_eq!(drops.load(Ordering::Relaxed), PARALLEL_DROP_ROWS - 1);
        let threads = threads.lock().expect("drop threads");
        assert_eq!(threads.len(), 1);
        assert!(threads.contains(&caller));
    }

    #[test]
    fn large_batches_respect_one_worker_calibration_scope() {
        let (batch, drops, threads) = tracked(PARALLEL_DROP_ROWS, false);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .expect("one-worker pool");
        pool.install(|| crate::with_private_calibration_pool_scope(|| drop(batch)));
        assert_eq!(drops.load(Ordering::Relaxed), PARALLEL_DROP_ROWS);
        assert_eq!(threads.lock().expect("drop threads").len(), 1);
    }

    #[test]
    fn large_batches_use_multiple_available_workers() {
        let (batch, drops, threads) = tracked(PARALLEL_DROP_ROWS * 4, true);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .expect("multiworker pool");
        pool.install(|| crate::with_private_calibration_pool_scope(|| drop(batch)));
        assert_eq!(drops.load(Ordering::Relaxed), PARALLEL_DROP_ROWS * 4);
        assert!(threads.lock().expect("drop threads").len() > 1);
    }

    #[test]
    fn iteration_vec_transfer_and_serialization_preserve_order() {
        let mut batch = BatchResults::new(vec![1, 2, 3]);
        for value in &mut batch {
            *value += 1;
        }
        assert_eq!((&batch).into_iter().copied().collect::<Vec<_>>(), [2, 3, 4]);
        assert_eq!(
            serde_json::to_string(&batch).expect("serialize batch"),
            "[2,3,4]"
        );
        assert_eq!(batch.into_vec(), [2, 3, 4]);

        let consumed: Vec<_> = BatchResults::new(vec![4, 5, 6]).into_iter().collect();
        assert_eq!(consumed, [4, 5, 6]);

        let (tracked, drops, _) = tracked(3, false);
        let transferred = tracked.into_vec();
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(transferred);
        assert_eq!(drops.load(Ordering::Relaxed), 3);
    }
}
