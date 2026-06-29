# ultravin

<p align="center">
  <a href="https://github.com/brycedrennan/ultravin/actions/workflows/ci.yaml"><img src="https://img.shields.io/github/actions/workflow/status/brycedrennan/ultravin/ci.yaml?branch=master&label=CI&logo=github" alt="CI Status"></a>
  <a href="https://pypi.org/project/ultravin/"><img src="https://img.shields.io/pypi/v/ultravin?logo=pypi&logoColor=white" alt="PyPI Version"></a>
  <a href="https://github.com/brycedrennan/ultravin/blob/master/LICENSE"><img src="https://img.shields.io/github/license/brycedrennan/ultravin" alt="License"></a>
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
