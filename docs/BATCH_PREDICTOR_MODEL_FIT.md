# Batch predictor model fit

The predictor is fitted to the fixed-size batch sweep in
[`scaling_2026_09_13_screen.json`](../scripts/bench/scaling_2026_09_13_screen.json).
That sweep contains one two-second observation at each combination of 100,
1,000, 10,000, and 50,000 rows and 1, 2, 4, and 12 workers on one Apple M2 Max.
The model therefore describes this machine and workload. Runtime adjustment by
a serial decode measurement is a heuristic until measurements from other
hardware are available.

## Throughput

The fit uses elapsed microseconds per row because batch costs add in time:

```text
t(B, C) = a + b/C + h0/B + h1(C-1)/B + d0 B + d1(C-1)B
```

`a + b/C` is the per-row floor, `h0/B + h1(C-1)/B` amortizes fixed batch and
worker-dispatch cost, and the `d` terms permit pressure from large batches. All
coefficients are fitted nonnegative. `B` is rows and `C` is workers; throughput
is `1,000,000 / t` rows/s.

| format | a | b | h0 | h1 | d0 | d1 | one-worker reference |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Parquet | 3.24202728 | 7.13701304 | 848.461203 | 46.9762419 | 0 | 0 | 96,348 rows/s |
| JSONL | 3.08295824 | 9.00551570 | 73.4312643 | 32.9378126 | 0 | 2.92333133e-7 | 82,723 rows/s |

The one-worker reference is `1,000,000 / (a + b)`, the modeled large-batch
rate on the measured machine. It is a full-path rate and is not interchangeable
with a native-only serial calibration timer. A hardware multiplier must use an
M2 Max reference collected with the same native timer as the runtime sample.
The shipped predictor uses those separately measured native references:
108,350 columnar rows/s and 82,790 JSONL rows/s, recorded in
[`predictor_reference.json`](../scripts/bench/predictor_reference.json).

| format | training MAPE | training R² | leave-one-size-out MAPE | leave-one-size-out R² | repeated-run MAPE |
| --- | ---: | ---: | ---: | ---: | ---: |
| Parquet | 9.11% | 0.923 | 13.66% | 0.853 | 16.14% (2 points) |
| JSONL | 6.40% | 0.944 | 23.99% | 0.194 | 3.49% (3 points) |

The repeated points come from the committed 5-second scaling follow-ups and
were not used for fitting. JSONL's weak leave-one-size-out result means the
model should rank a bounded candidate grid rather than support unrestricted
batch-size extrapolation. The selection rule is the smallest feasible candidate
whose modeled throughput reaches 99% of the feasible modeled peak.

## Peak RSS estimate

Peak RSS is fitted in MiB with a nonnegative power model:

```text
M(B, C) = base + worker(C-1) + row B^p + worker_row(C-1)B^p
```

| format | p | base | worker | row | worker_row | MAPE | R² |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Parquet | 1.29 | 214.5827 | 3.54735 | 0.000206771 | 0.00000698368 | 8.63% | 0.955 |
| JSONL | 0.38 | 2.32929 | 0 | 23.09847 | 0.972927 | 16.06% | 0.968 |

The exponent is selected from 0.01 through 1.50 by training squared error.
A simpler affine fit is nearly as good for Parquet (9.52% MAPE, R² 0.950), but
is substantially worse for JSONL (45.94% MAPE, R² 0.863). The shipped predictor
therefore uses the affine Parquet coefficients in the raw artifact and the
JSONL power coefficients above, with a 208 MiB reference-process floor. The RSS output is an
estimate of whole-process peak memory observed by the harness. It is suitable
for screening and heatmaps, not as a hard allocation guarantee.

Regenerate the coefficients and full-precision metrics without changing the
locked environment:

```console
uv run --frozen python scripts/bench/predictor_fit.py
```

The generated artifact is
[`predictor_model_fit.json`](../scripts/bench/predictor_model_fit.json).
