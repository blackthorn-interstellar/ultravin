# September 11 decoder optimization

**The 2× target was not reached.** Against `38fb2ff`, the changes improve
full Rust-result throughput by **6.3% single-core** and **5.3% with four workers**
on the fixed 5,000-VIN corpus. These gains apply to this before/after pair only.

## Sustained throughput

Medians of three alternating 60-second windows per build and mode:

| Mode | Before VIN/s (range) | After VIN/s (range) | Speedup |
|---|---:|---:|---:|
| Single core | 80,768 (78,472–81,030) | 85,847 (85,069–86,117) | 1.06× |
| Batch, four workers | 178,326 (178,286–184,979) | 187,707 (184,329–189,092) | 1.05× |

Both executables use the same artifact, harness, Rust 1.98.1, mimalloc allocator,
and release profile (optimization level 3, fat LTO, one codegen unit, no custom
Rust flags). Dependency versions are unchanged; the existing transitive
`memchr` 2.8.3 dependency is now also declared directly. Each process warms the
entire corpus before timing. Builds, tests and profiling were stopped during
timing; the Apple M1 Max remained shared with other work.

Median process CPU time per VIN fell from **12.00 to 11.24 µs** single-core and
**14.83 to 14.25 µs** in the batch process. Those CPU figures include startup,
warmup and teardown.

The existing 100,000-distinct-VIN stress corpus also improved: **4.5% single-core**
and **1.7% batch**, using two alternating 20-second windows per build and mode.
This is a stress distribution, not a fleet distribution. Caller years are omitted
for throughput. Every sample and the corpus hashes are in the
[raw report](../scripts/bench/throughput_2026_09_11.json).

## Changes

- Precompute the weighted check-digit contributions in an 8.5 KiB constant table.
  WMI-dependent numeric-position rules still run per call, including the original
  invalid-character `?` sentinel behavior.
- Use `memchr3` to find tabs, carriage returns and newlines during value cleanup.
  Replacement behavior and borrowed-string ownership remain unchanged.
- Move string fields directly from the ordered decode items into output rows,
  removing the intermediate conversion of every item into an `Option`.

No public API, result shape or decoded-VIN cache was added or changed.
A shared-prefix matcher did not establish a throughput gain. A borrowed-output
prototype missed the 2× target while changing public Rust field types. Both
experiments were discarded; their short, noisy exploratory trials are retained
in the raw report separately from the acceptance measurements.

## Correctness

All **1,862,306 fixed-clock full-result fingerprints** match the starting build,
including provenance, output order, caller years, errors, corrections and
malformed inputs. Both streams have SHA-256
`e985b2617222a111cb2444b02af3fa62e0f7bfb18345170dbd6829e9e047ba52`.
This establishes agreement on those cases, not a new SQL oracle run.

`make check checku` passes: **170 Rust tests and 825 Python tests**, with the
existing Python fork deprecation warning. New checks compare the check-digit
kernel with the original positional rules across ASCII mutations and exercise
text cleanup across vector boundaries, Unicode and borrowed/owned values.

## Startup and memory

Median first decode from seven alternating fresh processes per VIN, using the
system allocator in both builds:

| VIN | Before ms | After ms |
|---|---:|---:|
| `1HGCM82633A004352` | 2.264 | 1.978 |
| `1FTFW1ET5DFC10312` | 3.452 | 3.478 |
| `ZZZCM82633A004352` | 0.075 | 0.080 |

Filesystem caches were not flushed, so these do not measure cold-storage latency.
Median peak RSS from three alternating two-second full-result probes was
**134.5 → 134.6 MiB** single-core and **255.3 → 256.3 MiB** with four workers.

## Reproduce

With the pinned artifact present, build the baseline in a detached worktree and
the candidate in the main checkout:

```bash
git worktree add --detach target/september11-before 38fb2ff
export ULTRAVIN_DATA="$PWD/crates/ultravin/data/vpic.rkyv"
unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_PROFILE_RELEASE_DEBUG
CARGO_TARGET_DIR="$PWD/target/september11-before-build" cargo build --manifest-path target/september11-before/Cargo.toml -p ultravin --example throughput --example fingerprint --release --locked
cargo build -p ultravin --example throughput --example fingerprint --release --locked
uv run --frozen -- python scripts/bench/compare.py target/september11-before-build/release/examples/throughput target/release/examples/throughput --seconds 60 --rounds 3 --threads 4
uv run --frozen -- python scripts/bench/fingerprint_cases.py target/answerkey/answerkey-2026_08.jsonl target/september11-cases.jsonl
target/september11-before-build/release/examples/fingerprint < target/september11-cases.jsonl > target/september11-before.hashes
target/release/examples/fingerprint < target/september11-cases.jsonl > target/september11-after.hashes
cmp target/september11-before.hashes target/september11-after.hashes
make check checku
```
