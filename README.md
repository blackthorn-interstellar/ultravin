# ultravin

<p align="center">
  <a href="https://github.com/blackthorn-interstellar/ultravin/actions/workflows/ci.yaml"><img src="https://img.shields.io/github/actions/workflow/status/blackthorn-interstellar/ultravin/ci.yaml?branch=master&label=CI&logo=github" alt="CI Status"></a>
  <a href="https://pypi.org/project/ultravin/"><img src="https://img.shields.io/pypi/v/ultravin?logo=pypi&logoColor=white" alt="PyPI Version"></a>
  <a href="https://github.com/blackthorn-interstellar/ultravin/blob/master/LICENSE"><img src="https://img.shields.io/github/license/blackthorn-interstellar/ultravin" alt="License"></a>
</p>

A fast, pure-Rust **NHTSA vPIC VIN decoder** shipped as a Python library. It's a
faithful port of vPIC's `spVinDecode` — byte-for-byte parity with the reference
Postgres procedure, verified across every decodable VIN. The vehicle database is
baked into the binary, so decoding is **fully offline**: no network, no database,
no data files.

## Install

```bash
pip install ultravin
```

Prebuilt wheels require **Python 3.12+**; nothing else (the data ships inside the
wheel). From source you'll also need a Rust toolchain:

```bash
pip install .          # or: make build-dev  (maturin develop into a venv)
```

## Use it from Python

```python
import ultravin

r = ultravin.decode("1HGCM82633A004352")

r["model_year"]         # 2003
r["wmi"]                # '1HG'
r["check_digit_valid"]  # True
r["error_codes"]        # [0]

# `elements` is the full decoded attribute list; index it by variable name:
attrs = {e["variable"]: e["value"] for e in r["elements"]}
attrs["Make"]           # 'HONDA'
attrs["Model"]          # 'Accord'
```

`decode(vin)` returns a `dict` with keys `vin`, `wmi`, `descriptor`,
`model_year`, `error_codes`, `check_digit_valid`, `corrected_vin`, and
`elements` — a list of per-attribute dicts (`group_name`, `variable`, `value`,
`element_id`, `source`, …).

Decode many at once:

```python
results = ultravin.decode_batch(["1HGCM82633A004352", "5YJ3E1EA7JF000000"])
```

## Use it from the command line

```bash
ultravin decode 1HGCM82633A004352          # human-readable table
ultravin decode 1HGCM82633A004352 --json   # full JSON
ultravin decode-batch vins.txt --json      # one VIN per line
ultravin version
```

## Benchmarks

Decoding the same VIN (`1HGCM82633A004352`), warm, on Apple Silicon. ultravin
runs in-process with the database embedded; the others are listed for reference.

| engine | warm decode | VIN/s (single stream) | vs ultravin | notes |
|---|---|---|---|---|
| **ultravin** (Rust, in-process) | **~0.20 ms** (203 µs) | **~4,900** | **1×** | data embedded; ~1.3 ms cold start; ~31k VIN/s batched on 10 cores |
| corgi v3 — `@cardog/corgi` (binary index) | ~12 ms | ~83 | ~59× slower | project's published figure |
| corgi v2 — `@cardog/corgi` 2.0.1 (SQLite) | ~30 ms | ~33 | ~148× slower | project's published figure |
| NHTSA `spVinDecode` (Postgres) | ~62 ms | ~16 | ~300× slower | full SQL round-trip over localhost |

ultravin decodes a VIN **~60× faster than corgi v3, ~150× faster than corgi v2,
and ~300× faster than the reference Postgres procedure** — with the whole vehicle
database embedded (≈19 MB compressed in the wheel) and no DB or network at
runtime.

VIN/s above is single-stream (one sequential caller, ≈ 1 ÷ warm decode);
ultravin additionally batches in parallel to **~31,000 VIN/s on 10 cores**.

The corgi figures are its project's own published numbers (not re-measured here);
ultravin and Postgres were measured locally. Full methodology, baselines, and
reproduction steps are in [BENCHMARKS.md](BENCHMARKS.md).
