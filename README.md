# ultravin

<p align="center">
  <a href="https://github.com/blackthorn-interstellar/ultravin/actions/workflows/ci.yaml"><img src="https://img.shields.io/github/actions/workflow/status/blackthorn-interstellar/ultravin/ci.yaml?branch=master&label=CI&logo=github" alt="CI Status"></a>
  <a href="https://pypi.org/project/ultravin/"><img src="https://img.shields.io/pypi/v/ultravin?logo=pypi&logoColor=white" alt="PyPI Version"></a>
  <a href="https://github.com/blackthorn-interstellar/ultravin/blob/master/LICENSE"><img src="https://img.shields.io/github/license/blackthorn-interstellar/ultravin" alt="License"></a>
</p>

**An extremely fast, fully offline NHTSA vPIC VIN decoder, written in Rust.**

<p align="center">
  <img src="assets/benchmark.svg" alt="VINs decoded per second: ultravin 1,853,346 with automatic batching on 12 cores / 935,066 on 4 cores / 257,709 on 1 core vs corgi v3 83, corgi v2 33, NHTSA MSSQL 22.5, NHTSA Postgres 19.5" width="640"><br>
  <sub>VINs decoded per second — ultravin uses automatic batching over twenty million unique VINs.</sub>
</p>

- ⚡️ ~82,000× faster than NHTSA's own `spVinDecode` — ~1.85 million VIN/s on 12 cores
- 🦀 Pure Rust core, shipped as a Python library and a Rust crate
- 📦 The entire vPIC vehicle database baked into the wheel
- 🔌 Fully offline — no network, no database, no data files at runtime
- 🎯 Full-field vPIC parity, tested across decoding rules and their interactions — with documented upstream defects corrected ([accuracy policy](docs/ACCEPTANCE.md), [evidence](docs/KNOWN_DEVIATIONS.md))
- 🐍 Installable via `pip`, with a CLI and a library API
- 🗃️ Parquet in, parquet out — decodes a dataset of any size in the memory of one chunk

ultravin brings the complete `spVinDecode` algorithm into your process: vehicle
attributes, model-year resolution, VIN correction, errors, and provenance.
It checks every output field against NHTSA's unmodified Postgres procedure and
corrects documented defects in the upstream data and procedures. You get vPIC
fidelity with better answers on those defective cases, at **over 82,000× the
NHTSA SQL Server baseline** on 12 cores.
[Benchmarks and reproduction](docs/BENCHMARKS.md).

The complete vehicle database ships inside the binary. No network, no database
server, no runtime data files. Install it and decode.

## Getting Started

### Installation

```bash
uv add ultravin
```

Prebuilt wheels require **Python 3.10+** and nothing else — the data ships inside
the wheel.

### Usage

From Python:

```python
import ultravin

r = ultravin.decode("1HGCM82633A004352")

r["model_year"]  # 2003
r["wmi"]  # '1HG'
r["check_digit_valid"]  # True
r["error_codes"]  # [0]

# `attributes` is the decoded vehicle, one entry per vPIC variable:
r["attributes"]["Make"]  # 'HONDA'
r["attributes"]["Model"]  # 'Accord'
```

`decode(vin)` returns a `dict` with keys `vin`, `wmi`, `descriptor`,
`model_year`, `error_codes`, `check_digit_valid`, `corrected_vin`, and
`attributes` — a single `variable -> value` mapping. Values are `str`, except the
free-text note fields listed in `ultravin.MULTI_VALUED`, which are **always**
`list[str]`: those are the only vPIC elements allowed to repeat within one
decode, and each row is a separate note rather than a competing value.

Decode many at once with `decode_batch`:

```python
results = ultravin.decode_batch(["1HGCM82633A004352", "5YJ3E1EA7JF000000"])
```

If you already know a vehicle's model year, pass it — the same optional hint the
vPIC API calls `modelyear`. It matters for pre-2010 vehicles, where the VIN's
year character is ambiguous (`A` means 1980 *or* 2010): the hinted year gets its
own decode pass that competes against the VIN-derived one, and a hint that
contradicts the decoded year adds error code 12.

```python
ultravin.decode("1HGCM82633A004352", year=1995)  # decodes as a 1995
ultravin.decode_batch(vins, years=[2011, None, 1987])  # one entry per VIN
```

### Provenance: `full=True`

The default keeps the value and drops the provenance. If you need to know *where*
a value came from — `source`, over half of all rows being vehicle-type defaults
rather than something the VIN encodes — or the raw vPIC `attribute_id`, pass
`full=True`:

```python
r = ultravin.decode("1HGCM82633A004352", full=True)

next(e for e in r["elements"] if e["variable"] == "Make")
# {'group_name': 'General', 'variable': 'Make', 'value': 'HONDA', 'source': 'pattern - model', …}
```

`full=True` replaces `attributes` with `elements`, a list of per-attribute dicts
(`group_name`, `variable`, `value`, `element_id`, `attribute_id`, `source`,
`pattern_id`, …), and works the same on `decode_batch`, `decode_json` and
`decode_batch_json`. It is **~2× slower end to end**: decoding is not the
expensive part, and `elements` costs ~615 dict entries per VIN against the
default's ~41.

`ultravin.ELEMENTS` maps each variable name to its static metadata
(`element_id`, `group_name`, `data_type`, …). Pin to `element_id` if you need a
key that survives NHTSA renaming a variable between data releases.

From the command line:

```bash
ultravin decode 1HGCM82633A004352             # JSON object
ultravin decode 1HGCM82633A004352 --year 1995 # with a caller model-year hint
ultravin decode 1HGCM82633A004352 --full      # with per-element provenance
ultravin decode-batch vins.txt                # one VIN per line -> JSON array
ultravin decode-batch - --jsonl < vins.txt    # stdin -> streaming JSON Lines
ultravin decode-batch vins.txt --jsonl --batch-size 1000
ultravin info                                # decoder version + data identity as JSON
ultravin version
```

`decode-batch` accepts `VIN,year` on each input line. JSONL mode processes
bounded chunks and emits complete JSON objects in input order, ready for shell
pipelines. Its automatic default adapts under an 8 MiB working-buffer target;
pass `--batch-size N` to pin a measured size or `--batch-memory-mb N` to change
the automatic target. If a later input line is malformed, the command exits with
an error; already-emitted lines remain valid results.

### Reproducible results

Every batch automatically captures one clock for the whole job. That includes
Python and Rust batches, CLI input loading, every JSONL chunk, and every file in
a Parquet stream. A job that spans a date or year boundary keeps using its
starting instant.

Pin the package version and freeze the decode clock when a job needs to produce
the same answers on a later date:

```python
from datetime import datetime, timezone

as_of = datetime(2026, 9, 1, tzinfo=timezone.utc)
r = ultravin.decode("1HGCM82633A004352", now=as_of)
results = ultravin.decode_batch(vins, now=as_of)
identity = ultravin.provenance()
# data_month, artifact_blake3, decoder_version
```

`now=` also works on the JSON APIs and `decode_stream`. It controls publication
dates and year resolution; `year=` remains the vehicle's model-year hint.
Naive datetimes mean UTC. Each stream captures one clock reading for the whole
job, and its Arrow/Parquet metadata records the data identity and decode clock:
`ultravin.data_month`, `ultravin.artifact_blake3`, `ultravin.decoder_version`,
and `ultravin.now_micros` (Unix epoch microseconds).

## Rust

The engine is its own crate, [`ultravin`](https://crates.io/crates/ultravin):
`cargo add ultravin`, then

```rust
let r = ultravin::decode("1HGCM82633A004352", None);
assert_eq!(r.model_year, Some(2003));
```

The 83 MB vPIC database is too big for crates.io, so the first build fetches it
from the matching GitHub release, verifies it, caches it per machine and bakes it
into your binary — runtime stays fully offline. Offline builds and runtime
loading: [crates/ultravin/README.md](crates/ultravin/README.md).

## Datasets

For bulk work there is `decode_stream` — a stream of decoded Arrow batches,
without a single row ever becoming a Python object:

```python
import ultravin

rows = ultravin.decode_stream("registrations.parquet").to_parquet("decoded.parquet")

rows  # 4812004 — the rows written, not the rows themselves
```

Parquet output replaces its destination only after every batch and the footer
have been written successfully. A failed decode leaves an existing output intact.

The source is a parquet file, a directory of `*.parquet` read in sorted order, or
anything speaking the Arrow C data interface — so the same call takes a pandas
`DataFrame` (pandas ≥ 2.2, with pyarrow installed), a pyarrow `Table`, a polars
`DataFrame`, a duckdb result, or a `RecordBatchReader`. A `DecodeStream` is
itself an Arrow source, which is what lets it hand the decode straight to
whatever you already use:

```python
import pandas as pd, polars as pl, pyarrow as pa, duckdb

df = pd.DataFrame({"vin": ["1HGCM82633A004352", "5YJ3E1EA7KF328931"]})

pl.DataFrame(ultravin.decode_stream(df))  # -> polars
pa.table(ultravin.decode_stream(df))  # -> pyarrow
ultravin.decode_stream(df).to_pandas()  # -> pandas (needs pandas + pyarrow)

stream = ultravin.decode_stream(df)  # duckdb resolves the name from scope
duckdb.sql("select Make, count(*) from stream group by 1")
```

Each stream is single-use — note the fresh `decode_stream(...)` on every line
above. It pulls from a source that has already moved on, so a second consumer
would get a silently truncated result; consuming one twice raises `RuntimeError`
rather than handing back a short answer. Call `decode_stream` again to re-read.

### Picking columns

`columns=` takes vPIC variable names, `element_id`s, or both together; omit it for
every publicly decodable element:

```python
ultravin.decode_stream("registrations.parquet", columns=["Make", "Model", 13])
```

**Pin to `element_id` for anything long-lived.** The id is the one key NHTSA does
not rename between monthly data releases.

### Column naming and schema drift

Naming output columns after vPIC variables means a data refresh can silently
change your table's shape — NHTSA renames variables, and `Displacement (L)`
becoming something else takes every downstream query with it. `column_names="id"`
labels each projected column `attr_<element_id>` instead, which never moves:

```python
ultravin.decode_stream(src, columns=[26, 13], column_names="id")
# -> vin, decoded_model_year, attr_26, attr_13
```

Passthrough columns (the source VIN column, the source year column, and
`decoded_model_year`) keep their own names in both modes; only the projection is
renamed. The default is `"variable"` — reach for `"id"` when the output feeds a
persisted schema rather than a human.

You never lose the other name. Every projected column carries **both** keys as
Arrow field metadata, in both modes, and they survive the parquet round-trip:

```python
table = pa.table(ultravin.decode_stream(src, columns=[26, 13], column_names="id"))
{f.name: dict(f.metadata) for f in table.schema if f.metadata}
# {'attr_26': {b'element_id': b'26', b'variable': b'Make'},
#  'attr_13': {b'element_id': b'13', b'variable': b'Displacement (L)'}}
```

### Columns and layout

The VIN column is found by name (`vin`, case-insensitively) and then, for a
parquet source, by sniffing the leading rows — as is the optional caller-year
column (`year`, `model_year`, …); pass `vin_column=`/`year_column=` to name them
outright. Any text encoding works: `Utf8`, `LargeUtf8`, `Utf8View`, or the
dictionary a pandas categorical arrives as.

The output holds the VIN and caller year passed through, then
`decoded_model_year` (named so it cannot collide with an input column called
`model_year`), then one column per projected element — string, `int64` or
`float64` following vPIC's own `data_type`, with an empty value written as null.
Row order and row count always equal the input's: an undecodable VIN is a row of
nulls, never a raise and never a dropped row.

By default, `batch_size="auto"` adapts Parquet and Arrow work chunks to measured
throughput under a 64 MiB working-buffer budget. A shipped predictor uses the
worker count and a short single-core measurement on real input rows to choose
the starting size; runtime feedback then refines it. See the
[predictor and heatmaps](docs/BATCH_PREDICTOR.md), or call
`ultravin.predict_batch_size(workers=12, single_core_rows_per_second=100_000)`
to inspect an estimate. Change the working-buffer target with
`batch_memory_mb=`. It is a budget for buffers ultravin builds for the current
batch, not a process-RSS limit, and it does not include buffers retained by an
upstream Arrow producer. Adaptation respects `RAYON_NUM_THREADS`; it never
changes the process-global worker pool.

Pass an integer for fixed-size Parquet chunks. With an Arrow source an explicit
integer preserves the producer's batches and only sets the parquet row-group
size of `to_parquet`. In either mode the GIL is released while Rust decodes, and
memory stays bounded independently of total source rows. Reading and writing
parquet is the same Rust as the decoding, so this path needs no pyarrow and no
other install; only the pyarrow/polars/pandas hand-offs need those libraries.

From the command line:

```bash
ultravin decode-parquet registrations.parquet decoded.parquet --columns Make,Model
ultravin decode-parquet parts/ decoded.parquet --columns 26,28 --vin-column chassis_no
ultravin decode-parquet registrations.parquet decoded.parquet --column-names id
```

## Benchmarks

How many VINs each engine decodes **per second**, measured against the NHTSA
MSSQL `spVinDecode` baseline of 22.5 VIN/s.

| engine | VIN/s | vs NHTSA MSSQL |
|---|---:|---:|
| **ultravin** — automatic batches, 12 cores | **1,853,346** | ~82,371× faster |
| **ultravin** — automatic batches, 4 cores | **935,066** | ~41,558× faster |
| **ultravin** — automatic batches, 1 core | **257,709** | ~11,454× faster |
| corgi v3 — `@cardog/corgi` (binary index) | ~83 | ~3.7× faster |
| corgi v2 — `@cardog/corgi` 2.0.1 (SQLite) | ~33 | ~1.5× faster |
| NHTSA MSSQL — `spVinDecode` (SQL Server) | 22.5 | 1× (baseline) |
| NHTSA Postgres — `spvindecode` | 19.5 | ~1.2× slower |
| NHTSA vPIC web API — public rate limit | ~10 | ~2.3× slower |

Commit `5750746`, September 23, 2026, Apple M2 Max (eight performance and four
efficiency cores), 20,000,000 unique synthetic VINs, automatic batching on one,
four, or twelve workers, medians of two fresh-process trials. Across worker
counts ultravin scales to ~3.6× and ~7.2× its own single-core rate.

ultravin runs in-process with the database embedded — no server, no round-trip.
The corgi rows are derived from that project's published per-VIN latency, and
the vPIC web API row is its published request rate limit rather than a decode
time; neither was re-measured here.
[Methodology, inputs, caveats, reproduction, and prior sessions](docs/BENCHMARKS.md).

## Documentation

- [Vision](docs/VISION.md) — what this is, and what it deliberately is not
- [Benchmarks](docs/BENCHMARKS.md) — the numbers, the methodology, how to reproduce them
- [End-to-end performance](docs/PERFORMANCE_2026_09_13.md) — throughput, startup, and memory across all output paths
- [Performance improvements](PERFORMANCE_IMPROVEMENTS.md) — the optimization history, measured gains, tradeoffs, and rejected experiments
- [Acceptance](docs/ACCEPTANCE.md) — the parity policy: what counts as passing, how a divergence is adjudicated
- [Known deviations](docs/KNOWN_DEVIATIONS.md) — the vPIC defects ultravin does not reproduce, with evidence
- [Corpora](docs/CORPUS.md) — `generate`, `cover_vins`, `sweep`: hitting every decode behaviour with the fewest VINs
- [Data refresh](docs/DATA_REFRESH.md) — how the monthly NHTSA dump is integrated behind parity gates
- [Release](docs/RELEASE.md) — tags, wheels, the embedded artifact, crates.io

## License

MIT. The embedded NHTSA vPIC data has its own provenance — see [NOTICE](NOTICE).
