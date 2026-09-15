# Native predictor fit

This fit uses only native Rust full-result measurements over the exact committed 
5,000-row README corpus. Every child warms the full corpus before timing.

## Throughput

- Formula: `us_per_row = a + b/C + h0/B + h1*(C-1)/B + d0*B + d1*(C-1)*B`
- Coefficients: `{"a": 1.323291035836964, "b": 8.669023216126883, "d0": 0.00013180712580180103, "d1": 1.8428975878545342e-05, "h0": 0.0, "h1": 94.67501236299766}`
- Fit: R² 0.9466, MAPE 5.12%, RMSE 15,350 rows/s

## Peak RSS

- Power exponent: 0.92
- Power fit: R² 0.9967, MAPE 1.08%
- Affine fit: R² 0.9964, MAPE 1.16%

## Reproducibility

- [Raw fit matrix](../scripts/bench/native_predictor_2026_09_13.json)
- [Production calibration reference](../scripts/bench/native_predictor_auto_reference_2026_09_13.json)
- Reference single-core speed: 125,844.906 rows/s
- Measured native width: 9,571.9375 bytes/row
- Frozen clock: `2026-09-01T00:00:00+00:00`
- Corpus SHA-256: `b45ad4472c202ee176f86e8bc3c39609c76b6258df963ea04e08439aa3a1eb09`
- Executed binary SHA-256: `33f98cd59d28ad842cd55d19a392ebda88a8ae6708b1abc4561327f09d926a13`
- Throughput source SHA-256: `0c37bed66bc3cc225238314f7c3b462ad42205e025458439004ea6f80b4ec97e`

## Four-core validation

Three rotated 60-second rounds used the same full 5,000-row corpus and final
executable. Fixed modes warmed in their configured chunks; auto warmed in
256-row chunks. Auto's timed window includes production calibration.

Reproduce with `uv run --frozen python -m scripts.bench.native_predictor
--workers 4 --binary target/bench/native-predictor-validation-throughput
--no-build --validation-only` after building and snapshotting the release
example.

| Mode | Median rows/s | Range | Median peak RSS |
| --- | ---: | ---: | ---: |
| Fixed 5,000 baseline | 255,543 | 242,795–257,088 | 253.8 MiB |
| Predictor + feedback | 293,611 | 292,386–297,432 | 222.8 MiB |
| Fixed 1,000 grid winner | 300,390 | 298,376–301,481 | 178.4 MiB |

Auto improved throughput by 14.9% and reduced peak RSS by 12.2% against the
fixed-5,000 baseline. It ran 2.3% below the best fixed setting from this grid.
The 303,894 rows/s screening result for fixed 1,000 at eight workers is only the
best median in the two-second measured grid, not a machine-wide ceiling.

The initial auto predictions were 751, 753, and 927 rows. Runtime feedback then
placed most decoded rows in 256- and 512-row batches, with 1,024 the next most
common size. The initial prediction describes the model seed; the histogram in
the raw report records the sizes selected after online feedback.

- [Validation raw report](../scripts/bench/native_predictor_validation_2026_09_13.json)
- Validation binary SHA-256: `4270ebce45f78202fc7211714a4a2fd09b6983edf92aac05c9c0d2db9b4bdaeb`
- Predictor source SHA-256: `f310dfd85c65aaa2199eac6db6199f622ccd831a781624188d8e0b0d977b2bc6`
