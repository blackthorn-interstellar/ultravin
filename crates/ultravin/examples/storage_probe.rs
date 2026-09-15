//! Compare independently owned full results with experimental batch slabs.
//! `storage_probe CORPUS SECONDS MODE BATCH_ROWS SHARD_ROWS [COUNT_ROWS]`
//! MODE: owned, slab, or converted. COUNT_ROWS selects a separate allocation
//! diagnostic (one pass); those times are not throughput measurements.

#[path = "support/allocation_counter.rs"]
mod allocation_counter;
#[path = "support/counters.rs"]
mod counters;
#[path = "support/cpu.rs"]
mod cpu;

use std::hint::black_box;
use std::time::{Duration, Instant};

const NOW: i64 = 1_788_220_800_000_000;

fn owned_bytes(rows: &ultravin::BatchResults<ultravin::DecodeResult<'_>>) -> usize {
    use std::borrow::Cow;
    use std::mem::size_of;
    rows.capacity() * size_of::<ultravin::DecodeResult<'_>>()
        + rows
            .iter()
            .map(|row| {
                row.vin.capacity()
                    + row.wmi.capacity()
                    + row.descriptor.capacity()
                    + row.corrected_vin.capacity()
                    + row.error_codes.capacity() * size_of::<i32>()
                    + row.elements.capacity() * size_of::<ultravin::DecodedElement<'_>>()
                    + row
                        .elements
                        .iter()
                        .flat_map(|element| {
                            [
                                &element.value,
                                &element.attribute_id,
                                &element.keys,
                                &element.source,
                            ]
                        })
                        .map(|text| match text {
                            Cow::Borrowed(_) => 0,
                            Cow::Owned(text) => text.capacity(),
                        })
                        .sum::<usize>()
            })
            .sum::<usize>()
}

fn pass(inputs: &[String], mode: &str, batch_rows: usize, shard_rows: usize, count: bool) -> usize {
    let mut maximum_bytes = 0;
    for chunk in inputs.chunks(batch_rows) {
        match mode {
            "owned" => {
                let output = ultravin::decode_batch_managed_at(chunk, None, NOW);
                assert_eq!(output.len(), chunk.len());
                if count {
                    maximum_bytes = maximum_bytes.max(owned_bytes(&output));
                }
                drop(black_box(output));
            }
            "slab" | "converted" => {
                let output = ultravin::decode_batch_slab_with_options_at(
                    chunk,
                    None,
                    NOW,
                    ultravin::SlabOptions {
                        rows_per_shard: shard_rows,
                    },
                );
                assert_eq!(output.len(), chunk.len());
                if mode == "converted" {
                    let output = output.into_owned();
                    if count {
                        maximum_bytes = maximum_bytes.max(owned_bytes(&output));
                    }
                    drop(black_box(output));
                } else {
                    if count {
                        maximum_bytes = maximum_bytes.max(output.allocated_bytes());
                    }
                    drop(black_box(output));
                }
            }
            _ => unreachable!(),
        }
    }
    maximum_bytes
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("CORPUS required");
    let seconds: f64 = args
        .next()
        .expect("SECONDS required")
        .parse()
        .expect("SECONDS number");
    let mode = args.next().expect("MODE required");
    assert!(
        matches!(mode.as_str(), "owned" | "slab" | "converted"),
        "invalid mode"
    );
    let batch_rows: usize = args
        .next()
        .expect("BATCH_ROWS required")
        .parse()
        .expect("BATCH_ROWS integer");
    let shard_rows: usize = args
        .next()
        .expect("SHARD_ROWS required")
        .parse()
        .expect("SHARD_ROWS integer");
    let count_rows: Option<usize> = args
        .next()
        .map(|value| value.parse().expect("COUNT_ROWS integer"));
    assert!(batch_rows > 0 && shard_rows > 0 && seconds.is_finite() && seconds > 0.0);
    let corpus = std::fs::read_to_string(path).expect("read corpus");
    let inputs: Vec<String> = corpus
        .lines()
        .take(count_rows.unwrap_or(usize::MAX))
        .map(str::to_owned)
        .collect();
    drop(corpus);
    assert!(!inputs.is_empty(), "empty corpus");

    // Check every field and input order before timing, including the conversion
    // control. Separate tests cover invalid/short VINs and external DB lifetimes.
    let sample = &inputs[..inputs.len().min(1_000)];
    let expected = ultravin::decode_batch_managed_at(sample, None, NOW);
    let slab = ultravin::decode_batch_slab_with_options_at(
        sample,
        None,
        NOW,
        ultravin::SlabOptions {
            rows_per_shard: shard_rows,
        },
    );
    for (index, expected) in expected.iter().enumerate() {
        assert_eq!(
            serde_json::to_value(expected).unwrap(),
            serde_json::to_value(slab.get(index).unwrap()).unwrap()
        );
    }
    assert_eq!(expected.as_ref(), slab.into_owned().as_ref());
    drop(expected);

    pass(&inputs, &mode, batch_rows, shard_rows, false);
    eprintln!("storage probe: warmup complete; starting whole passes");
    if count_rows.is_some() {
        allocation_counter::start();
    }
    let cpu_start = cpu::snapshot();
    let counter_start = counters::snapshot();
    let start = Instant::now();
    let mut rows = 0;
    let mut maximum_bytes = 0;
    loop {
        maximum_bytes = maximum_bytes.max(pass(
            &inputs,
            &mode,
            batch_rows,
            shard_rows,
            count_rows.is_some(),
        ));
        rows += inputs.len();
        if count_rows.is_some() || start.elapsed() >= Duration::from_secs_f64(seconds) {
            break;
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    let process_counters = counters::elapsed(counter_start, counters::snapshot());
    let process_cpu = cpu::elapsed(cpu_start, cpu::snapshot());
    let allocations = count_rows.map(|_| allocation_counter::finish());
    println!(
        "{}",
        serde_json::json!({
            "kind": "batch_storage_probe",
            "mode": mode,
            "batch_rows": batch_rows,
            "shard_rows": shard_rows,
            "now_micros": NOW,
            "workers": ultravin::predictor::worker_count(),
            "rows": rows,
            "unique_input_rows": inputs.len(),
            "elapsed_seconds": elapsed,
            "actual_rows_per_second": if count_rows.is_none() { Some(rows as f64 / elapsed) } else { None },
            "process_counters": process_counters,
            "process_user_cpu_seconds": process_cpu.map(|cpu| cpu.user_seconds),
            "process_system_cpu_seconds": process_cpu.map(|cpu| cpu.system_seconds),
            "average_busy_cores": process_cpu.map(|cpu| cpu.average_busy_cores(elapsed)),
            "allocation_diagnostic": allocations,
            "maximum_live_output_bytes": if count_rows.is_some() { Some(maximum_bytes) } else { None },
            "parity_rows": sample.len(),
        })
    );
}
