# End-to-end performance

Measured 2026-09-13T17:23:29+00:00 on macOS-26.6.2-arm64-arm-64bit-Mach-O with ultravin 0.0.0 and vPIC data `2026_08`.

Ultravin sustained **225,982 VIN/s** returning complete native Rust results on 4 cores. Through its public data boundaries it delivered **147,787 VIN/s** as Python dictionaries, **345,055 VIN/s** as direct JSON, and **257,482 VIN/s** into a typed Parquet file.

Each operation repeats the committed 5,000 distinct VINs to a 50,000-row batch. The Rust row measures the parallel decoder returning native Rust results. The other rows use the installed Python extension and stop after constructing flat Python dictionaries, one direct flat JSON string, or a Parquet file with every public element projected to a typed column. These are useful output boundaries, so their rates should be read as endpoint costs rather than interchangeable microbenchmarks.

| completed output | median rows/s | observed range | median peak RSS |
|---|---:|---:|---:|
| Rust results | **225,982** | 223,537-226,316 | 987.4 MiB |
| Python dictionaries | **147,787** | 137,612-149,014 | 607.4 MiB |
| direct JSON | **345,055** | 344,969-347,855 | 743.9 MiB |
| Parquet file | **257,482** | 257,332-257,805 | 497.8 MiB |

Startup is measured from spawning a fresh process through its first completed output. Python rows include interpreter and extension import; Parquet opens and writes a one-row file.

| first completed output | median process wall time | median peak RSS |
|---|---:|---:|
| Rust results | 5.8 ms | 14.1 MiB |
| Python dictionaries | 24.7 ms | 33.7 MiB |
| direct JSON | 24.6 ms | 33.7 MiB |
| Parquet file | 26.1 ms | 38.3 MiB |

## Reproduce

```sh
uv run --frozen python -m scripts.bench.end_to_end --rows 50000 --rounds 3 --seconds 10 --threads 4 --now 2026-09-01T00:00:00+00:00
```

The run used `50,000` rows per throughput operation, 3 fresh-process rounds, 10 seconds per throughput sample, and `RAYON_NUM_THREADS=4`. Every decode used the fixed clock `2026-09-01T00:00:00+00:00`. Every throughput worker performs one untimed warm operation before the measured operation. Peak RSS is the operating system's maximum resident set for the complete worker and includes runtime, embedded data, inputs, outputs, and libraries. Parquet includes local filesystem I/O; filesystem caches were not flushed. The report retains every sample and reports medians and ranges.

## Provenance

- Git revision: `d8d233dd7a1aaa87a1c3b594a7b11353baf86a26` (dirty worktree)
- Python: `3.13.12 (main, Feb 12 2026, 01:06:02) [Clang 21.1.4 ]`
- Rust: `rustc 1.98.0 (88d9e12ae 2026-08-18)`
- CPU: `Apple M2 Max` (12 logical cores)
- Build: `release (maturin develop --release; Cargo workspace release profile)`
- Corpus SHA-256: `b45ad4472c202ee176f86e8bc3c39609c76b6258df963ea04e08439aa3a1eb09`
- Artifact SHA-256: `2d555d4db3fde3867f41e415ec6e848285a286b2fe5fb45f0bdcb88245bd7787`
- Embedded artifact BLAKE3: `907eedd5f2794a10e586e01ca6c2a5af4e8d7e0f966bcfdf030a9889d77ac538`
- Source dump SHA-256: `dd95bc6d6aa6e4f5103f38c792b8eccba2f4b06dad865a696327ac4dd37213c8`
- Extension SHA-256: `3a6cf3e5314802ad6f4ab6b569af2f6770baca8008eb5c94cc228099fc8d222a`
- Rust benchmark SHA-256: `e0d4bd675b5d827b393a5f1f623751cb6981cee24bb6f77e14446e50992cb608`
- Cargo.lock SHA-256: `06333688674e70f089b043d466bf8e0abda8dca1f45161fefbf368cfec9c384d`
- uv.lock SHA-256: `5ca117e340690822658c6b42e13ade269b9d02f75f166eacf7af53d23b8f4d21`

[Raw measurements](../scripts/bench/performance_2026_09_13.json) retain every sample and the complete machine-readable provenance.

## Batch-memory optimization

A separate paired release-wheel run compared the saved pre-change worktree with
the optimized batch paths. The full Python result is the important case: bounded
native-result marshalling reduced median peak RSS by **26.6%**, from 2,316.2 MiB
to 1,700.7 MiB, while median throughput remained effectively unchanged.

| completed output | before RSS | after RSS | change | before rows/s | after rows/s |
|---|---:|---:|---:|---:|---:|
| full Python dictionaries | 2,316.2 MiB | 1,700.7 MiB | **-26.6%** | 47,378 | 47,479 |
| flat Python dictionaries | 651.2 MiB | 627.4 MiB | **-3.7%** | 144,345 | 160,352 |
| full direct JSON | 4,072.9 MiB | 3,999.9 MiB | -1.8% | 187,075 | 186,432 |
| flat direct JSON | 834.3 MiB | 838.4 MiB | +0.5% | 487,232 | 474,467 |

The run used 50,000 rows, the complete committed corpus repeated in order, a
fixed `2026-09-13T00:00:00Z` clock, 8 Rayon workers, three alternating
before/after fresh-process trials, and an untimed warm call followed by at least
three measured seconds. Other workloads were active on the shared host, so the
paired order was reversed every other trial and medians are reported. JSON
movement is within run-to-run noise; an attempted 1,024-row JSON chunker was
rejected because it increased both RSS and elapsed time.

All four output shapes matched the baseline byte-for-byte over the 5,000-row
corpus at the fixed clock. Baseline source aggregate SHA-256 was
`78cb763446d33af455d37c44f24b4295ca9359a0204fab1d8bc9404dccf7ac9a`;
baseline and candidate wheel SHA-256 values were
`ff6aba1abef46d18961a0ede5c051e5ea0e39863ad498cc130a91feb3ebb4030` and
`802e9a796eced7045cb22147c2c8b65ea6c3fd0049d99a8985336f0bb6bff060`.

Reproduce from two isolated release-wheel environments with:

```sh
uv run --frozen python -m scripts.bench.batch_memory \
  --before-python /path/to/before/bin/python \
  --candidate-python /path/to/candidate/bin/python \
  --rows 50000 --seconds 3 --rounds 3 --threads 8
```

[Raw paired measurements](../scripts/bench/batch_memory_2026_09_13.json) retain
every trial, parity digest, and baseline/candidate artifact identity.
