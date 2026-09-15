# Twelve-worker native decoding exceeds one million VIN/s

The production native stream now averages **1,037,755 VIN/s** on this M2 Max
(eight performance cores and four efficiency cores), with twelve workers and
automatic batch selection. Both confirmation runs exceeded one million VIN/s.
Every timed pass decoded **20 million unique VINs**, constructed full results,
delivered batches in input order, and completed worker-owned cleanup before the
timer stopped. Calibration and worker startup are included.

## Confirmation

The comparison ran sequentially: candidate, baseline, baseline, candidate.
Each process completed a full-corpus warm pass before its full timed pass.
No other benchmark, compilation, test suite, or profiling job ran concurrently.

| Run | VIN/s | Timed seconds |
|---|---:|---:|
| Candidate 1 | 1,040,019 | 19.230 |
| Baseline 1 | 965,529 | 20.714 |
| Baseline 2 | 930,474 | 21.494 |
| Candidate 2 | 1,035,491 | 19.315 |

| Mean measurement | Baseline | Candidate | Change |
|---|---:|---:|---:|
| Throughput | 948,002 VIN/s | 1,037,755 VIN/s | +9.5% |
| Process CPU time per VIN | 11.357 µs | 10.514 µs | −7.4% |
| Process instructions per VIN | 75,028 | 63,552 | −15.3% |
| Peak process RSS | 2,021,179,392 bytes | 2,090,369,024 bytes | +3.4% |

These are arithmetic means of the two observations per binary. The host is
shared: between experiments, an unrelated OCR process and virtual machine were
observed using CPU. The repeated comparison and process counters distinguish
reduced decoder work from wall-time variation; this was not an isolated host.
RSS includes the input corpus, database, caches, allocator, and output slots.

[Complete measurements, raw output, commands, and hashes](../scripts/bench/native_million_v7_2026_09_15.json).

## What changed

- Workers retain bounded VIN/header and output-element buffers after clearing
  their previous values. A workspace tied to each worker's database also reuses
  intermediate item vectors and the candidate-year pass container. Losing,
  pruned, and winning passes return storage after their owned values are cleared.
- Pattern matching reuses bounded scratch maps and hit vectors. Sort keys are
  computed once for matched patterns and for the worker's locality order.
- Each database caches immutable projection labels and types, eliminating
  repeated archive string-offset lookups while constructing full results.
- Error text uses direct assembly for fixed VIN positions. Additional-info
  strings grow in place instead of repeatedly formatting and copying them.
  Unicode trimming, the 500-character limit after every step, and SQL NULL
  behavior remain unchanged.

The error-text step produced the substantial instruction-count reduction.
The reported 9.5% throughput gain measures the combined implementation against
the original production baseline, rather than attributing every gain to that step.

Retention has explicit limits: VIN buffers above 64 bytes and element vectors
above 256 entries are released; intermediate storage holds at most three item
vectors of at most 256 entries and one pass vector of at most four entries.
Full output-buffer retention is enabled only when physical slot capacity fits
the configured in-flight row limit. Workers release all retained storage before
joining, including on cancellation or panic. Decoder workspaces are separate
from the automatic planner's estimate of output-slot storage.

## Automatic selection

These improvements are used automatically by the native stream. The confirmed
plan remains **100 VINs per batch, five slots per worker, twelve workers**, with
at most **6,000 in-flight rows**. One captured clock covers the entire job.
The sampled output width in the final run estimated 104,604,000 bytes of slot
storage; this is not a total-process RSS limit.

A [production-engine plan sweep](../scripts/bench/native_slot_retune_2026_09_15.json)
on the preceding candidate retained 100×5: it measured 989,766 VIN/s versus
959,202 for 200×2, 975,694 for 200×5, and 961,260 for 400×2. The final improvement
does not change the planner's selection table or claim a newly fitted throughput
model. Final acceptance used the production **auto** path, including calibration.

## Experiments and profiling

[Three follow-up trials](NATIVE_THREE_TRIALS_2026_09_15.md) compare per-VIN
context reuse, conversion indices, and deferred correction output against this
confirmed V7 binary. Those results are separate from the historical comparison
above.

Earlier comparisons are retained rather than overwritten:
[buffer reuse](../scripts/bench/native_million_2026_09_15.json),
[deferred recycling](../scripts/bench/native_million_v2_2026_09_15.json),
[eager recycling restored](../scripts/bench/native_million_v3_2026_09_15.json),
[intermediate item reuse](../scripts/bench/native_million_v4_2026_09_15.json), and
[projection metadata](../scripts/bench/native_million_v6_2026_09_15.json).
Deferred recycling weakened the comparison and was removed.

The [current-worker profile](../scripts/bench/native_v5_profile_2026_09_15.json)
attributed 18.2% of non-wait exclusive observations to full projection, 16.8% to
errors/corrections, and 5.7% to formatting/string routines. Those are sampled
stack categories, not exact source-line CPU costs. Sampling slowed that run;
its throughput is diagnostic and is excluded from acceptance measurements.

A cross-VIN pattern cache was rejected after the
[corrected signature diagnostic](../scripts/bench/pattern_signature_diagnostic_2026_09_15.json)
found only 0.30% hits in a 4,096-entry cache over the first million corpus VINs.
The diagnostic uses extended low-volume WMIs and includes schema eligibility
when no model year is resolved. No cross-VIN result or pattern cache was shipped.

## Reproduction and validation

```sh
UV_FROZEN=1 make checku
cargo build --release -p ultravin --example throughput
UV_FROZEN=1 uv run -- python -m scripts.bench.native_million \
  --output scripts/bench/native_million_local.json
```

The runner requires the existing hashed 20-million-unique-VIN corpus and frozen
baseline binary. It refuses to overwrite evidence and archives the candidate
binary, source, and build inputs before timing. The frozen clock is
`1788220800000000` microseconds; both binaries use mimalloc.

Confirmed candidate SHA-256:
`0a1fd680a5126edd03fcb88c34f45cf9fa4ec84372c9d6c14bf244f8eeec2c58`.
Baseline SHA-256:
`ccc3982716ae6442a39740e577f6c0cc64f0fb1b5f6076816e17a0b624e93a58`.

`make checku` passes: 950 Python tests, 241 Rust library tests, Rust integration
tests, formatting, lint, and all checked feature combinations. The manual
signature diagnostic is ignored in the ordinary suite and was run separately.
Tests cover exact full-result parity, caller years, Unicode and whitespace,
extended WMIs, tails, tight row limits, database lifetimes, and panic cleanup.
