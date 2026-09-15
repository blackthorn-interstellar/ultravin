# ultravin

**A pure-Rust reimplementation of NHTSA's vPIC VIN decoder with full-field `spVinDecode` parity and documented upstream defects corrected — shipped as one Python wheel that is both a CLI and a library, and as a Rust crate. Clear, fast, boring.**

## The problem

NHTSA's vPIC is the canonical North American VIN decoder, but it ships as a database you must host: a MS SQL `.bak` and Postgres `.plain`/`.custom` dump (`vPICList_lite_YYYY_MM.{bak,plain,custom}.zip`), refreshed monthly. Decoding means standing up SQL Server or Postgres and calling the `spVinDecode` stored procedure — gigabytes of infrastructure to answer "what is this VIN?" That is absurd for a function that takes 17 characters in and a row out.

## The thesis

Decoding is not a database problem. It is WMI lookup → schema selection by model year → wildcard pattern match → priority-ranked attribute resolution. We compile vPIC's data and the *semantics* of `spVinDecode` into one embedded Rust artifact, and decode in-process with **zero** SQL engine, zero network, zero hosted database.

## The pipeline (deterministic, reproducible)

1. **Download** the monthly `vPICList_lite_*.plain.zip`.
2. **Extract** schema, stored procedures, and data into **committed plain-text** so every upstream change is diffable and auditable across months.
3. **Build** an embedded, content-addressed Rust artifact — same input bytes always yield the same artifact.
4. **Decode** in pure Rust: WMI via positions 1-3 (or 1-3+12-14 when position 3 is `9`), `Wmi → Wmi_VinSchema → VinSchema` year filtering, `Pattern.Keys` matched over positions 4-8 + 10-17, and per-`ElementId` resolution mirroring vPIC's `RANK() PARTITION BY ElementId` priority/specificity ordering. Check digit (weights `8,7,6,5,4,3,2,10,0,9,...`, mod 11, X=10), `SuggestedVIN`, and space-delimited error codes included.

## Correctness: vPIC fidelity, upstream defects corrected

The unmodified Postgres `spVinDecode` from the official monthly dump is the oracle. We compare all 15 output fields and the specified group ordering. Generated corpora exercise the data's decoding rules and pairwise descriptor interactions, alongside partial VINs, malformed inputs, caller years, and error-correction cases. The [acceptance policy](ACCEPTANCE.md) defines the contract; the [corpus design](CORPUS.md) makes coverage reproducible.

An unexplained difference is a decoder bug. A proven upstream defect gets a correction backed by the defective data or procedure, a regression case, and a [documented explanation](KNOWN_DEVIATIONS.md). That is how ultravin delivers vPIC fidelity and improves on vPIC's own wrong answers.

## Why we win on numbers

The published September 15 benchmark delivers **164,316 VIN/s on one core**, **521,234 VIN/s on four**, and **1,147,742 VIN/s on all twelve**. Single-core throughput exceeds the published corgi v3 rate by **over 1,900×** and the measured NHTSA SQL baselines by **over 7,300×**. The complete decoder runs in-process, with the database embedded and no service to host.

Speed and correctness advance together: preserve the full decoding algorithm, remove unnecessary work, and measure the result. [Benchmarks](BENCHMARKS.md) record the inputs, hardware, comparison sources, and reproduction commands.

## Principles & non-goals

- **No code beats clever code.** No SQL engine at runtime. The artifact is the product.
- **Diffable data.** Schema and procs live as text in git, not opaque binaries.
- **Parity is the spec; defects need evidence.** Match the official procedure. Correct upstream bugs only when their cause is proven and their regression is covered.
- **Non-goals:** non-vPIC/community WMIs, recalls, market values, listings. Decode only.
