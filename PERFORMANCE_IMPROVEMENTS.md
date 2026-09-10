# Performance improvements

This is the engineering history of ultravin's performance work through September
9, 2026: what was expensive, what changed, what we measured, and what must remain
true when changing it again. It covers the decoder, Python output, Arrow/parquet,
corpus generation, build resources, and the SQL oracle used for differential
testing. The September optimization batches and follow-up have their own sections.

Measurements below are historical observations, not new benchmarks of the current
checkout. Different dates used different toolchains, artifacts, worker counts,
output shapes, and timing methods. Compare each before/after pair on its own;
do not multiply successive speedups or divide unrelated historical rates to
claim a cumulative improvement. A change without an isolated measurement is
identified as such.

- [June: loading, matching, allocation, and Python output](#june-loading-matching-allocation-and-python-output)
- [July and August: reuse, locality, and columnar output](#july-and-august-reuse-locality-and-columnar-output)
- [September batch 1: index candidates and defer copies](#september-batch-1-index-candidates-and-defer-copies)
- [September batch 2: remove remaining work and output allocations](#september-batch-2-remove-remaining-work-and-output-allocations)
- [September 9 follow-up: index joins and narrow matching](#september-9-follow-up-index-joins-and-narrow-matching)
- [Experiments we rejected or replaced](#experiments-we-rejected-or-replaced)
- [SQL oracle and development resources](#sql-oracle-and-development-resources)
- [Correctness and measurement rules](#correctness-and-measurement-rules)
- [Evidence and reproduction](#evidence-and-reproduction)

## June: loading, matching, allocation, and Python output

### Read the archive directly and amortize compilation

Commit `7fe2379` (June 27) replaced deserialize-to-owned loading with direct
access to the archived rkyv data. Strings and tables can be read from the archive
without first constructing an owned copy of the database. The same round cached
compiled bracket-pattern regexes per thread, stored generated regex text only
for bracket patterns, and added Rayon batch decoding with the Python GIL released.

The commit records load plus first decode falling from **29.3 to 1.26 ms**, warm
decode from **4.20 ms to 203 µs**, and gzip artifact size from **20.0 to 19.25 MB**.
Its batch example improved from **325 to 4,342 VIN/s/core**. These are the original
June workloads, not the later 60-second corpus benchmark. Validation included
226 checks and a 500-VIN live parity sweep.

Current implementation: [db.rs](crates/ultravin/src/db.rs),
[matcher.rs](crates/ultravin/src/matcher.rs), and
[batch decoding](crates/ultravin/src/lib.rs). The matcher representation evolved
further below; regex remains the fallback for unsupported specialized shapes.

### Memoize correction data, specialize matching, and borrow strings

Commit `aad9a34` (June 29) contains **two rounds** in its message and diff:

- **Cache the suggested-VIN character sets by WMI and model year.** Rebuilding
  them walked WMI → schema → pattern and compiled regexes on every decode/pass,
  consuming about 60% of the profiled hot path. Per-thread `Rc` values make hits
  cheap. Unknown WMIs with no charset are not cached, so arbitrary garbage WMI
  strings do not each create a cache entry. The original commit records
  **3.5k → 8.9k VIN/s** single-stream and **19.5k → 40.9k** batch; later historical
  reruns in [BENCHMARKS.md](docs/BENCHMARKS.md#earlier-throughput-optimization-rounds)
  recorded different absolute rates. These are separate observations.
- **Use a fixed-length matcher for recognized bracket keys.** Those generated
  patterns consume one character per token, so a small prefix matcher replaces
  the general regex engine. Unrecognized shapes still use regex. This also
  removes large per-thread DFA caches from the common path. The commit records
  equivalence over 43,852 real patterns and 307 million checks.
- **Validate archive strings at the loading boundary, then borrow them.** Arena
  access avoids repeated UTF-8 validation; untrusted archives still require
  validation. `Cow` values keep static source labels and literals borrowed until
  ownership is needed. Pre-sized vectors and unstable sorting where the key is
  already total reduce allocation and sorting work.
- **Index element metadata and use inexpensive integer hashing.** A dense
  element-ID table replaces repeated metadata searches; FxHash replaces SipHash
  in internal integer maps/sets. Python result dictionary keys are interned.
- **Reuse the extracted WMI and borrow cache lookup keys.** This avoids repeated
  extraction and string allocation within one decode.

The second round's paired 60-second measurements were **9,717 → 14,291 VIN/s**
single-core and **43,608 → 54,801 VIN/s** with ten batch workers. The full output
digest for the 5,000-VIN corpus and the frozen parity corpus were unchanged.

### Reduce heap traffic and contention

Commit `06d5868` (June 29) borrowed immutable metadata in `DecodedElement`, moved
item values instead of cloning them, and built scrubbed text, error CSVs, and
messages directly into buffers instead of allocating pieces and joining them.
Lookup searches were narrowed to a lazily indexed table band; repeated lowercasing,
schema-year scans, and scoring were removed.

The Python wheel and throughput example also adopted **mimalloc** on mainstream
64-bit targets. Its sharded allocation reduces contention between Rayon workers.
This is an allocator choice in those binaries; a Rust library consumer controls
its own global allocator. Other targets retain the system allocator. Commit
`1ab7eeb` subsequently pinned mimalloc v2 with TLS suitable for dynamically loaded
wheels; allocator startup behavior is part of compatibility, not just throughput.

Commit `0e011de` cached the five immutable Python metadata strings per element
and pre-sized element lists. The commit records about **23% higher Python batch
throughput** and **4% higher single-call throughput**. It also changed release LTO
from thin to fat to inline small accessors across crate boundaries.

The combined allocator/marshalling round in the historical benchmark report
recorded **14,291 → 19,331 VIN/s** single-core and **54,801 → 111,496 VIN/s** with
ten workers, plus warm single-decode latency **202.8 → 44.8 µs**. Those combined
figures do not isolate the allocator, metadata cache, or LTO individually.

### Serialize JSON in Rust

Commit `9d93ca1` (June 29) added `decode_json` and `decode_batch_json`. Building
hundreds of Python dictionary entries per VIN serialized much of the end-to-end
work under the GIL even after Rust decoding became parallel. Rust serialization
hands Python one string and avoids constructing the intermediate Python object
tree. The CLI also stopped serializing that tree again with `json.dumps`.

The commit records approximately **83k VIN/s** for batch JSON versus **29k** for
batch dictionaries on that workload. Parsed JSON matched the dictionary API over
the benchmark corpus. This is a choice of representation for consumers that want
JSON; parsing it back into Python objects incurs another cost.

## July and August: reuse, locality, and columnar output

### Memoize pattern-key expansion

Commit `a791a67` (July 25) cached `valid_chars_in_key`. The unused-position error
scan had expanded each matched key on every pass, compiling a fresh regex for
bracket keys. The expansion depends only on immutable key text. A **16,384-entry
per-thread cap** limits memory growth when many schemas are visited.

Historical throughput moved **19,331 → 25,175 VIN/s** single-core and
**111,496 → 121,359 VIN/s** with ten workers; full JSON checksums matched over
7,900 decodes. Later fixed-position scans avoid needing the full expansion for
some questions, but correction charset construction still uses the helper.

### Make compact Python output available

Commit `3e86461` (July 25) introduced the flat attributes shape. About 41 elements
with 15 fields each meant roughly 615 GIL-serial dictionary stores per VIN. An
attributes dictionary needs about 41 stores. Four-worker measurements recorded
**30.9 → 15.7 µs/VIN** for dictionary batches and **41.2 → 20.9 µs/VIN** for a
decode-to-Pydantic pipeline.

This **changes the requested representation**: attributes omit per-element
provenance, while full results retain it. Repeated note elements remain lists,
including single-entry lists. Static `ELEMENTS` and `MULTI_VALUED` metadata were
made lazy so importing the package does not load the artifact unnecessarily.

Commit `1f2999e` (August 24) made attributes the default and replaced the old
`flat=True` option with `full=True` for provenance. That was an explicit API
redesign. The September optimizations preserve each existing output shape.

### Avoid quadratic correction and repeated generation setup

- **Invalid-character stamping:** `7c4a36d` (August 10) replaced rebuilding the
  entire corrected VIN for each invalid character with an in-place stamp or
  append. The scan's monotonically increasing positions preserve the original
  result while removing quadratic work on long malformed input. The same commit
  capped untrusted artifact element IDs before allocating the dense index.
  These have regression coverage; no ordinary-corpus throughput gain was isolated.
- **Corpus generation:** `1ac7513` (August 23) removed a per-iteration vector and
  two per-VIN allocations, consolidated WMI lookup structures into one index per
  entry point, and stopped rebuilding one inside the vehicle-spec schema loop.
  It also reads the default clock once. Seeded, pairwise, sweep, and cover outputs
  were checked by hash. No isolated throughput figure was recorded.

### Reuse scans and improve memory locality

Commit `220be20` (August 23) made three immutable facts reusable:

- `Db::pattern_element_ok` precomputes whether each element can contribute a
  pattern, replacing metadata lookup and flag checks inside the pattern loop.
- `PatternScan` computes a schema's key hits once per VIN and replays them for
  candidate model years; matching the key itself is year-independent.
- `ValidChars` caches its rendered text so repeated invalid inputs do not sort
  and format the same correction set again.

Interleaved historical trials measured **40.3 → 39.7 µs/VIN** single-core and
about **1% higher four-worker throughput**. Those used min-of-N timing, not the
September median methodology.

Commit `2f4e59b` (August 24) sorts batch input indices by the first eight VIN
bytes before decoding, then restores input order. Nearby WMIs and descriptor
prefixes share archive pages and pattern keys. Caller years stay attached to
their original input indices. It recorded gains of about **11%, 7%, and 3.4%**
with one, two, and four workers. The columnar path was deliberately excluded
because permuting its flat buffer would require extra storage.

Commit `d84ccc9` replaced the bracket matcher's nested token/range vectors with
one contiguous array of **256-bit allowed-byte sets**, one per position. A
shift-and-mask membership test avoids pointer chasing. Parsing and regex fallback
semantics remain intact. Its trials recorded **37.7 → 36.0 µs/VIN** single-core
and about **80.7k → 86.5k VIN/s** with four workers. The complete corpus JSON
digest matched, with additional comparisons against regex.

### Keep columnar work in Rust

Commit `4bb75a3` (August 21) introduced a bounded parquet pipeline: read a row
group, decode projected element IDs in Rust with the GIL released, and write
Arrow/parquet without per-VIN Python dictionaries. Commit `c767059` projected
parquet input down to the required VIN/year columns, avoiding decoding unrelated
file columns.

Commit `1f2999e` generalized this into `decode_stream`: Arrow C-stream input and
output, parquet sources/sinks, typed selected columns, and stable element-ID
metadata. Arrow interchange avoids conversion through Python row objects; it
does not mean decoding or every buffer operation is zero-copy. Memory is bounded
by batch size and selected columns. The September direct-builder change below
removed further intermediate allocations. No isolated before/after speedup is
recorded here for introducing these APIs.

## September batch 1: index candidates and defer copies

Implementation: **`58be09d`**, September 7. Benchmark refreshes: `f2913e8` and
`50261f1`, September 8. The target was **2× throughput with identical answers**;
the measured improvement fell short of that target.

### What changed and why it works

1. **Match a distinct key once per schema.** Many pattern rows share a key.
   The index groups those rows, tests each key once, then expands a hit back to
   every original row. Restoring global pattern order preserves duplicate and
   tie behavior; grouping must not silently deduplicate result rows.
2. **Choose candidates by one required literal character.** A key containing a
   required literal can be placed in a position/character bucket. A VIN only
   tests the matching bucket plus keys that have no usable literal. The existing
   matcher, including regex fallback, remains the final authority.
3. **Retain global pattern row indices.** Later passes address matches directly
   instead of searching the 1.67-million-row pattern table again. Formula rows
   have a separate index because their eligibility differs, including rows with
   orphan schema IDs.
4. **Index vehicle-spec schemas by make and model.** The join starts from the
   relevant candidates instead of scanning every spec schema. Archive order,
   year, vehicle type, QC, and key-pattern checks still apply.
5. **Delay string copies until the winning pass is projected.** Pattern keys
   and attribute IDs stay borrowed. Losing years and discarded duplicate rows
   never need owned copies.
6. **Use fixed flags for fixed questions.** Error code 14 asks six character
   membership questions; it does not need a set containing every possible
   character. Fixed flags answer exactly those questions.

The new indexes belong to each `Db`, initialize lazily with `OnceLock`, and are
shared by workers. They are derived from that database, preventing archive string
IDs from one artifact from being interpreted against another. Neither the
artifact format nor public result types changed.

Source: [matcher.rs](crates/ultravin/src/matcher.rs),
[db.rs](crates/ultravin/src/db.rs), [decode.rs](crates/ultravin/src/decode.rs),
[errors.rs](crates/ultravin/src/errors.rs), and
[result selection/projection](crates/ultravin/src/lib.rs).

### Results and costs

The September 8 rerun compared `3eafb62` with `f2913e8`: three alternating
60-second windows per build/mode, a warmed 5,000-VIN corpus, the same release
settings, artifact, lockfile, allocator, and Apple M1 Max host. Values are median
VIN/s with all-sample ranges in parentheses.

| Path | Before | After | Speedup |
|---|---:|---:|---:|
| Single core | 29,293 (28,021–30,480) | 45,376 (44,857–46,849) | 1.55× |
| Four-worker batch | 91,206 (90,039–94,582) | 130,381 (125,445–131,708) | 1.43× |

The initial September 7 run used 20-second windows and profiling line tables:
**24,627 → 38,587 VIN/s** single-core and **77,849 → 109,190 VIN/s** batch. It was
more contended; neither these rates nor the later rerun establish the precise
cause of the absolute-rate difference. Both complete sample sets are committed.

The initial run also measured an index cost: median fresh-process load plus
first Honda decode rose **0.675 → 1.597 ms**. Peak RSS rose **95.4 → 129.9 MiB**
single-core and **230.2 → 252.6 MiB** with four workers. Index memory depends on
visited schemas. These resource measurements were not repeated in the September
8 throughput rerun.

All **1,862,306 complete serialized-result fingerprints** matched at a fixed
clock. Tests also compared indexed matching with a row scan, archive spec joins,
duplicate/excluded elements, wildcards/fallbacks, separate databases, concurrent
initialization, and the original error-position algorithm.

## September batch 2: remove remaining work and output allocations

Seventeen performance commits on September 9 followed baseline **`50261f1`
(v2.1.2)**. The overnight target was **3× on both existing Rust throughput paths**,
with unchanged answers, or the morning cutoff. The cutoff was reached; the main
throughput target was not. Faster singleton calls and Arrow streams are separate
workloads and do not satisfy that target.

### Scoring, temporary storage, and conservative pass skipping

- **`682b299`: remove redundant allocation and scoring.** Copy bounded warning
  text at UTF-8 boundaries, skip scoring when there is only one candidate,
  score each competing candidate once, and append formula/default rows directly
  instead of staging another collection.
- **`846737e`: keep common element membership and winner indices inline.**
  Small element IDs use stack storage; larger or negative IDs retain a hash
  fallback. Empty conversion/spec work avoids allocating bookkeeping. The fast
  representation must not silently narrow the explicit-database API's IDs.
- **`a3cd036`: skip only passes that cannot win.** The best completed pass's
  error score provides a floor. Missing-WMI/no-pattern passes have a conservative
  ceiling from errors 7 and 8. Skip only when that ceiling is **strictly below**
  the floor, checking both before core work and after it when necessary. Equal
  error scores still need element weights, pattern counts, model year, and pass
  order to break ties. QC filtering stays after scoring. Correction strings also
  move into the winning result where possible.

### Database lookups without paying for the entire database on first use

- **`5b9afc5`: index WMI row ranges with packed integer keys.** Store the range
  of original rows, including duplicates, instead of repeatedly comparing WMI
  strings. Packing includes length so short strings and embedded NULs cannot
  alias. Publication-date eligibility remains a per-call check.
- **`ffb9914`: initialize WMI indexes by two-character manufacturer prefix.**
  A first decode should not build every manufacturer's index. There are bounded
  alphanumeric prefix slots; only the used prefix initializes. Unusual/long
  strings retain the original search.
- **`5df4f30`: index lookup values and borrow already-uppercase text.** Repeated
  `(table, numeric ID)` name resolution is indexed. ASCII names with no lowercase
  bytes are borrowed; other names use the full Unicode uppercase operation,
  including expansions.
- **`e885313`: replace the initial global lookup map with lazy per-table indexes.**
  Dense ID ranges use array access; sparse tables use bounded binary search.
  Dense allocation is capped and proportional to table rows. First-duplicate
  selection, negative IDs, empty-string values, missing IDs, and custom table
  tags retain their prior behavior. This refinement removes the startup cost
  of indexing every lookup table for one VIN.

### Corrections and scans that do only the required work

- **`e66103d`: delay the mutable corrected-VIN buffer.** Clean VINs do not build
  the character vector used for invalid-character stamping. The first invalid
  character initializes it; subsequent stamps mutate it in place.
- **`00461df`: compact correction character sets.** ASCII membership is a
  `u128` bitset, with a general character-set fallback for non-ASCII data. Cached
  rendering preserves the reference's character order, including underscore
  before digits; bit order is not used as public output order.
- **`ddc6ee3`: index the queried correction positions directly.** The helper
  reads VIN positions 4–14, so an eleven-slot array replaces position-map lookup.
  Missing years/unknown schemas avoid allocating a shared empty map. Schema
  existence checks stop after the first hit. Isolated paired 20-second trials
  recorded about **1.6% single-core** and **0.8% four-worker** improvement.
- **`981186c`: prepare formula keys lazily and stop completed position scans.**
  Substitute formula keys only after finding an eligible formula row. Stream
  matched keys into the unused-position check and stop once every relevant
  position is covered. The commit records approximately **1.5% throughput** from
  lazy formula preparation in both Rust paths, and **0.9% single-core** from the
  shorter position scan, measured as separate incremental experiments.

### Construct the output the caller requested

- **`4aa9328`: select the winning year before output-specific projection.**
  `RawResult` separates decode/scoring from full, flat, and selected-column
  projection. Flat and column consumers avoid building provenance objects they
  would discard. Column projection resolves only requested values after the
  winner is selected; it does not drop scoring inputs early.
- **`d45a4de`: group adjacent flat values when public variable names are unique.**
  Stable element ordering makes adjacent grouping sufficient for the usual
  archive. Databases with duplicate variable names retain name-based grouping.
  First values and repeated-note ordering remain intact. Incremental wheel
  medians were **59,181 → 63,840 VIN/s** single-flat and **127,271 → 131,206**
  batch-flat.
- **`5013273`: write borrowed values directly into Arrow builders.** Decode into
  bounded 256-row typed chunks, then assemble one output column at a time.
  Avoid a whole-batch row-major enum matrix and intermediate owned strings.
  The column path preserves first-occurrence behavior even when that first value
  is null, plus row order, types, metadata, and batch boundaries. Full results
  sort small element keys before constructing the larger projected records.

### Reduce Python work under the GIL

- **`a28ebb3`: reuse the finite set of Python variable-name strings** across flat
  dictionaries rather than allocating the same keys for every VIN.
- **`0bdb8ea`: copy private full-result dictionary templates.** Each element has
  a fixed 15-key layout and immutable metadata. Copy it, then fill changing fields.
  Returned dictionaries are independent, and insertion order is preserved.
  Release cache borrows before Python allocations can invoke reentrant callbacks.
  Incremental wheel medians were **33,150 → 38,208 VIN/s** single-full and
  **34,646 → 40,979 VIN/s** batch-full. These thread-local Python-object caches
  carry the binding's existing subinterpreter limitations; they are not evidence
  of support for independent interpreter GILs.
- **`0711a23`: bypass Rayon dispatch for one-VIN Python batches** using the
  existing single decoder. Year validation, GIL release, and list/JSON-array
  return shapes remain the same. Private templates also reuse fixed source
  labels. Against the immediate preceding candidate, flat dictionary calls fell
  **50.31 → 16.05 µs**, and JSON calls **46.72 → 15.52 µs**. The whole-batch
  before/after measurements below have a different baseline.

Source: [core and projection](crates/ultravin/src/lib.rs),
[inline sets/indexes](crates/ultravin/src/hash.rs),
[database indexes](crates/ultravin/src/db.rs),
[correction handling](crates/ultravin/src/errors.rs),
[column decoding](crates/ultravin/src/ids.rs),
[Arrow builders](crates/ultravin/src/arrow_io.rs), and
[Python bindings](crates/ultravin-py/src/lib.rs).

### Rust results: two separately paired measurement sessions

Both sessions used three alternating 60-second windows per build/mode on the
same 5,000-VIN corpus and Apple M1 Max. Values are median VIN/s and ranges.

| Session / path | Baseline | Candidate | Speedup |
|---|---:|---:|---:|
| Overnight / single | 46,626 (46,414–46,666) | 73,209 (72,842–73,646) | 1.570× |
| Overnight / four workers | 130,604 (130,213–130,914) | 177,788 (177,138–178,401) | 1.361× |
| Later rerun / single | 45,990 (45,120–46,192) | 76,581 (75,860–77,093) | 1.67× |
| Later rerun / four workers | 128,998 (128,573–130,517) | 181,135 (180,951–182,849) | 1.40× |

The overnight report identifies candidate `e4e35c27c1fad7ea5335038c40ba660ccbc0276a`
before the changes were rebased into the commit IDs listed above, using Rust
1.90.0. The later committed rerun identifies `ddc6ee3` plus the FxHasher
`as_chunks` adjustment subsequently committed as `2b56fbf`, using Rust 1.98.1.
Both used baseline `50261f1` and matched the toolchain, artifact, harness,
allocator, and release profile within their own pair. The later rates are not a
controlled measurement of the compiler change alone.

In the later run, median process CPU cost fell **21.91 → 13.16 µs/VIN** single
and **24.68 → 16.09 µs/VIN** batch. Process CPU includes startup, warmup, and
teardown; it is not single-call latency. The overnight run's median peak RSS
fell **130.0 → 128.0 MiB** single and **252.3 → 241.2 MiB** with four workers.

### Overnight Python and Arrow results

Separate release wheels, CPython 3.13.5, PyArrow 25.0.0, and three alternating
20-second trials per build/mode. Ordinary batches contain 5,000 VINs. Arrow
streams contain 65,536 rows with either six fields or all 140 public fields.
Values are median VIN/s with ranges; RSS is median process peak in MiB.

| Path | Before | After | Speedup | RSS before → after |
|---|---:|---:|---:|---:|
| Single flat dict | 34,747 (33,988–34,867) | 64,408 (64,268–64,891) | 1.854× | 153.7 → 151.7 |
| Single full dict | 25,780 (25,250–25,972) | 39,908 (39,137–39,982) | 1.548× | 153.6 → 151.9 |
| Single flat JSON | 36,606 (35,996–36,816) | 65,979 (65,110–67,411) | 1.802× | 154.1 → 151.7 |
| Single full JSON | 27,809 (27,594–27,811) | 35,760 (35,679–35,945) | 1.286× | 154.5 → 152.9 |
| Batch flat dict | 75,286 (74,229–76,426) | 130,207 (130,102–130,594) | 1.729× | 245.8 → 214.5 |
| Batch full dict | 32,315 (30,437–32,520) | 42,980 (42,838–43,286) | 1.330× | 421.5 → 402.4 |
| Batch flat JSON | 131,845 (130,984–132,214) | 224,711 (224,431–225,697) | 1.704× | 234.0 → 221.8 |
| Batch full JSON | 77,248 (76,962–77,715) | 92,034 (91,683–92,137) | 1.191× | 535.8 → 535.5 |
| Arrow, six fields | 175,406 (174,594–175,840) | 382,182 (380,151–382,611) | 2.179× | 241.7 → 208.9 |
| Arrow, 140 fields | 121,698 (117,240–122,033) | 272,407 (267,861–274,281) | 2.238× | 696.3 → 566.1 |

Small Python batches used three alternating three-second trials, cycling through
the corpus. These measure **µs per call**, not VIN/s:

| Shape / VINs per call | Before median (range) | After median (range) | Speedup |
|---|---:|---:|---:|
| Flat dict / 1 | 67.01 (64.50–67.36) | 15.85 (15.81–15.90) | 4.227× |
| Flat dict / 2 | 105.94 (104.39–109.34) | 77.78 (76.96–78.23) | 1.362× |
| Flat dict / 4 | 142.28 (141.39–143.73) | 105.32 (104.62–106.06) | 1.351× |
| Flat dict / 16 | 293.62 (291.90–303.21) | 208.85 (207.12–210.48) | 1.406× |
| Flat JSON / 1 | 61.49 (61.41–61.97) | 15.33 (15.29–15.39) | 4.012× |
| Flat JSON / 2 | 96.18 (95.42–96.49) | 63.32 (61.66–63.46) | 1.519× |
| Flat JSON / 4 | 115.75 (114.70–120.32) | 83.44 (82.90–84.16) | 1.387× |
| Flat JSON / 16 | 230.84 (230.80–234.08) | 153.27 (152.74–158.25) | 1.506× |

### Worker count, broader input, and startup costs

Worker-count trials used three alternating 55-second windows and the same
5,000-VIN Rust batch corpus. The broader test used three alternating 60-second
windows on 100,000 distinct, 17-byte ASCII VINs without CR/LF, sampled
deterministically from compatibility cases without caller-supplied years. It is
a stress corpus, not a model of production traffic.

| Workload | Before VIN/s (range) | After VIN/s (range) | Speedup | Peak RSS MiB before → after |
|---|---:|---:|---:|---:|
| Batch, 1 worker | 43,170 (43,118–43,657) | 63,877 (63,557–64,366) | 1.480× | 199.5 → 194.4 |
| Batch, 8 workers | 192,578 (192,319–198,243) | 251,293 (242,664–260,078) | 1.305× | 316.0 → 296.5 |
| Batch, 10 workers | 199,506 (194,923–202,194) | 255,235 (254,886–256,450) | 1.279× | 349.9 → 316.6 |
| Broader corpus, single | 46,979 (46,873–47,204) | 71,934 (71,328–72,282) | 1.531× | 197.0 → 191.2 |
| Broader corpus, 4 workers | 141,487 (140,125–142,298) | 185,706 (184,932–186,102) | 1.313× | 1714.2 → 1673.9 |

First-decode tests used eleven alternating fresh processes per VIN. Filesystem
caches were not flushed. Both Rust startup binaries used the system allocator;
throughput binaries used mimalloc. Rust load/first-result medians and ranges:

| VIN | Before ms (range) | After ms (range) |
|---|---:|---:|
| Ford `1FTFW1ET5DFC10312` | 3.210 (3.086–25.769) | 3.152 (3.087–5.290) |
| Honda `1HGCM82633A004352` | 1.545 (1.401–42.154) | 1.552 (1.447–5.225) |
| Unknown WMI `ZZZCM82633A004352` | 0.150 (0.127–3.229) | 0.069 (0.059–0.189) |

Python's first Honda call **after import** became about 0.1 ms slower: flat
**1.425 (1.396–1.775) → 1.541 (1.498–1.891) ms** and full
**1.437 (1.397–1.480) → 1.542 (1.481–1.728) ms**. Separate measurements including
package import but excluding interpreter startup were **4.200 → 4.135 ms** flat
and **4.176 → 4.269 ms** full. These are different timing boundaries; neither
establishes cold-storage latency. The steady-state improvements have a small
first-call cost on this case.

## September 9 follow-up: index joins and narrow matching

Commit **`54cbf7f`** compares against the starting September 9 revision
**`d536710`**. The additional **2× throughput target was not reached**.

- Compact, bounded indexes replace repeated schema-position, WMI/schema and
  model/make searches. Sparse IDs retain the original searches; duplicate rows
  retain their original selection and order. Normalized engine names are
  indexed once per database, keeping the first match.
- Pattern buckets use the first and last required literal bytes to narrow
  candidates before the existing matcher checks them.
- Plain ASCII correction keys use direct character membership checks; bracket
  and Unicode keys keep their expansion. Public output sort keys are precomputed.

Three alternating 60-second windows per build/mode used the unchanged 5,000-VIN
corpus, Apple M1 Max, Rust 1.98.1, artifact, allocator, lockfile and release
settings. Values are median VIN/s with all-sample ranges.

| Path | Before | After | Speedup |
|---|---:|---:|---:|
| Single core | 71,556 (70,903–74,751) | 84,822 (84,806–87,979) | 1.19× |
| Four-worker batch | 178,083 (174,951–179,079) | 195,203 (194,519–195,458) | 1.10× |

A separate 100,000-VIN stress sample measured **1.16× single-core and 1.08×
batch** gains in two alternating 20-second windows per build/mode. It is not a
fleet distribution. All **1,862,306 complete serialized-result fingerprints**
matched the starting revision. `make check checku` passed with 160 Rust tests and
825 Python tests.

The indexes add about **0.3 ms** to the first decode on the two registered WMIs
measured: Honda **1.459 → 1.755 ms**, Ford **3.082 → 3.367 ms**. These are medians
of seven alternating fresh processes per VIN/build using the system allocator;
filesystem caches were not flushed. A 64-pattern bitmap matcher and copied
output-metadata templates were rejected because they did not establish a useful
batch improvement. Hash indexes that added about 1.3 ms to startup were replaced
with the compact arrays.

The [follow-up report](docs/THROUGHPUT_2026_09_09_FOLLOWUP.md) records the full
methodology, reproduction commands and limits. These gains are relative to this
round's baseline; they must not be multiplied by earlier incremental results.

## Experiments we rejected or replaced

Keeping these failures is part of preserving the performance work. A compatible
rewrite with no established end-to-end benefit did not qualify for retention.

| Experiment | Outcome and lesson |
|---|---|
| Disable correction/error machinery | Earlier ablation gained about 1.20× but changed attributes for 0.8% of VINs because errors affect year selection. Reuse its work; do not omit it. |
| Disable the ambiguous fourth year pass | Earlier ablation gained about 1.42× but changed model year for 13.3% of VINs. September pruning instead requires a strict proof that a particular pass cannot win. |
| Intern all value/attribute-ID/source Python strings | Earlier experiment gained only 2–3% and was reverted; dictionary stores remained the dominant cost. September's fixed source labels and copied layouts are narrower changes with separate measurements. |
| Force contiguous WMI-sorted worker chunks | Lost load balance: the August experiment recorded about 79k versus 86k VIN/s. Sorting for locality was retained without forcing those worker boundaries. |
| Reuse truncated warning buffers | No useful gain; long malformed inputs left oversized allocations retained. Keep bounded copies instead. |
| Typed Arrow chunks alone | Increased peak memory. Retained only as part of the direct-builder implementation that eliminated other intermediate storage. |
| Deduplicate error keys with a per-pass hash set | Slower than streaming the keys through the existing checks. |
| Choose rare-literal pattern buckets | Slower than the simpler required-literal index. More selective indexing was not automatically cheaper overall. |
| Decimal parser and checked-integer arithmetic rewrite | Passed compatibility cases but established no end-to-end speedup; not retained. |
| Extra VIN buffers, stack correction prefixes, bounded conversion-source writers, early conversion filters | No useful measured improvement in the combined candidate; not retained. |
| Build all lookup/WMI indexes at first use | Startup regressed. Replaced with lazy per-table and per-prefix initialization. |
| Inline schema maps with hash-map overflow | Final isolated experiment slowed single-core throughput 0.46% and four-worker throughput 0.76%; simpler maps remain. |

The earlier ablations and Python interning experiment are recorded in
[BENCHMARKS.md](docs/BENCHMARKS.md). The September experiment outcomes above were
transcribed from the overnight report/journal so they survive deletion of local
build artifacts.

## SQL oracle and development resources

### Make bulk differential testing cheaper

This work accelerated the **reference database used to test ultravin**. It is
separate from the Rust decoder speedups. Commit `8b1fd0b` (July 27) applied the
Postgres tuning ladder; [ORACLE_TUNING.md](docs/ORACLE_TUNING.md) records the
Postgres and SQL Server experiments and reproduction details.

The July experiments used dedicated eight-vCPU AMD EPYC `c7a.2xlarge` hosts,
5,000 VINs, warmup, and 60-second windows. Historical final paired rates:

| Engine | Single connection, before → after VIN/s | Eight connections, before → after VIN/s |
|---|---:|---:|
| Postgres 16 | 13.1 → 52.4 (4.0×) | 76.9 → 413.6 (5.4×) |
| SQL Server 2022 | 38.9 → 40.3 (1.04×) | 194.2 → 285.2 (1.5×) |

The Postgres techniques were:

- **Debian/glibc instead of Alpine/musl:** the image swap alone improved the
  measured allocation-heavy PL/pgSQL workload about 72% on that x86 host.
- **Reuse temporary tables:** stock procedures create/drop 9–20 per decode,
  causing catalog writes, invalidations, and replanning. The mechanical load-time
  rewrite uses `ON COMMIT DELETE ROWS` and explicit `DELETE` instead of drop,
  preserving table identity and cached plans. The measured step improved single
  throughput about 75% and eight-connection throughput about 124%.
- **Batch ten decodes per transaction:** amortize commit overhead after applying
  the temp-table rewrite. Ten helped; 100 was slower and caused more ordering
  differences among tied rows. Lock capacity was increased for batching.
- **Tune the disposable oracle's storage/configuration:** relaxed durability
  and tmpfs supplied smaller gains. Unix sockets measured approximately no gain
  and were not applied locally. Oversubscribing past eight connections on the
  eight-vCPU host reduced throughput.

Current repository configuration uses `postgres:16`, relaxed durability,
1 GB shared buffers, a 2 GB WAL cap, and tmpfs only on the primary oracle.
**Autovacuum remains enabled**: disabling it during the initial ladder caused
catalog growth and disk exhaustion in bulk runs. The configuration is for
reloadable test data; primary-oracle data is lost on container restart.
`ULTRAVIN_ORACLE_FAST_PROCS=1` enables the procedure transform at load time;
`ULTRAVIN_ORACLE_BATCH=10` enables transaction batching. Both are opt-in in the
general scripts. Later answer-key automation explicitly enabled the rewrite
behind an equivalence gate (`effcfec`), so the old tuning document's blanket
statement that answer-key builds use untouched procedures is historical.

The rewritten and stock procedures had **zero content mismatches over 5,292
VINs**, with nine order-only differences among tied rows. This is canonical
content equivalence, not the strict serialized-byte agreement required of the
September Rust optimizations. A local 399-VIN check also passed. Local Docker
Desktop gains were smaller: **10.0 → 17.6 VIN/s** single and **25.5 → 52.3** with
four connections. Do not transfer the cloud multipliers to the Mac.

For SQL Server, the experimental recipe kept delayed durability, tmpfs/sizing
for tempdb, a warmed memory limit, trace flag 8008 to stabilize scheduler
assignment, and compatibility level 160. Memory-optimized tempdb metadata
regressed, whole-database tmpfs did not help, and host networking had no material
effect. These are recorded experiments, not changes to ultravin's decoder or a
claim that the current local SQL Server uses that recipe. The stored procedures
were unchanged; the experiment's restart checks covered only three canonical
VINs, a much narrower correctness sample than the Postgres equivalence test.

### Keep development artifacts smaller

Commit `07b7974` (August 8) set development debug information to
`line-tables-only`, retaining file/line backtraces without full type and variable
debug information. It reduces development artifact storage; no isolated build
time or decoder speedup is claimed. Release optimization remains level 3, fat
LTO, and one codegen unit. The i686 wheel later received a separate release-build
resource exception (`1101bc8`); it is not evidence of faster decoding.

## Correctness and measurement rules

The September batches optimize the existing behavior. They do not add a
whole-VIN result cache, remove errors, omit scoring inputs, change the embedded
artifact format, or change requested output shapes.

- **Compare the complete result at a fixed clock.** The 1,862,306-case suite
  includes original VINs, malformed/Unicode/partial/overlong inputs, deterministic
  mutations, and caller-year hints. Serialize row order, provenance, errors, and
  corrections as well as values. No normalization or upstream-defect exemptions
  are used for that comparison. It establishes equality on those cases, not all
  possible inputs or a fresh SQL-oracle comparison.
- **Validate each public representation.** The overnight batch additionally
  matched Python dictionaries and raw JSON over 277,315 cases, Arrow over
  277,316 rows including a null VIN, and singleton batches over 5,043 VIN/year
  cases in all four dict/JSON shapes. Arrow comparison canonicalized only
  metadata-map order, which varied between unchanged baseline processes;
  field order, types, entries, values, and batch boundaries stayed fixed.
- **Check sharing and mutation.** A 2,048-case comparison used one and eight
  concurrent Python callers and mutated returned dictionaries before repeated
  calls. Private cached layouts must never become shared mutable caller results.
- **Keep general fallbacks.** Dense/ASCII/unique-name specializations must keep
  behavior for sparse and negative IDs, Unicode, duplicate variable names,
  unusual WMI strings, duplicate rows, and publication dates.
- **Time paired release builds.** Use the same artifact, harness, lockfile,
  allocator, worker count, and compiler within a pair. Warm the whole corpus,
  alternate order, retain every sample, and report medians and ranges. Stop
  builds, tests, and profilers while timing; report that the host is shared.
- **Measure the cost moved elsewhere.** Check startup and peak RSS as well as
  throughput; separate Rust work from Python output and Arrow/parquet I/O. Time
  Python release wheels in separate environments, not the development extension.
- **Run the repository checks.** All three rounds passed `make check checku` in their
  recorded final states. The overnight result included 825 passing Python tests
  plus Rust tests, feature configurations, formatting, lints, and type checking.

## Evidence and reproduction

The committed sources of historical measurements are:

| Evidence | Contents |
|---|---|
| [BENCHMARKS.md](docs/BENCHMARKS.md) | Earlier rounds, API tradeoffs, ablations, and benchmark methodology |
| [THROUGHPUT_2026_09.md](docs/THROUGHPUT_2026_09.md) | Both September Rust batches, resource costs, and exact reproduction commands |
| [throughput_2026_09.json](scripts/bench/throughput_2026_09.json) | Initial September 7 samples, resources, and correctness digests |
| [throughput_2026_09_08.json](scripts/bench/throughput_2026_09_08.json) | Batch 1's later paired 60-second rerun |
| [throughput_2026_09_09.json](scripts/bench/throughput_2026_09_09.json) | Batch 2's later paired 60-second rerun, build/input hashes, and CPU measurements |
| [THROUGHPUT_2026_09_09_FOLLOWUP.md](docs/THROUGHPUT_2026_09_09_FOLLOWUP.md) | Follow-up changes, paired results, startup costs, and reproduction commands |
| [throughput_2026_09_09_followup.json](scripts/bench/throughput_2026_09_09_followup.json) | Follow-up raw samples, broader-corpus results, startup measurements, hashes, and correctness evidence |
| [ORACLE_TUNING.md](docs/ORACLE_TUNING.md) | SQL tuning ladders, rejected steps, equivalence limits, and local application |

The original overnight `REPORT.md`, `STATUS.md`, trial JSON, Python/Arrow runners,
and `final-provenance.json` remain in the **ignored local directory**
`target/overnight-20260909/`. They are not available from a fresh clone. This file
preserves their optimization rationale, rejected experiments, and principal
measurements; it does not archive all raw logs or executables. Overnight numbers
are labeled separately from the later committed rerun for that reason.

The overnight report records these SHA-256 identities:

```text
2026_08 artifact:
2d555d4db3fde3867f41e415ec6e848285a286b2fe5fb45f0bdcb88245bd7787
5,000-VIN speed corpus:
b45ad4472c202ee176f86e8bc3c39609c76b6258df963ea04e08439aa3a1eb09
Matching complete-result fingerprint file:
e985b2617222a111cb2444b02af3fa62e0f7bfb18345170dbd6829e9e047ba52
100,000-VIN broader corpus:
f3f4211f9faf9048736ac9a76f20ee72bb8ae85c1738814bcbe024f94762f1df
```

Use the matching dated reproduction section in
[the September report](docs/THROUGHPUT_2026_09.md) to rebuild a historical pair.
The committed [comparison runner](scripts/bench/compare.py) and
[fingerprint case generator](scripts/bench/fingerprint_cases.py) are the starting
points for a new experiment. Do not substitute a development build, change
worker count between sides, or treat this document's old measurements as a new
benchmark of later code.

Commit IDs throughout this document are searchable with `git show <commit>`.
Some early paths were named `crates/ultravin-core`; current source links use the
renamed `crates/ultravin` directory. When adding another optimization, record the
bottleneck, mechanism, correctness boundary, paired measurement, startup/memory
cost, rejected alternatives, and commit/evidence pointers here.
