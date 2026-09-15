# Native benchmark with five million unique VINs

The README benchmark uses five million distinct, well-formed synthetic VINs
from the embedded database's WMI/schema/pattern generator. It varies descriptor,
plant, serial, and model-year characters. It does not expand a small corpus by
repeating rows or changing only serial numbers.

The corpus contains 4,914,533 distinct first-eight-character/year-character keys,
12,933 normalized WMIs (including six-character low-volume WMIs), and all 30
model-year characters. Every VIN passes alphabet and modulus-11 check-digit
validation. These are generated test inputs, not a sample weighted to vehicle
registrations or sales.

## Corpus size requirement

The runner independently validates every VIN, uniqueness, count, and SHA-256.
It requires `unique VIN count / fastest measured VIN/s >= 10 seconds`. Faster
future hardware must use a larger corpus if this gate fails. Increasing the
number of timed repetitions cannot satisfy the gate.

Each fresh process warms one complete corpus pass, then times complete passes.
No VIN repeats within a pass. The requested ten-second measurement window is
a minimum: a pass always finishes, even when it takes longer. Automatic batching
uses its production predictor and live tuner, including calibration overhead
inside the measured window. Both paths use one frozen clock, 2026-09-01 UTC.
Four-worker automatic and single-core trials run in alternating sequence for
three rounds.

## Reproduce

```sh
UV_FROZEN=1 uv run --frozen maturin develop --uv --release --locked
uv run --frozen python -m scripts.bench.large_corpus
uv run --frozen python -m scripts.bench.large_native --mode both
```

Generation defaults to seed 42, 100,000-row chunks with successive seeds, and
exactly 5,000,000 unique rows. The 90 MB text file and its manifest live under
`target/bench/`; the measurement report embeds the manifest so the input hash,
generator hash, seed, database identity, and diversity counts travel with results.
The runner builds with Cargo's locked release profile and records its executable
hash, source hash, host CPU, and each child's peak RSS. RSS includes the large
in-memory input corpus as well as decoding; it is not just batch output memory.

The historical 5,000-input corpus remains unchanged for reproducing earlier
measurements. It contains pattern-derived synthetic inputs, including 721 strings
outside the standard VIN alphabet. Its throughput and the new large-corpus
throughput describe different workloads.

## Results

Apple M2 Max, three fresh-process trials per path, release build:

| Native output path | Median VIN/s | Range VIN/s | Median peak RSS |
|---|---:|---:|---:|
| Automatic batching, four workers | **210,808** | 197,452–213,062 | 1,306.4 MiB |
| Single core | **106,144** | 105,331–108,225 | 572.3 MiB |

Every timed trial decoded exactly 5,000,000 VINs. Automatic passes took
23.47–25.32 seconds; sequential passes took 46.2–47.5 seconds. The gate passed:
**23.47 seconds of unique input** at the fastest observed rate, above the
required ten seconds. At that rate the minimum corpus would be 2,130,620 VINs.

[Raw measurements and embedded input manifest](../scripts/bench/large_native_2026_09_14.json)
include every sample, automatic predictions, selected batch sizes, and build
provenance. Corpus SHA-256:
`94d156f7897934e048a2f1c4892408b03d9cb830d70eb3fa73c3dde3db4c1530`.

The README chart uses these medians. Its other-engine figures remain their
previously dated measurements or published figures; they were not remeasured
against this new corpus. Historical comparisons are in [BENCHMARKS.md](BENCHMARKS.md).
