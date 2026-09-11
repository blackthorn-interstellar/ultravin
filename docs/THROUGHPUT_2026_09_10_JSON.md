# September 10: direct full JSON decoding

Full JSON decoding is **1.98× faster single-core** and
**1.67× faster with four workers** against `8bce73a`.
The roughly 2× gain applies to the existing full JSON path in `4117c1a`.
The ordinary Rust result and Python dictionary APIs have different costs;
their output structures and signatures are preserved.

In Python, the optimized paths are `decode_json(vin, full=True)` and
`decode_batch_json(vins, full=True)`. In Rust they are `decode_json` and
`decode_batch_json`. These still include every full-provenance field.
The rates below measure the Rust APIs, including encoding and result destruction;
Python call and string-conversion overhead is not included.

## Sustained full JSON throughput

Three alternating 60-second windows per build/mode on the unchanged 5,000-VIN corpus:

| Mode | Before VIN/s (range) | After VIN/s (range) | Speedup |
|---|---:|---:|---:|
| Single core | 42,241 (41,762–42,295) | 83,607 (82,174–83,731) | 1.98× |
| Batch, four workers | 136,985 (136,716–137,021) | 229,174 (227,991–230,399) | 1.67× |

Both builds use the same corpus, artifact, Cargo.lock, harness, compiler,
mimalloc allocator and standard release settings. Each process warms the whole
corpus before timing. Builds and tests were stopped during measurement; the
Apple M1 Max (32 GiB RAM) remained shared with other work.

Every sample, executable/source/input hash and configuration is in the
[raw report](../scripts/bench/throughput_2026_09_10_json.json).

## Implementation

- Write JSON directly from the winning decode items, avoiding the intermediate
  owned result and its per-element string copies.
- Escape fixed element metadata once per database. Reuse a complete
  `Not Applicable` default row only when all its field values and timestamp match;
  a different timestamp follows the ordinary encoder.
- Check ordinary UTF-8 strings eight bytes at a time before copying them.
  Quotes, backslashes and control characters use serde's escaping.
- Reserve enough output capacity for the common full result. The initial
  320-byte-per-element estimate grew on 4,766 of 5,000 corpus VINs; the final
  400-byte estimate fits all 5,000, while larger results can still grow normally.
- Prepare WMI identifier/name strings once per database row, and recognize
  fixed source labels without repeated substring scans. Dynamic source text
  retains the original case-insensitive check.

Caches are bounded by database rows and elements. Every VIN still goes through
pattern matching, model-year selection and correction/error computation.
No decoded-VIN cache, dependency, artifact-format or public API change was added.

## Ordinary Rust result control

Two alternating 20-second windows per build/mode on the same 5,000 VINs:

| Mode | Before VIN/s (range) | After VIN/s (range) | Speedup |
|---|---:|---:|---:|
| Single core | 91,769 (91,713–91,825) | 93,072 (92,671–93,474) | 1.01× |
| Batch, four workers | 200,148 (200,141–200,156) | 202,212 (201,944–202,479) | 1.01× |

This isolates the smaller gains shared by callers that return Rust structs.
It must not be presented as the full JSON improvement.

## Broader input check

Two alternating 20-second windows per build/mode over 10,000 distinct VINs:

| Mode | Before VIN/s (range) | After VIN/s (range) | Speedup |
|---|---:|---:|---:|
| Single core | 46,120 (45,431–46,810) | 89,490 (89,394–89,585) | 1.94× |
| Batch, four workers | 157,474 (157,210–157,739) | 265,123 (262,335–267,911) | 1.68× |

This is a stress sample, not a fleet distribution. It is the first 10,000 rows
of the September 9 deterministic 100,000-VIN compatibility sample; construction
and hashes are recorded in the raw report. The 10,000-row batch limits the size of simultaneously materialized full JSON
results in this local experiment. Caller years
are omitted for throughput, but included in correctness checks.

## Correctness

All **1,862,306 fixed-clock full-result fingerprints** match `8bce73a`.
The new JSON writer also matches serde's complete JSON bytes on every one of
those cases, with the clock held consistent within each comparison.
Both streams have SHA-256
`e985b2617222a111cb2444b02af3fa62e0f7bfb18345170dbd6829e9e047ba52`.
This covers complete row order, provenance, errors, corrections, caller years,
and malformed/Unicode/partial/overlong inputs; it is not a fresh SQL oracle run.

`make check checku` passed: **168 Rust tests and 825 Python tests**, with the
existing Python fork deprecation warning. Focused tests cover exact escaping,
word boundaries, sparse/absent/private elements, repeated notes, null and extreme
integers, varying default timestamps, multiple databases and shared templates
across threads. Full JSON batches preserve exact bytes and input/year alignment.

## Memory and startup

Median process peak RSS from three alternating two-second full JSON probes per
build/mode, with a full-corpus warmup and four batch workers:

| Full JSON mode | Before peak RSS MiB | After peak RSS MiB |
|---|---:|---:|
| single | 133.9 | 135.1 |
| batch | 359.7 | 376.9 |

Median first ordinary Rust decode from seven alternating fresh processes per VIN,
using the system allocator in both builds:

| VIN | Before first decode ms | After first decode ms |
|---|---:|---:|
| `1HGCM82633A004352` | 1.944 | 2.015 |
| `1FTFW1ET5DFC10312` | 3.421 | 3.361 |
| `ZZZCM82633A004352` | 0.069 | 0.068 |

Filesystem caches were not flushed. These startup figures do not measure cold
storage or the additional first-use JSON metadata initialization.

## Reproduce

The baseline needs the candidate's extended throughput harness to expose `json`:

```bash
git worktree add --detach target/json-before 8bce73a
cp crates/ultravin/examples/throughput.rs target/json-before/crates/ultravin/examples/throughput.rs
export ULTRAVIN_DATA="$PWD/crates/ultravin/data/vpic.rkyv"
unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_PROFILE_RELEASE_DEBUG
CARGO_TARGET_DIR="$PWD/target/json-before-build" cargo build --manifest-path target/json-before/Cargo.toml -p ultravin --example throughput --example fingerprint --release --locked
cargo build -p ultravin --example throughput --example fingerprint --release --locked
uv run --frozen -- python scripts/bench/compare.py target/json-before-build/release/examples/throughput target/release/examples/throughput --format json --seconds 60 --rounds 3 --threads 4
uv run --frozen -- python scripts/bench/fingerprint_cases.py target/answerkey/answerkey-2026_08.jsonl target/json-cases.jsonl
target/json-before-build/release/examples/fingerprint < target/json-cases.jsonl > target/json-before.hashes
target/release/examples/fingerprint < target/json-cases.jsonl > target/json-after.hashes
cmp target/json-before.hashes target/json-after.hashes
target/release/examples/fingerprint json < target/json-cases.jsonl > target/json-encoded.hashes
make check checku
```
