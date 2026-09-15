# Multicore native optimization — 2026-09-14

This investigation isolated four additive changes to the managed native result path. In fixed 12-worker, 12,000-row managed batches, moving valid-character charset intervals from duplicate per-worker caches into the database improved median throughput by 21.5% and reduced peak process RSS by about 60.5%. Increasing the decode item capacity to 96 added about 5.0%. Replacing temporary error-code vectors with stack character arrays and a long-VIN fallback added about 1.2% in a noisy two-pair screen while measurably reducing allocations. A short-string control-byte detector did not reproduce an improvement and was rejected.

These controlled fixed-batch ablations do not by themselves establish the
overall gain against the older automatic-batch implementation. The paired
automatic comparison and managed native model are reported below.

The subsequent [coordination experiments](COORDINATION_EXPERIMENTS_2026_09_14.md)
retain direct output placement (+3.3% in eight-worker automatic mode) and test
bounded decode/cleanup pipelining.

## Fixed-batch screens

| Change | Before median VIN/s | After median VIN/s | Change | Before peak RSS | After peak RSS |
|---|---:|---:|---:|---:|---:|
| Share valid-character charset intervals | 492,566 | 598,473 | +21.5% | 3.46 GB | 1.37 GB |
| Decode item capacity 64 → 96 | 579,534 | 608,404 | +5.0% | 1.41 GB | 1.36 GB |
| Stack error-code character buffers | 613,797 | 621,255 | +1.2% | 1.37 GB | 1.36 GB |
| Borrow preformatted model-year text | 689,600 | 699,519 | +1.4% | — | — |
| Short-string control-byte detector | 631,189 | 624,916 | −1.0% | — | — |

Each row compares only the two binaries in its own artifact. The screens were short and ran on a shared host, so the sequence is stronger evidence than multiplying the percentages into a synthetic total. In particular, the buffer result is small relative to observed run-to-run movement and should be treated as allocation evidence with a modest throughput signal.

The cache comparison used immutable binaries `c610c276e555acf0c1e948c64dff048f50ac77e6384a03611a047fe97ece3a59` and `42cf587ace2350d48a4834094594444b83fc93279e7e0c10ee9c629fef92b48e`. The capacity comparison used cache binary `42cf…b48e` and capacity-96 binary `b8ce53ace84f814c4c61a1349c40dd3b7cc56c6648762438d0fd2cc513ddecf8`. The buffer comparison used capacity-96 binary `b8ce…decf8` and buffer-reuse binary `1ffdaa1fb7fffd98208ddd723dda791ad90302d0320ce135aafe8f12dc85d894`.

## What the changes address

The managed API returns a full, ordered native batch. Every output is materialized, black-boxed, and synchronously destroyed inside the timed workload. This preserves the cost that a caller of the managed return API actually incurs.

The largest result came from valid-character lookup ownership. The previous implementation kept a thread-local `CHARSET_CACHE`, keyed by WMI and year, so every worker independently built and retained equivalent WMI/year character sets. The new implementation stores valid-character charset intervals once on the database and shares those immutable lookups across workers. Removing the per-worker duplication is consistent with the 21.5% throughput increase and the large RSS reduction in the fixed 12,000-row comparison.

The decode core previously reserved 64 item slots even though common results grew beyond that capacity. An allocation probe over 3,000 VINs found 2,448 reallocations from the 8,192-byte size class. Reserving 96 entries removed that class: reallocations fell from 34,242 to 31,794 and allocated bytes fell from 122.1 MB to 95.5 MB. The paired fixed-batch screen then measured a 5.0% median throughput improvement.

The error formatter now uses stack character arrays for ordinary VINs and falls back to a `Vec` for unusually long input. This reduced allocations from 123,459 to 116,937 and reallocations from 31,794 to 24,471 for the same 3,000-row diagnostic. Allocated bytes moved from 95.46 MB to 94.96 MB. The corresponding throughput screen improved 1.2%, but the two sample ranges overlap enough that the allocation counts are the clearer evidence.

The rejected experiment replaced the general short-string scan in `scrub_value` with a word-at-a-time detector for ASCII control bytes. It measured 624,916 VIN/s against 631,189 VIN/s for its paired current build and was removed rather than carried forward on an unsupported assumption.

The final allocation change borrows compile-time model-year strings for 1900–2199 and retains owned formatting for every other `i32` year. Both output fields preserve their exact text. This removes 6,598 allocation calls per 3,000 VINs (116,937 → 110,339). In reversed paired 12-worker, 12,000-row runs, median throughput rose from 689,600 to 699,519 VIN/s (+1.4%), with both comparisons positive.

## Rejected scheduler changes

The scheduler screen independently tested `with_max_len(32)` for parallel decoding and `with_min_len(16)` for parallel result cleanup. Six fresh processes ran in reversed order: current, decode candidate, cleanup candidate, cleanup candidate, decode candidate, current. Each processed both 1,500- and 12,000-row batches for at least ten seconds of unique input.

| Variant | 1,500 rows: median VIN/s | 12,000 rows: median VIN/s |
|---|---:|---:|
| Current Rayon scheduling | 455,472 | 610,973 |
| Decode tasks capped at 32 rows | 461,134 | 613,388 |
| Cleanup tasks at least 16 rows | 455,289 | 608,371 |

The decode candidate's large-batch gain was only 0.4%, with opposite results in the two comparisons. The cleanup candidate also failed to improve throughput. Both were removed. The short-string and scheduling experiments leave the simpler existing code in place.

## Profiling evidence

The latest timed sampling profile, `multicore-current-profile.txt`, captured 132 main-thread samples: 108 waiting for parallel decode completion and 24 waiting for managed result cleanup. It showed the main thread plus 12 active Rayon workers, with no second unused pool. Worker samples included decode, result projection, destruction, allocation, and scheduler work; Rayon helper frames can contain inlined projection work and must not all be counted as scheduler overhead. The instrumented cell reported 607,837 VIN/s and 7.793 average busy cores.

The scheduler screens above tested that possible source of overhead without finding a reproducible improvement. The stack sample counts remain qualitative. They identify where the main thread waited and where worker stacks were sampled; they are not elapsed-time percentages and do not quantify a change's benefit. The reported process CPU delta is the stronger utilization measurement for that cell.

## Method and limits

The fixed screens used a corpus of 10,000,000 validated, distinct VINs, SHA-256 `de80fb8727de3d89a7885929645953eed7b51a67c1dc988d309bbaa0ab4ec5f3`. The machine was an Apple M2 Max with eight performance and four efficiency cores. The decoder clock was fixed at `2026-09-01T00:00:00Z`.

Each fresh process loaded the corpus and completed a full untimed unique-corpus warm pass. The cache, capacity, and buffer screens used 12 workers and a 12,000-row live budget, then timed one complete 10-million-row pass. They included batch sorting, full output construction, input-order restoration, and synchronous cleanup. These `multicore_probe` records did not sample returned-output bytes. Process startup, corpus loading, and warmup were outside the native timer.

The later control-byte and profiling records used `native_grid`'s unique-prefix policy. Each cell decoded complete 12,000-row batches without repeating a VIN until at least ten seconds elapsed; it did not need to exhaust the 10-million-row corpus. Those records include the 16-row returned-output estimator and process CPU deltas. Other CPU work was active on the shared host; paired ordering reduces drift but does not reproduce an isolated machine.

The allocation probes are separate 3,000-row diagnostics. Their counts explain particular allocation mechanisms but are not RSS measurements for the 10-million-row process. The sampling profile is a separate instrumented run.

## Final automatic-batch comparison

### Automatic batching and worker scaling

The final engine was compared with the preceding managed-worker binary in two
reversed-order, fresh-process pairs at 1, 4, 8, and 12 workers. The one-worker
comparison ran immediately after the multicore comparison using the same binaries and corpus. Each automatic-mode
sample decoded the complete 10-million-row unique corpus after an untimed warm
pass. The timed interval includes prediction, live tuning, full result
materialization, order restoration, and synchronous cleanup.

| Workers | Before median VIN/s (range) | Final median VIN/s (range) | Gain | Before / final median peak RSS | Initial batch, before / final |
|---:|---:|---:|---:|---:|---:|
| 1 | 88,510 (72,106–104,914) | 105,372 (100,162–110,582) | +19.1% | 1.29 / 1.38 GiB | 256 / 16,384 |
| 4 | 325,621 (321,194–330,047) | 400,843 (397,409–404,278) | +23.1% | 1.81 / 1.43 GiB | 822–856 / 15,784 |
| 8 | 483,418 (469,431–497,405) | 656,900 (652,298–661,502) | +35.9% | 2.45 / 1.48 GiB | 1,152–1,310 / 11,719 |
| 12 | 507,556 (506,625–508,486) | 630,092 (619,375–640,810) | +24.1% | 3.19 / 1.69 GiB | 1,368–1,506 / 11,018 |

The final binary's eight-worker median was 63.9% above its four-worker median.
Its twelve-worker median was 4.1% below eight workers, and the two ranges do not
overlap. On this Apple M2 Max, increasing the worker count from eight to twelve did not improve
this full-result workload; workers were not pinned to particular core types. That result can reflect heterogeneous cores,
shared-host scheduling, allocation and cleanup pressure, or memory-system
contention; this experiment does not prove that memory bandwidth was saturated.

Sampling and controlled scheduler screens did not identify another supported
code change: the tested Rayon split/grain changes did not reproduce a useful gain.
The sampled run is diagnostic and is excluded from throughput comparisons. Full native results
remain allocation-heavy because the public managed API returns every decoded
result at once. The final one-worker median was 105,372 VIN/s, giving speedups of 3.80×,
6.23×, and 5.98× at four, eight, and twelve workers. One-worker samples varied
substantially: the paired gains were 5.4% and 38.9%, and the baseline ranged from
72,106 to 104,914 VIN/s. The +19.1% ratio of medians is recorded but does not
establish a stable single-core gain. All samples are retained.

The highest observed fixed-batch full-result run was 702,495 VIN/s with twelve
workers and 12,000-row batches over the complete ten-million-VIN corpus. That is
a diagnostic fixed-size observation, separate from the automatic medians above.

### Managed native model and selection policy

`ultravin-native-batch-v2` was fitted to 32 managed fixed-batch cells and 64
runs of at least ten seconds of unique input. The grid covered 1, 4, 8, and 12
workers and batch sizes from 256 through 16,384; batch size 9,000 was held out.
The selected eight-term nonnegative model has five active coefficients and adds
two square-root scheduling/locality terms. Its held-out mean absolute percentage
error is 2.99%, and leave-one-batch-out error is 4.84%. The reference speed is
142,713.934 VIN/s, pooled from 20 calibration offsets on the same host.

Returned-output estimates ranged from 15,799.875 to 16,513.6875 bytes per row.
The predictor uses 16,640 bytes per row, the conservative upper observation
rounded to a 256-byte boundary. The width artifact records coverage of 321,240 distinct rows and
2,590,176 sampled draws.

A separate check measured the previous 99% choice, a 99.9% screening choice,
the modeled-peak choice, and the largest tested batch. These are fixed chosen
batch sizes through the managed return API, not automatic-mode measurements.
Every cell used the final model-year borrowing engine and v2 decoder, completed
whole batches over a unique corpus prefix for at least ten seconds, materialized
all results, and included synchronous cleanup.

| Workers | Previous 99% | 99.9% screen | Modeled peak | Largest tested |
|---:|---:|---:|---:|---:|
| 4 (`n=1`) | B=7,815: 390,364 | B=12,734: 401,071 | B=15,784: 405,723 | B=16,384: 410,818 |
| 8 (`n=2`) | B=7,664: 628,123 (622,028–634,219) | B=10,281: 645,335 (644,298–646,372) | B=11,719: 654,316 (643,942–664,690) | B=16,384: 658,691 (653,292–664,089) |
| 12 (`n=2`) | B=7,921: 633,290 (622,003–644,576) | B=9,948: 650,254 (644,959–655,548) | B=11,018: 661,872 (655,796–667,949) | B=16,384: 660,348 (659,385–661,310) |

Values are median VIN/s, with the observed range in parentheses for paired
rows. The modeled-peak choice was 4.17% faster than the previous 99% choice at
eight workers and 4.51% faster at twelve. The largest tested batch was 0.67%
faster than the modeled peak at eight workers, while the modeled peak was 0.23%
faster at twelve; those overlapping two-run ranges do not support a finer
distinction. The single four-worker run is a screen and has no replication.

The fit records the immutable binary, corpus, grid-wrapper, and input-JSONL
hashes. Its historical round ordering left the largest batch last in
both rounds; those measurements remain recorded as run. The later selection
checks used exact reversed pairs. Automatic-controller performance is reported
separately above and should not be inferred from these fixed chosen-size results.

## Reproduction

Build the current engine and generate the same deterministic unique corpus:

```bash
cargo build -p ultravin --release --example throughput
UV_FROZEN=1 uv run python -m scripts.bench.large_corpus \
  --count 10000000 --out target/bench/multicore-corpus.txt \
  --manifest target/bench/multicore-corpus.manifest.json
RAYON_NUM_THREADS=8 ULTRAVIN_NOW_MICROS=1788220800000000 \
  target/release/examples/throughput target/bench/multicore-corpus.txt 10 batch full auto
```

The ten-second argument is a minimum: the harness finishes the full unique pass.
Set `RAYON_NUM_THREADS` to 1, 4, or 12 for the other worker counts. The
`worker_scaling` runner records reversed before/after pairs, input and binary
hashes, process RSS, and the duration gate:

```bash
UV_FROZEN=1 uv run python -m scripts.bench.worker_scaling \
  --before-binary target/bench/worker-managed-after-throughput \
  --after-binary target/bench/multicore-release-throughput \
  --corpus target/bench/multicore-corpus.txt \
  --manifest target/bench/multicore-corpus.manifest.json \
  --output target/bench/reproduced-auto-scaling.json \
  --workers 12 --workers 8 --workers 4 --rounds 2 --seconds 10
```

The paired command requires the preserved binaries from this workspace; their
hashes are embedded in the report. A fresh checkout can build and measure its
current engine using the first command sequence.

## Artifacts

The [raw ablation and allocation records](../scripts/bench/multicore_optimizations_2026_09_14.json) include the corpus manifest, individual measurements, and immutable binary hashes. The [managed predictor grid](../scripts/bench/native_managed_grid_2026_09_14.json) records every timed cell and production calibration sample.

The [automatic scaling comparison](../scripts/bench/multicore_auto_scaling_2026_09_14.json)
records the individual 4-, 8-, and 12-worker samples, corpus manifest, and
immutable before/final binary hashes. The [one-worker comparison](../scripts/bench/multicore_auto_single_2026_09_14.json)
retains its four samples and the same provenance.

The [native v2 fit](../scripts/bench/native_managed_fit_2026_09_14.json) records
the selected formula, validation errors, calibration reference, and output-width
evidence. Fixed selection checks are retained for [4 workers](../scripts/bench/native_selection_4w_2026_09_14.json),
[8 workers](../scripts/bench/native_selection_8w_2026_09_14.json), and
[12 workers](../scripts/bench/native_selection_12w_2026_09_14.json). Their
records preserve each measurement and its immutable binary hash. The
[width artifact](../scripts/bench/native_managed_width_2026_09_14.json) records
sample coverage and the fallback-width calculation.

The sampling profile remains available in this workspace at `target/bench/multicore-current-profile.txt`, with its raw timing record in the adjacent `.stdout` file. Profile-derived quantities above are diagnostic and are not used as uninstrumented benchmark results.
