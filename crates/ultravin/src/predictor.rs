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

    // Under the memory budget, drop slots first, then rows. batch * slots
    // never exceeds max_rows_per_slot, so the products below cannot overflow.
    let (plan_batch, plan_slots) = NATIVE_PLAN;
    let (batch_size, slots_per_worker) = if max_rows_per_slot >= plan_batch {
        (plan_batch, plan_slots.min(max_rows_per_slot / plan_batch))
    } else {
        (max_rows_per_slot, 1)
    };
    let max_inflight_rows = workers * batch_size * slots_per_worker;
    // A seed estimate, not a guarantee: the plan's measured cache-miss rate,
    // scaled by calibrated speed and, under the budget, by in-flight rows.
    let full =
        native_plan_miss_rate(workers) * single_core_rows_per_second / NATIVE_PLAN_REFERENCE_SINGLE;
    let estimated =
        full * (batch_size * slots_per_worker) as f64 / (plan_batch * plan_slots) as f64;

    Ok(BatchPrediction {
        model_version: "ultravin-native-slots-v1",
        batch_size,
        workers,
        single_core_rows_per_second,
        estimated_rows_per_second: estimated,
        estimated_peak_rows_per_second: estimated,
        target_fraction: 1.0,
        estimated_peak_rss_bytes: None,
        estimated_working_bytes: max_inflight_rows * row_bytes,
        memory_limited: (batch_size, slots_per_worker) != NATIVE_PLAN,
        slots_per_worker: Some(slots_per_worker),
        max_inflight_rows: Some(max_inflight_rows),
    })
}

/// (rows per worker batch, slots per worker) at every worker count.
///
/// scripts/bench/native_plan_grid_2026_11_12.json measured plans of 16-128
/// rows and 3-4 slots at 1, 4, 8 and 12 workers in two regimes: sorted New
/// York registrations with the result cache hitting (~94%), and a synthetic
/// corpus that never repeats. 64x4 has the best worst-regime share of peak
/// at 8 and 12 workers (95.6%, 95.1%) and is within about 1% of the best at
/// 1 and 4. Smaller batches lose up to half the hit-regime rate, because a
/// hit-speed batch is so short that the per-batch hand-off dominates; larger
/// ones lose up to 13% when nothing repeats.
const NATIVE_PLAN: (usize, usize) = (64, 4);

/// Cache-miss whole-pass VIN/s of `NATIVE_PLAN` per worker count, from the
/// synthetic corpus in the same grid (its 2M slice at one worker). They seed
/// `estimated_rows_per_second` only; they do not choose the plan. Between
/// anchors the rate interpolates; above 12 workers it holds.
const NATIVE_PLAN_MISS_RATES: [(usize, f64); 4] = [
    (1, 286_899.0),
    (4, 970_969.0),
    (8, 1_708_093.0),
    (12, 1_863_444.0),
];

/// Median of five production calibration samples on the same synthetic
/// corpus and build as `NATIVE_PLAN_MISS_RATES`.
const NATIVE_PLAN_REFERENCE_SINGLE: f64 = 351_126.0;

fn native_plan_miss_rate(workers: usize) -> f64 {
    let anchors = NATIVE_PLAN_MISS_RATES;
    let upper = anchors
        .iter()
        .position(|&(anchor, _)| anchor >= workers)
        .unwrap_or(anchors.len() - 1);
    let (right, high) = anchors[upper];
    if workers >= right || upper == 0 {
        return high;
    }
    let (left, low) = anchors[upper - 1];
    low + (high - low) * (workers - left) as f64 / (right - left) as f64
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
        assert_eq!(native.batch_size, 64);
        assert_eq!(native.slots_per_worker, Some(4));
        assert_eq!(native.max_inflight_rows, Some(3_072));
        assert_eq!(native.estimated_peak_rss_bytes, None);
        assert_eq!(native.target_fraction, 1.0);
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
        assert_eq!(memory_capped.batch_size, 64);
        assert_eq!(memory_capped.slots_per_worker, Some(4));
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
            // 90 rows per worker: room for one 64-row slot.
            12 * 90 * 17_408,
            17_408.0,
        )
        .unwrap();
        assert!(constrained.memory_limited);
        assert_eq!(
            (constrained.batch_size, constrained.slots_per_worker),
            (64, Some(1))
        );
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
    fn native_slot_plan_is_one_plan_until_the_default_budget_binds() {
        let width = BatchFormat::Native.default_bytes_per_row();
        let plan = |workers| {
            predict_batch(workers, 351_126.0, BatchFormat::Native, 512 << 20, width)
                .expect("native plan")
        };
        let mut previous = 0.0;
        for workers in [1, 2, 4, 6, 8, 12, 16, 32, 120] {
            let prediction = plan(workers);
            assert_eq!(
                (prediction.batch_size, prediction.slots_per_worker),
                (64, Some(4))
            );
            assert!(!prediction.memory_limited);
            assert!(prediction.estimated_rows_per_second >= previous);
            previous = prediction.estimated_rows_per_second;
        }
        assert_eq!(plan(12).estimated_rows_per_second, 1_863_444.0);
        // 121 workers x 64 rows x 4 slots x 17,408 bytes exceeds 512 MiB.
        let crowded = plan(121);
        assert_eq!(
            (crowded.batch_size, crowded.slots_per_worker),
            (64, Some(3))
        );
        assert!(crowded.memory_limited);
        assert!(crowded.estimated_working_bytes <= 512 << 20);
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
