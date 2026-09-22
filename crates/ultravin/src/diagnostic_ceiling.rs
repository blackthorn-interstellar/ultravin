//! Compile-gated architecture diagnostics. These are measurement probes, not APIs.

use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use crate::{
    current_year_at, decode_full_reusing, decode_full_with_workspace,
    decode_items_with_buffers_and_workspace, Db, DecodeResult, DecodeWorkspace, NativeStreamConfig,
};

pub const BATCH_SIZE: usize = 100;
pub const SLOTS_PER_WORKER: usize = 5;

#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkerCount {
    pub worker: usize,
    pub batches: usize,
    pub rows: usize,
    pub active_wall_ns: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RunReport {
    pub mode: &'static str,
    pub rows: usize,
    pub batches: usize,
    pub workers: Vec<WorkerCount>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StageReport {
    pub label: &'static str,
    pub full_result_black_box: bool,
    pub rows: usize,
    pub batches: usize,
    pub source_batches: usize,
    pub source_batch_stride: usize,
    pub workers: usize,
    pub slots: usize,
    pub instrumented_total_ns: u64,
    pub internal_decode_ns: u64,
    pub full_projection_ns: u64,
    pub cleanup_ns: u64,
    pub checksum: u64,
    pub warning: &'static str,
    pub timing_mechanism: &'static str,
}

fn mix(mut state: u64, bytes: &[u8]) -> u64 {
    for &byte in bytes {
        state ^= u64::from(byte);
        state = state.wrapping_mul(0x100_0000_01b3);
    }
    state
}

/// Visit every public output field. Use for untimed parity checks only.
pub fn full_checksum<'a>(results: impl IntoIterator<Item = &'a DecodeResult<'static>>) -> u64 {
    let mut state = 0xcbf2_9ce4_8422_2325;
    for result in results {
        for text in [
            &result.vin,
            &result.wmi,
            &result.descriptor,
            &result.corrected_vin,
        ] {
            state = mix(state, text.as_bytes());
        }
        state = mix(state, &result.model_year.unwrap_or_default().to_le_bytes());
        state = mix(state, &[result.check_digit_valid as u8]);
        for code in &result.error_codes {
            state = mix(state, &code.to_le_bytes());
        }
        for element in &result.elements {
            for text in [
                element.group_name,
                element.variable,
                &element.value,
                &element.attribute_id,
                element.code,
                element.data_type,
                element.decode,
                &element.source,
                &element.keys,
            ] {
                state = mix(state, text.as_bytes());
            }
            for value in [
                element.element_id,
                element.pattern_id.unwrap_or_default(),
                element.vin_schema_id.unwrap_or_default(),
                element.wmi_id.unwrap_or_default(),
            ] {
                state = mix(state, &value.to_le_bytes());
            }
            state = mix(state, &element.created_on.unwrap_or_default().to_le_bytes());
            state = mix(state, &[element.to_be_qced as u8]);
        }
    }
    state
}

/// Production ordered stream with the same minimal timed consumer as throughput probes.
pub fn run_ordered(inputs: &[String], workers: usize, now_micros: i64) -> RunReport {
    let batches = inputs.len().div_ceil(BATCH_SIZE);
    let mut rows = 0;
    let mut next_batch = 0;
    Db::embedded()
        .decode_native_stream_at(
            inputs,
            None,
            now_micros,
            NativeStreamConfig {
                workers,
                batch_size: BATCH_SIZE,
                slots_per_worker: SLOTS_PER_WORKER,
                max_inflight_rows: workers * BATCH_SIZE * SLOTS_PER_WORKER,
            },
            |batch| {
                assert_eq!(batch.batch_index(), next_batch);
                assert_eq!(batch.start_index(), rows);
                rows += batch.len();
                black_box(batch);
                next_batch += 1;
            },
        )
        .expect("ordered diagnostic stream");
    assert_eq!((rows, next_batch), (inputs.len(), batches));
    RunReport {
        mode: "ordered-production",
        rows,
        batches,
        workers: Vec::new(),
    }
}

/// Remove ordered publication and let each native worker consume its own full results.
pub fn run_worker_local(inputs: &[String], workers: usize, now_micros: i64) -> RunReport {
    run_worker_local_impl::<false, _>(inputs, workers, now_micros, |_, _| {})
}

fn run_worker_local_impl<const CAPTURE: bool, F>(
    inputs: &[String],
    workers: usize,
    now_micros: i64,
    capture: F,
) -> RunReport
where
    F: Fn(usize, &DecodeResult<'static>) + Sync,
{
    let batches = inputs.len().div_ceil(BATCH_SIZE);
    let next_batch = AtomicUsize::new(0);
    let reports = std::thread::scope(|scope| {
        let handles = (0..workers)
            .map(|worker| {
                let next_batch = &next_batch;
                let capture = &capture;
                scope.spawn(move || {
                    let worker_started = Instant::now();
                    let db = Db::embedded();
                    let current_year = current_year_at(now_micros);
                    let mut workspace = DecodeWorkspace::default();
                    let mut slots = (0..SLOTS_PER_WORKER)
                        .map(|_| {
                            (0..BATCH_SIZE)
                                .map(|_| None)
                                .collect::<Vec<Option<DecodeResult<'static>>>>()
                        })
                        .collect::<Vec<_>>();
                    let mut order = Vec::with_capacity(BATCH_SIZE);
                    let mut worker_batches = 0;
                    let mut worker_rows = 0;
                    let mut slot_cursor = 0;
                    loop {
                        let batch = next_batch.fetch_add(1, Ordering::Relaxed);
                        if batch >= batches {
                            break;
                        }
                        let start = batch * BATCH_SIZE;
                        let end = (start + BATCH_SIZE).min(inputs.len());
                        let slot = &mut slots[slot_cursor];
                        slot_cursor = (slot_cursor + 1) % SLOTS_PER_WORKER;
                        for result in slot.iter_mut().flatten() {
                            crate::native_stream::prepare_result_reuse(result);
                        }
                        order.clear();
                        order.extend(
                            (start..end).map(|index| (crate::locality_key(&inputs[index]), index)),
                        );
                        order.sort_unstable_by_key(|&(key, _)| key);
                        for (_, index) in order.iter().copied() {
                            let output = &mut slot[index - start];
                            *output = Some(match output.take() {
                                Some(previous) => decode_full_reusing(
                                    db,
                                    &inputs[index],
                                    now_micros,
                                    current_year,
                                    None,
                                    previous,
                                    &mut workspace,
                                ),
                                None => decode_full_with_workspace(
                                    db,
                                    &inputs[index],
                                    now_micros,
                                    current_year,
                                    None,
                                    &mut workspace,
                                ),
                            });
                        }
                        if CAPTURE {
                            for index in start..end {
                                capture(
                                    index,
                                    slot[index - start]
                                        .as_ref()
                                        .expect("local result slot initialized"),
                                );
                            }
                        }
                        black_box(&slot[..end - start]);
                        worker_batches += 1;
                        worker_rows += end - start;
                    }
                    drop(slots);
                    WorkerCount {
                        worker,
                        batches: worker_batches,
                        rows: worker_rows,
                        active_wall_ns: worker_started.elapsed().as_nanos() as u64,
                    }
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("local diagnostic worker"))
            .collect::<Vec<_>>()
    });
    assert_eq!(
        reports.iter().map(|report| report.batches).sum::<usize>(),
        batches
    );
    assert_eq!(
        reports.iter().map(|report| report.rows).sum::<usize>(),
        inputs.len()
    );
    RunReport {
        mode: "worker-local-diagnostic",
        rows: inputs.len(),
        batches,
        workers: reports,
    }
}

/// Untimed full-output capture through worker-local decode for parity checks.
pub fn worker_local_outputs(
    inputs: &[String],
    workers: usize,
    now_micros: i64,
) -> Vec<DecodeResult<'static>> {
    let captured = std::sync::Mutex::new(
        (0..inputs.len())
            .map(|_| None)
            .collect::<Vec<Option<DecodeResult<'static>>>>(),
    );
    let report = run_worker_local_impl::<true, _>(inputs, workers, now_micros, |index, result| {
        let mut captured = captured.lock().unwrap_or_else(|error| error.into_inner());
        assert!(
            captured[index].is_none(),
            "worker-local duplicate input visit"
        );
        captured[index] = Some(result.clone());
    });
    assert_eq!(report.rows, inputs.len());
    captured
        .into_inner()
        .unwrap_or_else(|error| error.into_inner())
        .into_iter()
        .map(|result| result.expect("worker-local input visited exactly once"))
        .collect()
}

/// Time stage boundaries over complete batches while preserving workspace/output reuse.
pub fn measure_stages(inputs: &[String], requested_rows: usize, now_micros: i64) -> StageReport {
    let source_batches = inputs.len().div_ceil(BATCH_SIZE);
    let requested_batches = requested_rows.div_ceil(BATCH_SIZE).max(1);
    let sampled_batches = requested_batches.min(source_batches);
    let source_batch_stride = (source_batches / sampled_batches).max(1);
    let mut sampled = Vec::with_capacity(sampled_batches * BATCH_SIZE);
    for sample in 0..sampled_batches {
        let source_batch = sample * source_batches / sampled_batches;
        let start = source_batch * BATCH_SIZE;
        sampled.extend(inputs[start..(start + BATCH_SIZE).min(inputs.len())].iter());
    }
    let db = Db::embedded();
    let current_year = current_year_at(now_micros);
    let mut workspace = DecodeWorkspace::default();
    let mut slots = (0..BATCH_SIZE)
        .map(|_| None)
        .collect::<Vec<Option<DecodeResult<'static>>>>();
    let mut decode_ns = 0_u64;
    let mut projection_ns = 0_u64;
    let mut cleanup_ns = 0_u64;
    let mut checksum = 0_u64;
    let mut order = Vec::with_capacity(BATCH_SIZE);
    let total_started = Instant::now();
    for (batch, chunk) in sampled.chunks(BATCH_SIZE).enumerate() {
        order.clear();
        let start = batch * BATCH_SIZE;
        order.extend(
            (start..start + chunk.len()).map(|index| (crate::locality_key(sampled[index]), index)),
        );
        order.sort_unstable_by_key(|&(key, _)| key);
        for (_, index) in order.iter().copied() {
            let cell = &mut slots[index - start];
            let cleanup_started = Instant::now();
            let (vin, wmi, descriptor, elements) = match cell.take() {
                Some(mut previous) => {
                    crate::native_stream::prepare_result_reuse(&mut previous);
                    let DecodeResult {
                        vin,
                        wmi,
                        descriptor,
                        error_codes,
                        corrected_vin,
                        mut elements,
                        ..
                    } = previous;
                    drop((error_codes, corrected_vin));
                    elements.clear();
                    (vin, wmi, descriptor, elements)
                }
                None => (String::new(), String::new(), String::new(), Vec::new()),
            };
            cleanup_ns += cleanup_started.elapsed().as_nanos() as u64;
            let decode_started = Instant::now();
            let raw = decode_items_with_buffers_and_workspace(
                db,
                sampled[index],
                now_micros,
                current_year,
                None,
                vin,
                wmi,
                descriptor,
                &mut workspace,
            );
            decode_ns += decode_started.elapsed().as_nanos() as u64;
            let projection_started = Instant::now();
            let result = raw.full_into_workspace(elements, &mut workspace);
            projection_ns += projection_started.elapsed().as_nanos() as u64;
            checksum = checksum.wrapping_add(result.elements.len() as u64);
            black_box(&result);
            *cell = Some(result);
        }
    }
    let cleanup_started = Instant::now();
    drop(slots);
    cleanup_ns += cleanup_started.elapsed().as_nanos() as u64;
    let instrumented_total_ns = total_started.elapsed().as_nanos() as u64;
    StageReport {
        label: "diagnostic-stage-timing-non-additive",
        full_result_black_box: true,
        rows: sampled.len(),
        batches: sampled_batches,
        source_batches,
        source_batch_stride,
        workers: 1,
        slots: 1,
        instrumented_total_ns,
        internal_decode_ns: decode_ns,
        full_projection_ns: projection_ns,
        cleanup_ns,
        checksum,
        warning: "diagnostic stage wall-time ceilings; non-additive because timing boundaries alter cache and allocation state",
        timing_mechanism: "Instant around each row stage, aggregated over evenly spaced complete source batches",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_788_220_800_000_000;

    #[test]
    fn worker_local_capture_matches_full_decode_after_slot_reuse() {
        let inputs = (0..1_203)
            .map(|index| format!("malformed-{index}"))
            .collect::<Vec<_>>();
        let expected = inputs
            .iter()
            .map(|input| crate::decode_full(Db::embedded(), input, NOW, current_year_at(NOW), None))
            .collect::<Vec<_>>();
        assert_eq!(worker_local_outputs(&inputs, 2, NOW), expected);
    }

    #[test]
    fn worker_local_counts_every_partial_tail_row_once() {
        let inputs = (0..503)
            .map(|index| format!("short-{index}"))
            .collect::<Vec<_>>();
        let report = run_worker_local(&inputs, 1, NOW);
        assert_eq!((report.rows, report.batches), (503, 6));
        assert_eq!(
            report
                .workers
                .iter()
                .map(|worker| worker.rows)
                .sum::<usize>(),
            503
        );
        assert_eq!(
            report
                .workers
                .iter()
                .map(|worker| worker.batches)
                .sum::<usize>(),
            6
        );
    }
}
