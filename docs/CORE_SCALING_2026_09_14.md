# Native automatic-batch core scaling — September 14, 2026

One immutable optimized release build, automatic full-native batching at every
worker count, five million unique VINs, three fresh-process trials per setting.
The worker-count order rotates across rounds. Each child warms and then times
complete corpus passes, including calibration and result cleanup. No load was
added by the runner; other host activity was not isolated.

| Rayon workers | Median VIN/s | Range VIN/s | Speedup versus one worker |
|---|---:|---:|---:|
| 1 | 122,010 | 102,869–122,674 | 1.00× |
| 2 | 221,767 | 193,839–222,756 | 1.82× |
| 4 | 355,589 | 354,331–359,810 | 2.91× |
| 8 | 460,056 | 436,451–475,211 | 3.77× |
| 12 | 461,832 | 388,914–463,695 | 3.79× |

Throughput scales strongly through four workers, gains another 29.4% from four
to eight, then plateaus: twelve workers add only 0.4% over eight in the medians.
The eight- and twelve-worker ranges overlap substantially. Eight workers are
the useful operating point for this workload on this host; twelve did not
produce a meaningful throughput gain in this sweep.

The Apple M2 Max has eight performance and four efficiency cores. Worker counts
are Rayon pool sizes, not pinned assignments to particular physical cores.
The one-worker row uses automatic batching, so this is a consistent batch-path
comparison rather than a comparison against the separate single-VIN API.

The fastest sample represents 10.52 seconds of unique input, passing the
ten-second requirement. All samples are retained, including the slower second
one-worker and third twelve-worker trials. The exact source-build identity is
the immutable executable SHA-256 in the report.

[Raw samples, batch histories, executable hash, and embedded corpus manifest](../scripts/bench/core_scaling_2026_09_14.json).

```sh
uv run --frozen python -m scripts.bench.core_scaling
```

The runner verifies the corpus and binary hashes, checkpoints every sample,
and rejects a corpus that supplies less than ten seconds of unique input.
`make checku` passed, including 918 Python tests and all Rust checks.
