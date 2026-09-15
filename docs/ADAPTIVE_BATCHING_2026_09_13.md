# Adaptive batch sizing

Measured September 13, 2026 on an Apple M2 Max over one 200,000-row pass per
fresh process. Adaptive sizing gives Parquet a portable middle ground and keeps
JSONL close to the measured 1,000-row hand-tuned choice without assuming this
machine's worker count or row width.

For Parquet, the 64 MiB working-buffer target selected output batches up to
18,549 rows. With four workers it delivered 158,447 rows/s at 316 MiB peak RSS,
within 1.3% of fixed 8,192-row throughput and 6.4% below fixed 50,000, while using
32% less RSS than 50,000. With twelve workers it delivered 163,846 rows/s at
381 MiB: 4.1% faster than fixed 8,192 and 12.6% below fixed 50,000, with 31% less
RSS than 50,000.

| Parquet setting | 4 workers rows/s | 4 workers RSS | 12 workers rows/s | 12 workers RSS |
|---|---:|---:|---:|---:|
| **auto, 64 MiB target** | **158,447** | **315.9 MiB** | **163,846** | **381.1 MiB** |
| fixed 1,000 | 151,214 | 224.2 MiB | 126,497 | 239.4 MiB |
| fixed 8,192 | 160,604 | 254.9 MiB | 157,458 | 319.3 MiB |
| fixed 50,000 | 169,212 | 465.7 MiB | 187,419 | 548.6 MiB |

JSONL uses a format-specific 8 MiB target. Its adaptive training cost 1.1% at
four workers and 3.0% at twelve workers against fixed 1,000-row chunks; peak RSS
was about 8% higher. This is the measured portability cost of the default, not a
claim that adaptation beats a batch size chosen after benchmarking this exact
machine and corpus.

| JSONL setting | 4 workers rows/s | 4 workers RSS | 12 workers rows/s | 12 workers RSS |
|---|---:|---:|---:|---:|
| **auto, 8 MiB target** | **141,986** | **297.2 MiB** | **157,706** | **339.4 MiB** |
| fixed 1,000 | 143,557 | 275.3 MiB | 162,610 | 313.6 MiB |

A 64 MiB JSONL target was rejected as the default. At twelve workers it gained
4.6% over fixed 1,000 but reached 1,185 MiB instead of 315 MiB; at four workers
it was slower and used 31% more RSS. The smaller target kept requested chunks
near 900 rows on this corpus and avoided that tradeoff.

The budget estimates buffers used for the current batch. It is not a cap on
whole-process RSS, and it excludes buffers retained by an upstream Arrow
producer. `RAYON_NUM_THREADS` still controls parallelism; sizing never replaces
or changes the process-global worker pool. Pass an integer batch size when a
deployment has already measured and pinned its preferred tradeoff.

## Method

Every sample decodes one 200,000-row input, built by cycling through all 5,000
committed VINs. Parquet writes every public element to a real file. JSONL uses
the real CLI parser, chunker, flat serializer, and operating-system null sink;
terminal and network backpressure are outside the boundary. Every job uses one
fixed clock.

The runner records exact child-process peak RSS with `wait4`. After each Parquet
child exits, the parent reads its row-group sizes, so PyArrow does not affect the
timed child or its RSS. Adaptive JSONL samples record each requested tuner size;
that small tracing cost is included in their elapsed time. Results are medians
of three rotated fresh-process rounds. Raw reports retain ranges, every sample,
chosen sizes, input and executable hashes, toolchains, build mode, and work
flags.

```sh
uv run --frozen python -m scripts.bench.adaptive \
  --rows 200000 --rounds 3 --workers 4,12 --batch-memory-mb 64
```

- [Parquet and 64 MiB JSONL comparison](../scripts/bench/adaptive_2026_09_13.json)
- [Final 8 MiB JSONL comparison](../scripts/bench/adaptive_jsonl_8mb_2026_09_13.json)

