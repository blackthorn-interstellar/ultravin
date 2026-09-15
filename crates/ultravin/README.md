# ultravin

<p align="center">
  <img src="https://raw.githubusercontent.com/blackthorn-interstellar/ultravin/master/assets/benchmark.svg" alt="VINs decoded per second: ultravin with automatic batching on 12, 4 and 1 cores vs corgi v3, corgi v2, NHTSA MSSQL, NHTSA Postgres" width="640">
</p>

Pure-Rust NHTSA vPIC VIN decoder: full-field `spVinDecode` parity with documented
upstream defects corrected, ~164,000 VIN/s on one core, fully offline at runtime.
Same engine as the [`ultravin`](https://pypi.org/project/ultravin/) Python
package; the repo, benchmarks and parity evidence live at
[github.com/blackthorn-interstellar/ultravin](https://github.com/blackthorn-interstellar/ultravin).

```bash
cargo add ultravin
```

```rust
let r = ultravin::decode("1HGCM82633A004352", None);
assert_eq!(r.model_year, Some(2003));
let make = r.elements.iter().find(|e| e.variable == "Make").unwrap();
assert_eq!(make.value, "HONDA");

// Parallel over rayon; output order matches input.
let vins = vec!["1HGCM82633A004352".to_string(), "5YJ3E1EA7KF317000".to_string()];
let results = ultravin::decode_batch(&vins, None);

// `variable -> value` only, no per-element provenance:
let flat = ultravin::decode_batch_flat(&vins, None);
```

For sustained full-result processing, `decode_native_stream` automatically
chooses a batch size and reusable result slots per worker:

```rust
let prediction = ultravin::decode_native_stream(&vins, None, |batch| {
    for result in batch.iter() {
        println!("{}: {:?}", result.vin, result.model_year);
    }
})?;
```

Workers decode whole batches. Callbacks receive full results in input order;
the owning worker clears and reuses each slot after the callback returns.
One captured clock covers the entire job, including calibration.
`decode_native_stream_auto_at` accepts a fixed clock and `NativeAutoOptions`;
`decode_native_stream_at` accepts an explicit `NativeStreamConfig` to bypass
automatic selection. The default working-output budget is 512 MiB, excluding
the input and database. Existing owned batch APIs retain their behavior.

## The data

The decoder runs against `vpic.rkyv` (~83 MB): the whole vPIC database, built
deterministically from NHTSA's monthly dump. It is too big for crates.io, so the
**first build** fetches it from this version's GitHub release, checks its blake3
against the pin shipped in the crate (`data/manifest.json`), validates it, caches
it in `~/.cache/ultravin` (override with `ULTRAVIN_CACHE_DIR`), and bakes it into
your binary with `include_bytes!`. One download per machine; the executable you
ship is self-contained and never touches the network.

### Offline or reproducible builds

Either supply the file yourself — the build script validates and embeds it and
attempts no download:

```bash
# v<this crate version> — the release whose asset matches the pin in data/manifest.json
gh release download v$VERSION --repo blackthorn-interstellar/ultravin --pattern vpic.rkyv
ULTRAVIN_DATA=/abs/path/to/vpic.rkyv cargo build --release
```

(check its blake3 against `artifact_blake3` in that tag's `vpic/manifest.json`;
`[env] ULTRAVIN_DATA = "..."` in `.cargo/config.toml` persists it), or turn the
download off and load the file at runtime instead:

```toml
ultravin = { version = "2", default-features = false, features = ["external-data"] }
```

```rust
use ultravin::Db;

let db = Db::open(std::path::Path::new("/path/to/vpic.rkyv"))?;
let r = db.decode("1HGCM82633A004352", None);
let batch = db.decode_batch(&vins, None);
```

`Db::open` fully validates the file (it is untrusted input); do not modify it
while the `Db` lives. `Db::from_bytes` takes an owned buffer instead. Without
any artifact the crate still compiles (an empty placeholder is embedded so docs
and CI work), but `decode` panics with a message saying so and
`Db::try_embedded()` returns `None`.

### Data identity and decode clock

```rust
let identity = ultravin::provenance();
println!("{} {}", identity.data_month, identity.artifact_blake3);

// Freeze publication-date and year resolution for a reproducible rerun.
let as_of = 1_788_220_800_000_000; // 2026-09-01 00:00:00 UTC, epoch microseconds
let result = ultravin::decode_at("1HGCM82633A004352", None, as_of);
```

`Db::provenance()` identifies a database loaded at runtime, and `Db::decode_at`
and `Db::decode_batch_at` use an explicit clock with that database. A custom
artifact whose digest differs from the release pin reports its data month as
`"unknown"`; its digest still identifies the exact data.

## Results

`DecodeResult` carries the header fields (`vin`, `wmi`, `descriptor`,
`model_year`, `error_codes`, `check_digit_valid`, `corrected_vin`) plus
`elements`: one `DecodedElement` per resolved attribute, the 15-column
`spVinDecode` row including provenance (`source`, `pattern_id`,
`vin_schema_id`, `keys`, …). `FlatResult` collapses that to `variable -> value`.
Both implement `serde::Serialize`.

Full results borrow database text wherever possible. `DecodedElement.value`,
`attribute_id`, `keys`, and `source` are `Cow<'a, str>`: database-backed strings
and cached correction text are borrowed; other computed or scrubbed text remains owned. This removes
per-element copies and their later cleanup. The backing `Db` must outlive the
result; the embedded database has a static lifetime.

Each database lazily caches correction text by error-code combination and note
flags. Cache keys never contain VINs or timestamps, so more input rows do not
grow the cache beyond the fixed set of supported combinations. Projection also
reuses a bounded scratch buffer per worker.

For native batches processed by reference, `decode_batch_managed` and
`decode_batch_managed_at` return `BatchResults<DecodeResult>` with indexing,
iteration, and serialization. Dropping a large batch completes its cleanup
across decoder workers before returning. The corresponding flat functions and
`Db` methods support the same ownership model.

Existing `decode_batch` functions still return `Vec`. Calling `.into_vec()` or
consuming a managed batch with `.into_iter()` transfers cleanup responsibility
to the returned vector or iterator; iterate over `&batch` to retain managed
cleanup.

Rust callers migrating the first three fields from `String` can read them with
`.as_ref()`, construct them with `"text".into()`, or obtain an owned `String`
with `.into_owned()`. Serialized output and Python dictionaries retain the same
fields and values.

The optional second argument is the caller-supplied model year (`@year` in the
procedure): a hint that competes in the best-pass scoring and flags error 12
when it contradicts the VIN.

## Features

| feature | default | what it adds |
|---|---|---|
| `download-data` | **on** | build.rs fetches, verifies and embeds this version's `vpic.rkyv` when none is supplied (pulls in `ureq` as a build dependency) |
| `external-data` | off | `Db::open` (mmap an artifact at runtime; pulls in `memmap2`) |
| `arrow` | off | `arrow_io`: `RecordBatch` in, `RecordBatch` out, no file I/O — the door every Arrow source enters through (pulls in `arrow-array`/`arrow-cast`/`arrow-schema`) |
| `parquet` | off | `parquet_io`: decode a parquet file or directory to parquet, streaming by row group (implies `arrow`; pulls in `parquet`) |

## License

MIT. The vPIC data has its own provenance — see the repo's `NOTICE`.
