# ultravin benchmarks

Latest verified results are at the top; the **W3 baseline** (the starting point,
before the zero-copy / artifact-slimming / hot-path optimizations) is kept lower
down for comparison. All numbers are deliberately honest and reproducible.

Host: Apple Silicon (aarch64-apple-darwin), `cargo 1.90`, release profile
(`opt-level=3`, `lto="thin"`, `codegen-units=1`). Artifact:
`crates/ultravin-core/data/vpic.rkyv` (gitignored build product).

## Latest results (verified)

Re-measured after the latest hot-path round (cut remaining per-decode
allocations + a sharded `mimalloc` global allocator, and interned
element-metadata PyStrings on the marshalling path), on top of the earlier
zero-copy load + `valid_charset` memoization work. The single-threaded criterion
figures (warm decode, single-core batch) are scheduler-stable; the multi-core
batch throughput reads ~111k VIN/s on a quiet host (an earlier run under heavy
load read ~102k, so treat that as the conservative floor).

### Acceptance targets

| metric | target | baseline | latest | met? |
|---|---|---|---|---|
| warm single-decode | < 50 us | 4204 us | **44.8 us** | **yes** |
| cold-start (fresh process, load + 1 decode) | < 5 ms | 29.3 ms | **0.635 ms** (median, n=11) | **yes** |
| batch throughput (1 core) | > 100k VIN/s | 325 VIN/s | **20.3k VIN/s** | no |
| artifact download (compressed) | <= ~21 MB | 20.0 MB gzip | **19.25 MB** gzip-9 / 14.18 MB zstd-19 | **yes** |

3 of 4 acceptance targets met (warm decode, cold-start, download). Warm decode
crossed the 50 us line this round: interning element metadata and cutting the
remaining per-decode allocations took it 202.8 → 44.8 us, and cold-start 1.26 →
0.635 ms on top of the slimmer zero-copy artifact + sharded allocator. Single-core
batch (20.3k VIN/s) is the one remaining miss — the > 100k VIN/s/core target
needs a deeper compute rewrite that risks parity and was not attempted.

### ultravin vs corgi vs Postgres (identical "decode one VIN" task)

| engine | single decode (warm) | cold-start | artifact (download) | notes |
|---|---|---|---|---|
| **ultravin** (Rust, in-proc) | **44.8 us** | **0.635 ms** | **19.25 MB** gzip | zero-copy embedded rkyv; multi-core batch ~111k VIN/s @10 cores |
| corgi v2 (SQLite, published) | ~30 ms | n/a | ~21 MB gzip | `@cardog/corgi` 2.0.1, ISC/TS |
| corgi v3 (binary index, published) | ~12 ms | n/a | ~21 MB gzip | blog/roadmap figure |
| Postgres oracle (`spvindecode`) | ~61.5 ms | n/a (server) | n/a | full SQL round-trip over localhost TCP |

ultravin warm decode is ~268x faster than corgi v3 (published), ~670x faster
than corgi v2, and ~1,370x faster than the Postgres round-trip oracle, on the
same VIN, with a smaller compressed download.

## Throughput (random corpus)

A second, harder benchmark: **how many VINs each engine decodes per second**,
single sequential caller, over an identical random corpus of 5,000 valid VINs
(seeded shuffle of the full WMI→schema→pattern set; the oracle is authoritative
for what's decodable), measured over a 60 s wall-clock window. This is a varied
workload, not one cache-friendly VIN, so the per-decode cost is higher than the
warm single-decode number above.

| engine | VIN/s | vs ultravin (1 core) |
|---|---|---|
| **ultravin** — batched, 10 cores | **111,496** | ~5.8× faster |
| **ultravin** — 1 core | **19,331** | 1× |
| corgi v3 (binary index, published) | ~83 | ~233× slower |
| corgi v2 (SQLite, published) | ~33 | ~586× slower |
| NHTSA MSSQL (`spVinDecode`, SQL Server) | 22.5 | ~859× slower |
| NHTSA Postgres (`spvindecode`) | 19.5 | ~991× slower |
| NHTSA vPIC web API (public rate limit) | ~10 | ~1,933× slower |

These figures are after three rounds of hot-path work, all byte-identical output:

- the per-thread `(wmi, model_year)` memoization of the suggested-VIN correction
  charset (`valid_charset`), which removed ~60% of the hot path: single-core
  3,756 → 10,339 VIN/s, batch 22,338 → 47,990 VIN/s; then
- an allocation + matching rewrite (custom fixed-length token matcher for
  bracket keys in place of the regex engine, `from_utf8_unchecked` arena reads
  validated once at load, `Cow<'static, str>` decode items, an O(1) `element_by_id`
  index, FxHash for the integer-keyed sets, interned PyDict keys), which raised
  single-core **9,717 → 14,291 VIN/s** and batch **43,608 → 54,801 VIN/s**; then
- an allocator + marshalling round (cut the remaining per-decode allocations, a
  sharded `mimalloc` global allocator so the parallel batch path stops
  serializing on the global heap lock, and interned element-metadata PyStrings),
  which raised single-core **14,291 → 19,331 VIN/s** and batch **54,801 →
  111,496 VIN/s**, and cut warm single-decode 202.8 → 44.8 us (same host, same
  60 s methodology, before/after measured together).

Notes on honesty:
- **ultravin** is the in-process Rust engine (system-clock path). The single-core
  number (19,331 VIN/s) is over a varied corpus, not one repeated VIN; batched
  (111,496 VIN/s) scales ~5.8× across 10 cores — sublinear because varied patterns
  and shared memory bandwidth bound the per-thread matcher/charset caches, but up
  from ~3.8× the previous round, as the sharded allocator removed the global
  heap-lock contention that was throttling the parallel path. An earlier run on a
  loaded host (load avg ~16) read ~102k; the ~111k here is on a quiet host.
- **corgi v2/v3** are *derived* from the project's published per-VIN latency
  (~30 ms / ~12 ms → ~33 / ~83 VIN/s), not re-measured here.
- **NHTSA Postgres** runs the unmodified `vpic.spvindecode` over localhost TCP
  (psycopg). The varied corpus averages ~51 ms/VIN vs the ~61.5 ms single-VIN
  baseline — both in the same ballpark.
- **NHTSA MSSQL** runs the unmodified `dbo.spVinDecode` shipped in
  `vPICList_lite_2026_06.bak`, restored into SQL Server 2022. On Apple Silicon
  that image only runs under **amd64 emulation (Rosetta)**, so its throughput
  understates native SQL Server hardware — yet ultravin is still ~859× faster.
- **NHTSA vPIC web API** is not a decode measurement: it's the public API's
  ~10 requests/s rate limit ([corgi blog](https://cardog.app/blog/corgi-vin-decoder)),
  the practical ceiling for anyone decoding against the hosted service. It's a
  hard cap regardless of hardware, included for context.

All measured on the same Apple Silicon host (10 cores). The ultravin rows were
re-measured after the allocator/marshalling round (median of 3× 60 s runs); the
SQL-oracle rows are round-trip-bound and carried over from the prior run — they
are dominated by query execution, not client CPU, so they are effectively
host-independent at this scale.

### Reproduce the throughput benchmark

```sh
# 1. Postgres oracle (parity dump already loaded) + corpus
make oracle-up
uv run -- python -m scripts.bench.build_corpus            # writes scripts/bench/corpus.txt

# 2. ultravin (in-process engine): single-stream + batched, 60 s each
cargo run -p ultravin-core --example throughput --release -- scripts/bench/corpus.txt 60

# 3. NHTSA Postgres
uv run -- python -m scripts.bench.throughput postgres --seconds 60

# 4. NHTSA MSSQL: SQL Server 2022 (amd64 emulation) + restore the .bak
uv pip install pymssql                                     # optional client, not a project dep
make download-bak MONTH=2026_06
docker run -d --name ultravin-mssql --platform linux/amd64 \
  -e ACCEPT_EULA=Y -e MSSQL_SA_PASSWORD='Ultravin!2026' -e MSSQL_PID=Developer \
  -p 1433:1433 -v "$PWD/downloads:/bak:ro" mcr.microsoft.com/mssql/server:2022-latest
uv run -- python -m scripts.bench.mssql_setup --bak /bak/VPICList_lite_2026_06.bak
uv run -- python -m scripts.bench.throughput mssql --seconds 60

# 5. Regenerate assets/benchmark.svg from scripts/bench/results.json
uv run -- python -m scripts.bench.make_chart
```

## Acceptance targets vs baseline

| metric | target | baseline | status |
|---|---|---|---|
| warm single-decode | < 50 us | **4204 us** (4.20 ms) | far off |
| cold-start (fresh process, load + 1 decode) | < 5 ms | **29.3 ms** (in-proc, Rust) | far off |
| batch throughput (1 core) | > 100k VIN/s | **~325 VIN/s** | far off |
| artifact download (compressed) | <= ~21 MB | **20.0 MB** gzip-9 / 14.5 MB zstd-19 | within target |

The compute and load numbers are the levers W3 has to move (deserialize-to-owned
on load + an alloc-heavy hot path). The compressed artifact already lands under
21 MB; slimming the format (keys_regex only for bracket patterns, share interned
keys) is expected to shrink both download and load further.

## Phase 2 (batch + hot-path micro-opt)

Behaviour-preserving; parity fence green (`make check` 226 passed; live sweep
300/300 exact, 0 diverged). Changes: per-thread cache of compiled bracket-pattern
regexes (keyed by interned `keys_regex` id) so the hot path compiles each
distinct pattern once per worker instead of once per pattern per decode;
`decode_batch` now runs rayon over the shared immutable archive; the PyO3 batch
path releases the GIL during decode and only re-takes it to marshal dicts;
`dedup_cmp` compares bracket-stripped keys without allocating.

| metric | before | after |
|---|---|---|
| warm single-decode | 4204 us | **200 us** (21x) |
| batch, 1 core (criterion corpus) | 325 VIN/s | **4371 VIN/s** (13x) |
| batch, multi-core (10 cores, `--example batch`) | n/a | **~31k VIN/s** |

Notes: the regex-compile-per-pattern was the dominant warm cost; caching it is
the bulk of both wins. Multi-core scaling is limited by per-decode `String`
allocations (allocator contention). The Python `decode_batch` total is gated by
PyDict marshalling under the GIL (~2.4k VIN/s for 66.9k VINs), not decode
compute; the GIL-released parallel decode still cuts the compute portion. The
< 100k VIN/s/core acceptance target needs further per-decode compute work
(beyond batch parallelism) and is not reached here. Reproduce the multi-core
number: `cargo run -p ultravin-core --example batch --release`.

## Methodology

### Warm single-decode & batch (criterion)
`crates/ultravin-core/benches/decode.rs` (criterion, `harness = false`).
Run: `cargo bench -p ultravin-core --bench decode`.

- `warm_single`: `decode_with(db, "1HGCM82633A004352", fixed_clock, 2026)` with the
  db already loaded (`Db::embedded()`); fixed clock so the number is stable.
  → **time: [44.64 µs 44.76 µs 44.90 µs]**.
- `batch/corpus`: single-thread loop over the 223 valid 17-char VINs from the
  frozen parity corpus (`benches/vins.txt`).
  → **thrpt: [20.08k 20.29k 20.48k elem/s]** ≈ 20.3k VIN/s/core
  (10.99 ms median for 223 VINs).
- `warm_single_sysclock`: same as `warm_single` but via the system-clock
  `decode()` entry point → 45.6 µs (clock read is negligible).

### Cold-start
`crates/ultravin-core/examples/cold.rs` — a fresh process that times from `main`
entry to first decode complete (this captures the artifact load: `AlignedVec`
copy of the ~79 MB body + `rkyv::access` validation — zero-copy, no
deserialize-to-owned — then one decode).
Run: `cargo build -p ultravin-core --example cold --release && target/release/examples/cold <VIN>`.

- In-process (Rust engine, load + first decode), 11 fresh runs, median: **0.635 ms**
  (min 0.579, with one 4.38 ms cold-cache outlier).
- External wall-clock (process spawn + exit, `time`): below the `time` 10 ms
  resolution — the in-process load + decode is 0.6 ms; the rest is OS process setup.
- Python fresh process `uv run python -c "import ultravin; ultravin.decode(VIN)"`,
  median **~20 ms** wall-clock — essentially the interpreter + uv startup
  (`import ultravin` alone is also ~20 ms; the zero-copy engine load + decode adds
  under 1 ms on top). Python warm decode (second call, same process) ≈ **0.058 ms**,
  matching the Rust criterion warm number.

### Artifact size
`crates/ultravin-core/data/vpic.rkyv`.

| measure | bytes | MB |
|---|---|---|
| on-disk (uncompressed) | 82,923,600 | 79.1 |
| gzip -9 (wheel-download proxy) | 20,186,705 | 19.25 |
| zstd -19 | 14,867,212 | 14.18 |

### Postgres oracle baseline
`vpic.spvindecode('1HGCM82633A004352')` via psycopg over localhost TCP
(`host=localhost port=55432 db=vpic`), 25 calls after a warm-up, fetching all
rows: **median 61.5 ms** (min 55.4, max 74.0). This is full SQL round-trip incl.
client/server marshalling — the closest apples-to-apples "decode service" number.

### corgi (`@cardog/corgi`)
`npx -y @cardog/corgi decode <VIN>` runs but emits no decode output and `--help`
is empty; the package is a library that decodes against a separately-downloaded
SQLite/binary index, so a clean CLI timing wasn't obtainable here. **Published
numbers cited**: ~30 ms (v2, SQLite) / ~12 ms (v3, binary index), ~21 MB gzip
artifact (ISC, TypeScript).

### MS SQL
Now measured — see [Throughput (random corpus)](#throughput-random-corpus).
The unmodified `dbo.spVinDecode` from `vPICList_lite_2026_06.bak` restored into
SQL Server 2022 decodes **~22.5 VIN/s** (amd64 emulation on Apple Silicon).

## Reproduce

```sh
cargo bench -p ultravin-core --bench decode
cargo build -p ultravin-core --example cold --release
for i in $(seq 1 9); do target/release/examples/cold 1HGCM82633A004352; done | sort -n
ls -l crates/ultravin-core/data/vpic.rkyv
gzip -9 -c crates/ultravin-core/data/vpic.rkyv | wc -c
zstd -19 -c crates/ultravin-core/data/vpic.rkyv | wc -c
```

## Parity fence (must stay green after every change)
- `make check` — 226 passing (incl. 224-VIN frozen corpus, no oracle).
- `uv run -- python -m scripts.parity.sweep --sample 2 --limit 500` — 500/500
  exact, 0 diverged (live oracle).
