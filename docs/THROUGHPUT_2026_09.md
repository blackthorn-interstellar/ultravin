# September 2026 throughput work

The goal was twice the decoding throughput with identical answers. These changes
improve the existing decoder without an alternate output mode, an artifact
format change, or a cache of previously decoded VINs.

## September 8 rerun

**1.55× single-core and 1.43× four-core batch throughput. The 2× target remains
unmet.** Medians of three 60-second windows per build and mode:

| Mode | Before VIN/s | After VIN/s | Speedup | Before / after process CPU µs per VIN |
|---|---:|---:|---:|---:|
| Single core | 29,293 | 45,376 | 1.55× | 34.26 / 22.15 |
| Batch, four cores | 91,206 | 130,381 | 1.43× | 35.96 / 24.49 |

The previous README reported 29,568 single-core and 94,030 four-core VIN/s.
This rerun's baseline is within about 1% and 3% of those figures, respectively.
September 7's absolute measurements were lower for both builds. The paired
improvement is similar, but those lower rates understated the throughput seen
in this rerun. An older 121,359 VIN/s figure used ten cores, not four.

Single-core samples ranged from 28,021–30,480 before and 44,857–46,849 after;
batch samples ranged from 90,039–94,582 before and 125,445–131,708 after. Every
sample is retained in
[`throughput_2026_09_08.json`](../scripts/bench/throughput_2026_09_08.json), along
with commit ids, toolchain, input and executable hashes, and process CPU times.
The README and chart now use these medians.

Both executables used the same harness, 5,000-VIN corpus, embedded artifact,
Cargo.lock, mimalloc allocator and standard release profile (`opt-level=3`,
fat LTO, one codegen unit, no debug information). The baseline is `3eafb62`,
immediately before the optimization; the candidate is `f2913e8`. Both include
the newly rebased upstream changes. The host was the same Apple M1 Max on AC
power, with other applications running and no reported thermal or performance
warning. This does not isolate which environmental or build difference caused
the lower September 7 rates.

The harness warmed the whole corpus before each window and alternated build
order. Process CPU time includes startup, warmup and teardown. No decoder code
changed for this rerun; correctness fingerprints, startup and memory figures
below remain the earlier measurements and were not rerun.

To reproduce from `f2913e8`, with the artifact already present:

```bash
git worktree add --detach /tmp/ultravin-before-20260908 3eafb62
cp crates/ultravin/examples/throughput.rs /tmp/ultravin-before-20260908/crates/ultravin/examples/
cp Cargo.lock /tmp/ultravin-before-20260908/Cargo.lock
export ULTRAVIN_DATA="$PWD/crates/ultravin/data/vpic.rkyv"
unset CARGO_PROFILE_RELEASE_DEBUG
CARGO_TARGET_DIR="$PWD/target/bench-before-20260908" cargo build \
  --manifest-path /tmp/ultravin-before-20260908/Cargo.toml \
  -p ultravin --example throughput --release --locked
cargo build -p ultravin --example throughput --release --locked
uv run --frozen -- python scripts/bench/compare.py \
  target/bench-before-20260908/release/examples/throughput \
  target/release/examples/throughput --seconds 60 --rounds 3 --threads 4 \
  --output target/throughput-20260908.json
```

## Initial results (September 7)

**1.57× single-core and 1.40× four-core batch throughput. The 2× target was not
reached.** Medians of three 20-second windows per executable and mode, measured
on the same Apple Silicon machine on September 7, 2026:

| Mode | Before VIN/s | After VIN/s | Speedup | Before / after process CPU µs per VIN |
|---|---:|---:|---:|---:|
| Single core | 24,627 | 38,587 | 1.57× | 40.36 / 26.16 |
| Batch, four cores | 77,849 | 109,190 | 1.40× | 40.39 / 27.54 |

The host was contended. Single-core samples ranged from 16,231–25,090 before
and 35,745–39,697 after; batch samples ranged from 61,699–82,725 before and
96,181–112,141 after. One slow baseline window was retained, not discarded.
The CPU-cost reduction corroborates the improvement, but the exact wall-clock
speedup will vary by host and workload. These measure the Rust engine, not
Python dictionary construction or parquet I/O.

There is a startup and memory cost. Median time inside a fresh process to load
and decode the canonical Honda VIN rose from **0.675 ms to 1.597 ms** (11 samples
each, including a roughly 36–38 ms outlier for each build). Maximum RSS in
separate five-second corpus runs rose from **95.4 to 129.9 MiB** for single-core
and **230.2 to 252.6 MiB** for four-core batches. These are whole-process peaks;
the index memory depends on which schemas the process has visited.

All samples, resource measurements and correctness digests are committed in
[`throughput_2026_09.json`](../scripts/bench/throughput_2026_09.json).

## What changed

- Match each distinct pattern key once per schema, then expand its matches to
  the original rows. Restore global pattern order before resolving attributes.
- Index keys by one required literal character. Keys without a usable literal
  still use the existing matcher, including its regex fallback.
- Keep global row indices so later passes avoid searching the 1.67-million-row
  pattern table again. Index formula rows separately while preserving their
  distinct eligibility rules, including orphan schema ids.
- Index vehicle-spec schemas by make and model, preserving archive order and
  the existing year, vehicle-type, QC and key-pattern checks.
- Borrow keys and attribute ids until the winning pass is projected. Losing
  passes and discarded duplicate rows avoid string copies.
- Answer error code 14's six character-membership questions with fixed flags
  instead of constructing a set containing every possible character.

The new indexes belong to each `Db`, initialize lazily with `OnceLock`, and are
shared across workers. String ids cannot collide across loaded artifacts. The
embedded artifact and public result types are unchanged.

## Correctness

Original and optimized executables produced identical full-result BLAKE3
fingerprints for **1,862,306 cases**: every VIN in the local 2026_08 answer key,
the benchmark corpora, and deterministic mutations and year hints for the first
10,000 distinct VINs. Inputs include partial, lowercase, whitespace-padded and
overlong VINs, invalid ASCII, non-ASCII characters, and caller years 0, 1980,
1995, 2010, 2026 and 2028.

The comparison serializes the entire `DecodeResult` at a fixed clock, including
row order, provenance, errors and corrections. It does not normalize differences
or exempt known upstream defects. This proves agreement on this corpus, not on
every possible input; it is not a fresh run against the SQL oracle.

Input file SHA-256:
`93d318f511450137545ee64bfe0311dc737233748ee9bf32250e8043e12b6ea8`.
Both fingerprint files have SHA-256:
`e985b2617222a111cb2444b02af3fa62e0f7bfb18345170dbd6829e9e047ba52`.

Tests also compare indexed matching against a row scan, exercise duplicate and
excluded elements, wildcards, regex fallbacks, separate databases and concurrent
initialization, compare every archived make/model spec join, and compare the
error-position flags against the original character-set algorithm.

`make check` passed all 136 Rust tests and 807 Python tests, plus formatting,
lints and type checking. The fork test emitted its existing Python deprecation
warning.

## Reproduce

Run from the optimized checkout with the pinned artifact built. Both revisions
must use the same harness, Cargo.lock, artifact, allocator and release settings.
Both measured builds enabled line tables for profiling; release optimization
settings were unchanged.

```bash
git worktree add --detach /tmp/ultravin-before 0fe7564
cp crates/ultravin/examples/throughput.rs /tmp/ultravin-before/crates/ultravin/examples/
cp crates/ultravin/examples/fingerprint.rs /tmp/ultravin-before/crates/ultravin/examples/
export ULTRAVIN_DATA="$PWD/crates/ultravin/data/vpic.rkyv"
export CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
CARGO_TARGET_DIR="$PWD/target/perf-before" cargo build \
  --manifest-path /tmp/ultravin-before/Cargo.toml -p ultravin \
  --example throughput --example fingerprint --example cold --release --locked
cargo build -p ultravin --example throughput --example fingerprint --example cold --release --locked

uv run -- python scripts/bench/compare.py \
  target/perf-before/release/examples/throughput target/release/examples/throughput \
  --seconds 20 --rounds 3 --threads 4

uv run -- python scripts/bench/fingerprint_cases.py \
  target/answerkey/answerkey-2026_08.jsonl target/perf-cases.jsonl
target/perf-before/release/examples/fingerprint < target/perf-cases.jsonl > target/before.hashes
target/release/examples/fingerprint < target/perf-cases.jsonl > target/after.hashes
cmp target/before.hashes target/after.hashes
```

The comparison alternates executable order, warms the entire 5,000-VIN corpus
before each window, retains every measurement in JSON, and reports medians.
Process CPU time includes startup, warmup and teardown; it helps diagnose host
contention but is not warm-decode latency.
