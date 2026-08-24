# ultravin

<p align="center">
  <a href="https://github.com/blackthorn-interstellar/ultravin/actions/workflows/ci.yaml"><img src="https://img.shields.io/github/actions/workflow/status/blackthorn-interstellar/ultravin/ci.yaml?branch=master&label=CI&logo=github" alt="CI Status"></a>
  <a href="https://pypi.org/project/ultravin/"><img src="https://img.shields.io/pypi/v/ultravin?logo=pypi&logoColor=white" alt="PyPI Version"></a>
  <a href="https://github.com/blackthorn-interstellar/ultravin/blob/master/LICENSE"><img src="https://img.shields.io/github/license/blackthorn-interstellar/ultravin" alt="License"></a>
</p>

**An extremely fast, fully offline NHTSA vPIC VIN decoder, written in Rust.**

<p align="center">
  <img src="assets/benchmark.svg" alt="VINs decoded per second: ultravin 94,030 batched on 4 cores / 29,568 single-core vs corgi v3 83, corgi v2 33, NHTSA MSSQL 22.5, NHTSA Postgres 19.5" width="640"><br>
  <sub>VINs decoded per second over a random corpus, single sequential caller — ultravin also batches across cores.</sub>
</p>

- ⚡️ ~0.038 ms per decode — orders of magnitude faster than the NHTSA SQL procedures (corgi, Postgres, MSSQL)
- 🦀 Pure Rust core, shipped as a Python library and a Rust crate
- 📦 The entire vPIC vehicle database baked into the wheel
- 🔌 Fully offline — no network, no database, no data files at runtime
- 🎯 Byte-for-byte parity with vPIC's `spVinDecode`, verified across every decodable VIN — except documented vPIC defects, which ultravin deliberately does not reproduce ([the registry](scripts/known_problems.json), [evidence](docs/KNOWN_DEVIATIONS.md))
- 🐍 Installable via `pip`, with a CLI and a library API
- 🧵 Batches in parallel to ~94,000 VIN/s on 4 cores
- 🗃️ Parquet in, parquet out — decodes a dataset of any size in the memory of one chunk

ultravin is a faithful port of NHTSA's `spVinDecode` — the SQL procedure behind
vPIC — reimplemented in Rust and verified against the reference Postgres
implementation. Because the vehicle database ships inside the binary, decoding
needs no network, no database server, and no data files. Install it and decode.

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

r["elements"][0]  # {'variable': 'Make', 'value': 'HONDA', 'source': 'Manu. Name', …}
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

From the command line — every command emits JSON:

```bash
ultravin decode 1HGCM82633A004352             # JSON object
ultravin decode 1HGCM82633A004352 --year 1995 # with a caller model-year hint
ultravin decode 1HGCM82633A004352 --full      # with per-element provenance
ultravin decode-batch vins.txt                # one VIN per line -> JSON array
ultravin version
```

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
ultravin.decode_stream(df).to_pandas()  # -> pandas (needs pyarrow)

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

For a parquet source, rows stream through in `batch_size`-row chunks with the GIL
released, so peak memory is one chunk no matter how large the source is. For an
Arrow source the producer decides the input chunking, and `batch_size` only sets
the parquet row-group size of `to_parquet`. Reading and writing parquet is the
same Rust as the decoding, so the parquet path needs no pyarrow and no other
install; only the pyarrow/polars/pandas hand-offs need those libraries.

From the command line:

```bash
ultravin decode-parquet registrations.parquet decoded.parquet --columns Make,Model
ultravin decode-parquet parts/ decoded.parquet --columns 26,28 --vin-column chassis_no
ultravin decode-parquet registrations.parquet decoded.parquet --column-names id
```

## Benchmarks

How many VINs each engine decodes **per second**, single sequential caller,
over an identical random corpus of 5,000 valid VINs (measured over 60 s; Apple
Silicon, batched across 4 cores):

| engine | VIN/s | vs ultravin (1 core) |
|---|---|---|
| **ultravin** — batched, 4 cores | **94,030** | ~3.2× faster |
| **ultravin** — 1 core | **29,568** | 1× |
| corgi v3 — `@cardog/corgi` (binary index) | ~83 | ~356× slower |
| corgi v2 — `@cardog/corgi` 2.0.1 (SQLite) | ~33 | ~896× slower |
| NHTSA MSSQL — `spVinDecode` (SQL Server) | 22.5 | ~1,314× slower |
| NHTSA Postgres — `spvindecode` | 19.5 | ~1,516× slower |
| NHTSA vPIC web API — public rate limit | ~10 | ~2,957× slower |

ultravin runs in-process with the database embedded — no server, no round-trip.
The corgi figures are derived from its project's published per-VIN latency
(~12 ms v3 / ~30 ms v2, not re-measured here). The NHTSA Postgres and MSSQL
oracles run the **unmodified** `spVinDecode` over localhost; MSSQL is SQL Server
under amd64 emulation on Apple Silicon, so its number understates native
hardware — ultravin is still ~1,314× faster. The NHTSA vPIC web API row is its
[published](https://cardog.app/blog/corgi-vin-decoder) ~10 req/s rate limit, not
a decode time — a hard ceiling regardless of hardware. Methodology and
reproduction: [docs/BENCHMARKS.md](docs/BENCHMARKS.md).

## License

MIT. The embedded NHTSA vPIC data has its own provenance — see [NOTICE](NOTICE).
