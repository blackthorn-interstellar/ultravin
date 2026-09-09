# ultravin benchmarks

## Throughput (random corpus)

Measured September 9, 2026: the Rust engine decodes **76,581 VIN/s on one core** and **181,135 VIN/s in
four-core batches** over the random 5,000-VIN corpus. Both are medians of three
60-second windows after warming the entire corpus; the previous release and the
current build ran in alternating order on the same Apple M1 Max.

| engine | VIN/s | vs ultravin (1 core) |
|---|---|---|
| **ultravin** — batched, 4 cores | **181,135** | ~2.4× faster |
| **ultravin** — 1 core | **76,581** | 1× |
| corgi v3 (binary index, published) | ~83 | ~923× slower |
| corgi v2 (SQLite, published) | ~33 | ~2,321× slower |
| NHTSA MSSQL (`spVinDecode`, SQL Server) | 22.5 | ~3,404× slower |
| NHTSA Postgres (`spvindecode`) | 19.5 | ~3,927× slower |
| NHTSA vPIC web API (public rate limit) | ~10 | ~7,658× slower |

The paired baseline is the previous release, v2.1.2, which measured 45,990 VIN/s
single-core and 128,998 VIN/s batched: **1.67× and 1.40× improvements**,
respectively. Both builds produced identical output over all **1,862,306
full-result fingerprints** in the `2026_08` answer key. The
[September report](THROUGHPUT_2026_09.md) includes reproduction commands,
all samples, and the startup and memory tradeoffs.

The baseline reproduces v2.1.2's published 45,376 / 130,381 VIN/s to within about
1.4% single-core and 1.1% batched, so this round's before/after rates are directly
comparable with the figures that shipped with that release. An older 121,359 VIN/s
result used ten cores, not four.

Measurement notes:

- The host was shared. Current samples ranged from 75,860–77,093 VIN/s
  single-core and 180,951–182,849 VIN/s batched. These measure the Rust engine;
  Python dictionary construction and parquet I/O have separate costs.
- Batches use `RAYON_NUM_THREADS=4`; the machine has 10 physical cores. Batched
  throughput is about 2.4× the single-core rate — the single-core path gained
  more than the batch path this round, so the parallel multiple fell from 2.9×.
- The September 9 rerun used the release profile (`opt-level=3`, `lto="fat"`,
  `codegen-units=1`) without debug information, with the same artifact,
  allocator, lockfile and harness in both builds.
- corgi figures are derived from previously published latency (~12 ms v3,
  ~30 ms v2), not re-measured here. SQL-oracle figures are carried forward from
  earlier runs over the shared corpus; neither SQL engine was re-run in September.
- MSSQL ran SQL Server 2022 under amd64 emulation on Apple Silicon with the
  `2026_06` dump. That understates its performance on native hardware.
- The vPIC API row is the previously cited [~10 requests/s rate limit](https://cardog.app/blog/corgi-vin-decoder), not a
  decode-latency measurement. Ratios use the rounded comparison rates shown.

## Earlier latency and artifact measurements (2026-08-24)

Re-measured 2026-08-24 on the same Apple Silicon host, against the
`2026_08` artifact — no benchmark-motivated code change in this round, so the
gains are the accumulated effect of the API redesign and the newer toolchain,
not a targeted optimization. Criterion's own stored baseline scores the warm
decode 6.2% faster and the fixed-work batch 12.6% faster (both p = 0.00).

The prior round's context still applies: memoizing `valid_chars_in_key` (the E6
unused-position scan in `errors.rs` re-expanded every matched pattern key on
every pass, compiling a regex per bracket key) is byte-identical output —
verified by checksumming the full JSON of 7,900 decodes before and after — and
is worth ~1.4× on decode; the attributes shape is worth ~2× on the Python
`decode_batch` path (2.4× on 10 cores), which is marshalling-bound rather than
decode-bound. These August measurements used a contended host (load average
5–9); their methodology differs from the September throughput runs above.

These measurements predate the September optimization. Warm single-VIN latency
and the Criterion corpus were not re-measured in September; the latest measured
cold-start is **1.597 ms**. The tables below preserve the August results.

### Acceptance targets (August snapshot)

| metric | target | baseline | August 24 | met? |
|---|---|---|---|---|
| warm single-decode | < 50 us | 4204 us | **38.4 us** | **yes** |
| cold-start (fresh process, load + 1 decode) | < 5 ms | 29.3 ms | **0.753 ms** (median, n=11) | **yes** |
| batch throughput (1 core) | > 100k VIN/s | 325 VIN/s | **31.4k VIN/s** | no |
| artifact download (compressed) | <= ~21 MB | 20.0 MB gzip | **19.42 MB** gzip-9 / 14.26 MB zstd-19 | **yes** |

3 of 4 acceptance targets met (warm decode, cold-start, download). Single-core
batch (31.4k VIN/s, up from 27.4k) is the one remaining miss — the > 100k
VIN/s/core target needs a deeper compute rewrite that risks parity and was not
attempted. Cold-start is unchanged in substance (it is dominated by the artifact
load, which this round did not touch); the 0.670 → 0.753 ms difference is host
load and a slightly larger artifact, not a regression. The artifact grew with the
`2026_08` data refresh (82.9 → 83.4 MB on disk), which is why the compressed
sizes ticked up while staying well inside the 21 MB target.

### Where the remaining decode time goes

A stage ablation (each stage disabled in turn, fixed work, min-of-9 over the
5,000-VIN corpus) after the memo, for anyone tempted to add a "fast mode":

| stage disabled | speedup | what it costs |
|---|---|---|
| suggested-VIN repair (`errorcode`) | 1.20× | codes 2/3/4/5/14, Suggested VIN, Possible Values; 0.8% of VINs decode different attributes, because error weight is the top key of the best-pass scorer |
| 4th decode pass (ambiguous model year) | 1.42× | 13.3% of VINs get a different model year |
| vehicle specs / defaults / formula patterns / conversions | 1.01–1.09× each | the corresponding elements |

The memo already captured ~70% of what disabling the error machinery outright
would have bought, with identical output — which is why no fast-mode flag
exists. Everything below 1.1× is not worth an API surface.

### ultravin vs corgi vs Postgres (identical "decode one VIN" task)

| engine | single decode (warm) | cold-start | artifact (download) | notes |
|---|---|---|---|---|
| **ultravin** (Rust, in-proc) | **38.4 us** | **0.753 ms** | **19.42 MB** gzip | zero-copy embedded rkyv; multi-core batch ~94k VIN/s @4 cores |
| corgi v2 (SQLite, published) | ~30 ms | n/a | ~21 MB gzip | `@cardog/corgi` 2.0.1, ISC/TS |
| corgi v3 (binary index, published) | ~12 ms | n/a | ~21 MB gzip | blog/roadmap figure |
| Postgres oracle (`spvindecode`) | ~61.5 ms | n/a (server) | n/a | full SQL round-trip over localhost TCP |

ultravin warm decode is ~312x faster than corgi v3 (published), ~781x faster
than corgi v2, and ~1,601x faster than the Postgres round-trip oracle, on the
same VIN, with a smaller compressed download.

## Earlier throughput optimization rounds

Before the September optimization, four rounds improved throughput with
byte-identical output:

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
  60 s methodology, before/after measured together); then
- a per-thread memo of `valid_chars_in_key`, the key expansion the E6
  unused-position scan re-ran for every matched pattern key on every pass
  (compiling a fresh regex per bracket key), which raised single-core **19,331 →
  25,175 VIN/s**, and batch **111,496 → 121,359 VIN/s** measured across all 10
  cores as the earlier rounds were. Verified identical by checksumming the full
  JSON output of 7,900 decodes with and without the memo. These 10-core numbers
  are historical and use a different core count from the current headline table.

## Earlier Python output-shape measurements (`full=True`)

Past a certain point the Rust decode stops being the cost and **marshalling into
Python does**. The `full=True` shape returns ~41 elements per VIN as 15-key dicts
— about 615 `PyDict_SetItem` calls, all GIL-serial, after the parallel decode has
already finished. Measured at 22.6 ns per dict store, that is ~13.9 us/VIN, i.e.
most of the wall clock. The default `attributes` shape replaces the element list
with one `variable -> value` dict: ~41 stores instead of ~615.

These figures are why the cheap shape is the default: the columns below were
measured when `elements` still was, so read `full=True` as "what you used to get
for free" and `default` as the shape a caller now gets without asking.

Earlier measurements over the 5,000-VIN corpus, four cores, min-of-15, release
wheel. These were not re-run in September:

| path | `full=True` | default |
|---|---|---|
| `decode_batch` → `list[dict]` | 30.9 us/VIN | **15.7 us/VIN** (2.0×) |
| `decode_batch_json` → str | 16.4 us/VIN | **11.9 us/VIN** (1.4×) |
| realistic pipeline: decode → 40-field pydantic model | 41.2 us/VIN | **20.9 us/VIN** (2.0×) |

(On all 10 cores the same comparison reads 26.9 → 11.0 us/VIN, a 2.4× gain: the
more cores the parallel decode gets, the larger a share the GIL-serial
marshalling is, so the shape matters *more* on bigger machines, not less.)

The pipeline row is the one that matters: with `full=True` the caller pays us to
build 615 dict entries, then pays Python again to collapse them to the ~41 it
wanted. Pydantic validation of 40 fields is only 4.0 us of that total — the
decoder's output shape, not the consumer's validation, was the bottleneck.

Two things the default shape is *not*: it is not a decode-time saving (identical
work happens in Rust), and it is not lossless — it keeps `variable -> value` and
drops the 13 provenance columns, including `source`, which distinguishes a value
the VIN encodes from a vehicle-type default (53.6% of all rows are `Default`).
That is what `full=True` is for.

An earlier attempt at the same target — interning the `value`/`attribute_id`/
`source`/`keys` `PyString`s — was measured at **+2–3%** and reverted: CPython
already interns much of that text, and the cost is the dict stores themselves,
not the string allocation. Recorded here so nobody re-tries it.

### Reproduce the earlier 60-second comparison

For the current three-run, 20-second medians, use the
[September reproduction commands](THROUGHPUT_2026_09.md#reproduce).

```sh
# 1. Postgres oracle (parity dump already loaded) + corpus
make oracle-up
uv run -- python -m scripts.bench.build_corpus            # writes scripts/bench/corpus.txt

# 2. ultravin (in-process engine): single-stream + batched, 60 s each
RAYON_NUM_THREADS=4 cargo run -p ultravin --example throughput --release -- scripts/bench/corpus.txt 60

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

The MSSQL steps pin `2026_06` while the Postgres oracle runs the current dump.
That one-month skew is deliberate and irrelevant for throughput — the row counts
are near-identical, and the two engines are never compared row-for-row here (see
[ORACLE_TUNING.md](ORACLE_TUNING.md)).

## Earlier measurement methodology (August snapshot)

### Warm single-decode & batch (criterion)
`crates/ultravin/benches/decode.rs` (criterion, `harness = false`).
Run: `cargo bench -p ultravin --bench decode`.

- `warm_single`: `decode_with(db, "1HGCM82633A004352", fixed_clock, 2026)` with the
  db already loaded (`Db::embedded()`); fixed clock so the number is stable.
  → **time: [38.282 µs 38.410 µs 38.545 µs]**.
- `batch/corpus`: single-thread loop over the 223 valid 17-char VINs from the
  frozen parity corpus (`benches/vins.txt`).
  → **thrpt: [31.118k 31.411k 31.697k elem/s]** ≈ 31.4k VIN/s/core
  (7.10 ms median for 223 VINs).
- `warm_single_sysclock`: same as `warm_single` but via the system-clock
  `decode()` entry point → 38.8 µs (clock read is negligible).

### Cold-start
`crates/ultravin/examples/cold.rs` — a fresh process that times from `main`
entry to first decode complete (this captures the artifact load: `AlignedVec`
copy of the ~79 MB body + `rkyv::access` validation — zero-copy, no
deserialize-to-owned — then one decode).
Run: `cargo build -p ultravin --example cold --release && target/release/examples/cold <VIN>`.

- In-process (Rust engine, load + first decode), 11 fresh runs, median: **0.753 ms**
  (min 0.647, with one 6.76 ms cold-cache outlier on the first run).
- External wall-clock (process spawn + exit, `time`): below the `time` 10 ms
  resolution — the in-process load + decode is 0.6 ms; the rest is OS process setup.
- Python fresh process `uv run python -c "import ultravin; ultravin.decode(VIN)"`,
  median **~20 ms** wall-clock — essentially the interpreter + uv startup
  (`import ultravin` alone is also ~20 ms; the zero-copy engine load + decode adds
  under 1 ms on top). Python warm decode (second call, same process) ≈ **0.058 ms**,
  matching the Rust criterion warm number.

### Artifact size
`crates/ultravin/data/vpic.rkyv`.

| measure | bytes | MB |
|---|---|---|
| on-disk (uncompressed) | 83,372,856 | 79.5 |
| gzip -9 (wheel-download proxy) | 20,367,633 | 19.42 |
| zstd -19 | 14,954,343 | 14.26 |

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
cargo bench -p ultravin --bench decode
cargo build -p ultravin --example cold --release
for i in $(seq 1 9); do target/release/examples/cold 1HGCM82633A004352; done | sort -n
ls -l crates/ultravin/data/vpic.rkyv
gzip -9 -c crates/ultravin/data/vpic.rkyv | wc -c
zstd -19 -c crates/ultravin/data/vpic.rkyv | wc -c
```

## Parity fence (must stay green after every change)
- `make check` — full suite green, including the frozen parity corpus (no oracle).
- `uv run -- python -m scripts.parity.sweep --sample 2 --limit 500` — 500/500
  exact, 0 diverged (live oracle).
