# Contention-aware batch tuning — September 14, 2026

The predictor included dispatch costs, but two runtime behaviors undermined its
estimate: native/JSONL calibration escaped into the multiworker pool, and the
live tuner repeatedly selected smaller batches within 5% of a local peak without
requiring a speed improvement. Sequential short probes also mixed batch-size
effects with changing host load.

Calibration now keeps actual per-VIN work on its private one-thread pool.
The tuner compares incumbent/challenger/incumbent windows of at least three
full batches and 10 ms, requires three consecutive improvements exceeding 5%
over both incumbent brackets, and rejects bracket drift over 15%. Unsuccessful
probes retain the incumbent. Retuning requires both 32 batches and 250 ms of
observed work; memory limits still take effect immediately.

## Controlled comparison

Both saved release binaries processed the same five-million-unique-VIN corpus
with 12 workers and a frozen clock. Each fresh child warmed a full pass before
timing a full pass, including calibration and tuning. Two paired rounds reversed
binary and load-condition order. The added-load condition ran four owned CPU
load processes continuously from before warmup until after measurement. All four
were alive at the end of every loaded trial and were then terminated and reaped.
Other host activity was not isolated; “no added load” means no harness load.

| Condition | Before median VIN/s | After median VIN/s | Change |
|---|---:|---:|---:|
| No added load | 268,589 | 275,760 | +2.7% |
| Four competing CPU processes | 226,990 | 221,683 | -2.3% |

The correction improves this no-added-load comparison by 2.7%, while the
loaded comparison is 2.3% slower. Two rounds establish a measured tradeoff,
not a general speedup under contention. The deterministic tests establish the
controller behavior under transient delays, rising/falling load, genuine
sustained gains, and changing memory caps.

In the first no-added-load pair, the old build reported 494,925 VIN/s as its
single-core calibration rate; the corrected build measured 134,386 VIN/s.
The new model started at 1,446 rows and used that size for most of the job.
The prior claim that 256–512-row batches necessarily caused poor all-core
scaling was a hypothesis; these measurements do not isolate that cause.

[Raw paired results, input manifest, batch histories, and executable hashes](../scripts/bench/contention_2026_09_14.json).

## Reproduce

Save the old and new locked release builds as separate executables, then run:

```sh
uv run --frozen python -m scripts.bench.contention \
  --before-binary target/bench/contention-before-throughput \
  --new-binary target/bench/contention-after-throughput \
  --rounds 2 --workers 12 --burners 4 --burner-lifetime 300 \
  --output target/bench/contention.json
```

The harness checkpoints each sample, rejects changed binaries or expired load
processes, and enforces at least ten seconds of unique input at the fastest
observed rate. Source and binary behavior are versioned by the saved executable
hashes; older benchmark reports retain their original build results.

Validation: `make checku` passed, including 917 Python tests and all Rust checks.
