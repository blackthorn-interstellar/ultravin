# PLAN.md — ultravin

A pure-Rust reimplementation of NHTSA vPIC's `spVinDecode`, shipped as one Python wheel (CLI + library) and a standalone Rust crate. Bar: **byte-for-byte parity** with the official `.plain` dump's `spVinDecode`, **deterministic** reproducible monthly imports, **diffable** committed schema + stored-procedure text, the decode data **baked into a single self-contained binary**, and **faster + smaller than corgi** (~12-30ms, ~21MB).

---

## 1. Chosen architecture

**Design C's resolver on Design A's load layer.** Concretely:

- **Decode engine** = the *Simplest Thing That Is Correct* (Design C): integer-interned struct-of-arrays sorted by lookup key, WMI by binary search, schema by `partition_point` range scan, patterns by a linear wildcard-LIKE scan over a schema's tiny contiguous slice, and a **line-for-line transcription of `spVinDecode`'s RANK dedup**. No FST, no perfect hashing, no clever Automaton.
- **Storage/load layer** = a content-addressed **rkyv 0.8 archive baked into the binary via aligned `include_bytes!`** (default), accessed zero-copy over the embedded `&'static [u8]`; an optional `external-data` feature `memmap2`s the same artifact from a file path (fast dev iteration; standalone-crate use). Both sit behind a `Db` accessor that is *provably non-load-bearing for correctness*. Because the embedded bytes live in the binary's read-only segment, the OS **demand-pages** them on load — a single self-contained binary that still gets zero-copy access, low RAM, and fast cold start (killing the postcard-cold-start weakness the judges flagged).

### Why (citing the verdicts)

- **2 of 3 judges chose Design C.** The parity lens and the simplicity lens both ranked it #1: "decode reads like the proc body line-for-line," "determinism falls out of sort+postcard+zstd as a pure function," and at ~19MB it already beats corgi's 21MB. Every parity-critical behavior lives in a **resolution layer that is logically identical across all three designs**; storage/index is *semantically inert*. So the right pick is the design whose decode path is the most transparent and diffable against the canonical `.plain` `spVinDecode`.
- **The one knock on C — cold start — is a storage problem, not a decode problem.** All three judges named the fix: swap the load layer to rkyv+mmap behind the same `Db` accessor "without touching decode logic." We adopt that as the default rather than the fallback. This also wins the latency lens (judge #2's reason for ranking A first), so we get A's near-zero cold start *without* A's CHD perfect hash or permanent rkyv-everywhere coupling. (Per the owner's call we **do** bake the artifact into the binary via `include_bytes!` for a single self-contained artifact — this keeps zero-copy + demand-paging; the only real costs are binary-on-disk size and link time, both mitigated in §4.)
- **Rejected Design B (FST).** Its own author and all three judges agree the global-FST + custom wildcard Automaton + post-hoc schema filter "helps SIZE not SPEED" on per-schema sets of tens of patterns, carries three deterministic serialization paths, and is the hardest to debug when a field diverges. FST is kept **only as a back-pocket size lever** if a future dump pushes the artifact past the corgi line.

### Grafted ideas (the judges' "best ideas to graft")

| Source | Graft | Where it lives |
|---|---|---|
| A | rkyv zero-copy load behind the `Db` accessor — embedded `include_bytes!` (default) or `memmap2` file (`external-data` dev) | `ultravin-core::db` |
| — | Oracle already deterministic (`id ASC` tiebreak in the Postgres port — no `NEWID()`); run it **unpatched** for exact row-identity parity | parity harness |
| A | Carry the real per-row `coalesce(UpdatedOn, CreatedOn)` column for the dedup `CreatedOn DESC` order; `Pattern.Id ASC` is the final tiebreak | importer + decode |
| A | Precompute `specificity = LEN(REPLACE(Keys,'*',''))`, monotonic `seq`, and a **const bitset** for exempt elements `{114,121,129,150,154,155,169,186}` | importer + decode |
| A | Single `u128` sort key for the RANK dedup → one `sort_unstable` + linear scan | `decode::dedup` |
| A/B | Content-addressed artifact: `blake3(canonical tables ++ builder_version)` in header + filename, `format_version` gate, CI asserts two clean rebuilds are bit-identical | importer + CI |
| B | Columnar SoA; resolve strings from the arena **only for rank-1 survivors** | data model |
| B | "Storage is semantically inert" framing — all parity reasoning localized to the resolver | architecture invariant |
| B | FST/front-coding held in reserve as a pure size lever | documented, not built |

**Net:** simpler than A, smaller than A, faster cold-start than C-baseline, far more auditable than B. Clear, fast, boring, correct.

---

## 2. Repo layout

Cargo workspace + uv-managed Python package, maturin build backend.

```
ultravin/
├── Cargo.toml                      # workspace
├── pyproject.toml                  # maturin backend, abi3, uv-managed
├── Makefile                        # make check: fmt+clippy+ty+cargo test+pytest
├── crates/
│   ├── ultravin-core/              # THE engine + standalone Rust crate
│   │   ├── src/
│   │   │   ├── lib.rs              # pub fn decode(&Db, &str) -> DecodeResult
│   │   │   ├── db.rs              # Db accessor; rkyv types; embedded include_bytes! (default) | memmap2 (dev)
│   │   │   ├── tables.rs          # struct-of-arrays (Wmi, Pattern, Element, ...)
│   │   │   ├── normalize.rs       # uppercase, len, charset (I/O/Q) → error 6/400
│   │   │   ├── wmi.rs             # WMI extract + mask + low-volume '9' rule + lookup
│   │   │   ├── year.rs           # fVinModelYear2/fVinDescriptor port (pos10/pos7, -30 retry)
│   │   │   ├── keys.rs           # @keys build (4-8 | 10-17) + LIKE-prefix matcher
│   │   │   ├── resolve.rs        # #DecodingItem build, sources/priorities, RANK dedup
│   │   │   ├── checkdigit.rs     # weights, transliterate, mod 11, X, error 1
│   │   │   └── errors.rs         # error codes + SuggestedVIN
│   │   ├── build.rs               # ensures aligned vpic.rkyv exists for include_bytes! (runs vpic-import if missing)
│   │   ├── data/vpic.rkyv         # GITIGNORED build product — baked into the binary via include_bytes!
│   │   └── tests/                 # rust unit + differential-generator tests
│   ├── ultravin-build/             # importer/builder bin (vpic-import)
│   │   └── src/main.rs            # download → parse .plain → emit schema/procs text → build .rkyv (gitignored)
│   └── ultravin-py/                # PyO3 + maturin bindings (abi3)
│       └── src/lib.rs            # #[pyfunction] decode/decode_batch (GIL released)
├── python/
│   └── ultravin/
│       ├── __init__.py           # re-export native module (no decode logic)
│       └── cli.py               # typer: ultravin decode <VIN> [--json]
│                                 # (no data file — baked into the ultravin-py .so via include_bytes!)
├── vpic/                          # committed CODE only — overwritten monthly, git tag per month
│   ├── schema/                    # DDL per table (CREATE TABLE ...) — "track schema changes" deliverable
│   ├── procs/                     # spVinDecode.sql, fVinWMI.sql, fVinModelYear2.sql, ... (parity reference)
│   └── manifest.json              # url, sha256(zip), dump date, builder_version, per-table row counts, artifact blake3
│                                  # ^ data ROWS not committed — rebuilt in CI from the pinned dump; counts+hash make deltas auditable
├── tests/                          # pytest (mirrors python/), no classes
│   ├── test_decode.py  test_cli.py  test_import_smoke.py
└── .github/workflows/
    ├── ci.yaml                     # make check + parity (postgres oracle)
    ├── parity.yaml                 # generated-VIN exhaustive parity (matrix)
    └── wheels.yaml                 # cibuildwheel manylinux/macOS/Windows, abi3
```

**Source-of-truth invariant:** the upstream `.plain` dump (pinned by sha256 in `manifest.json`) is canonical. From it we commit the **code** — `vpic/schema/` + `vpic/procs/` — as diffable text (the "track schema + stored-procedure changes over time" deliverable), overwritten each month with a git tag per month so any past month rebuilds deterministically. Data **rows** are not committed: the `.rkyv` artifact is rebuilt from the pinned dump in CI, its `blake3` asserted against `manifest.json`, and baked into the binary. The manifest's per-table row counts + artifact hash keep month-to-month *data* deltas auditable without bloating the repo.

---

## 3. Download → extract → commit pipeline

Confirmed URL pattern (ground truth): `https://vpic.nhtsa.dot.gov/Downloads/vPICList_lite_YYYY_MM.{bak,custom,plain}.zip` — all three formats and prior months return HTTP 200 and are individually addressable, enabling deterministic pinning.

`vpic-import` (one Rust bin, deterministic, prints the artifact blake3):

1. **Download** the pinned month's `vPICList_lite_<YYYY_MM>.plain.zip`. Pin the **sha256 of the zip** in `manifest.json`; re-runs verify the byte hash before proceeding. (`.bak` downloaded separately only for the tertiary MS SQL oracle.)
2. **Extract** the plain-text SQL (DDL + `COPY` data + PL/pgSQL functions, ~72MB zip).
3. **Parse**:
   - DDL → `vpic/schema/*.sql` (overwritten; the diffable "track schema changes" deliverable).
   - PL/pgSQL functions/procs (`spVinDecode`, `fVinWMI`, `fVinModelYear2`, `fVinDescriptor`, ...) → `vpic/procs/*.sql` (verbatim, the parity reference).
   - `COPY` blocks → **in-memory** normalized tables (NOT committed): assign each row a monotonic `seq` = source dump line index (the `CreatedOn`/`NEWID` surrogate); sort each table by its declared key with a **total order** (ties broken by primary id) so dump row order is irrelevant.
4. **Build** the content-addressed `.rkyv` artifact (§4) from the sorted in-memory tables; write per-table row counts + artifact `blake3` into `manifest.json`.
5. **Commit** `vpic/schema/` + `vpic/procs/` + `manifest.json` and tag the month (e.g. `data-2026_06`). The `.rkyv` is a gitignored build product, rebuilt deterministically in CI from the pinned dump and baked into the binary; the month-to-month diff a reviewer sees is plain-text code plus the manifest's counts/hash.

Monthly cadence is a one-PR operation: `vpic-import --month 2026_07`, review the code diff + manifest delta, CI rebuilds the artifact + reruns parity.

---

## 4. Deterministic, content-addressed build

Same dump bytes ⇒ **byte-identical** artifact. Guaranteed by:

1. Every table sorted by a total order before serialization (no hashmaps in the archive — only sorted slices + offset tables; no pointer/address leakage).
2. String arena built in **first-seen order over the sorted traversal** → deterministic interning to `u32 StrId`.
3. Precompute at build time: per-Pattern `specificity` (`LEN(REPLACE(Keys,'*',''))`), per-row `seq`, the exempt-element bitset, and Pattern `priority` from `YearFrom`.
4. rkyv serialization of a fixed-order input is itself deterministic (pinned little-endian, fixed 16-byte alignment).
5. **Header** (64 bytes): magic, `format_version`, `builder_version`, `blake3(canonical parsed tables ++ builder_version)`, root offset. The blake3 is also the **filename suffix** (`vpic-<blake3_12>.rkyv`).
6. **CI gate:** two clean rebuilds from the same dump must produce identical bytes; mismatch fails the build.

**Distribution — data baked into a single self-contained binary.** The `.rkyv` is embedded via aligned `include_bytes!` directly into `ultravin-core`, so both the standalone CLI binary and the PyO3 extension (`ultravin-py` `.so`/`.pyd`/`.dylib`) carry the data in their read-only segment — no sidecar file, no runtime path lookup. Because that segment is **mmapped and demand-paged by the OS loader**, embedding keeps zero-copy access and low RAM (only touched pages fault in); cold start stays well under target. rkyv zero-copy needs alignment, forced with a `#[repr(align(16))]` wrapper (or `include_bytes_aligned!`). The wheel is then just the `.so` + thin Python shims; its zip compresses the embedded data to the ~18-20MB download class, pip expands on install. One arch-independent data payload serves every cibuildwheel target.

**Costs & mitigations of embedding:** (1) the binary/`.so` is artifact-sized on disk (~20-60MB) — acceptable, comparable to corgi's install; (2) relinking the full artifact on every build is slow — mitigated by the **`external-data` dev feature** (`--no-default-features`) that `memmap2`s the artifact from a path instead, so iteration never relinks tens of MB; (3) crates.io has a package-size cap — if/when we publish the standalone crate there, ship it `external-data`-by-default and fetch the artifact as a release asset, while wheels and single-binary builds embed by default. `build.rs` materializes the artifact (running `vpic-import` if absent) so `include_bytes!` always has an aligned file to embed.

---

## 5. Decode engine spec (mirrors `spVinDecode` step by step)

The engine mirrors `vpic.spvindecode` + `spvindecode_core` + `spvindecode_errorcode`; reviewers diff each module against `vpic/procs/*.sql` (canonical). **Parity is the spec — we replicate observed behavior, including bugs.** (This spec was rewritten against the real 2026_06 dump; it supersedes the single-pass `LIKE` model from the SQL-Server-era recon.)

### 5a. `spvindecode` — multi-pass best-of orchestration
1. **Descriptor lookup — replicate the bug.** The proc calls `fVinDescriptor(vin)` *before* assigning `vin` from the input, so it always runs on the empty-string default → descriptor `'***********'` → the `VinDescriptor` year lookup (`dmy`) is ~always NULL and **pass 1 is effectively dead code**. Port verbatim; a dedicated regression test pins it.
2. **Model-year hypotheses.** With `dmy` NULL, take the `fVinModelYear2` branch: compute `rmy` (negative ⇒ inconclusive, derive `omy = -rmy-30`); swap to `altMY` (±30) only when the alternate year has matching schemas and the primary has none (`cnt1=0 and cnt2>0`); honor a caller-supplied `year`.
3. **Run `spvindecode_core` per pass** (1 = descriptor [dead], 2 = caller year, 3 = `rmy`, 4 = `omy`), each tagged with a distinct `DecodingId`.
4. **Pick the best pass** by `(ErrorValue desc, ElementsWeight desc, Pattern-count desc, ModelYear desc)` — `ErrorValue` from element **143** via `fErrorValue`; `ElementsWeight` = Σ `Element.weight` of non-empty resolved elements; ModelYear = element **29** (+10000 if it equals the caller's `year`). Delete all non-best passes.
5. **QC filter, `'XXX'` resolution, output.** Strip QC-only rows (unless `includeNotPublicilyAvailable`); resolve `'XXX'` sentinels via `fElementAttributeValue`; emit one row per `Element` that has a `Decode`, ordered by the fixed `GroupName` priority — the 15-column contract `(groupname, variable, value, itempatternid, itemvinschemaid, itemkeys, itemelementid, itemattributeid, itemcreatedon, itemwmiid, code, datatype, decode, itemsource, itemtobeqced)`.

### 5b. `spvindecode_core(pass, modelYear, …)` — one decode pass
- **WMI** (`fVinWMI`): `left(vin,3)`, or 6 chars (`+ pos12-14`) when `pos3='9'`. WMI not in `Wmi` (respecting `PublicAvailabilityDate`) → **error 7**.
- **keys** = `pos4-8 ++ '|' ++ pos10-17`.
- **Pattern match → `#DecodingItem`** (value `'XXX'`, source `Pattern`, priority = `Wmi_VinSchema.YearFrom`): join `Wmi→Wmi_VinSchema→VinSchema→Pattern` filtered to the year range, matching **either** `keys LIKE replace(Keys,'*','_')||'%'` (plain) **or** `keys ~ Pattern.keys_regex` (bracket-class — regex is a **precomputed column in the dump**, not built at runtime). Excludes elements 26/27/29/39; insert `ORDER BY Pattern.Id ASC` (defines the deterministic `id`).
- **Layered sources**: EngineModelPattern=50 (matched via element 18 engine-model name); VehType=39, Manuf.Name=27, Manuf.Id=157, ModelYear=29 (all 100); Formula Pattern=100 (`#`-digit-substitution keys); Make-from-Model=26 (`pattern - model`, **1000**) else single-WMI Make=26 (`Make`, −100); **Conversion=100 (dynamic `Formula` evaluated via `execute`)**; Vehicle Specs=−100 (VehicleSpecSchema/Pattern by make/model/year/type); DefaultValue=10 (per vehicle type, only for missing elements).
- **Per-element dedup (already deterministic).** `DELETE WHERE RANK() OVER (PARTITION BY ElementId ORDER BY Priority DESC, CreatedOn DESC, LENGTH(REPLACE(COALESCE(Keys,''),'*','')) ASC, REPLACE(REPLACE(COALESCE(Keys,''),'[',''),']','') ASC, id ASC) > 1`, **except** multi-value elements `{114,121,129,150,154,155,169,186}`. Port as a `u128` sort key `(element_id, ¬priority, ¬createdon, len_no_star ASC, keys_no_brackets ASC, pattern_id ASC)` + `sort_unstable` + linear keep-first. **Note the corrections vs the old plan: the length term is ASC (not DESC), and the final tiebreak is `id ASC` — there is no `NEWID()`, so no oracle patch is needed.** `CreatedOn` is the real `coalesce(UpdatedOn, CreatedOn)` column, carried in the artifact.
- **Errors & validity**: per-position char scan (rules vary by `startPos` = 13/14/15 from `pos3='9'`/carMPV-LT) builds the invalid-char list + `CorrectedVIN`; check digit via `fVINCheckDigit2(vin, isCarMpvLT)` unless a `VinException` row exempts it. Codes accumulated: 0 ok, 1 checkdigit, 6 len<17, 7 WMI, 8 no pattern, 9/10 off-road, 11 year null, 12 year mismatch, 14 no model, 400 bad chars, 4/5 extra `ErrorCode` text. Result fields are stored as pseudo-elements 142 (CorrectedVIN), 143 (codes), 191 (messages), 144 (ErrorBytes), 156 (AdditionalInfo), 196 (descriptor) at priority 999.

### 5c. `spvindecode_errorcode(vin, modelYear)` → corrected/suggested VIN + error bytes + unused positions. Ported in the correctness workflow (W2).

**Check digit** (`fVINCheckDigit`/`fVINCheckDigit2`): weights `[8,7,6,5,4,3,2,10,0,9,8,7,6,5,4,3,2]` (pos9 weight 0), transliterate (I/O/Q invalid), `mod 11`, remainder 10 → `'X'`; per-position char-class validity pre-check returns `'?'` on an invalid char. **Year decode** (`fVinModelYear2`): pos10 `A–H`→`2010+(c−A)`; `J–N`→`−1`; `P`→2023; `R–T`→`−3`; `V–Y`→`−4`; `1–9`→`2031+(c−'1')`. For carMPV-LT WMIs (`vehicleTypeId∈{2,7}` or `{3, truckType 1}`): pos7 digit ⇒ `year−=30` conclusive; pos7 alpha ⇒ conclusive; `year > thisYear+2` ⇒ `year−=30`. Inconclusive ⇒ return a **negative** year (the caller derives `omy`). *(Highest parity risk; first-class generator target.)*

---

## 6. API surface

**Rust crate (`ultravin-core`):**
```rust
let db = Db::embedded();                 // zero-copy view of the baked-in artifact (default); Db::open(path) for external
let res: DecodeResult = ultravin::decode(&db, "1HGCM82633A004352");
// DecodeResult { elements: Vec<Element{code,name,value,...}>, errors: Vec<(u16,&str)>, suggested_vin: Option<String> }
```
Hot path is alloc-light and no_std-friendly; `Db` is the swappable storage seam.

**Python (`import ultravin`):**
```python
import ultravin
ultravin.decode("1HGCM82633A004352")          # -> dict
ultravin.decode_batch([...])                    # -> list[dict]; GIL released, optional rayon
```
Native module exposes a zero-copy view of the baked-in artifact (no file lookup, demand-paged).

**CLI (thin typer wrapper, logic in core):**
```
ultravin decode <VIN> [--json]
ultravin decode-batch <file> [--json]
ultravin version            # dump month + artifact blake3
```

---

## 7. Testing & parity strategy

**Oracle = the `.plain` dump's `spVinDecode`** (canonical). The ssrpw2 repo proc is ALTERED — reference only, never the oracle.

1. **Deterministic oracle, run unmodified (corrected vs the design).** The real `vpic.spvindecode_core` dedup tiebreak already ends in `id ASC` (insertion order ≡ `Pattern.Id ASC`) — **there is no `NEWID()` in the Postgres port.** So the oracle is already deterministic: load the pinned dump into a throwaway Postgres CI container and run `vpic.spvindecode` **unpatched**. Parity is exact row-identity by construction; genuine ties are reproducible, not avoided.
2. **Exhaustive generated VINs (the parity workhorse).** Enumerate every `Wmi`, walk each reachable `Wmi_VinSchema`/`VinSchema`, synthesize VINs satisfying each `Pattern`'s fixed positions (wildcards filled with valid chars), **sweep pos10/pos7 to exercise every model-year branch incl. the 2010+ `-30` retry**, compute correct check digits plus single-char corruptions (error 1 + SuggestedVIN), and emit short/partial/bad-char/unknown-WMI/no-pattern cases (errors 6/400/7/8/11). Year and SuggestedVIN are first-class generation targets (agreed top parity risk).
3. **Differential assert.** Decode each VIN with ultravin and the unmodified `vpic.spvindecode`; **any** difference in resolved `ElementId`/`Value`, `ErrorCode`/`ErrorText`, or `SuggestedVIN` is a failing test.
4. **Committed corpus** of representative VINs + expected outputs for fast local `make check` (full exhaustive sweep runs in the parity CI matrix).
5. **Secondary oracles:** live vPIC REST API = sampled smoke check (set-equivalence only for true ties); MS SQL `.bak` via mssql docker = tertiary cross-check.
6. **Diffability:** committed `vpic/schema` + `vpic/procs` text plus the `manifest.json` row-counts/hash make every monthly upstream change reviewable, so a parity regression traces to a specific schema/proc/data delta.

---

## 8. Benchmark harness (vs Postgres + MS SQL + corgi)

Single committed VIN corpus, identical inputs across engines, honest reproducible numbers published in CI:

- **ultravin:** warm in-process single-decode (criterion in Rust; `pytest-benchmark` for the wheel) + cold-start (fresh process, first decode, incl. mmap) + batch throughput (single core and 8-core rayon).
- **corgi:** v2 (shipping SQLite, ~30ms/decode) and v3 if released (~12ms target).
- **Postgres** and **MS SQL Server** (docker): `spVinDecode` round-trip on the same corpus (includes client/RTT — reported separately as the realistic baseline).
- **Size:** wheel download size, on-disk artifact size, vs corgi's 21MB gzip / 64MB uncompressed.

Targets (committed to hard numbers once benchmarked): warm single-decode **< 50µs**, cold-start **< 5ms**, batch **> 100k VIN/s/core**, artifact download **≤ ~21MB**.

---

## 9. Key risks & mitigations

- **Year logic** (pos10 cycle + pos7 + 2010+ `-30` retry) is the gnarliest parity surface → make it a first-class generator target; port line-for-line from `fvinmodelyear2.sql`.
- **Replicate the descriptor-ordering bug** — `spvindecode` calls `fVinDescriptor` before assigning `vin`, making pass 1 dead. Port verbatim and pin with a dedicated regression test; "fixing" it would diverge from vPIC.
- **Conversion dynamic formulas** — `spvindecode_core` evaluates `Conversion.Formula` via runtime `execute`. Port the finite set of formulas in-engine, cover each in generation, and fall back to `'0'` on error exactly as the proc does.
- **Multi-pass best-of scoring** — a wrong pass selection silently changes many fields → reproduce the `(ErrorValue, ElementsWeight, Patterns, ModelYear)` ranking exactly and test model-year-ambiguous VINs.
- **AttributeId literal-vs-lookup per Element** must mirror the proc's joins exactly → drive resolution off the per-Element lookup flag; cover in generation.
- **rkyv 0.8 format/alignment drift** → `format_version` gate + content hash + validated `access` on load; alignment enforced by `#[repr(align(16))]`.
- **Storage seam discipline** → CI asserts decode output is identical across the embedded `include_bytes!` backend and the `external-data` memmap backend (proves storage is semantically inert).
- **Embedding cost (binary size / link time)** → the `external-data` dev feature avoids large relinks during iteration; release/wheel builds embed by default; `build.rs` keeps the baked artifact present and hash-verified.
