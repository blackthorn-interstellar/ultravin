//! JSON Lines written as they decode: the native stream in input order, each
//! batch's lines handed to a writer, so a whole job never sits in memory.

use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::{Mutex, PoisonError};

use crate::native_stream::Years;
use crate::predictor::{predict_batch, BatchFormat};
use crate::{json, Db, NativeStreamConfig, NativeStreamError};

/// Why [`Db::write_batch_jsonl_at`] stopped.
#[derive(Debug)]
pub enum WriteJsonlError<E> {
    /// The writer failed; nothing after the failed write was written.
    Write(E),
    Stream(NativeStreamError),
}

/// Unwinds out of the stream's consumer once the writer fails, which cancels
/// the workers instead of decoding the rest of the inputs for nothing.
struct Stop;

impl Db {
    /// Decode `inputs` and pass their JSON Lines to `write` in input order,
    /// one batch of lines per call: the bytes
    /// [`decode_batch_jsonl_at`](crate::decode_batch_jsonl_at) returns, in
    /// pieces. Runs on the native stream (the result cache included) however
    /// few the inputs, with at most `memory_bytes` of JSON Lines (at the
    /// JSONL reference row width) claimed across the workers' slots.
    pub fn write_batch_jsonl_at<E>(
        &self,
        inputs: &[String],
        years: Option<&[Option<i32>]>,
        now_micros: i64,
        full: bool,
        memory_bytes: usize,
        mut write: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), WriteJsonlError<E>> {
        if inputs.is_empty() {
            return Ok(());
        }
        let config = jsonl_config(inputs.len(), memory_bytes);
        let encode = |result: &crate::DecodeResult<'_>, out: &mut Vec<u8>| {
            if full {
                json::encode_into(self, result, out);
            } else {
                serde_json::to_writer(out, &self.flatten(result))
                    .expect("FlatResult is infallibly serializable");
            }
        };
        // Written buffers go back to the workers: freed on this thread while
        // the workers seldom allocate, they stay held (+50 MB at 8 workers).
        let buffers = Mutex::new(Vec::<Vec<u8>>::new());
        let convert = |batch: &crate::NativeBatch<'_, '_>| {
            let pooled = buffers.lock().unwrap_or_else(PoisonError::into_inner).pop();
            let mut out = pooled.unwrap_or_else(|| Vec::with_capacity(batch.len() * 2048));
            json::lines_into(batch, encode, &mut out);
            out
        };
        let mut failed = None;
        let streamed = catch_unwind(AssertUnwindSafe(|| {
            self.stream_at(
                inputs,
                None,
                Years::each(years),
                now_micros,
                config,
                crate::result_cache::configured_entries(),
                convert,
                |_, mut lines: Vec<u8>| {
                    if let Err(error) = write(&lines) {
                        failed = Some(error);
                        resume_unwind(Box::new(Stop));
                    }
                    if lines.capacity() <= json::MAX_SCRATCH {
                        lines.clear();
                        let mut pool = buffers.lock().unwrap_or_else(PoisonError::into_inner);
                        pool.push(lines);
                    }
                },
            )
        }));
        match streamed {
            Ok(done) => done.map_err(WriteJsonlError::Stream),
            Err(payload) => match failed {
                Some(error) if payload.is::<Stop>() => Err(WriteJsonlError::Write(error)),
                _ => resume_unwind(payload),
            },
        }
    }
}

/// The native plan (64-row batches, four slots a worker) cut to fit
/// `memory_bytes` of JSON Lines at the reference row width, down to one row
/// a worker however small the budget.
fn jsonl_config(rows: usize, memory_bytes: usize) -> NativeStreamConfig {
    let workers = crate::predictor::worker_count().min(rows).max(1);
    let width = BatchFormat::Jsonl.default_bytes_per_row();
    let floor = workers.saturating_mul(width as usize);
    let plan = predict_batch(
        workers,
        1.0,
        BatchFormat::Native,
        memory_bytes.max(floor),
        width,
    )
    .expect("a budget of one row a worker always has a plan");
    NativeStreamConfig {
        workers: plan.workers,
        batch_size: plan.batch_size,
        slots_per_worker: plan.slots_per_worker.expect("native slot prediction"),
        max_inflight_rows: plan.max_inflight_rows.expect("native row prediction"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_767_225_600_000_000;

    fn inputs() -> Vec<String> {
        let mut vins: Vec<String> = [
            "1M8GDM9AXKP042788",
            "1HGCM82633A004352",
            "5YJ3E1EA7KF317000",
            "JH4KA8260MC000000",
            "WP0ZZZ99ZTS392124",
            "INVALID",
            "",
            "1FTFW1ET1EFA00001",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        // Repeats that differ only by serial hit the result cache.
        vins.extend((0..300).map(|n| format!("1HGCM82633A{n:06}")));
        vins
    }

    fn written(
        inputs: &[String],
        years: Option<&[Option<i32>]>,
        full: bool,
        memory: usize,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        let mut calls = 0;
        Db::embedded()
            .write_batch_jsonl_at(inputs, years, NOW, full, memory, |bytes| {
                calls += 1;
                out.extend_from_slice(bytes);
                Ok::<_, ()>(())
            })
            .expect("write");
        assert!(calls > 0 || inputs.is_empty());
        out
    }

    #[test]
    fn writes_the_bytes_of_the_jsonl_batch_call() {
        if Db::try_embedded().is_none() {
            eprintln!("skip: artifact not built");
            return;
        }
        let vins = inputs();
        let years: Vec<Option<i32>> = (0..vins.len())
            .map(|i| (i % 3 == 0).then_some(1999))
            .collect();
        for full in [false, true] {
            for years in [None, Some(years.as_slice())] {
                let expected = crate::decode_batch_jsonl_at(&vins, years, NOW, full);
                // A budget below one row a worker still decodes, a row at a time.
                for memory in [1, 4_500 * 64, 8 << 20] {
                    assert_eq!(written(&vins, years, full, memory), expected.as_bytes());
                }
            }
        }
        assert!(written(&[], None, false, 8 << 20).is_empty());
    }

    #[test]
    fn a_failed_write_stops_the_stream() {
        if Db::try_embedded().is_none() {
            eprintln!("skip: artifact not built");
            return;
        }
        let vins = inputs();
        let mut calls = 0;
        let error = Db::embedded()
            .write_batch_jsonl_at(&vins, None, NOW, false, 8 << 20, |_| {
                calls += 1;
                Err("closed")
            })
            .expect_err("the writer failed");
        assert!(matches!(error, WriteJsonlError::Write("closed")));
        assert_eq!(calls, 1);
    }
}
