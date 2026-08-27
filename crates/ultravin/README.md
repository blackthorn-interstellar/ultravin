# ultravin

<p align="center">
  <img src="https://raw.githubusercontent.com/blackthorn-interstellar/ultravin/master/assets/benchmark.svg" alt="VINs decoded per second: ultravin batched on 4 cores and single-core vs corgi v3, corgi v2, NHTSA MSSQL, NHTSA Postgres" width="640">
</p>

Pure-Rust NHTSA vPIC VIN decoder: byte-for-byte parity with the official
`spVinDecode` stored procedure, ~0.04 ms per decode, fully offline at runtime.
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

## Results

`DecodeResult` carries the header fields (`vin`, `wmi`, `descriptor`,
`model_year`, `error_codes`, `check_digit_valid`, `corrected_vin`) plus
`elements`: one `DecodedElement` per resolved attribute, the 15-column
`spVinDecode` row including provenance (`source`, `pattern_id`,
`vin_schema_id`, `keys`, …). `FlatResult` collapses that to `variable -> value`.
Both implement `serde::Serialize`.

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
