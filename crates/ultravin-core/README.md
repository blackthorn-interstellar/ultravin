# ultravin-core

Pure-Rust NHTSA vPIC VIN decoder: byte-for-byte parity with the official
`spVinDecode` stored procedure, ~0.04 ms per decode, fully offline. This is the
engine behind the [`ultravin`](https://pypi.org/project/ultravin/) Python
package; the repo, benchmarks and parity evidence live at
[github.com/blackthorn-interstellar/ultravin](https://github.com/blackthorn-interstellar/ultravin).

```toml
[dependencies]
ultravin-core = "1"
```

## The data artifact

The decoder runs against `vpic.rkyv` (~82 MB): the whole vPIC database, built
deterministically from NHTSA's monthly dump. It is too big for crates.io, so the
crate ships **without** it and every GitHub release attaches the exact file that
release was verified against. Download it from the release whose tag matches
your crate version and check its `blake3` against `artifact_blake3` in that
tag's `vpic/manifest.json`:

```bash
gh release download v1.1.0 --repo blackthorn-interstellar/ultravin --pattern vpic.rkyv
```

Then pick one of two ways to use it.

### Bake it in (recommended)

Point `ULTRAVIN_DATA` (absolute path) at the file when you build. The crate's
build script validates it and embeds it with `include_bytes!`, giving you the
same single self-contained binary the Python wheel is — no files at runtime.

```bash
ULTRAVIN_DATA=/abs/path/to/vpic.rkyv cargo build --release
```

Or persist it in your project's `.cargo/config.toml`:

```toml
[env]
ULTRAVIN_DATA = "/abs/path/to/vpic.rkyv"
```

```rust
let r = ultravin_core::decode("1HGCM82633A004352", None);
assert_eq!(r.model_year, Some(2003));
let make = r.elements.iter().find(|e| e.variable == "Make").unwrap();
assert_eq!(make.value, "HONDA");

// Parallel over rayon; output order matches input.
let vins = vec!["1HGCM82633A004352".to_string(), "5YJ3E1EA7KF317000".to_string()];
let results = ultravin_core::decode_batch(&vins, None);

// `variable -> value` only, no per-element provenance:
let flat = ultravin_core::decode_batch_flat(&vins, None);
```

Without `ULTRAVIN_DATA` the crate still compiles (an empty placeholder is
embedded so docs and CI work), but `decode` panics with a message saying so and
`Db::try_embedded()` returns `None`.

### Load it at runtime

Enable the `external-data` feature and memory-map the file yourself:

```toml
ultravin-core = { version = "1", features = ["external-data"] }
```

```rust
use ultravin_core::Db;

let db = Db::open(std::path::Path::new("/path/to/vpic.rkyv"))?;
let r = db.decode("1HGCM82633A004352", None);
let batch = db.decode_batch(&vins, None);
```

`Db::open` fully validates the file (it is untrusted input); do not modify it
while the `Db` lives. `Db::from_bytes` takes an owned buffer instead.

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
| `external-data` | off | `Db::open` (mmap an artifact at runtime; pulls in `memmap2`) |
| `parquet` | off | `parquet_io`: decode a parquet dataset to parquet, streaming by row group (pulls in `arrow`/`parquet`) |

## License

MIT. The vPIC data has its own provenance — see the repo's `NOTICE`.
