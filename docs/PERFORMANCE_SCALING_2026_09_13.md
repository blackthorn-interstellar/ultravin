# Batch scaling and memory

Measured September 13, 2026 on an Apple M2 Max with 12 logical cores. The useful
default is a **1,000-row batch**: at all 12 workers it sustained 288,357 native
results/s, 181,515 flat dictionaries/s, and 225,198 JSONL rows/s while keeping
median peak RSS at 244, 229, and 326 MiB respectively. A 50,000-row batch used
1,196, 678, and 3,892 MiB in those paths without a proportionate throughput
gain.

For direct JSON and Parquet, 10,000 rows is a useful high-throughput compromise.
At 12 workers direct JSON delivered 396,562 rows/s at 354 MiB, while the
50,000-row setting delivered 522,824 rows/s at 800 MiB. Parquet delivered
223,847 rows/s at 341 MiB versus 322,877 rows/s at 595 MiB. The Parquet samples
were the noisiest measurements in the shared-host session, so use these as
capacity guidance rather than a precise prediction.

| completed output | practical setting | median rows/s | median peak RSS | 50,000-row setting | median rows/s | median peak RSS |
|---|---:|---:|---:|---:|---:|---:|
| native Rust results | 1,000 / 12 workers | 288,357 | 243.8 MiB | 50,000 / 12 workers | 265,435 | 1,195.8 MiB |
| flat Python dictionaries | 1,000 / 12 workers | 181,515 | 228.7 MiB | 50,000 / 12 workers | 164,505 | 678.4 MiB |
| direct flat JSON | 10,000 / 12 workers | 396,562 | 354.1 MiB | 50,000 / 12 workers | 522,824 | 799.6 MiB |
| typed Parquet | 10,000 / 12 workers | 223,847 | 340.7 MiB | 50,000 / 12 workers | 322,877 | 595.3 MiB |
| CLI JSONL to `/dev/null` | 1,000 / 12 workers | 225,198 | 326.1 MiB | 50,000 / 12 workers | 260,997 | 3,891.8 MiB |

Use 1,000 rows and four workers to reduce memory further. Its repeated medians
were 227,404 native results/s at
181 MiB, 160,395 dictionaries/s at 184 MiB, and 177,572 JSONL rows/s at 281 MiB.
The screen also shows why worker count should follow the batch size: 12 workers
made 100-row batches slower than four workers in every in-memory path, while
adding roughly 41–48 MiB of peak RSS.

## Method

The screening run measured all five paths at batch sizes 100, 1,000, 10,000,
and 50,000 with 1, 2, 4, and all 12 workers. It used one two-second sample per
configuration. Thirteen selected settings were then measured in three rotated,
fresh-process rounds with a five-second minimum window. Every loop finishes a
complete 50,000-row pass, so a sample can exceed its requested duration.

Every child first performs one complete untimed warm pass; its allocations are
included in exact child-process peak RSS reported by `wait4`. Every measured pass
cycles through all 5,000 distinct committed VINs in the same order, repeated to
50,000 rows, and uses one fixed decode clock for the entire job. The Rust row
returns complete native results. Python dictionaries, direct JSON, and JSONL use
the flat result shape. Parquet writes all public element columns.

JSONL measures the actual CLI parser, chunker, Rust serializer, and writes to the
operating system's `/dev/null` sink. It excludes terminal and network
backpressure. Its sustained 50,000-row samples reached substantially higher RSS
than the shorter screen, strengthening the case for the CLI's 1,000-row default.

This was a serial run on a shared host with intermittent media, screen-sharing,
and indexing activity. The repeated ranges expose that contention: for example,
10,000-row Parquet ranged from 141,871 to 255,966 rows/s and 50,000-row direct
JSON ranged from 313,402 to 534,030 rows/s. Memory comparisons were much more
stable except for the sustained JSONL peak.

## Reproduce

```sh
uv run --frozen python -m scripts.bench.scaling \
  --batch-sizes 100,1000,10000,50000 --workers 1,2,4,12 \
  --rounds 1 --seconds 2 --output target/bench/scaling-screen.json
```

The runner builds the Rust example and Python extension in release mode with
locked, frozen dependencies. Raw files retain every sample plus the Git
revision, dirty state, platform, toolchains, CPU, fixed clock, work flags, and
SHA-256 hashes for the source corpus, expanded input, Parquet input, embedded
artifact, extension, Rust executable, benchmark workers, and lockfiles.

- [Full screening matrix](../scripts/bench/scaling_2026_09_13_screen.json)
- [Repeated 1,000-row/all-worker settings](../scripts/bench/scaling_2026_09_13_practical.json)
- [Repeated 10,000-row/all-worker settings](../scripts/bench/scaling_2026_09_13_balanced.json)
- [Repeated 50,000-row/all-worker settings](../scripts/bench/scaling_2026_09_13_fast.json)
- [Repeated 1,000-row/four-worker settings](../scripts/bench/scaling_2026_09_13_practical_1000x4.json)
