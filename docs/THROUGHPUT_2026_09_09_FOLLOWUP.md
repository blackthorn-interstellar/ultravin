# September 9 decoder follow-up

**The 2× target was not reached.** Against the starting revision `d536710`,
`54cbf7f` improves single-core throughput by 18.5% and four-worker batch
throughput by 9.6%. These are additional gains over the September 9 code;
they do not count the earlier optimization rounds again.

## Sustained throughput

Medians of three alternating 60-second windows per build and mode on the
unchanged 5,000-VIN corpus:

| Mode | Before VIN/s (range) | After VIN/s (range) | Speedup |
|---|---:|---:|---:|
| Single core | 71,556 (70,903–74,751) | 84,822 (84,806–87,979) | 1.19× |
| Batch, four workers | 178,083 (174,951–179,079) | 195,203 (194,519–195,458) | 1.10× |

The executables use the same artifact, Cargo.lock, harness, mimalloc allocator,
Rust 1.98.1 and standard release profile: optimization level 3, fat LTO, one
codegen unit, no debug information or custom Rust flags. Each process warms the
whole corpus before timing. Our builds and tests were stopped during timing;
the Apple M1 Max remained shared with other work.

Median process CPU time per VIN fell from
14.07 to 11.88 µs single-core and from
16.42 to 14.45 µs in the batch process.
These CPU figures include startup, warmup and teardown.

Every sample, input/source/executable hash, cold-start measurement and validation
result is in [the raw report](../scripts/bench/throughput_2026_09_09_followup.json).

## Changes

- Index schema positions, WMI/schema ranges and model/make ranges with compact
  arrays when IDs are dense. Sparse IDs retain the original searches, and
  duplicate rows retain their original order and selection behavior.
- Index normalized engine names once per database, keeping the first match.
- Filter pattern candidates using their first and last required literal bytes.
  The existing matcher still decides whether each surviving candidate matches.
- Check plain ASCII correction keys directly, including strict wildcard and
  digit-placeholder behavior. Bracket and Unicode keys retain their expansion.
- Precompute public output sort keys instead of resolving group names and
  comparing separate group/element keys for every decoded row.

The indexes belong to each database and are bounded by its data. No decoded-VIN
result cache, artifact-format change, public API change or dependency was added.

A 64-pattern bitmap matcher and copied output-metadata templates were rejected:
their small single-core gains did not establish a useful batch improvement.
Hash indexes for archive joins were replaced after adding about 1.3 ms to the first decode.

## Broader input check

Two alternating 20-second windows per build and mode on 100,000 distinct VINs:

| Mode | Before VIN/s (range) | After VIN/s (range) | Speedup |
|---|---:|---:|---:|
| Single core | 74,528 (74,139–74,917) | 86,236 (86,031–86,442) | 1.16× |
| Batch, four workers | 188,817 (188,812–188,822) | 203,746 (201,329–206,163) | 1.08× |

This is a stress corpus, not a fleet distribution. It samples the full
compatibility inputs using `random.Random(20260909).sample(sorted(vins), 100000)`,
after keeping distinct 17-byte ASCII VINs without CR/LF. Caller years are omitted
for throughput. The source-case and sample hashes are recorded in the raw report.

## Correctness and startup

All **1,862,306 full serialized-result fingerprints** match the starting build,
including row order, provenance, errors and corrections. Both fingerprint streams
have SHA-256
`e985b2617222a111cb2444b02af3fa62e0f7bfb18345170dbd6829e9e047ba52`.
The fixed-clock comparison proves agreement on these cases; it is not a new SQL
oracle run or a proof over every possible input.

`make check checku` passes: 160 Rust tests and 825 Python tests, with the existing
Python fork deprecation warning. Added checks compare the indexes against the
original archive searches and exercise duplicate, sparse and extreme IDs.

The remaining startup tradeoff is about **0.3 ms** on the two registered WMIs.
Medians of seven alternating fresh processes per VIN, from main entry through
the first result, using the system allocator in both probes:

| VIN | Before ms | After ms |
|---|---:|---:|
| `1HGCM82633A004352` | 1.459 | 1.755 |
| `1FTFW1ET5DFC10312` | 3.082 | 3.367 |
| `ZZZCM82633A004352` | 0.062 | 0.066 |

The filesystem cache was not flushed, and cold-start outliers are retained in
the raw report. These measurements do not establish cold-storage latency.

## Reproduce

From the candidate revision with the pinned artifact present:

```bash
git worktree add --detach target/throughput-before d536710
export ULTRAVIN_DATA="$PWD/crates/ultravin/data/vpic.rkyv"
unset CARGO_PROFILE_RELEASE_DEBUG RUSTFLAGS CARGO_ENCODED_RUSTFLAGS
CARGO_TARGET_DIR="$PWD/target/throughput-before-build" cargo build --manifest-path target/throughput-before/Cargo.toml -p ultravin --example throughput --example fingerprint --example cold --release --locked
cargo build -p ultravin --example throughput --example fingerprint --example cold --release --locked
uv run --frozen -- python scripts/bench/compare.py target/throughput-before-build/release/examples/throughput target/release/examples/throughput --seconds 60 --rounds 3 --threads 4
uv run --frozen -- python scripts/bench/fingerprint_cases.py target/answerkey/answerkey-2026_08.jsonl target/perf-cases.jsonl
target/throughput-before-build/release/examples/fingerprint < target/perf-cases.jsonl > target/before.hashes
target/release/examples/fingerprint < target/perf-cases.jsonl > target/after.hashes
cmp target/before.hashes target/after.hashes
```
