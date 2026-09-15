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

    pub const fn adaptive_max_rows(self) -> usize {
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

    if format == BatchFormat::Native {
        return predict_native_slots(
            workers,
            single_core_rows_per_second,
            memory_bytes,
            bytes_per_row,
        );
    }

    let c = match format {
        BatchFormat::Native => unreachable!("native slot plans return above"),
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
    let target_fraction = match format {
        BatchFormat::Native => 1.0,
        BatchFormat::Jsonl | BatchFormat::Columnar => 0.99,
    };
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
        model_version: match format {
            BatchFormat::Native => "ultravin-native-batch-v2",
            BatchFormat::Jsonl | BatchFormat::Columnar => "ultravin-batch-v1",
        },
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
    // Candidate rates come from scripts/bench/slot_budget_sweep.py. They seed
    // a plan; they are not throughput guarantees or cross-machine validation.
    // The reference is the median of the four production-kernel calibration
    // samples in native_worker_calibration_2026_09_15.json, not the retired
    // shared-batch controller's contiguous-offset calibration.
    const REFERENCE_SINGLE: f64 = 174_387.694_628_043_04;

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
            // Rate scales linearly by batch and applies the measured W12
            // B200/S1-to-S5 loss; this is an explicit fallback extrapolation.
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

fn interpolate(workers: usize, left: (usize, f64), right: (usize, f64)) -> f64 {
    let fraction = (workers - left.0) as f64 / (right.0 - left.0) as f64;
    left.1 + (right.1 - left.1) * fraction
}

fn native_measured_rate(workers: usize, batch: usize, slots: usize) -> f64 {
    let w12 = match (batch, slots) {
        (100, 1) => 910_005.0,
        (100, 2) => 945_406.0,
        (100, 5) => 961_448.0,
        (100, 10) => 942_917.0,
        (200, 1) => 887_306.0,
        (200, 2) => 943_377.0,
        (200, 5) => 943_428.0,
        (200, 10) => 956_335.0,
        (400, 1) => 874_224.0,
        (400, 2) => 921_613.0,
        (400, 5) => 946_747.0,
        (400, 10) => 959_363.0,
        _ => unreachable!("plan is outside the measured W12 grid"),
    };
    if workers >= 12 {
        // No worker counts above 12 were measured; retain the W12 rate as a
        // conservative seed rather than inventing additional scaling.
        return w12;
    }
    let w4 = match (batch, slots) {
        (100, 5) => 492_493.0,
        (200, 2) => 478_356.0,
        (200, 5) => 473_278.0,
        (400, 2) => 458_530.0,
        _ => unreachable!("plan was not measured at W4"),
    };
    let w8 = match (batch, slots) {
        (100, 5) => 843_852.0,
        (200, 2) => 841_184.0,
        (200, 5) => 845_172.0,
        (400, 2) => 823_082.0,
        _ => unreachable!("plan was not measured at W8"),
    };
    match workers {
        0 => unreachable!("workers validated above"),
        1 => 134_166.0,
        2..=3 => interpolate(workers, (1, 134_166.0), (4, w4)),
        4 => w4,
        5..=8 => interpolate(workers, (4, w4), (8, w8)),
        9..=11 => interpolate(workers, (8, w8), (12, w12)),
        _ => unreachable!("W12 and larger returned above"),
    }
}

fn native_measured_candidates(workers: usize) -> Vec<(usize, usize, f64)> {
    if workers <= 3 {
        return vec![(200, 5, native_measured_rate(workers, 200, 5))];
    }
    if workers <= 11 {
        return [(100, 5), (200, 2), (200, 5), (400, 2)]
            .map(|(batch, slots)| (batch, slots, native_measured_rate(workers, batch, slots)))
            .to_vec();
    }
    [100, 200, 400]
        .into_iter()
        .flat_map(|batch| [1, 2, 5, 10].map(move |slots| (batch, slots)))
        .map(|(batch, slots)| (batch, slots, native_measured_rate(workers, batch, slots)))
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
        assert_eq!(BatchFormat::Native.adaptive_max_rows(), 400);
        let native = predict_batch(
            12,
            174_387.694_628_043_04,
            BatchFormat::Native,
            usize::MAX,
            BatchFormat::Native.default_bytes_per_row(),
        )
        .unwrap();
        assert_eq!(native.model_version, "ultravin-native-slots-v1");
        assert_eq!(native.batch_size, 100);
        assert_eq!(native.slots_per_worker, Some(5));
        assert_eq!(native.max_inflight_rows, Some(6_000));
        assert_eq!(native.estimated_peak_rss_bytes, None);
        assert_eq!(native.target_fraction, 0.99);
        assert_eq!(
            native.estimated_rows_per_second,
            native.estimated_peak_rows_per_second
        );
        let memory_capped = predict_batch(
            1,
            174_387.694_628_043_04,
            BatchFormat::Native,
            512 * 1024 * 1024,
            BatchFormat::Native.default_bytes_per_row(),
        )
        .unwrap();
        assert_eq!(memory_capped.batch_size, 200);
        assert_eq!(memory_capped.slots_per_worker, Some(5));
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
            64 * 1024 * 1024,
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
        assert_eq!((w4.batch_size, w4.slots_per_worker), (100, Some(5)));
        assert_eq!((w8.batch_size, w8.slots_per_worker), (200, Some(2)));
        assert_eq!((w12.batch_size, w12.slots_per_worker), (100, Some(5)));
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
