# ultravin

**A pure-Rust reimplementation of NHTSA's vPIC VIN decoder that produces byte-identical results to `spVinDecode` — shipped as one Python wheel that is both a CLI and a library, and as a Rust crate. Clear, fast, boring.**

## The problem

NHTSA's vPIC is the canonical North American VIN decoder, but it ships as a database you must host: a MS SQL `.bak` and Postgres `.plain`/`.custom` dump (`vPICList_lite_YYYY_MM.{bak,plain,custom}.zip`), refreshed monthly. Decoding means standing up SQL Server or Postgres and calling the `spVinDecode` stored procedure — gigabytes of infrastructure to answer "what is this VIN?" That is absurd for a function that takes 17 characters in and a row out.

## The thesis

Decoding is not a database problem. It is WMI lookup → schema selection by model year → wildcard pattern match → priority-ranked attribute resolution. We compile vPIC's data and the *semantics* of `spVinDecode` into one embedded Rust artifact, and decode in-process with **zero** SQL engine, zero network, zero hosted database.

## The pipeline (deterministic, reproducible)

1. **Download** the monthly `vPICList_lite_*.plain.zip`.
2. **Extract** schema, stored procedures, and data into **committed plain-text** so every upstream change is diffable and auditable across months.
3. **Build** an embedded, content-addressed Rust artifact — same input bytes always yield the same artifact.
4. **Decode** in pure Rust: WMI via positions 1-3 (or 1-3+12-14 when position 3 is `9`), `Wmi → Wmi_VinSchema → VinSchema` year filtering, `Pattern.Keys` matched over positions 4-8 + 10-17, and per-`ElementId` resolution mirroring vPIC's `RANK() PARTITION BY ElementId` priority/specificity ordering. Check digit (weights `8,7,6,5,4,3,2,10,0,9,...`, mod 11, X=10), `SuggestedVIN`, and space-delimited error codes included.

## Correctness: exact parity or it's a bug

Parity is measured against the official `.plain` dump and live vPIC API as oracle. We **generate VINs exhaustively** — every WMI, every schema, every pattern path, partial and corrupted VINs, error-correction cases — and assert field-for-field equality. The one place vPIC is non-deterministic (the `NEWID()` tie-break in dedup) gets a single defined, documented ordering.

## Why we win on numbers

corgi — the current open-source bar — reduces vPIC's **1.5GB to 64MB uncompressed / 21MB gzip** and decodes at **~30ms (v2 SQLite)**, with **~12ms** targeted by its unreleased v3 binary-index/`corgi-rs` (FST + rkyv) rewrite. It self-reports **93.6% Tier-1 accuracy** and concedes trim "falls apart." We beat both axes: **exact parity** (not 93.6%) and **sub-millisecond** in-process decode — faster than corgi, MS SQL, and Postgres baselines — published as honest, reproducible benchmarks.

## Principles & non-goals

- **No code beats clever code.** No SQL engine at runtime. The artifact is the product.
- **Diffable data.** Schema and procs live as text in git, not opaque binaries.
- **Parity is the spec.** We replicate vPIC; we don't "improve" decode logic.
- **Non-goals:** non-vPIC/community WMIs, recalls, market values, listings. Decode only.
