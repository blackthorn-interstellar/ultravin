# Native coordination experiments — 2026-09-14

This follow-up tests the two remaining coordination candidates: writing decoded
results into input-order slots and overlapping one batch's cleanup with the next
batch's decode. The benchmark SVG also now paints its own theme-matched background,
so its dark-mode text remains readable in viewers with a white canvas.

## Method

Every timed sample completes one pass over the same 10,000,000 distinct VINs
used in the multicore optimization report, after a complete untimed warm pass.
The corpus SHA-256 is
`de80fb8727de3d89a7885929645953eed7b51a67c1dc988d309bbaa0ab4ec5f3`;
the decode clock is fixed at `2026-09-01T00:00:00Z`. Both experiments use full
ordered native results and include destruction in elapsed time. Eight and twelve
workers each run two fresh-process trials per condition, reversing order in the
second round. The host is the same shared Apple M2 Max (8P + 4E); workers are not
pinned. Compare conditions within each experiment rather than comparing these
rates directly with earlier sessions.

## Direct placement

The candidate allocates input-order `Option<T>` slots, locality-sorts disjoint
mutable references to those slots, and lets workers write each output to its
assigned slot. It then extracts the values into the required `Vec<T>`. This
removes the serial permutation swaps while retaining WMI/descriptor locality,
caller-year indexing, and input order. It uses no new unsafe code. On panic,
initialized slots own and drop their values normally. Extraction uses shallow
moves; allocation reuse is an implementation detail, not an API guarantee.

| Workers | Before median VIN/s (range) | Placement median VIN/s (range) | Change |
|---:|---:|---:|---:|
| 8 | 619,743 (618,862–620,624) | 634,814 (628,333–641,294) | +2.4% |
| 12 | 617,405 (614,921–619,889) | 623,404 (612,122–634,687) | +1.0% |

These fixed 12,000-row runs show a repeated gain at eight workers. Twelve-worker
pairs moved in opposite directions. Median peak process RSS changed from 1.216
to 1.233 GB at eight workers and from 1.354 to 1.336 GB at twelve.

[Raw placement measurements](../scripts/bench/direct_placement_2026_09_14.json)
retain exact commands, binary hashes, full timing records, and the corpus manifest.
The [candidate patch](../scripts/bench/direct_placement_candidate_2026_09_14.patch)
records the isolated implementation and its ordering/panic-cleanup test.

The candidate is retained in the normal native full/flat batch path. A separate
paired automatic-mode comparison includes predictor calibration, live tuning,
full result materialization, order preservation, and synchronous cleanup:

| Workers | Before median VIN/s (range) | Placement median VIN/s (range) | Change |
|---:|---:|---:|---:|
| 8 | 606,722 (599,179–614,266) | 627,043 (626,259–627,827) | +3.3% |
| 12 | 584,192 (573,937–594,446) | 587,914 (571,520–604,308) | +0.6% |

Both eight-worker pairs improved. Twelve-worker pairs moved in opposite
directions, so its +0.6% median difference is effectively neutral. This is a
separate paired session from the earlier multicore report; its absolute rates
reflect that session's shared-host conditions. The earlier README graph remains
a dated measurement rather than mixing in new four-/one-worker numbers that
were not measured in this follow-up.

[Raw automatic-mode comparison](../scripts/bench/direct_placement_auto_2026_09_14.json)
retains all eight samples and their provenance. All timed samples in this
follow-up completed ten million unique input rows and lasted at least ten seconds.

## Bounded pipeline

The diagnostic producer decodes complete managed batches and hands them to an
ordered consumer over a zero-capacity rendezvous channel. The consumer observes
and destroys each result before accepting the next. At most two batches can be
live or under construction, including partial tails. Decode and cleanup both
submit work to the same process-owned Rayon pool; the two coordinating threads
do not create additional decoder workers. Channel ownership makes either side's
panic/disconnect unblock its partner before scoped threads are joined.

For a 24,000-row live budget, pipeline batches contain 12,000 rows. The
`sequential_budget` control decodes 24,000 rows at a time for the same maximum
live-row budget. The `sequential_batch` control decodes 12,000 rows at a time to
isolate overlap at the same batch size. Live-row limits do not imply identical
RSS: temporary decode state, allocator retention, input, and database memory are
also present. The report records measured peak RSS and observed live rows.

| Workers | Sequential, B=24,000 | Sequential, B=12,000 | Pipeline, B=12,000 | Pipeline vs equal live-row budget | Pipeline vs same B |
|---:|---:|---:|---:|---:|---:|
| 8 | 616,697 | 584,998 | 632,518 | +2.6% | +8.1% |
| 12 | 635,341 | 592,223 | 597,298 | −6.0% | +0.9% |

Values are median VIN/s. At eight workers, both same-B pairs improved, but the
equal-live-budget pairs moved in opposite directions. At twelve workers, both
equal-budget pairs regressed; the same-B pairs moved in opposite directions.
Pipeline ranges were 621,151–643,885 VIN/s at eight workers and
568,954–625,643 VIN/s at twelve. The corresponding equal-budget sequential
ranges were 599,942–633,453 and 628,127–642,556 VIN/s.

Peak RSS medians for sequential-budget / sequential-batch / pipeline were
1.498 / 1.204 / 1.419 GB at eight workers and 1.731 / 1.374 / 1.391 GB at twelve.
All pipeline samples observed at most 24,000 live or under-construction rows.
The pipeline can hide some cleanup at eight workers, but it does not establish
a repeatable equal-budget improvement across these configurations. It remains
a diagnostic example; it is not enabled by default or exposed as a new library
API. Larger sequential batches are the better measured choice at twelve workers.

[Raw pipeline measurements](../scripts/bench/bounded_pipeline_2026_09_14.json)
retain every sample, range, exact command, phase timing, and binary hash. Decode
and cleanup phase wall times overlap in pipeline mode and include waiting for
work submitted to the shared pool; summing them does not give CPU time.

## Reproduction

The corpus generation command is in the
[multicore report](MULTICORE_OPTIMIZATION_2026_09_14.md#reproduction). To run the
pipeline comparison on the current checkout:

```bash
cargo build -p ultravin --release --example pipeline_probe
RAYON_NUM_THREADS=8 target/release/examples/pipeline_probe \
  target/bench/multicore-corpus.txt 10 8 24000 sequential_budget
RAYON_NUM_THREADS=8 target/release/examples/pipeline_probe \
  target/bench/multicore-corpus.txt 10 8 24000 sequential_batch
RAYON_NUM_THREADS=8 target/release/examples/pipeline_probe \
  target/bench/multicore-corpus.txt 10 8 24000 pipeline
```

Repeat in reverse order and use twelve workers for the other configuration.
These commands build the current engine; the historical comparison used the
immutable baseline binary recorded in its JSON, before direct placement.
