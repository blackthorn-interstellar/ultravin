//! Small, deterministic controller for choosing a batch size from real work.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::predictor::{predict_batch, BatchFormat, BatchPredictError, BatchPrediction};

const MIN_ROWS: usize = 256;
const MAX_ROWS: usize = 65_536;
const RETUNE_AFTER: usize = 32;
const RETUNE_AFTER_ELAPSED: Duration = Duration::from_millis(250);
const WINDOW_MIN_ELAPSED: Duration = Duration::from_millis(10);
const WINDOW_MIN_BATCHES: usize = 3;
const REQUIRED_WINS: usize = 3;
const MAX_PAIR_ATTEMPTS: usize = 5;
const MATERIAL_GAIN: f64 = 1.05;
const MAX_BRACKET_CHANGE: f64 = 1.15;
pub const DEFAULT_MEMORY_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BatchTunerStatus {
    pub next_rows: usize,
    pub bytes_per_row: f64,
    pub observations: usize,
}

#[derive(Debug, Clone)]
pub struct BatchFeedback(Arc<Mutex<FeedbackState>>);

#[derive(Debug)]
struct FeedbackState {
    tuner: BatchTuner,
    pending: Option<(usize, Duration, usize)>,
    calibration: Option<PredictorCalibration>,
    prediction: Option<BatchPrediction>,
}

#[derive(Debug)]
struct PredictorCalibration {
    format: BatchFormat,
    memory_bytes: usize,
    workers: usize,
    completed: usize,
    measured_rows: usize,
    measured_elapsed: Duration,
    measured_bytes: usize,
    pool: Option<Arc<rayon::ThreadPool>>,
}

impl BatchFeedback {
    pub fn new(tuner: BatchTuner) -> Self {
        Self(Arc::new(Mutex::new(FeedbackState {
            tuner,
            pending: None,
            calibration: None,
            prediction: None,
        })))
    }

    pub fn new_predictive(
        format: BatchFormat,
        memory_bytes: usize,
        workers: usize,
    ) -> Result<Self, BatchPredictError> {
        if format == BatchFormat::Native {
            return Err(BatchPredictError(
                "native predictive batching requires the native stream API; batch_size is per worker"
                    .into(),
            ));
        }
        if workers == 0 {
            return Err(BatchPredictError("workers must be at least 1".into()));
        }
        if memory_bytes == 0 {
            return Err(BatchPredictError("memory_bytes must be positive".into()));
        }
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .map_err(|e| BatchPredictError(format!("building calibration thread pool: {e}")))?;
        let feedback = Self::new(BatchTuner::new(MIN_ROWS, memory_bytes));
        feedback
            .0
            .lock()
            .expect("batch feedback mutex poisoned")
            .calibration = Some(PredictorCalibration {
            format,
            memory_bytes,
            workers,
            completed: 0,
            measured_rows: 0,
            measured_elapsed: Duration::ZERO,
            measured_bytes: 0,
            pool: Some(Arc::new(pool)),
        });
        Ok(feedback)
    }

    pub fn calibration_needed(&self) -> bool {
        self.0
            .lock()
            .expect("batch feedback mutex poisoned")
            .calibration
            .as_ref()
            .is_some_and(|c| c.completed < 2)
    }

    pub fn prediction(&self) -> Option<BatchPrediction> {
        self.0
            .lock()
            .expect("batch feedback mutex poisoned")
            .prediction
    }

    /// Decode a batch, using a private serial worker during calibration. The
    /// measured calibration sample is replayed once before timing to separate
    /// CPU speed from lazy cache construction. The closure must be repeatable;
    /// only the timed result is returned. Ordinary batches run exactly once.
    pub fn decode<T: Send, E: Send, F>(&self, rows: usize, mut decode: F) -> Result<T, E>
    where
        F: FnMut() -> Result<(T, usize), E> + Send,
    {
        let (pool, warm_sample) = self
            .0
            .lock()
            .expect("batch feedback mutex poisoned")
            .calibration
            .as_ref()
            .filter(|c| c.completed < 2)
            .map(|c| (c.pool.clone(), c.completed == 1))
            .unwrap_or((None, false));
        let (value, bytes, elapsed) = match pool {
            Some(pool) => pool.install(|| {
                crate::with_private_calibration_pool_scope(|| {
                    if warm_sample && rows > 0 {
                        drop(decode()?);
                    }
                    let started = std::time::Instant::now();
                    decode().map(|(value, bytes)| (value, bytes, started.elapsed()))
                })
            })?,
            None => {
                let started = std::time::Instant::now();
                decode().map(|(value, bytes)| (value, bytes, started.elapsed()))?
            }
        };
        self.decoded(rows, elapsed, bytes);
        Ok(value)
    }

    pub fn next_rows(&self) -> usize {
        let mut state = self.0.lock().expect("batch feedback mutex poisoned");
        if let Some((rows, elapsed, bytes)) = state.pending.take() {
            state.tuner.observe(rows, elapsed, bytes);
        }
        state.tuner.next_rows()
    }

    pub fn decoded(&self, rows: usize, elapsed: Duration, output_bytes: usize) {
        if rows == 0 {
            return;
        }
        let mut state = self.0.lock().expect("batch feedback mutex poisoned");
        let calibrating = state.calibration.as_ref().is_some_and(|c| c.completed < 2);
        let calibration_values = state.calibration.as_mut().and_then(|calibration| {
            match calibration.completed {
                0 => {
                    calibration.completed = 1;
                    None
                }
                1 => {
                    // A short file tail must not become the whole CPU estimate.
                    // Accumulate real native work across tails until there are
                    // enough timed rows. Each measured contribution has been
                    // warmed once by decode(), and is emitted only once.
                    calibration.measured_rows += rows;
                    calibration.measured_elapsed += elapsed;
                    calibration.measured_bytes += output_bytes;
                    if calibration.measured_rows < MIN_ROWS {
                        return None;
                    }
                    calibration.completed = 2;
                    calibration.pool = None;
                    Some((
                        calibration.format,
                        calibration.memory_bytes,
                        calibration.workers,
                        calibration.measured_rows,
                        calibration.measured_elapsed,
                        calibration.measured_bytes,
                    ))
                }
                _ => None,
            }
        });
        if calibrating {
            state.tuner.learn_width(rows, output_bytes);
        }
        if let Some((
            format,
            memory_bytes,
            workers,
            measured_rows,
            measured_elapsed,
            measured_bytes,
        )) = calibration_values
        {
            let speed = measured_rows as f64 / measured_elapsed.as_secs_f64().max(1e-9);
            let width = if measured_bytes > 0 {
                measured_bytes as f64 / measured_rows as f64
            } else {
                format.default_bytes_per_row()
            };
            if let Ok(prediction) = predict_batch(workers, speed, format, memory_bytes, width) {
                state.tuner = BatchTuner::new(prediction.batch_size, memory_bytes)
                    .with_max_rows(format.adaptive_max_rows());
                // Use the shipped estimate directly while the job warms up.
                // Periodic tuning starts after the normal settled interval,
                // rather than immediately comparing cold candidate batches.
                state.tuner.candidates = vec![prediction.batch_size];
                state.tuner.probing = false;
                state.tuner.bytes_per_row = width;
                state.tuner.memory_measured = output_bytes > 0;
                state.prediction = Some(prediction);
            }
            return;
        }
        if state.calibration.as_ref().is_some_and(|c| c.completed < 2) {
            return;
        }
        state.pending = Some((rows, elapsed, output_bytes));
    }

    /// Replace the native decode duration of a pending non-calibration batch
    /// with a caller's end-to-end duration.
    pub fn observe_total(&self, rows: usize, elapsed: Duration, output_bytes: usize) {
        let mut state = self.0.lock().expect("batch feedback mutex poisoned");
        if let Some((pending_rows, pending_elapsed, pending_bytes)) = &mut state.pending {
            if *pending_rows == rows {
                *pending_elapsed = elapsed;
                *pending_bytes = output_bytes;
            }
        }
    }

    pub fn add_output_time(&self, elapsed: Duration) {
        let mut state = self.0.lock().expect("batch feedback mutex poisoned");
        if let Some((_, duration, _)) = &mut state.pending {
            *duration += elapsed;
        }
    }

    pub fn status(&self) -> BatchTunerStatus {
        self.0
            .lock()
            .expect("batch feedback mutex poisoned")
            .tuner
            .status()
    }
}

#[cfg(feature = "arrow")]
pub struct BatchRebatcher {
    reader: Box<dyn arrow_array::RecordBatchReader + Send>,
    buffered: crate::arrow_io::ArrowBatchRebatcher,
    exhausted: bool,
}

#[cfg(feature = "arrow")]
impl BatchRebatcher {
    pub fn new(reader: Box<dyn arrow_array::RecordBatchReader + Send>) -> Self {
        Self {
            reader,
            buffered: crate::arrow_io::ArrowBatchRebatcher::new(),
            exhausted: false,
        }
    }

    pub fn next_batch(
        &mut self,
        rows: usize,
    ) -> Result<Option<arrow_array::RecordBatch>, arrow_schema::ArrowError> {
        loop {
            if let Some(batch) = self
                .buffered
                .take(rows)
                .map_err(arrow_schema::ArrowError::from)?
            {
                return Ok(Some(batch));
            }
            if self.exhausted {
                return self
                    .buffered
                    .finish()
                    .map_err(arrow_schema::ArrowError::from);
            }
            match self.reader.next().transpose()? {
                Some(batch) => self.buffered.push(batch),
                None => self.exhausted = true,
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct BatchTuner {
    memory_bytes: usize,
    candidates: Vec<usize>,
    candidate: usize,
    observations: usize,
    bytes_per_row: f64,
    memory_measured: bool,
    max_rows: usize,
    probing: bool,
    window: ProbeWindow,
    phase: ProbePhase,
    wins: usize,
    pair_attempts: usize,
    before_rate: Option<f64>,
    challenger_rate: Option<f64>,
    settled_elapsed: Duration,
    prefer_larger: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct ProbeWindow {
    rows: usize,
    elapsed: Duration,
    batches: usize,
}

impl ProbeWindow {
    fn add(&mut self, rows: usize, elapsed: Duration) {
        self.rows += rows;
        self.elapsed += elapsed;
        self.batches += 1;
    }

    fn complete(self) -> bool {
        self.batches >= WINDOW_MIN_BATCHES && self.elapsed >= WINDOW_MIN_ELAPSED
    }

    fn rate(self) -> f64 {
        self.rows as f64 / self.elapsed.as_secs_f64().max(1e-9)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbePhase {
    Before,
    Challenger,
    After,
}

impl BatchTuner {
    pub fn new(initial_rows: usize, memory_bytes: usize) -> Self {
        let initial = initial_rows.clamp(MIN_ROWS, MAX_ROWS);
        Self {
            memory_bytes: memory_bytes.max(1),
            candidates: candidates(initial, MAX_ROWS),
            candidate: 0,
            observations: 0,
            bytes_per_row: 1_024.0,
            memory_measured: false,
            max_rows: MAX_ROWS,
            probing: true,
            window: ProbeWindow::default(),
            phase: ProbePhase::Before,
            wins: 0,
            pair_attempts: 0,
            before_rate: None,
            challenger_rate: None,
            settled_elapsed: Duration::ZERO,
            prefer_larger: false,
        }
    }

    pub fn with_max_rows(mut self, max_rows: usize) -> Self {
        self.max_rows = max_rows.clamp(MIN_ROWS, MAX_ROWS);
        let center = self.candidates[self.candidate].min(self.max_rows);
        self.candidates = candidates(center, self.max_rows);
        self.candidate = 0;
        self.observations = 0;
        self.probing = true;
        self.reset_comparison();
        self
    }

    pub fn status(&self) -> BatchTunerStatus {
        BatchTunerStatus {
            next_rows: self.next_rows(),
            bytes_per_row: self.bytes_per_row,
            observations: self.observations,
        }
    }

    pub fn next_rows(&self) -> usize {
        let requested = self.candidates[self.candidate];
        requested.min(self.memory_limit()).min(self.max_rows).max(1)
    }

    pub fn observe(&mut self, rows: usize, elapsed: Duration, output_bytes: usize) {
        if rows == 0 {
            return;
        }
        // Snapshot the request against the estimate that produced this batch,
        // before learning its new width below.
        let requested = self.next_rows();
        self.learn_width(rows, output_bytes);
        if self.next_rows() != requested {
            self.cap_changed(rows == requested, elapsed);
            return;
        }
        // EOF tails still teach the memory model, but their fixed overhead is
        // not comparable with a full batch's throughput.
        if rows != requested {
            return;
        }
        self.observations += 1;

        if !self.probing {
            self.settled_elapsed += elapsed;
            if self.observations >= RETUNE_AFTER && self.settled_elapsed >= RETUNE_AFTER_ELAPSED {
                self.start_probe();
            }
            return;
        }
        self.window.add(rows, elapsed);
        if self.window.complete() {
            self.finish_window();
        }
    }

    fn learn_width(&mut self, rows: usize, output_bytes: usize) {
        if rows == 0 || output_bytes == 0 {
            return;
        }
        let measured = output_bytes as f64 / rows as f64;
        self.bytes_per_row = if !self.memory_measured || measured > self.bytes_per_row {
            measured
        } else {
            self.bytes_per_row * 0.75 + measured * 0.25
        };
        self.memory_measured = true;
    }

    fn memory_limit(&self) -> usize {
        if self.memory_bytes == 0 || self.bytes_per_row == 0.0 {
            return self.max_rows;
        }
        // Input and output coexist while a batch is decoded. Treat the measured
        // output size as one half of that working set rather than pretending it
        // is process RSS.
        ((self.memory_bytes as f64 / (self.bytes_per_row * 2.0)) as usize).clamp(1, self.max_rows)
    }

    fn finish_window(&mut self) {
        let rate = self.window.rate();
        self.window = ProbeWindow::default();
        match self.phase {
            ProbePhase::Before => {
                self.before_rate = Some(rate);
                self.phase = ProbePhase::Challenger;
                self.candidate = 1;
            }
            ProbePhase::Challenger => {
                self.challenger_rate = Some(rate);
                self.phase = ProbePhase::After;
                self.candidate = 0;
            }
            ProbePhase::After => {
                let before = self.before_rate.take().expect("before probe window");
                let challenger = self
                    .challenger_rate
                    .take()
                    .expect("challenger probe window");
                self.pair_attempts += 1;
                let stable = before.max(rate) <= before.min(rate) * MAX_BRACKET_CHANGE;
                let won = stable && challenger > before.max(rate) * MATERIAL_GAIN;
                self.wins = if won { self.wins + 1 } else { 0 };
                if self.wins >= REQUIRED_WINS {
                    let incumbent = self.candidates[1];
                    self.settle_on(incumbent);
                } else if self.pair_attempts >= MAX_PAIR_ATTEMPTS {
                    if self.candidates.len() > 2 {
                        self.candidates.remove(1);
                        self.reset_comparison();
                    } else {
                        let incumbent = self.candidates[0];
                        self.settle_on(incumbent);
                    }
                } else {
                    self.phase = ProbePhase::Before;
                    self.candidate = 0;
                }
            }
        }
    }

    fn settle_on(&mut self, chosen: usize) {
        self.candidates = vec![chosen];
        self.candidate = 0;
        self.observations = 0;
        self.probing = false;
        self.settled_elapsed = Duration::ZERO;
        self.reset_comparison();
    }

    fn start_probe(&mut self) {
        let current = self.candidates[0];
        let mut challengers = candidates(current, self.memory_limit().min(self.max_rows));
        challengers.retain(|size| *size != current);
        challengers.sort_unstable_by_key(|size| {
            if self.prefer_larger {
                usize::MAX - *size
            } else {
                *size
            }
        });
        self.prefer_larger = !self.prefer_larger;
        self.candidates = std::iter::once(current).chain(challengers).collect();
        if self.candidates.len() == 1 {
            self.settle_on(current);
            return;
        }
        self.candidate = 0;
        self.observations = 0;
        self.probing = true;
        self.settled_elapsed = Duration::ZERO;
        self.reset_comparison();
    }

    fn reset_comparison(&mut self) {
        self.window = ProbeWindow::default();
        self.phase = ProbePhase::Before;
        self.wins = 0;
        self.pair_attempts = 0;
        self.before_rate = None;
        self.challenger_rate = None;
        self.candidate = 0;
    }

    fn cap_changed(&mut self, completed_full_batch: bool, elapsed: Duration) {
        let was_probing = self.probing;
        let incumbent = self.candidates[0]
            .min(self.memory_limit())
            .min(self.max_rows)
            .max(1);
        self.candidates = vec![incumbent];
        self.probing = false;
        if was_probing {
            // A moving memory cap invalidates the active before/challenger/after
            // comparison. Start the settled interval again at the new cap.
            self.observations = 0;
            self.settled_elapsed = Duration::ZERO;
        } else if completed_full_batch {
            // Width sampling can move a memory-limited cap on every batch. The
            // completed incumbent work still counts toward the retuning gate;
            // retaining it prevents repeated cap reductions from postponing it.
            self.observations += 1;
            self.settled_elapsed += elapsed;
        }
        self.reset_comparison();
        if !was_probing
            && self.observations >= RETUNE_AFTER
            && self.settled_elapsed >= RETUNE_AFTER_ELAPSED
        {
            self.start_probe();
        }
    }
}

fn candidates(center: usize, max_rows: usize) -> Vec<usize> {
    let mut sizes = vec![
        center,
        (center / 2).max(MIN_ROWS),
        center.saturating_mul(2).min(max_rows),
    ];
    let mut unique = Vec::with_capacity(3);
    for size in sizes.drain(..) {
        if !unique.contains(&size) {
            unique.push(size);
        }
    }
    unique
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(tuner: &mut BatchTuner, rate: f64, bytes_per_row: usize) {
        let rows = tuner.next_rows();
        tuner.observe(
            rows,
            Duration::from_secs_f64(rows as f64 / rate),
            rows * bytes_per_row,
        );
    }

    fn settle_with(tuner: &mut BatchTuner, rate: impl Fn(usize) -> f64) {
        for _ in 0..200 {
            if !tuner.probing {
                return;
            }
            let rows = tuner.next_rows();
            sample(tuner, rate(rows), 10);
        }
        panic!("controller did not settle");
    }

    #[test]
    fn dispatch_overhead_does_not_ratchet_down() {
        let mut t = BatchTuner::new(1_000, usize::MAX);
        settle_with(&mut t, |rows| {
            let work = rows as f64 / 100_000.;
            rows as f64 / (work + 0.005)
        });
        assert!(t.next_rows() >= 1_000);
    }

    #[test]
    fn memory_budget_caps_working_buffers() {
        let mut t = BatchTuner::new(8_192, 10_000);
        sample(&mut t, 1_000., 10);
        assert_eq!(t.next_rows(), 500);
    }

    #[test]
    fn probes_are_bounded() {
        assert_eq!(BatchTuner::new(1, usize::MAX).next_rows(), 256);
        let mut high = BatchTuner::new(usize::MAX, usize::MAX);
        sample(&mut high, 1., 1);
        sample(&mut high, 1., 1);
        assert!(high.next_rows() <= 65_536);
    }

    #[test]
    fn consistently_faster_challenger_is_adopted() {
        let mut t = BatchTuner::new(1_000, usize::MAX);
        settle_with(&mut t, |rows| if rows == 500 { 1_200. } else { 1_000. });
        assert_eq!(t.next_rows(), 500);
    }

    #[test]
    fn transient_load_phase_cannot_steal_a_win() {
        let mut t = BatchTuner::new(1_000, usize::MAX);
        let mut window = 0;
        for _ in 0..200 {
            if !t.probing {
                break;
            }
            let rate = if window == 1 { 1_200. } else { 1_000. };
            sample(&mut t, rate, 10);
            if t.window.batches == 0 {
                window += 1;
            }
        }
        assert!(!t.probing);
        assert_eq!(t.next_rows(), 1_000);
    }

    #[test]
    fn monotonic_load_drift_in_either_direction_does_not_look_like_a_gain() {
        for factor in [1.01_f64, 0.99] {
            let mut t = BatchTuner::new(1_000, usize::MAX);
            let mut n = 0_u32;
            for _ in 0..200 {
                if !t.probing {
                    break;
                }
                n += 1;
                sample(&mut t, 1_000. * factor.powi(n as i32), 10);
            }
            assert_eq!(t.next_rows(), 1_000);
        }
    }

    #[test]
    fn partial_tail_only_updates_memory() {
        let mut t = BatchTuner::new(1_000, 4 << 20);
        assert_eq!(t.next_rows(), 1_000);
        t.observe(100, Duration::from_secs(1), 1_000_000);
        assert_eq!(t.observations, 0);
        assert_eq!(t.next_rows(), 209);
        assert_eq!(t.window.batches, 0);
    }

    #[test]
    fn settled_workload_retunes_after_32_full_batches() {
        let mut t = BatchTuner::new(1_000, usize::MAX);
        settle_with(&mut t, |_| 1_000.);
        assert!(!t.probing);
        for _ in 0..RETUNE_AFTER {
            sample(&mut t, 100., 10);
        }
        assert!(t.probing);
        settle_with(&mut t, |rows| if rows > 1_000 { 1_200. } else { 1_000. });
        if t.next_rows() != 2_000 {
            for _ in 0..RETUNE_AFTER {
                sample(&mut t, 100., 10);
            }
            settle_with(&mut t, |rows| if rows > 1_000 { 1_200. } else { 1_000. });
        }
        assert_eq!(t.next_rows(), 2_000);
    }

    #[test]
    fn memory_estimate_reacts_immediately_to_a_sharp_increase() {
        let mut t = BatchTuner::new(8_192, 1_000_000);
        sample(&mut t, 1_000., 10);
        assert!(t.next_rows() > 1_000);
        let rows = t.next_rows();
        t.observe(rows, Duration::from_secs(1), rows * 1_000);
        assert_eq!(t.next_rows(), 500);
        assert_eq!(t.bytes_per_row, 1_000.);
    }

    #[test]
    fn varying_memory_caps_do_not_stall_or_grow_probe_state() {
        let mut t = BatchTuner::new(8_192, 1 << 20);
        assert_eq!(t.next_rows(), 512, "the conservative prior caps batch one");
        let mut saw_settled = false;
        let mut saw_reprobe = false;
        for i in 0..1_000 {
            let was_probing = t.probing;
            let rows = t.next_rows();
            let bytes_per_row = 400 + i % 101;
            t.observe(
                rows,
                Duration::from_secs_f64(rows as f64 / 10_000.),
                rows * bytes_per_row,
            );
            saw_settled |= !t.probing;
            saw_reprobe |= !was_probing && t.probing;
            assert!(t.window.batches < WINDOW_MIN_BATCHES);
        }
        assert!(saw_settled);
        assert!(saw_reprobe);
    }

    #[test]
    fn repeated_settled_memory_cap_reductions_still_reach_the_retune_gate() {
        let mut tuner = BatchTuner::new(usize::MAX, 100_000_000);
        // Establish the first measured cap; changing it invalidates the initial
        // comparison and leaves the tuner settled at the hard memory boundary.
        let rows = tuner.next_rows();
        tuner.observe(rows, Duration::from_millis(20), rows * 1_100);
        assert!(!tuner.probing);

        for batch in 0..RETUNE_AFTER {
            let rows = tuner.next_rows();
            let bytes_per_row = 1_200 + batch * 100;
            let previous_rows = rows;
            tuner.observe(rows, Duration::from_millis(20), rows * bytes_per_row);
            assert!(tuner.next_rows() <= tuner.memory_limit());
            if batch + 1 < RETUNE_AFTER {
                assert!(
                    tuner.next_rows() < previous_rows,
                    "each larger measured width must lower the hard cap"
                );
            }
        }
        assert!(
            tuner.probing,
            "repeated cap reductions must not discard all settled evidence"
        );
        assert!(tuner.next_rows() <= tuner.memory_limit());
    }

    #[test]
    fn feedback_includes_output_time_and_jobs_are_independent() {
        let feedback = BatchFeedback::new(BatchTuner::new(1_000, usize::MAX));
        for _ in 0..100 {
            let rows = feedback.next_rows();
            feedback.decoded(
                rows,
                Duration::from_secs_f64(rows as f64 / 10_000.),
                rows * 10,
            );
            // Fixed per-batch output cost makes larger batches materially faster.
            feedback.add_output_time(Duration::from_secs(1));
            let settled = {
                let mut state = feedback.0.lock().expect("feedback");
                if let Some((rows, elapsed, bytes)) = state.pending.take() {
                    state.tuner.observe(rows, elapsed, bytes);
                }
                !state.tuner.probing
            };
            if settled {
                break;
            }
        }
        assert!(feedback.status().next_rows > 1_000);

        let other = BatchFeedback::new(BatchTuner::new(8_192, 1 << 20));
        let untouched = other.status();
        let writer_side = feedback.clone();
        std::thread::spawn(move || writer_side.add_output_time(Duration::from_millis(1)))
            .join()
            .expect("writer feedback thread");
        assert_eq!(other.status(), untouched);
    }

    #[test]
    fn predictive_feedback_calibrates_exactly_two_serial_batches() {
        let feedback =
            BatchFeedback::new_predictive(BatchFormat::Columnar, DEFAULT_MEMORY_BYTES, 4).unwrap();
        let caller = std::thread::current().id();
        let mut calls = [0; 3];
        for (batch, call_count) in calls.iter_mut().enumerate() {
            assert_eq!(feedback.calibration_needed(), batch < 2);
            let rows = feedback.next_rows();
            if batch < 2 {
                assert_eq!(rows, MIN_ROWS);
            }
            let value = feedback
                .decode(rows, || {
                    *call_count += 1;
                    if batch < 2 {
                        assert_eq!(rayon::current_num_threads(), 1);
                        assert_ne!(std::thread::current().id(), caller);
                    } else {
                        assert_eq!(std::thread::current().id(), caller);
                    }
                    Ok::<_, std::convert::Infallible>((batch, rows * 100))
                })
                .unwrap();
            assert_eq!(value, batch);
        }
        assert_eq!(calls, [1, 2, 1], "only the measured sample is replayed");
        assert!(!feedback.calibration_needed());
        assert!(feedback.prediction().is_some());
    }

    #[test]
    fn failed_decode_does_not_consume_a_calibration_sample() {
        let feedback =
            BatchFeedback::new_predictive(BatchFormat::Jsonl, DEFAULT_MEMORY_BYTES, 2).unwrap();
        let result = feedback.decode(256, || Err::<((), usize), _>("decode failed"));
        assert_eq!(result, Err("decode failed"));
        assert!(feedback.calibration_needed());
        assert!(feedback.prediction().is_none());
    }

    #[test]
    fn prediction_is_used_before_periodic_tuning() {
        let feedback = BatchFeedback::new_predictive(BatchFormat::Columnar, 1_000_000, 4)
            .expect("calibration pool");
        for _ in 0..2 {
            let rows = feedback.next_rows();
            feedback
                .decode(rows, || Ok::<_, std::convert::Infallible>(((), rows * 100)))
                .expect("decode");
        }
        let predicted = feedback.prediction().expect("prediction").batch_size;
        for _ in 0..RETUNE_AFTER {
            assert_eq!(feedback.next_rows(), predicted);
            assert!(!feedback.0.lock().expect("feedback").tuner.probing);
            feedback.decoded(predicted, Duration::from_millis(10), predicted * 100);
        }
        feedback.next_rows();
        assert!(feedback.0.lock().expect("feedback").tuner.probing);
    }

    #[test]
    fn columnar_predictive_feedback_can_adopt_a_larger_runtime_batch() {
        let feedback = BatchFeedback::new_predictive(BatchFormat::Columnar, usize::MAX, 12)
            .expect("calibration pool");
        for _ in 0..2 {
            let rows = feedback.next_rows();
            feedback.decoded(rows, Duration::from_millis(10), rows * 10);
        }
        assert!(feedback.prediction().expect("prediction").batch_size <= 16_384);

        {
            let mut state = feedback.0.lock().expect("feedback");
            // Isolate the runtime boundary from the model's particular seed.
            state.tuner.candidates = vec![5_000];
            state.tuner.probing = false;
            state.tuner.observations = 0;
            state.tuner.settled_elapsed = Duration::ZERO;
        }
        for _ in 0..RETUNE_AFTER {
            let rows = feedback.next_rows();
            feedback.decoded(rows, Duration::from_millis(10), rows * 10);
        }
        feedback.next_rows();
        for _ in 0..200 {
            let rows = feedback.next_rows();
            let rate = if rows > 5_000 { 1_200. } else { 1_000. };
            feedback.decoded(rows, Duration::from_secs_f64(rows as f64 / rate), rows * 10);
            feedback.next_rows();
            let state = feedback.0.lock().expect("feedback");
            if !state.tuner.probing && state.tuner.next_rows() > 5_000 {
                break;
            }
        }
        let state = feedback.0.lock().expect("feedback");
        assert!(!state.tuner.probing);
        let rows = state.tuner.next_rows();
        assert!(rows > 5_000);
        assert!(rows <= BatchFormat::Columnar.adaptive_max_rows());
    }

    #[test]
    fn legacy_predictive_feedback_rejects_native_worker_batch_semantics() {
        let error = BatchFeedback::new_predictive(BatchFormat::Native, usize::MAX, 12)
            .expect_err("native stream boundary");
        assert!(error.to_string().contains("native stream API"));
        assert!(error.to_string().contains("per worker"));
    }

    #[test]
    fn columnar_runtime_exploration_obeys_memory_and_runtime_caps() {
        let mut memory_limited = BatchTuner::new(5_000, 120_000)
            .with_max_rows(BatchFormat::Columnar.adaptive_max_rows());
        memory_limited.bytes_per_row = 10.;
        memory_limited.memory_measured = true;
        memory_limited.candidates = vec![5_000];
        memory_limited.probing = false;
        for _ in 0..RETUNE_AFTER {
            let rows = memory_limited.next_rows();
            assert!(rows <= 6_000);
            memory_limited.observe(rows, Duration::from_millis(10), rows * 10);
        }
        for _ in 0..200 {
            let rows = memory_limited.next_rows();
            assert!(rows <= 6_000);
            let rate = if rows > 5_000 { 1_200. } else { 1_000. };
            memory_limited.observe(rows, Duration::from_secs_f64(rows as f64 / rate), rows * 10);
            if !memory_limited.probing && memory_limited.next_rows() > 5_000 {
                break;
            }
        }
        assert_eq!(memory_limited.next_rows(), 6_000);

        let runtime_limited = BatchTuner::new(usize::MAX, usize::MAX)
            .with_max_rows(BatchFormat::Columnar.adaptive_max_rows());
        assert_eq!(
            runtime_limited.next_rows(),
            BatchFormat::Columnar.adaptive_max_rows()
        );
    }

    #[test]
    fn total_duration_replaces_only_a_pending_regular_batch() {
        let feedback =
            BatchFeedback::new_predictive(BatchFormat::Columnar, DEFAULT_MEMORY_BYTES, 1).unwrap();
        for _ in 0..2 {
            let rows = feedback.next_rows();
            feedback
                .decode(rows, || Ok::<_, std::convert::Infallible>(((), rows * 10)))
                .unwrap();
            feedback.observe_total(rows, Duration::from_secs(99), rows * 20);
        }
        assert!(feedback.prediction().is_some());
        let rows = feedback.next_rows();
        feedback
            .decode(rows, || Ok::<_, std::convert::Infallible>(((), rows * 10)))
            .unwrap();
        feedback.observe_total(rows, Duration::from_secs(1), rows * 20);
        let state = feedback.0.lock().unwrap();
        assert_eq!(
            state.pending,
            Some((rows, Duration::from_secs(1), rows * 20))
        );
    }
}
