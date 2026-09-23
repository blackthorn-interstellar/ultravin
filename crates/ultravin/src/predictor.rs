//! Shipped batch-size model used to seed the adaptive controller.

use std::fmt;

const MIN_ROWS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum BatchFormat {
    Native,
    Jsonl,
    Columnar,
}

impl BatchFormat {
    pub const fn default_bytes_per_row(self) -> f64 {
        match self {
            // 256-byte ceiling of the new production calibration sample maximum.
            // See scripts/bench/native_worker_calibration_2026_09_15.json.
            Self::Native => 17_408.0,
            Self::Jsonl => 4_500.0,
            Self::Columnar => 1_800.0,
        }
    }

    pub const fn max_rows(self) -> usize {
        // Keep prediction inside each shipped model's training domain.
        match self {
            Self::Native => 400,
            Self::Jsonl => 16_384,
            Self::Columnar => 65_536,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct BatchPrediction {
    pub model_version: &'static str,
    pub batch_size: usize,
    pub workers: usize,
    pub single_core_rows_per_second: f64,
    pub estimated_rows_per_second: f64,
    pub estimated_peak_rows_per_second: f64,
    pub target_fraction: f64,
    pub estimated_peak_rss_bytes: Option<usize>,
    pub estimated_working_bytes: usize,
    /// Whether the working-memory budget lowers the modeled peak throughput.
    pub memory_limited: bool,
    /// Number of reusable output slots owned by each native worker.
    pub slots_per_worker: Option<usize>,
    /// Maximum native rows concurrently held across all worker slots.
    pub max_inflight_rows: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchPredictError(pub String);

impl fmt::Display for BatchPredictError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BatchPredictError {}

#[derive(Clone, Copy)]
struct Coefficients {
    a: f64,
    b: f64,
    d0: f64,
    d1: f64,
    h0: f64,
    h1: f64,
    l0: f64,
    l1: f64,
    rss_base: f64,
    rss_worker: f64,
    rss_row: f64,
    rss_worker_row: f64,
    rss_exponent: f64,
    rss_floor: f64,
    reference_single_core_rows_per_second: f64,
}

// Fits and validation: scripts/bench/predictor_model_fit.json.
// Native CPU reference: scripts/bench/predictor_reference.json, measured with
// the same production calibration kernel. These are model estimates; the
// adaptive controller refines the prediction using the current job's work.
const JSONL: Coefficients = Coefficients {
    a: 3.082_958_24e-6,
    b: 9.005_515_70e-6,
    d0: 0.0,
    d1: 2.923_331_33e-13,
    h0: 73.431_264_3e-6,
    h1: 32.937_812_6e-6,
    l0: 0.0,
    l1: 0.0,
    rss_base: 2.329_29 * 1_048_576.0,
    rss_worker: 0.0,
    rss_row: 23.098_47 * 1_048_576.0,
    rss_worker_row: 0.972_927 * 1_048_576.0,
    rss_exponent: 0.38,
    rss_floor: 208.0 * 1_048_576.0,
    reference_single_core_rows_per_second: 82_789.509_427_348_49,
};

const COLUMNAR: Coefficients = Coefficients {
    a: 3.242_027_28e-6,
    b: 7.137_013_04e-6,
    d0: 0.0,
    d1: 0.0,
    h0: 848.461_203e-6,
    h1: 46.976_241_9e-6,
    l0: 0.0,
    l1: 0.0,
    rss_base: 208.582_004_048_113_45 * 1_048_576.0,
    rss_worker: 3.172_851_290_860_285 * 1_048_576.0,
    rss_row: 0.004_808_882_070_538_252 * 1_048_576.0,
    rss_worker_row: 0.000_173_667_981_166_413_92 * 1_048_576.0,
    rss_exponent: 1.0,
    rss_floor: 208.0 * 1_048_576.0,
    reference_single_core_rows_per_second: 108_349.957_695_907_85,
};

pub fn predict_batch(
    workers: usize,
    single_core_rows_per_second: f64,
    format: BatchFormat,
    memory_bytes: usize,
    bytes_per_row: f64,
) -> Result<BatchPrediction, BatchPredictError> {
    if workers == 0 {
        return Err(BatchPredictError("workers must be at least 1".into()));
    }
    if !single_core_rows_per_second.is_finite() || single_core_rows_per_second <= 0.0 {
        return Err(BatchPredictError(
            "single_core_rows_per_second must be finite and positive".into(),
        ));
    }
    if !bytes_per_row.is_finite() || bytes_per_row <= 0.0 {
        return Err(BatchPredictError(
            "bytes_per_row must be finite and positive".into(),
        ));
    }
    if memory_bytes == 0 {
        return Err(BatchPredictError("memory_bytes must be positive".into()));
    }

    let c = match format {
        BatchFormat::Native => {
            return predict_native_slots(
                workers,
                single_core_rows_per_second,
                memory_bytes,
                bytes_per_row,
            )
        }
        BatchFormat::Jsonl => JSONL,
        BatchFormat::Columnar => COLUMNAR,
    };
    let worker_extra = workers.saturating_sub(1) as f64;
    let memory_rows = (memory_bytes as f64 / (2.0 * bytes_per_row)).floor() as usize;
    let max_rows = format.max_rows().min(memory_rows);
    if max_rows == 0 {
        return Err(BatchPredictError(
            "memory_bytes is too small for one row's working buffers".into(),
        ));
    }
    let min_rows = MIN_ROWS.min(max_rows);
    let rate = |rows: usize| {
        let batch = rows as f64;
        let root_batch = batch.sqrt();
        let seconds_per_row =
            (c.a + c.b / workers as f64 + c.d0 * batch + c.d1 * worker_extra * batch)
                * (c.reference_single_core_rows_per_second / single_core_rows_per_second)
                + (c.h0 + c.h1 * worker_extra) / batch
                + (c.l0 / (workers as f64 * root_batch) + c.l1 * worker_extra / root_batch)
                    * (c.reference_single_core_rows_per_second / single_core_rows_per_second);
        1.0 / seconds_per_row
    };
    let mut peak_rate: f64 = 0.0;
    for rows in min_rows..=max_rows {
        peak_rate = peak_rate.max(rate(rows));
    }
    let target_fraction = 0.99;
    let threshold = peak_rate * target_fraction;
    let batch_size = (min_rows..=max_rows)
        .find(|&rows| rate(rows) >= threshold)
        .unwrap_or(max_rows);
    let memory_limited = memory_rows < format.max_rows()
        && (max_rows + 1..=format.max_rows()).any(|rows| rate(rows) > peak_rate);
    let working = (2.0 * bytes_per_row * batch_size as f64).ceil() as usize;
    let rss_rows = (batch_size as f64).powf(c.rss_exponent);
    let rss = (c.rss_base
        + c.rss_worker * worker_extra
        + c.rss_row * rss_rows
        + c.rss_worker_row * worker_extra * rss_rows)
        .max(c.rss_floor);
    Ok(BatchPrediction {
        model_version: "ultravin-batch-v1",
        batch_size,
        workers,
        single_core_rows_per_second,
        estimated_rows_per_second: rate(batch_size),
        estimated_peak_rows_per_second: peak_rate,
        target_fraction,
        estimated_peak_rss_bytes: Some(rss.ceil() as usize),
        estimated_working_bytes: working,
        memory_limited,
        slots_per_worker: None,
        max_inflight_rows: None,
    })
}

fn predict_native_slots(
    workers: usize,
    single_core_rows_per_second: f64,
    memory_bytes: usize,
    bytes_per_row: f64,
) -> Result<BatchPrediction, BatchPredictError> {
    // Candidate rates come from NATIVE_PLAN_RATES. They seed a plan; they are
    // not throughput guarantees or cross-machine validation. The reference is
    // the median of five production-kernel calibration samples from the same
    // build, recorded with the plan grid.
    const REFERENCE_SINGLE: f64 = 360_035.0;

    let bytes_per_row_ceil = bytes_per_row.ceil();
    if bytes_per_row_ceil > usize::MAX as f64 {
        return Err(BatchPredictError(
            "bytes_per_row is too large to represent native slot storage".into(),
        ));
    }
    let row_bytes = bytes_per_row_ceil as usize;
    let bytes_per_worker_row = workers.checked_mul(row_bytes).ok_or_else(|| {
        BatchPredictError("native slot working-memory calculation overflowed".into())
    })?;
    let max_rows_per_slot = memory_bytes / bytes_per_worker_row;
    if max_rows_per_slot == 0 {
        return Err(BatchPredictError(
            "memory_bytes is too small for one native row per worker".into(),
        ));
    }

    let working = |batch: usize, slots: usize| {
        workers
            .checked_mul(batch)
            .and_then(|rows| rows.checked_mul(slots))
            .and_then(|rows| rows.checked_mul(row_bytes))
    };
    let measured = native_measured_candidates(workers);
    let preferred = smallest_near_peak(&measured).expect("native measured candidates");
    let feasible = measured
        .iter()
        .filter_map(|&(batch, slots, rate)| {
            let bytes = working(batch, slots)?;
            (bytes <= memory_bytes).then_some((batch, slots, bytes, rate))
        })
        .collect::<Vec<_>>();
    let feasible_peak = feasible
        .iter()
        .map(|candidate| candidate.3)
        .fold(0.0, f64::max);
    let (batch_size, slots_per_worker, estimated_working_bytes, reference_plan_rate) =
        if feasible.is_empty() {
            // Below the measured-plan budget, retain up to B200 with one slot.
            // Rate scales linearly by batch and applies the W12 B200/S1-to-S5
            // loss measured September 15; an explicit fallback extrapolation.
            let batch = max_rows_per_slot.min(200);
            let bytes = working(batch, 1).expect("validated native fallback memory product");
            let anchor = native_measured_rate(workers, 200, 5);
            let extrapolated = anchor * (887_306.0 / 943_428.0) * batch as f64 / 200.0;
            (batch, 1, bytes, extrapolated)
        } else {
            feasible
                .into_iter()
                .filter(|candidate| candidate.3 >= feasible_peak * 0.99)
                .min_by_key(|candidate| (candidate.2, candidate.0, candidate.1))
                .expect("feasible peak candidate")
        };
    let max_inflight_rows = workers
        .checked_mul(batch_size)
        .and_then(|rows| rows.checked_mul(slots_per_worker))
        .ok_or_else(|| BatchPredictError("native in-flight row calculation overflowed".into()))?;
    let speed_scale = single_core_rows_per_second / REFERENCE_SINGLE;
    let estimated = reference_plan_rate * speed_scale;
    let memory_limited = (batch_size, slots_per_worker) != (preferred.0, preferred.1);

    Ok(BatchPrediction {
        model_version: "ultravin-native-slots-v1",
        batch_size,
        workers,
        single_core_rows_per_second,
        estimated_rows_per_second: estimated,
        estimated_peak_rows_per_second: feasible_peak.max(reference_plan_rate) * speed_scale,
        target_fraction: 0.99,
        estimated_peak_rss_bytes: None,
        estimated_working_bytes,
        memory_limited,
        slots_per_worker: Some(slots_per_worker),
        max_inflight_rows: Some(max_inflight_rows),
    })
}

fn smallest_near_peak(candidates: &[(usize, usize, f64)]) -> Option<(usize, usize, f64)> {
    let peak = candidates
        .iter()
        .map(|candidate| candidate.2)
        .fold(0.0, f64::max);
    candidates
        .iter()
        .copied()
        .filter(|candidate| candidate.2 >= peak * 0.99)
        .min_by_key(|candidate| (candidate.0 * candidate.1, candidate.0, candidate.1))
}

/// Measured whole-pass native rates (VIN/s) per (batch, slots) plan at each
/// measured worker count: medians of alternating runs over the 20M-VIN corpus,
/// scripts/bench/native_plan_grid_2026_09_23.json. Worker counts between two
/// anchors interpolate the plans both measured; above 12 the W12 rates are a
/// conservative seed rather than invented scaling.
const NATIVE_PLAN_RATES: [(usize, &[NativePlanRate]); 4] = [
    (
        1,
        &[(32, 3, 260_050.0), (100, 5, 247_345.0), (200, 5, 251_356.0)],
    ),
    (
        4,
        &[
            (16, 4, 820_000.0),
            (24, 4, 873_000.0),
            (32, 3, 897_000.0),
            (48, 3, 887_000.0),
            (100, 5, 814_000.0),
            (200, 2, 697_000.0),
            (200, 5, 796_000.0),
            (400, 2, 740_000.0),
        ],
    ),
    (
        8,
        &[
            (16, 4, 1_575_000.0),
            (24, 4, 1_509_000.0),
            (32, 3, 1_425_000.0),
            (48, 3, 1_403_000.0),
            (100, 5, 1_480_000.0),
            (200, 2, 1_315_000.0),
            (200, 5, 1_234_000.0),
            (400, 2, 1_140_000.0),
        ],
    ),
    (
        12,
        &[
            (16, 4, 1_579_000.0),
            (24, 4, 1_640_000.0),
            (32, 3, 1_645_000.0),
            (32, 4, 1_611_000.0),
            (48, 3, 1_495_000.0),
            (64, 2, 1_599_000.0),
            (100, 5, 1_609_000.0),
            (200, 2, 1_577_000.0),
            (200, 5, 1_403_000.0),
            (400, 2, 1_383_000.0),
        ],
    ),
];

/// (batch rows, slots per worker, VIN/s).
type NativePlanRate = (usize, usize, f64);

fn native_measured_rate(workers: usize, batch: usize, slots: usize) -> f64 {
    native_measured_candidates(workers)
        .into_iter()
        .find(|&(b, s, _)| (b, s) == (batch, slots))
        .map(|(_, _, rate)| rate)
        .expect("plan was measured at the bracketing worker counts")
}

fn native_measured_candidates(workers: usize) -> Vec<(usize, usize, f64)> {
    let upper = NATIVE_PLAN_RATES
        .iter()
        .position(|&(anchor, _)| anchor >= workers)
        .unwrap_or(NATIVE_PLAN_RATES.len() - 1);
    let (right, right_plans) = NATIVE_PLAN_RATES[upper];
    if workers >= right || upper == 0 {
        return right_plans.to_vec();
    }
    let (left, left_plans) = NATIVE_PLAN_RATES[upper - 1];
    let fraction = (workers - left) as f64 / (right - left) as f64;
    left_plans
        .iter()
        .filter_map(|&(batch, slots, low)| {
            let &(_, _, high) = right_plans
                .iter()
                .find(|&&(b, s, _)| (b, s) == (batch, slots))?;
            Some((batch, slots, low + (high - low) * fraction))
        })
        .collect()
}

/// Worker count of the Rayon pool used by the native decoder.
pub fn worker_count() -> usize {
    crate::install_batch_work(rayon::current_num_threads)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_inputs_and_respects_caps() {
        assert!(predict_batch(0, 1.0, BatchFormat::Jsonl, 1_000_000, 10.0).is_err());
        assert!(predict_batch(1, f64::NAN, BatchFormat::Jsonl, 1_000_000, 10.0).is_err());
        let p = predict_batch(4, 10_000.0, BatchFormat::Jsonl, usize::MAX, 10.0).unwrap();
        assert!(p.batch_size <= 16_384);
        let capped = predict_batch(4, 10_000.0, BatchFormat::Columnar, 10_000, 10.0).unwrap();
        assert!(capped.batch_size <= 500);
        assert!(capped.estimated_working_bytes <= 10_000);
        assert_eq!(BatchFormat::Native.max_rows(), 400);
        let native = predict_batch(
            12,
            174_387.694_628_043_04,
            BatchFormat::Native,
            usize::MAX,
            BatchFormat::Native.default_bytes_per_row(),
        )
        .unwrap();
        assert_eq!(native.model_version, "ultravin-native-slots-v1");
        assert_eq!(native.batch_size, 24);
        assert_eq!(native.slots_per_worker, Some(4));
        assert_eq!(native.max_inflight_rows, Some(1_152));
        assert_eq!(native.estimated_peak_rss_bytes, None);
        assert_eq!(native.target_fraction, 0.99);
        assert!(native.estimated_rows_per_second >= 0.99 * native.estimated_peak_rows_per_second);
        let memory_capped = predict_batch(
            1,
            174_387.694_628_043_04,
            BatchFormat::Native,
            512 * 1024 * 1024,
            BatchFormat::Native.default_bytes_per_row(),
        )
        .unwrap();
        assert_eq!(memory_capped.batch_size, 32);
        assert_eq!(memory_capped.slots_per_worker, Some(3));
        assert!(memory_capped.estimated_working_bytes <= 512 * 1024 * 1024);
    }

    #[test]
    fn memory_limit_reports_peak_restriction_instead_of_domain_clipping() {
        let roomy = predict_batch(
            12,
            140_000.0,
            BatchFormat::Native,
            512 * 1024 * 1024,
            17_408.0,
        )
        .unwrap();
        let unlimited =
            predict_batch(12, 140_000.0, BatchFormat::Native, usize::MAX, 17_408.0).unwrap();
        assert_eq!(roomy.batch_size, unlimited.batch_size);
        assert!(!roomy.memory_limited);
        let constrained = predict_batch(
            12,
            140_000.0,
            BatchFormat::Native,
            // 90 rows per worker: below the 96-row preferred plans.
            12 * 90 * 17_408,
            17_408.0,
        )
        .unwrap();
        assert!(constrained.memory_limited);
        assert!(constrained.estimated_peak_rows_per_second < roomy.estimated_peak_rows_per_second);
    }

    #[test]
    fn native_slot_plan_handles_tiny_budgets_and_checked_products() {
        let width = BatchFormat::Native.default_bytes_per_row();
        let tiny = predict_batch(4, 160_000.0, BatchFormat::Native, 4 * 17 * 17_408, width)
            .expect("seventeen rows per worker fit");
        assert_eq!(tiny.batch_size, 17);
        assert_eq!(tiny.slots_per_worker, Some(1));
        assert_eq!(tiny.max_inflight_rows, Some(68));
        assert!(tiny.estimated_working_bytes <= 4 * 17 * 17_408);
        let too_small = predict_batch(4, 160_000.0, BatchFormat::Native, 4 * 17_408 - 1, width)
            .expect_err("one row per worker does not fit");
        assert!(too_small.to_string().contains("one native row per worker"));
        let overflow = predict_batch(
            usize::MAX,
            160_000.0,
            BatchFormat::Native,
            usize::MAX,
            width,
        )
        .expect_err("worker memory product overflows");
        assert!(overflow.to_string().contains("overflowed"));
    }

    #[test]
    fn native_slot_plan_uses_smallest_measured_plan_within_99_percent() {
        let width = BatchFormat::Native.default_bytes_per_row();
        let plan = |workers| {
            predict_batch(
                workers,
                174_387.694_628_043_04,
                BatchFormat::Native,
                usize::MAX,
                width,
            )
            .expect("native plan")
        };
        let w4 = plan(4);
        let w8 = plan(8);
        let w12 = plan(12);
        assert_eq!((w4.batch_size, w4.slots_per_worker), (32, Some(3)));
        assert_eq!((w8.batch_size, w8.slots_per_worker), (16, Some(4)));
        assert_eq!((w12.batch_size, w12.slots_per_worker), (24, Some(4)));
        for prediction in [w4, w8, w12] {
            assert!(
                prediction.estimated_rows_per_second
                    >= prediction.estimated_peak_rows_per_second * prediction.target_fraction
            );
            assert_eq!(prediction.estimated_peak_rss_bytes, None);
        }
    }

    #[test]
    fn legacy_formats_have_no_native_slot_metadata() {
        for format in [BatchFormat::Jsonl, BatchFormat::Columnar] {
            let prediction =
                predict_batch(4, 100_000.0, format, usize::MAX, 100.0).expect("legacy prediction");
            assert_eq!(prediction.slots_per_worker, None);
            assert_eq!(prediction.max_inflight_rows, None);
            assert!(prediction.estimated_peak_rss_bytes.is_some());
        }
    }

    #[test]
    fn workload_changes_prediction() {
        let slow = predict_batch(1, 2_000.0, BatchFormat::Columnar, usize::MAX, 10.0).unwrap();
        let fast = predict_batch(1, 20_000.0, BatchFormat::Columnar, usize::MAX, 10.0).unwrap();
        let parallel = predict_batch(8, 20_000.0, BatchFormat::Columnar, usize::MAX, 10.0).unwrap();
        assert_ne!(slow.batch_size, fast.batch_size);
        assert_ne!(fast.batch_size, parallel.batch_size);
    }

    #[test]
    fn worker_count_reports_the_decoder_pool_outside_custom_rayon_pools() {
        let decoder_workers = crate::install_batch_work(rayon::current_num_threads);
        for caller_workers in [1, 2] {
            let caller = rayon::ThreadPoolBuilder::new()
                .num_threads(caller_workers)
                .build()
                .expect("custom caller pool");
            caller.install(|| {
                assert_eq!(rayon::current_num_threads(), caller_workers);
                assert_eq!(worker_count(), decoder_workers);
                assert_eq!(
                    worker_count(),
                    crate::install_batch_work(rayon::current_num_threads)
                );
            });
        }

        let calibration = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .expect("private calibration pool");
        calibration.install(|| {
            crate::with_private_calibration_pool_scope(|| assert_eq!(worker_count(), 1));
        });
    }
}
