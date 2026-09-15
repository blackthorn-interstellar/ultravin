# Allocation and automatic retuning follow-up — 2026-09-14

This follow-up implements two allocation reductions and fixes automatic retuning
when measured result widths repeatedly reduce the memory cap. Cleanup task sizing
was tested separately and rejected: neither setting improved twelve-worker CPU
efficiency over the ordinary scheduler.

## Changes retained

The decoder builds its descriptor directly from the already sanitized VIN. The
public descriptor helper retains its arbitrary-input normalization behavior.
Decimal conversion now builds its digit magnitude in one input-sized buffer,
eliminating separate integer/fraction buffers and their concatenation. Both
changes preserve decoded values and ownership.

The automatic batch tuner previously reset its retuning interval whenever a
changed memory estimate reduced the requested batch size. Repeated reductions
could prevent a settled tuner from reaching its normal retuning gate. Completed
full batches now continue counting toward that gate. An interrupted active
comparison is still discarded, partial EOF tails do not count, and the memory
limit continues to constrain every request. This changes feedback behavior; it
does not introduce new predictor coefficients or increase the memory budget.

## Allocation evidence

A warmed one-million-unique-VIN diagnostic counts allocation operations:

| Per VIN | Before | After | Reduction |
|---|---:|---:|---:|
| Allocations | 38.1705 | 36.7769 | 3.65% |
| Reallocations | 8.3831 | 8.3011 | 0.98% |
| Requested allocation bytes | 31,744.99 | 31,722.82 | 0.07% |

These are allocation calls and requested bytes, not live memory or DRAM traffic.
[Raw allocation measurements](../scripts/bench/activation_allocations_2026_09_14.json)
retain the corpus and binary hashes.

## Performance method

Before/after binaries decode the same ten million unique VINs, with a complete
untimed warm pass and at least ten seconds of complete timed passes. Full ordered
native results and destruction are included. Stage tracing is disabled. Fixed
batch runs use 12,000 rows; automatic runs include calibration, feedback, and the
existing 512 MiB result-memory budget. The host is an Apple M2 Max with eight
performance and four efficiency cores. Source snapshots and immutable binaries
were separated before timing; no agent builds or tests ran during measurements.

The probe and native automatic benchmark now record whole-pass process CPU,
instructions, and cycles. macOS supplies process instruction/cycle counters;
unsupported systems report unavailable counters. Background processes do not add
their instructions to this process's count, although contention can still change
our scheduling, cache behavior, and elapsed time. These measurements therefore
help distinguish less computational work from more CPU availability.

The fixed-batch screen is followed by reversed-order confirmation pairs. The
automatic-mode samples use the same reversed-order pairing to check the complete
default path.
[Screen results](../scripts/bench/activation_screen_2026_09_14.json) and
[confirmation results](../scripts/bench/activation_confirmation_2026_09_14.json)
retain exact commands, binary hashes, CPU time, hardware counters, and native JSON.

To repeat the production comparison, save before/after `pipeline_probe` binaries
from `cargo build --release --locked -p ultravin --example pipeline_probe`, then run:

```sh
UV_FROZEN=1 uv run python -m scripts.bench.activation_optimizations \
  /absolute/path/to/before /absolute/path/to/after /tmp/comparison.json
```

For automatic mode, build/save `throughput` instead and add `--auto`. The runner
uses `target/bench/multicore-corpus.txt`, records its hash, and requires available
process instruction counters. Each condition runs twice at eight and twelve
workers. Keep builds, tests, and other profiling sessions out of the timed run.

Archived experiment binaries included the now-removed cleanup feature with its
zero/zero control, which executes the original cleanup iterator. The final source
also aligns the automatic benchmark's starting wall-clock snapshot with the
probe's ordering; the archived auto binaries include the two starting counter
queries in wall time. That measurement overhead is negligible over a full pass,
and identical in before/after binaries.

## Fixed-batch confirmation

Each value below is the mean of two fresh-process samples, reversing the
before/after order. Instructions decreased in all four individual comparisons.

| Workers | Instructions/VIN before → after | Change | CPU µs/VIN before → after |
|---:|---:|---:|---:|
| 8 | 76,946 → 76,452 | −0.64% | 11.466 → 11.108 |
| 12 | 77,875 → 77,453 | −0.54% | 12.761 → 12.775 |

Eight-worker throughput ranged from 549–630 thousand VIN/s before and 642–647
thousand after. Twelve-worker throughput ranged from 637–647 thousand before
and 611–690 thousand after. The twelve-worker wall-time comparisons moved in
opposite directions. The retained changes demonstrably remove allocation calls
and a small amount of instruction work; these samples do not establish a large
twelve-worker throughput improvement or lower twelve-worker CPU time.

## Automatic mode

Two fresh-process samples per condition, with reversed order:

| Workers | Before VIN/s range | After VIN/s range | Mean instructions/VIN before → after | Mean CPU µs/VIN before → after |
|---:|---:|---:|---:|---:|
| 8 | 619,824–650,996 | 576,325–657,484 | 77,248 → 77,008 | 11.208 → 11.384 |
| 12 | 651,189–658,684 | 476,092–678,966 | 78,294 → 78,179 | 12.815 → 12.841 |

The candidate reached 678,966 VIN/s using all twelve workers in its second
sample, but the first sample was much slower. Neither worker count establishes
a repeatable automatic-mode throughput gain. Mean CPU time did not improve.
The retuning fix is retained for its demonstrated correctness: its regression
test passes with the fix and fails with the exact previous reset behavior.
It should not be advertised as a measured speedup on this corpus. Automatic
prediction and the existing memory budget remain in use without new settings.

## Cleanup experiment

The same candidate binary tested ordinary Rayon splitting, a minimum leaf length
of 64 results, and a maximum leaf length of 256 results. At twelve workers,
ordinary/minimum/maximum settings consumed respectively 12.547/12.552/12.581 CPU
microseconds per VIN and 77,377/77,406/77,435 instructions per VIN. At eight
workers, the maximum setting had slightly lower CPU time in one sample but did
not reduce instructions. This does not establish a portable scheduling gain.
The experimental feature and environment settings were removed from production.

The remaining scaling constraint is still worker scheduling and shared resources
on this mixed-core machine. Fewer allocations remove real work; cleanup grain
alone does not remove late worker starts. Earlier bounded pipeline experiments
also failed to establish a repeatable gain under equal live-memory budgets.
The README benchmark graph is unchanged by this follow-up.
