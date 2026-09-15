//! Automatic plans for the ordered native worker-slot stream.

use std::borrow::Cow;
use std::fmt;
use std::mem::size_of;
use std::time::Instant;

use crate::predictor::{predict_batch, BatchFormat, BatchPredictError, BatchPrediction};
use crate::{Db, DecodeResult, DecodedElement, NativeBatch, NativeStreamConfig, NativeStreamError};

/// Working-output budget and decoder count for an automatic native job.
/// The budget estimates slot storage; it excludes the input, database, and allocator.
#[derive(Debug, Clone, Copy)]
pub struct NativeAutoOptions {
    /// `None` uses the decoder worker count, including `RAYON_NUM_THREADS`.
    pub workers: Option<usize>,
    pub memory_bytes: usize,
}

impl Default for NativeAutoOptions {
    fn default() -> Self {
        Self {
            workers: None,
            memory_bytes: 512 * 1024 * 1024,
        }
    }
}

#[derive(Debug)]
pub enum NativeAutoError {
    Prediction(BatchPredictError),
    Stream(NativeStreamError),
}

impl fmt::Display for NativeAutoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Prediction(error) => error.fmt(f),
            Self::Stream(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for NativeAutoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Prediction(error) => Some(error),
            Self::Stream(error) => Some(error),
        }
    }
}

/// Decode an ordered stream of full native results with automatic worker batches.
/// The callback borrows each batch; its owner clears and reuses the storage afterward.
/// One clock reading covers calibration and every delivered result.
pub fn decode_native_stream<F>(
    inputs: &[String],
    model_year: Option<i32>,
    consume: F,
) -> Result<Option<BatchPrediction>, NativeAutoError>
where
    F: for<'batch> FnMut(NativeBatch<'batch, 'static>),
{
    decode_native_stream_auto_at(
        inputs,
        model_year,
        crate::now_micros(),
        NativeAutoOptions::default(),
        consume,
    )
}

/// Automatic native streaming with an explicit clock and working-output budget.
pub fn decode_native_stream_auto_at<F>(
    inputs: &[String],
    model_year: Option<i32>,
    now_micros: i64,
    options: NativeAutoOptions,
    consume: F,
) -> Result<Option<BatchPrediction>, NativeAutoError>
where
    F: for<'batch> FnMut(NativeBatch<'batch, 'static>),
{
    Db::embedded().decode_native_stream_auto_at(inputs, model_year, now_micros, options, consume)
}

impl Db {
    /// Calibrate with up to 256 actual input rows on the calling thread, then
    /// run the worker-owned stream. Calibration rows are delivered only once.
    /// Empty inputs return `None` without calibration or worker creation.
    pub fn decode_native_stream_auto_at<'db, F>(
        &'db self,
        inputs: &[String],
        model_year: Option<i32>,
        now_micros: i64,
        options: NativeAutoOptions,
        consume: F,
    ) -> Result<Option<BatchPrediction>, NativeAutoError>
    where
        F: for<'batch> FnMut(NativeBatch<'batch, 'db>),
    {
        if options.workers == Some(0) || options.memory_bytes == 0 {
            return Err(NativeAutoError::Prediction(BatchPredictError(
                "workers and memory_bytes must be positive".into(),
            )));
        }
        if inputs.is_empty() {
            return Ok(None);
        }
        let workers = options
            .workers
            .unwrap_or_else(crate::predictor::worker_count)
            .min(inputs.len());
        let sample_rows = inputs.len().min(256);
        let sample: Vec<_> = (0..sample_rows)
            .map(|index| {
                let offset = if sample_rows == 1 {
                    0
                } else {
                    (index as u128 * (inputs.len() - 1) as u128 / (sample_rows - 1) as u128)
                        as usize
                };
                &inputs[offset]
            })
            .collect();
        // Populate lazy database caches before timing the same sample. Include
        // full-result construction and destruction in the serial speed input.
        for vin in &sample {
            drop(std::hint::black_box(
                self.decode_at(vin, model_year, now_micros),
            ));
        }
        let started = Instant::now();
        for vin in &sample {
            drop(std::hint::black_box(
                self.decode_at(vin, model_year, now_micros),
            ));
        }
        let speed = sample.len() as f64 / started.elapsed().as_secs_f64().max(f64::MIN_POSITIVE);
        // Measure width outside the timer, retaining the conservative corpus
        // reference for narrow samples. This is an estimate, not a byte allocator.
        let mut width = BatchFormat::Native.default_bytes_per_row();
        for vin in &sample {
            width = width.max(owned_row_bytes(&self.decode_at(vin, model_year, now_micros)) as f64);
        }
        let prediction = predict_batch(
            workers,
            speed,
            BatchFormat::Native,
            options.memory_bytes,
            width,
        )
        .map_err(NativeAutoError::Prediction)?;
        let config = NativeStreamConfig {
            workers: prediction.workers,
            batch_size: prediction.batch_size,
            slots_per_worker: prediction.slots_per_worker.expect("native slot prediction"),
            max_inflight_rows: prediction.max_inflight_rows.expect("native row prediction"),
        };
        self.decode_native_stream_at(inputs, model_year, now_micros, config, consume)
            .map_err(NativeAutoError::Stream)?;
        Ok(Some(prediction))
    }
}

fn owned_row_bytes(result: &DecodeResult<'_>) -> usize {
    let mut bytes = size_of::<Option<DecodeResult<'_>>>()
        + result.vin.capacity()
        + result.wmi.capacity()
        + result.descriptor.capacity()
        + result.corrected_vin.capacity()
        + result.error_codes.capacity() * size_of::<i32>()
        + result.elements.capacity() * size_of::<DecodedElement<'_>>();
    for element in &result.elements {
        for text in [
            &element.value,
            &element.attribute_id,
            &element.keys,
            &element.source,
        ] {
            if let Cow::Owned(value) = text {
                bytes += value.capacity();
            }
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_stream_delivers_calibration_rows_once_with_fixed_clock() {
        let inputs: Vec<_> = ["1HGCM82633A004352", "5YJ3E1EA7KF317001", "invalid"]
            .into_iter()
            .cycle()
            .take(603)
            .map(str::to_owned)
            .collect();
        let now = 1_788_220_800_000_000;
        let mut actual = Vec::new();
        let prediction = decode_native_stream_auto_at(
            &inputs,
            Some(2004),
            now,
            NativeAutoOptions {
                workers: Some(2),
                ..Default::default()
            },
            |batch| actual.extend(batch.iter().cloned()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(prediction.model_version, "ultravin-native-slots-v1");
        assert_eq!(
            actual,
            inputs
                .iter()
                .map(|vin| crate::decode_at(vin, Some(2004), now))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn empty_auto_job_has_no_callback_or_prediction() {
        assert!(decode_native_stream_auto_at(
            &[],
            None,
            0,
            NativeAutoOptions {
                workers: Some(1),
                ..Default::default()
            },
            |_| panic!("empty callback")
        )
        .unwrap()
        .is_none());
        assert!(decode_native_stream_auto_at(
            &[],
            None,
            0,
            NativeAutoOptions {
                workers: Some(0),
                ..Default::default()
            },
            |_| {}
        )
        .is_err());
    }

    #[test]
    fn one_row_job_does_not_budget_for_idle_requested_workers() {
        let inputs = vec!["1HGCM82633A004352".to_owned()];
        let mut rows = 0;
        let prediction = decode_native_stream_auto_at(
            &inputs,
            None,
            1_788_220_800_000_000,
            NativeAutoOptions {
                workers: Some(12),
                memory_bytes: 32_768,
            },
            |batch| rows += batch.len(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(rows, 1);
        assert_eq!(prediction.workers, 1);
        assert!(prediction.estimated_working_bytes <= 32_768);
    }
}
