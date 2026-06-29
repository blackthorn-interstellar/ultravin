# ultravin benchmarks

W3 baseline. Numbers below are the **starting point** (before the zero-copy /
artifact-slimming optimizations). They are deliberately honest and reproducible.

Host: Apple Silicon (aarch64-apple-darwin), `cargo 1.90`, release profile
(`opt-level=3`, `lto="thin"`, `codegen-units=1`). Artifact:
`crates/ultravin-core/data/vpic.rkyv` (gitignored build product).

## Phase 3 final results (verified)

Independently re-measured with the phase-1 harness after the zero-copy load +
artifact-slimming work. Parity fence green: `make check` 226 passed; live sweep
`--sample 2 --limit 500` = 500/500 exact, 0 diverged.

### Acceptance targets (final)

| metric | target | baseline | final | met? |
|---|---|---|---|---|
| warm single-decode | < 50 us | 4204 us | **202.8 us** | no |
| cold-start (fresh process, load + 1 decode) | < 5 ms | 29.3 ms | **1.26 ms** (median, n=11) | **yes** |
| batch throughput (1 core) | > 100k VIN/s | 325 VIN/s | **4342 VIN/s** | no |
| artifact download (compressed) | <= ~21 MB | 20.0 MB gzip | **19.25 MB** gzip-9 / 14.18 MB zstd-19 | **yes** |

2 of 4 acceptance targets met (cold-start, download). Cold-start is the
headline win: switching `db.rs` from deserialize-to-owned (~79 MB) to true
zero-copy archived access (`rkyv::access` once, references into the buffer,
`binary_search`/`partition_point` over archived little-endian slices) cut
cold-start 23x to 1.26 ms. Warm decode (202 us) and single-core batch
(4342 VIN/s) remain bounded by per-decode `String` allocation and regex/LIKE
matching in the hot path; closing the gap to 50 us / 100k VIN/s/core needs a
deeper compute rewrite that risks parity and was not attempted.

### ultravin vs corgi vs Postgres (identical "decode one VIN" task)

| engine | single decode (warm) | cold-start | artifact (download) | notes |
|---|---|---|---|---|
| **ultravin** (Rust, in-proc) | **202.8 us** | **1.26 ms** | **19.25 MB** gzip | zero-copy embedded rkyv; multi-core batch 31.3k VIN/s @10 cores |
| corgi v2 (SQLite, published) | ~30 ms | n/a | ~21 MB gzip | `@cardog/corgi` 2.0.1, ISC/TS |
| corgi v3 (binary index, published) | ~12 ms | n/a | ~21 MB gzip | blog/roadmap figure |
| Postgres oracle (`spvindecode`) | ~61.5 ms | n/a (server) | n/a | full SQL round-trip over localhost TCP |

ultravin warm decode is ~59x faster than corgi v3 (published), ~148x faster
than corgi v2, and ~300x faster than the Postgres round-trip oracle, on the
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
| **ultravin** — batched, 10 cores | **54,801** | ~3.8× faster |
| **ultravin** — 1 core | **14,291** | 1× |
| corgi v3 (binary index, published) | ~83 | ~172× slower |
| corgi v2 (SQLite, published) | ~33 | ~433× slower |
| NHTSA MSSQL (`spVinDecode`, SQL Server) | 22.5 | ~635× slower |
| NHTSA Postgres (`spvindecode`) | 19.5 | ~733× slower |
| NHTSA vPIC web API (public rate limit) | ~10 | ~1,430× slower |

These figures are after two rounds of hot-path work, both byte-identical output:

- the per-thread `(wmi, model_year)` memoization of the suggested-VIN correction
  charset (`valid_charset`), which removed ~60% of the hot path: single-core
  3,756 → 10,339 VIN/s, batch 22,338 → 47,990 VIN/s; then
- an allocation + matching rewrite (custom fixed-length token matcher for
  bracket keys in place of the regex engine, `from_utf8_unchecked` arena reads
  validated once at load, `Cow<'static, str>` decode items, an O(1) `element_by_id`
  index, FxHash for the integer-keyed sets, interned PyDict keys), which raised
  single-core **9,717 → 14,291 VIN/s** and batch **43,608 → 54,801 VIN/s**
  (same host, same 60 s methodology, before/after measured together).

Notes on honesty:
- **ultravin** is the in-process Rust engine (system-clock path). The single-core
  number (14,291 VIN/s) is over a varied corpus, not one repeated VIN; batched
  (54,801 VIN/s) scales ~3.8× across 10 cores — sublinear because varied patterns
  and shared memory bandwidth bound the per-thread matcher/charset caches (the
  faster the per-core decode, the sooner memory bandwidth caps the parallel run,
  so this ratio fell from ~4.6× as single-core throughput rose).
- **corgi v2/v3** are *derived* from the project's published per-VIN latency
  (~30 ms / ~12 ms → ~33 / ~83 VIN/s), not re-measured here.
- **NHTSA Postgres** runs the unmodified `vpic.spvindecode` over localhost TCP
  (psycopg). The varied corpus averages ~51 ms/VIN vs the ~61.5 ms single-VIN
  baseline — both in the same ballpark.
- **NHTSA MSSQL** runs the unmodified `dbo.spVinDecode` shipped in
  `vPICList_lite_2026_06.bak`, restored into SQL Server 2022. On Apple Silicon
  that image only runs under **amd64 emulation (Rosetta)**, so its throughput
  understates native SQL Server hardware — yet ultravin is still ~635× faster.
- **NHTSA vPIC web API** is not a decode measurement: it's the public API's
  ~10 requests/s rate limit ([corgi blog](https://cardog.app/blog/corgi-vin-decoder)),
  the practical ceiling for anyone decoding against the hosted service. It's a
  hard cap regardless of hardware, included for context.

All measured on the same quiescent Apple Silicon host (10 cores); the Postgres
parity campaign was idle during measurement. The ultravin rows were re-measured
after the allocation/matching rewrite (the SQL-oracle rows are round-trip-bound
and carried over from the prior run — they are dominated by query execution, not
client CPU, so they are effectively host-independent at this scale).

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
  → **time: [4.188 ms 4.204 ms 4.220 ms]**.
- `batch/corpus`: single-thread loop over the 223 valid 17-char VINs from the
  frozen parity corpus (`benches/vins.txt`).
  → **thrpt: [324.4 325.6 326.7 elem/s]** ≈ 325 VIN/s/core
  (684.8 ms median for 223 VINs).
- `warm_single_sysclock`: same as `warm_single` but via the system-clock
  `decode()` entry point → 4.19 ms (clock read is negligible).

### Cold-start
`crates/ultravin-core/examples/cold.rs` — a fresh process that times from `main`
entry to first decode complete (this captures the artifact load: `AlignedVec`
copy of the ~79 MB body + `rkyv::access` validation + **deserialize-to-owned
`VpicData`**, then one decode).
Run: `cargo build -p ultravin-core --example cold --release && target/release/examples/cold <VIN>`.

- In-process (Rust engine, load + first decode), 9 fresh runs, median: **29.3 ms**
  (min 28.3, with one 95 ms cold-cache outlier).
- External wall-clock (process spawn + exit, `time`/subprocess), median: **36.0 ms**.
- Python fresh process `python -c "import ultravin; ultravin.decode(VIN)"`, median
  **54.4 ms** (interpreter + import-only baseline ≈ 19.4 ms; remainder is engine
  load + decode). Python warm decode (second call, same process) ≈ 4.6 ms,
  matching the Rust criterion warm number.

### Artifact size
`crates/ultravin-core/data/vpic.rkyv`.

| measure | bytes | MB |
|---|---|---|
| on-disk (uncompressed) | 83,271,240 | 79.4 |
| gzip -9 (wheel-download proxy) | 21,004,606 | 20.0 |
| zstd -19 | 15,195,907 | 14.5 |

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
