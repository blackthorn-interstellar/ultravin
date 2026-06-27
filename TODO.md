# TODO.md — ultravin

Phased, checkbox roadmap. **Phase 0** scaffolds the repo and the download/extract/commit pipeline. Then the three execution workflows the human asked for run as explicit fan-out passes: **W1 initial implementation**, **W2 intense correctness**, **W3 intense optimization**. Each workflow states its parallel unit of work, stages/agents, and exit gate.

---

## Phase 0 — Scaffold + pipeline (sequential foundation)

- [ ] Convert repo to a Cargo workspace; add `crates/ultravin-core`, `crates/ultravin-build`, `crates/ultravin-py`.
- [ ] Switch `pyproject.toml` build backend to **maturin**; configure **abi3** (cp312+), keep uv-managed dev deps.
- [ ] Wire `python/ultravin/` (thin `__init__.py` re-export, `cli.py` typer skeleton).
- [ ] Extend `make check` to run rustfmt + clippy + cargo test alongside ruff + ty + pytest.
- [ ] `vpic-import` skeleton: download `vPICList_lite_<YYYY_MM>.plain.zip` (confirmed URL pattern), verify pinned sha256, extract.
- [ ] `.plain` parser: split DDL / `COPY` blocks / PL-pgSQL functions.
- [ ] Emit committed CODE text: `vpic/{schema,procs}/` + `manifest.json` (url, zip sha256, dump date, builder_version, per-table row counts, artifact blake3). Data rows NOT committed.
- [ ] Assign monotonic `seq` per row (CreatedOn/NEWID surrogate); sort every table by total order.
- [ ] Commit `vpic/` code text for the pinned ground-truth month (2026_06) and tag it `data-2026_06`.
- [ ] Define the `Db` accessor trait + rkyv archived SoA types (`Wmi`, `Wmi_VinSchema`, `VinSchema`, `Pattern`, `Element`, `ElementAttributes`, `Make`, `Model`, `Make_Model`, `Manufacturer`, `DefaultValue`, `VinDescriptor`, string arena).
- [ ] Builder: sorted in-memory tables → archived `.rkyv`, content-addressed (`blake3` header), `format_version` gate; aligned `include_bytes!` bakes it into the binary via `build.rs`.
- [ ] CI: throwaway Postgres container loads the committed dump; smoke-run `spVinDecode` on 1 VIN.
- **Exit gate:** `make check` green; `vpic-import --month 2026_06` reproduces the committed code text byte-identically; the `.rkyv` builds, hash-matches `manifest.json`, and is embedded + accessed zero-copy; Postgres oracle answers one VIN.

---

## WORKFLOW 1 — Initial implementation

**Goal:** a complete, mostly-correct decoder end to end (core → bindings → CLI), each step a faithful transcription of `spVinDecode`.

**Fan-out unit:** the **11 decode steps** (§5 of PLAN) as independent modules with stubbed `Db` fixtures — agents work in parallel on `normalize`, `wmi`, `year`, `keys`, `resolve`, `checkdigit`, `errors`, plus the SoA loader, PyO3 bindings, and CLI.

**Stages / agents:**
- [ ] **Loader agent:** embedded `include_bytes!` rkyv `Db` impl (default) + an `external-data` memmap2 backend behind the same trait (proves the seam).
- [ ] **Engine agents (parallel):**
  - [ ] `normalize.rs` — len/charset, errors 6/400.
  - [ ] `wmi.rs` — extract + mask + low-volume `'9'` rule + binary-search lookup (error 7).
  - [ ] `year.rs` — `fVinModelYear2` port: pos10 cycle, pos7 disambiguation, 2010+ `-30` retry, `VinDescriptor` (error 11).
  - [ ] `keys.rs` — `@keys` build (4-8 | 10-17) + LIKE-prefix wildcard matcher.
  - [ ] `resolve.rs` — schema select (YearFrom/YearTo), `#DecodingItem` build, source priorities, RANK dedup (`u128` key + exempt bitset).
  - [ ] `checkdigit.rs` — weights/transliterate/mod-11/X, error 1.
  - [ ] `errors.rs` — space-delimited codes + SuggestedVIN single-error retry.
- [ ] **Bindings agent:** `#[pyfunction] decode/decode_batch`, GIL released; result dict shape.
- [ ] **CLI agent:** typer `decode` / `decode-batch` / `version`, logic delegated to core.
- [ ] Integrate: wire steps into `decode(&Db, &str)`; build wheel locally.

**Exit gate:** end-to-end decode works for a hand-picked smoke set (normal VIN, low-volume `'9'` VIN, bad-checkdigit, short, bad-char, unknown-WMI); `import ultravin; ultravin.decode(...)`, the CLI, and a Rust integration test all return a result; `make check` green. Field-level parity not yet required — that's W2.

---

## WORKFLOW 2 — Intense correctness (exhaustive generated-VIN parity)

**Goal:** 100% field-for-field parity vs the patched `.plain` oracle.

**Fan-out unit:** **per-WMI parity shards** — partition the ~30-50k WMIs across parallel workers; each worker enumerates that WMI's schemas/patterns, generates VINs, runs both engines, and diffs. Embarrassingly parallel, deterministic per shard.

**Stages / agents:**
- [ ] **Oracle agent:** Postgres container from the pinned dump; run `vpic.spvindecode` **unmodified** — the Postgres port's dedup already ends in `id ASC` (no `NEWID()`), so it's deterministic with no patch.
- [ ] **Generator agent:** synthesize VINs satisfying each Pattern's fixed positions (wildcards filled valid); **sweep pos10/pos7** for every year branch incl. 2010+ `-30` retry; valid + single-char-corrupted check digits; short/partial/bad-char/unknown-WMI/no-pattern cases (errors 1/6/7/8/11/400); deliberate tie cases (now testable via the patched oracle).
- [ ] **Differential agents (parallel over WMI shards):** decode each VIN with ultravin and the patched proc; assert equality of every resolved `ElementId`/`Value`, `ErrorCode`/`ErrorText`, `SuggestedVIN`.
- [ ] **Triage agent:** cluster diffs by element/step; fix the responsible module; re-run the shard. Year and AttributeId-resolution diffs prioritized.
- [ ] **Seam check:** assert identical output across the embedded and `external-data` memmap backends (storage is semantically inert).
- [ ] **Regression corpus:** freeze a representative VIN+expected set for fast local `make check`.
- [ ] **Secondary oracles:** sampled live vPIC REST API smoke; MS SQL `.bak` cross-check via mssql docker.

**Exit gate:** **zero diffs** across the full exhaustive corpus vs the unmodified `vpic.spvindecode` (the Postgres port is deterministic — no allowed divergence); committed regression corpus passes in `make check`; secondary oracles show no unexplained divergence.

---

## WORKFLOW 3 — Intense optimization & performance

**Goal:** beat corgi on latency and size with honest published numbers, without touching the parity-validated decode logic.

**Fan-out unit:** **independent hot-path optimizations**, each guarded by a criterion benchmark and the W2 parity corpus as a correctness fence — agents pursue candidates in parallel and only land changes that keep parity green.

**Stages / agents:**
- [ ] **Bench harness agent:** criterion (Rust) + `pytest-benchmark` (wheel); cold-start (fresh process incl. mmap), warm single-decode, batch (1-core + 8-core rayon); plus corgi v2/v3, Postgres, MS SQL on the same corpus; size report.
- [ ] **Optimization agents (parallel candidates):**
  - [ ] SoA layout / cache-line packing; lazy string resolution (rank-1 only).
  - [ ] Branch-light SIMD-friendly LIKE-prefix matcher.
  - [ ] Single `u128` sort key + `sort_unstable` dedup tuning; `SmallVec` sizing to keep the hot path alloc-free.
  - [ ] mmap load path: validated `access` once → `access_unchecked` on verified hash.
  - [ ] `decode_batch` rayon over the shared archive (no locks, shared page cache).
  - [ ] Artifact size: interning/varint review; **FST/front-coding held in reserve, pulled ONLY if size exceeds the ~21MB corgi line**.
- [ ] **Profiling agent:** flamegraph the hot path; confirm year/pattern-scan dominate and micro-opts target real cost.
- [ ] **Wheel agent:** cibuildwheel manylinux/macOS/Windows, abi3 (cp312+); verify the baked-in artifact is accessed zero-copy on all three; check `.so` size and install footprint.

**Exit gate:** warm single-decode **< 50µs**, cold-start **< 5ms**, batch **> 100k VIN/s/core**, artifact download **≤ ~21MB** — all beating corgi; published reproducible benchmark table; **W2 parity still 100%**; abi3 wheels build green on 3 OSes; `make check` green.
