# Managed parallel worker scaling — 2026-09-14

Native automatic batching improved median throughput by 32.9–36.5% at 4, 8, and 12 workers after the allocation and parallel-cleanup changes. The 8- and 12-worker ranges overlap, so this measurement does not establish a meaningful advantage for 12 workers over 8. One-worker throughput improved 9.9%, while twelve-worker speedup relative to one worker increased from 3.70× to 4.60×.

## Result

| Workers | Before median VIN/s (range) | Managed median VIN/s (range) | Median gain | Paired gain range |
|---:|---:|---:|---:|---:|
| 1 | 91,885 (91,322–92,449) | 100,985 (100,437–101,534) | +9.9% | +8.6% to +11.2% |
| 4 | 225,187 (181,740–268,634) | 307,312 (305,252–309,373) | +36.5% | +13.6% to +70.2% |
| 8 | 341,744 (334,975–348,512) | 454,049 (412,274–495,824) | +32.9% | +23.1% to +42.3% |
| 12 | 340,058 (338,005–342,112) | 464,220 (437,559–490,881) | +36.5% | +27.9% to +45.2% |

The paired gain range compares managed and before samples from the same round. It is wide at four workers because one before sample fell to 181,740 VIN/s while the other reached 268,634 VIN/s. At eight and twelve workers, the managed distributions also overlap each other: 412,274–495,824 VIN/s at eight workers and 437,559–490,881 VIN/s at twelve. Treat their ordering as unresolved on this host.

The fastest sample was 495,824 VIN/s. Five million unique VINs therefore cover 10.084 seconds at the fastest observed rate, passing the minimum ten-second unique-input gate. Each timed sample completed at least one full corpus pass rather than cycling a small hot input.

## What changed

The new path adds `BatchResults`, an owning result container used by `decode_batch_managed`, its explicit-clock variant, and the corresponding flat and `Db` APIs. Dropping a large container completes cleanup across decoder workers. The native automatic-batch benchmark uses this public path. Existing `Vec`-returning signatures are preserved and retain ordinary caller-thread destruction. Consuming iteration or `into_vec()` transfers cleanup responsibility; iteration over `&batch` retains managed cleanup.

The database now owns cached canonical correction text in sparse `OnceLock` storage. Decode workers reuse thread-local scratch space and borrow common correction CSV and message values from the database instead of rebuilding equivalent owned values for every result. This changes result ownership and repeated allocation; it does not change the predictor's calibrated domain. The model remains calibrated through 5,000 rows, while runtime batches may grow to 16,384 rows when the memory limit permits.

A timed profile motivated this ownership change. Of 434 samples in the main timed region, 317 were waiting in decode work and about 115 were in synchronous cleanup, roughly 26.5% of samples. Those are profiler samples, not elapsed-time percentages or throughput results; they identify cleanup and ownership as a material optimization target.

## Method

Both immutable binaries decoded the same validated corpus of 5,000,000 distinct VINs with full native output and automatic batch selection. Every fresh process loaded the corpus, performed a complete untimed warm pass, then ran timed full-corpus passes for at least ten seconds. The clock was fixed at `2026-09-01T00:00:00+00:00`.

The benchmark retained the native predictor record, calibration and selected-size histograms, exact elapsed time and throughput, raw output, and peak process RSS for every sample. Allocation, memory sampling, and synchronous result cleanup were inside the native timer. Corpus validation, process startup, corpus loading, and the complete warm pass were outside it.

Worker counts 12, 8, and 4 each ran twice. Binary order was reversed in the second round, and worker order was rotated. Other CPU jobs were active on this shared host. Reversal reduces monotonic ordering bias but cannot remove bursty external contention, so these results describe this measurement window rather than an isolated-machine ceiling.

The machine is an Apple M2 Max with eight performance and four efficiency cores.
Worker counts specify Rayon pool sizes; they do not pin threads to particular cores.

The compared binaries were:

- Before: `target/bench/scaling-before-throughput`, SHA-256 `f0fac22fd112ba3afc1119fee3e0450db179f151bbb8d4d632fc5055433ee6aa`
- Managed: `target/bench/worker-managed-after-throughput`, SHA-256 `3abcfc3f23ac47814542c16382ec5b4f4d66ffb0ff0183cbb72a34386ae97011`

Reproduction command:

```bash
uv run --frozen python -m scripts.bench.worker_scaling \
  --before-binary target/bench/scaling-before-throughput \
  --after-binary target/bench/worker-managed-after-throughput \
  --workers 12 --workers 8 --workers 4 \
  --rounds 2 --seconds 10 \
  --output scripts/bench/worker_scaling_2026_09_14.json
```

## One-worker baseline

The paired one-worker run used the same binaries, corpus, fixed clock, and two-round reversed binary order after the parallel-worker sweep. Its median improved from 91,885 to 100,985 VIN/s (+9.9%). These are one-worker automatic **batch** results, consistent with every parallel row, rather than the separate single-VIN API.

| Workers | Before speedup vs one worker | Managed speedup vs one worker |
|---:|---:|---:|
| 4 | 2.45× | 3.04× |
| 8 | 3.72× | 4.50× |
| 12 | 3.70× | 4.60× |

Reproduce that baseline by using `--workers 1` and the one-worker output path with the command above. All sixteen fresh-process samples are retained across the two reports.

## Validation

`UV_FROZEN=1 make checku` passed: all Rust formatting and Clippy checks,
217 Rust tests, and 920 Python tests. The throughput example's three memory
accounting tests also passed. Cargo and uv lockfile hashes remained unchanged.

Raw data:

- [Parallel paired samples](../scripts/bench/worker_scaling_2026_09_14.json)
- [One-worker paired samples](../scripts/bench/worker_scaling_single_2026_09_14.json)
