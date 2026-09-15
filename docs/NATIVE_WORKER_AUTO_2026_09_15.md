# Native worker-slot automatic batching

Native automatic streaming now sends whole batches to workers. Each worker
decodes into reusable slots; the caller consumes complete results in input
order, and the original worker clears the slot before using it again.

Subsequent buffer and error-text improvements reached **1,037,755 VIN/s** with
twelve workers in production auto mode. See the
[million-VIN/s confirmation](NATIVE_MILLION_2026_09_15.md) for repeated results,
source hashes, and the full ordered-output benchmark contract.

`ultravin-native-slots-v1` replaces the shared-batch native predictor. It chooses
two quantities: VINs per worker batch and outstanding slots per worker. The
JSONL and Arrow/Parquet predictors retain their existing execution paths.

## Use it

```rust
let vins = vec!["1HGCM82633A004352".to_owned()];
let prediction = ultravin::decode_native_stream(&vins, None, |batch| {
    for result in batch.iter() {
        // Read every field here, or write it to your output.
        println!("{}: {:?}", result.vin, result.model_year);
    }
})?;
```

This entry point uses auto by default. `decode_native_stream_auto_at` accepts
an explicit clock and `NativeAutoOptions` (worker count and output-memory
budget). The corresponding `Db` method supports a caller-owned database.
`decode_native_stream_at` accepts an explicit `NativeStreamConfig` to bypass
calibration and prediction.

The callback borrows results from a slot until it returns. Cloning results to
retain them is possible, but those copies become caller-owned storage outside
the slot budget. Existing `decode_batch` and `decode_batch_managed` return
owned results and keep their existing contract.

Python's `predict_batch_size(output="native", ...)` returns the same plan.
Its `batch_size` now means rows **per worker batch**. New fields are
`slots_per_worker` and `max_inflight_rows`. It does not change Python
`decode_batch` into a borrowed-result stream.

## Selection and memory

The native selector searches measured combinations of batch size and slot
count under the supplied memory budget, choosing the least storage within
1% of the estimated feasible peak. Worker counts between measured points
use interpolation; CPU speed scales the throughput estimate, not the selected
batch size at fixed worker count and memory. This model is
based on one machine's measurements, not a cross-machine optimum guarantee.

At the reference output width and default budget, the measured worker points
select:

| Workers | VINs per worker batch | Slots per worker | Maximum in-flight rows | Estimated slot storage |
|---:|---:|---:|---:|---:|
| 4 | 100 | 5 | 2,000 | 33.2 MiB |
| 8 | 200 | 2 | 3,200 | 53.1 MiB |
| 12 | 100 | 5 | 6,000 | 99.6 MiB |

The one-worker measurement covers the 200-VIN/five-slot plan. Between worker
points, the selector compares plans measured at both endpoints. It uses the
full batch/slot surface at twelve workers. Beyond twelve it retains that
throughput surface as a starting assumption while scaling slot memory with
the requested workers; additional-core speed gains are not estimated.
Budgets too small for a measured plan use a smaller single-slot fallback.

Working output storage is estimated as:

```text
workers × batch_size × slots_per_worker × bytes_per_row
```

The default budget is 512 MiB. The former global 12,000-row constant is gone
from the production automatic path: `max_inflight_rows` comes from the selected
plan. Smaller budgets can reduce the batch or the number of outstanding slots.
More workers under the same budget can force fewer slots per worker, so total
slot storage need not rise monotonically with worker count.
The estimate excludes database caches, input storage, allocator overhead, and
transient decoding allocations. The stream enforces a live **row** bound; it
does not implement a hard byte allocator. Native `estimated_peak_rss_bytes` is
`None`, because the old shared-batch RSS fit does not describe this engine.

Automatic calibration selects up to 256 VINs evenly across the input, warms
that sample, then times serial
full-result decoding and destruction on the same sample. A separate pass
measures the maximum returned-result width in that sample, with the existing
17,408-byte reference as a floor. Calibration adds work but never duplicates
delivered rows. One captured `now` covers calibration and every worker batch.
Empty jobs return no prediction and invoke no callback.

The reference serial rate is **174,387.695 VIN/s**, the median of all four
production calibration observations on this corpus. They ranged from 158,408
to 181,678 VIN/s; individual throughput estimates remain noisier than batch/slot
selection, which does not depend on the speed multiplier. The sampled maximum
row width was 17,288 bytes in all four jobs; the default rounds this up to
**17,408 bytes**. These align the reference inputs with the new kernel rather
than carrying over the retired controller's sampling method. See the
[calibration inputs and derivation](../scripts/bench/native_worker_calibration_2026_09_15.json).

## Benchmarks and migration

`throughput CORPUS SECONDS batch full auto` (also the default when the full
batch size is omitted) now calls the production automatic
worker-slot API. Its timed interval includes calibration, full results in input
order, and cleanup on the owning workers. It warms one complete corpus pass
before timing complete passes. `result_owner` is `worker_slots`, and the JSON
record includes the selected plan.

`BatchFeedback::new_predictive(BatchFormat::Native, ...)` now returns an error
directing callers to native streaming. A per-worker plan cannot be fed into
the old loop that splits each global batch across a Rayon pool. Arrow/Parquet
and JSONL feedback remain supported.

Historical benchmark JSON files and the README graph retain their recorded
measurements; changing the implementation does not rewrite old scores. The
[retired native model](NATIVE_SHARED_PREDICTOR_HISTORY.md) remains documented
separately.

Measurements for this change use the existing 20-million-unique-VIN corpus,
fixed `now = 1788220800000000`, full warm and timed passes, and archived probe
binaries. The [batch × slot sweep](../scripts/bench/slot_budget_sweep_2026_09_15.json)
records the selection data and artifact hashes.

## Production comparison

| Workers | Shared batch, VIN/s | Production auto, VIN/s | Gain |
|---:|---:|---:|---:|
| 4 | 421,961 | 474,963 | 12.6% |
| 8 | 755,204 | 846,679 | 12.1% |
| 12 | 824,674 | 965,228 | 17.0% |

The twelve-worker values average two reversed fresh-process pairs. Four and
eight workers each have one pair. Every timed observation processes all
20 million unique VINs and includes full results in input order and their
destruction. Auto also includes calibration, slot allocation, and worker
startup/shutdown. The shared baseline uses the archived 12,000-row managed
batch implementation. See the [raw production comparison and source archives](../scripts/bench/native_worker_auto_2026_09_15.json).

These comparisons supplied the calibration alignment above. Updating the
reference rate and width preserves all three selected batch/slot plans. A
separate [final-build confirmation](../scripts/bench/native_worker_auto_confirmation_2026_09_15.json)
records the aligned model's own timing.

Updated model figures: [batch size](figures/batch-size-heatmap.svg) and
[working output memory](figures/batch-memory-heatmap.svg).
