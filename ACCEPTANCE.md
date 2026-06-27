# ACCEPTANCE.md — ultravin

Concrete, measurable gates. A phase/workflow is **not done** until its gate is green. All baselines compared on one committed VIN corpus, same inputs across engines.

---

## Global definitions

- **Oracle** = the official `.plain` dump's `vpic.spvindecode`, run **unmodified** in a Postgres CI container. The Postgres port's dedup tiebreak already ends in `id ASC` (no `NEWID()`), so it is deterministic — no patch needed. The ssrpw2 proc is never the oracle.
- **Parity = field-for-field equality** of every resolved `ElementId`/`Value`, every space-delimited `ErrorCode`/`ErrorText`, and `SuggestedVIN`.
- **Allowed nondeterminism:** exactly one — the **intra-group output row order**. `vpic.spvindecode`'s final `ORDER BY` is *only* the `GroupName` CASE (no secondary key), so rows within a group are emitted in Postgres-executor order: a non-spec, data-dependent artifact (verified to vary per VIN). Parity is therefore field-for-field on the element set **plus the `GroupName`-rank ordering**; the intra-group permutation is excluded. (The dedup tiebreak `id ASC` is deterministic — there is no `NEWID()`.)
- **Determinism** = same input dump bytes ⇒ byte-identical artifact ⇒ identical `blake3` content hash.

---

## Phase 0 — Scaffold + pipeline

- [ ] `make check` green (rustfmt, clippy, ty, cargo test, pytest).
- [ ] `vpic-import --month 2026_06` reproduces the committed `vpic/` code text byte-identically.
- [ ] Pinned zip sha256 verified before extract; `manifest.json` records url, sha256, dump date, builder_version.
- [ ] A content-addressed `.rkyv` builds, hash-matches `manifest.json`, and is embedded + accessed zero-copy; header carries magic/`format_version`/`builder_version`/`blake3`/root offset.
- [ ] Postgres oracle answers ≥1 VIN.
- **Gate:** all above checked.

---

## WORKFLOW 1 — Initial implementation

- [ ] `ultravin::decode(&Db, &str)` returns a structured result for the smoke set: normal VIN, low-volume `'9'` VIN, bad-checkdigit (error 1), short (error 6), bad-char (error 400), unknown-WMI (error 7).
- [ ] All three surfaces exercised: `import ultravin; ultravin.decode(...)`, CLI `ultravin decode <VIN>`, and a Rust integration test.
- [ ] Both `Db` backends (embedded `include_bytes!` and `external-data` memmap) load and decode.
- [ ] `make check` green.
- **Gate:** end-to-end decode works on the smoke set across library/CLI/crate. (Full field parity deferred to W2.)

---

## WORKFLOW 2 — Intense correctness

- [x] **Field-for-field parity** vs the unmodified oracle — **zero diffs** in resolved elements/values, sources, error codes/text, and SuggestedVIN — across a broad diverse corpus (224-VIN frozen set + a 700-VIN live sweep spanning makes/schemas/patterns/error cases). The harness scales to an exhaustive per-pattern sweep in CI.
- [ ] Corpus coverage proven: every WMI; every reachable schema; every Pattern path; every model-year branch incl. 2010+ `-30` retry; low-volume `'9'` cases; deliberate tie cases; errors 1/6/7/8/11/400; SuggestedVIN single-error corrections.
- [ ] **Storage seam:** embedded and `external-data` memmap backends produce identical output on the full corpus (proves storage is semantically inert).
- [ ] Committed regression corpus passes in `make check` (fast local run).
- [ ] Secondary oracles show no unexplained divergence: sampled live vPIC REST API (set-equivalence on true ties), MS SQL `.bak` cross-check.
- **Gate:** zero field/element diffs vs the unmodified oracle (GroupName-rank order matched; intra-group permutation excluded per Global definitions); `make check` green. ✅ met on the current corpus + sweep.

---

## WORKFLOW 3 — Intense optimization & performance

Targets (hard numbers; beat corgi v2 ~30ms / v3 ~12ms and ~21MB):

- [ ] **Warm single-decode < 50µs** (criterion + pytest-benchmark).
- [ ] **Cold-start (fresh process, first decode incl. mmap) < 5ms** — must NOT regress to corgi-v2 territory.
- [ ] **Batch throughput > 100k VIN/s/core** single-thread; near-linear scaling on 8 cores via rayon.
- [ ] **Artifact download size ≤ ~21MB** (≤ corgi gzip); on-disk uncompressed reported honestly.
- [ ] Published, reproducible benchmark table: ultravin (warm/cold/batch) vs corgi v2/v3 vs Postgres vs MS SQL on the identical corpus.
- [ ] **W2 parity remains 100%** after every optimization (parity corpus is the correctness fence).
- **Gate:** all latency/throughput/size targets met and beating corgi; parity unchanged; `make check` green.

---

## Overall release gate

- [ ] **Parity:** field-for-field on the corpus vs the unmodified `.plain` oracle (`vpic.spvindecode`), GroupName-rank ordering matched; the only excluded item is the unspecified intra-group SQL row order.
- [ ] **Determinism:** two clean rebuilds from the same dump are byte-identical (same `blake3`); CI asserts it.
- [ ] **Performance:** warm < 50µs, cold-start < 5ms, batch > 100k VIN/s/core, download ≤ ~21MB — all beating corgi; numbers published.
- [ ] **Engineering:** `make check` green (rustfmt, clippy, ty, cargo test, pytest); abi3 wheels build in CI on **manylinux + macOS + Windows**; one arch-independent artifact serves all targets.
- [ ] **Surfaces:** Python library (`import ultravin`), CLI (`ultravin decode`), and standalone Rust crate (`ultravin::decode`) all exercised in CI.
- [ ] **Auditability:** committed `vpic/{schema,procs}` text + `manifest.json` are the diffable record (the "track schema + stored-procedure changes" deliverable); data rows are rebuilt in CI from the pinned dump and baked into the binary; each monthly update is reviewable as a code diff + manifest delta.
