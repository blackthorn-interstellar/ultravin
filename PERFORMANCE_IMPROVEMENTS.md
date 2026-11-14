# Performance improvements

This is the engineering history of ultravin's performance work through September
11, 2026: what was expensive, what changed, what we measured, and what must remain
true when changing it again. It covers the decoder, Python output, Arrow/parquet,
corpus generation, build resources, and the SQL oracle used for differential
testing. Each dated optimization round has its own section.

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
- [September 10: direct full JSON output](#september-10-direct-full-json-output)
- [September 11: check digits and result projection](#september-11-check-digits-and-result-projection)
- [September 22-23: deferred defaults and retuned slot plans](#september-22-23-deferred-defaults-and-retuned-slot-plans)
- [September 25: sorted-input instruction cuts](#september-25-sorted-input-instruction-cuts)
- [November 11: key positions checked in place](#november-11-key-positions-checked-in-place)
- [November 13: native stream wakes only the worker that can go on](#november-13-native-stream-wakes-only-the-worker-that-can-go-on)
- [November 13: cache hits share the element list](#november-13-cache-hits-share-the-element-list)
- [November 13: batch calls run on the native stream](#november-13-batch-calls-run-on-the-native-stream)
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

## September 10: direct full JSON output

Commit **`4117c1a`**, against **`8bce73a`**, improves the existing full JSON APIs
by **1.98× single-core** and **1.67× with four workers**.
These are medians of three alternating 60-second windows on the same 5,000 VINs,
with matched artifact, compiler, allocator, lockfile and release settings.
The roughly 2× gain describes full JSON decoding, including encoding, rather
than every output representation.

The JSON writer copies fixed, pre-escaped element metadata into a correctly sized
buffer and writes variable values directly from the winning decode items.
Matching, year selection, errors, corrections, output order and all provenance
remain identical. WMI string preparation and fixed source-label checks also
reduce work in the common decode path. No new public API or decoded-VIN cache
was added.

All 1,862,306 complete-result fingerprints match the baseline, and all 1,862,306
JSON outputs match serde byte-for-byte. `make check checku` passed with 168 Rust
and 825 Python tests. The [full report](docs/THROUGHPUT_2026_09_10_JSON.md) records
ordinary-struct controls, the broader stress sample, peak RSS, startup, rejected
experiments, exact samples and reproduction commands.

## September 11: check digits and result projection

Against `38fb2ff`, precomputed check-digit contributions, vectorized text cleanup
and direct movement of output fields improved full Rust-result throughput by
**6.3% single-core** and **5.3% with four workers**. The **2× target was not met**.
Three alternating 60-second windows per build and mode measured medians of
**80,768 → 85,847 VIN/s** and **178,326 → 187,707 VIN/s**, respectively.

The 100,000-distinct-VIN stress corpus improved **4.5% single-core** and **1.7%
batch**. All **1,862,306 complete-result fingerprints** matched the baseline;
`make check checku` passed with 170 Rust tests and 825 Python tests. Public APIs
and output ownership remain unchanged. The
[September 11 report](docs/THROUGHPUT_2026_09_11.md) records the measurements,
startup/memory checks, rejected experiments and reproduction commands, with
[raw evidence](scripts/bench/throughput_2026_09_11.json).

## September 22-23: deferred defaults and retuned slot plans

Commits `6712776` through `5750746`, against `35ff816`. The target was 2× on
the headline benchmark; it was not reached. The [September 23
headline](docs/BENCHMARKS.md#headline-throughput-september-23-2026) measures
1.57× on one worker, 1.79× on four, and 1.61× on twelve.

- **Defaults leave the passes.** 44 of 58 output elements per headline VIN are
  vehicle-type defaults. When no default row targets an element that errors,
  scoring, or corrections read (checked once per database), passes carry only
  the vehicle type: scoring adds the applicable rows' element weights, and the
  winner copies finished default records bitwise (embedded database, full
  results) or inserts the items exactly where the core pass would have (every
  other output shape). A unit test compares both paths over the built-in cover.
- **Shared texts.** AdditionalDecodingInfo without invalid characters is a
  function of five flags and the unused-position set, so it is built once per
  combination; make id text and uppercase names come from a per-database table.
- **Waste removed.** ASCII truncation by byte instead of `char_indices().nth`;
  word-at-a-time tab/CR/LF scrub; projection by a precomputed per-element output
  rank bitmap instead of a sort; records written into reserved capacity; one
  scan for error item facts; a hash prefilter for check-digit exception VINs.
- **Compact lookups.** Pattern literal buckets in one open-addressed table;
  correction charsets frozen into inline ASCII masks plus one rendered string
  (peak RSS 398 → 378 MiB on 2M VINs).
- **Slot plans re-measured.** On the faster decoder, plans with ~64-96
  in-flight rows per worker lead; the table now picks 32×3 at four workers
  (+10%), 16×4 at eight (+20%), and 24×4 at twelve (+2%, within noise).
  [Raw runs](scripts/bench/native_plan_grid_2026_09_23.json).

Steady-state instructions per VIN on the native path fell from 62.6k to about
33k. About a third of the remaining single-core time is cache misses in
database lookups: the same build runs ~1.5× faster when each VIN repeats four
times in a row. Rejected as within noise: flattening per-schema key groups into
one allocation, probing all literal buckets before matching, a per-database
conversion-source prefilter, presizing error-byte buffers, a larger key
expansion memo, and unchecked projection-order writes. Correctness: all
1,862,306 full-result fingerprints and 298,507 headline-corpus fingerprints
matched the baseline after every change; `make check` passed on each commit.

## September 25: sorted-input instruction cuts

Commits `03b82d3` through `56cf2e9`, against `c7cea32`. Sorted input decodes
at about 3.5 instructions per cycle, so instruction cuts show up there almost
one for one; shuffled input also waits on memory, which these changes do not
address.

- **Output projection.** Each vehicle type's default records are stored in
  output-rank order with their mask bits, and items are ordered by rank (a
  bitmap, or a small sort when notes repeat). A full result is a plain merge of
  the two, including results with repeated elements, which previously sorted
  items and default records together.
- **Table lookups instead of searches.** Default masks are one OR per item over
  a per-type element-to-row-bits table; element lookup tags come from an array
  instead of comparing table names; each WMI row's make lists are computed
  once; items no conversion reads skip the conversion scan.
- **Scrub once.** Dense lookup indexes mark names holding a tab or line break,
  and value resolution scrubs every value, so full results skip a second scan.
- **Smaller costs.** Recycled native slots skip the drop checks of default
  record copies (recognized by a uniquely addressed static source text); the
  pattern-scan pool and the var-keys text avoid redundant clearing and UTF-8
  validation.

Steady-state instructions per VIN on the native path fell from 33.1k to 26.8k.
Paired runs on 2M- and 8M-VIN slices of the realistic corpus, Apple M2 Max,
one-minute load 3-5 from other work:

| workers | input | before VIN/s | after VIN/s | ratio |
|---:|---|---:|---:|---:|
| 1 | sorted | 369,519 | 466,631 | 1.26× |
| 1 | shuffled | 220,864 | 262,075 | 1.19× |
| 4 | sorted | 1,413,126 | 1,769,212 | 1.25× |
| 4 | shuffled | 840,795 | 959,682 | 1.14× |
| 12 | sorted | 12.35k cycles/VIN | 10.42k cycles/VIN | 1.19× |
| 12 | shuffled | 20.39k cycles/VIN | 18.29k cycles/VIN | 1.11× |

Twelve-worker throughput swung with host load during the run, so that row
compares cycles per VIN. All 1,862,306 compatibility fingerprints, 298,507
headline-corpus fingerprints and 200,000 realistic-corpus fingerprints matched
the baseline after every change; `make check` passed on each commit. The
full 20M-VIN [headline rerun](docs/BENCHMARKS.md#headline-throughput-september-25-2026-commit-56cf2e9)
paired this build with the previous decoder: 1.12–1.30× fewer cycles per VIN.

## November 11: key positions checked in place

- **E6 unused-position check without the key memo.** On shuffled input the
  check spent its time in a per-thread `HashMap<String, Rc<[(i32, char)]>>`
  of expanded pattern keys: a string-keyed probe about 1.2 times per VIN, and
  a regex compile on each of ~93k misses per 4M VINs, because every job starts
  on a fresh worker thread. The memo is gone. Literal keys still index their
  bytes; a key with `[...]` groups is walked in place, a group admitting the
  VIN character by list membership or, for `-` ranges, by comparing against
  the ranges over `VALIDCHARS`, which is the answer the regex gave. Any other
  class shape (`^`, a stray `-`, reversed ranges; the archive has none) still
  goes to the regex. No new tables, no load-time work, and no per-thread
  memory. A unit test checks the walk against the `fValidCharsInKey`
  expansion for all 74,849 distinct archive keys, at every E6 position and
  for 129 probe characters. All 1,862,306 compatibility fingerprints, 298,507
  headline-corpus fingerprints and 200,000 realistic-corpus fingerprints
  matched the baseline. Paired run on a 2M-VIN shuffled slice, one worker,
  four alternating runs each, Apple M2 Max, load about 2: 270.5k → 282.2k
  VIN/s (+4.3%), 12.37k → 11.85k cycles/VIN (−4.2%), 28.24k → 27.52k
  instructions/VIN (−2.5%). Every candidate run beat every baseline run.

## November 12: result reuse cache

- **Bottleneck.** Real fleets repeat: in New York's 11.9M-VIN registration
  file about 320k keys occur and 97% of VINs share theirs with an earlier
  VIN, yet every VIN paid a full decode (~21k cycles in file order).
- **Mechanism.** Each native stream owns one direct-mapped table
  ([result_cache.rs](crates/ultravin/src/result_cache.rs)) of clean full
  results, shared by all its workers and dropped when the stream ends. A hit
  copies the stored result into the recycled output slot and patches `vin`.
  Entries are immutable once published; each slot is an `arc-swap`
  `ArcSwapOption`, so a probe borrows the entry through a thread-local debt
  and a hit writes no shared cache line, while a store swaps in a fresh entry
  (the displaced one is freed when its last reader lets go). Two workers
  missing the same key both decode and both store; the last one wins and
  nobody waits. Table size comes from `ULTRAVIN_RESULT_CACHE` (total entries,
  rounded to a power of two, 0 disables), default 16,384. The slot array
  (8 bytes per slot) allocates on the first probe, entries as they are stored.
- **Key.** The sanitized VIN with position 9 masked; positions 12-17 with
  digits collapsed to `#`, except positions 12-14 of a `9` WMI and any
  position up to `Db::wmi_key_depth` (the deepest position any pattern key of
  any schema of any row of that WMI constrains, computed lazily per WMI;
  53 WMIs reach 12-14, none reach 15-17); the caller year. A table serves one
  stream, so one database and one clock. Only 17 valid characters, a valid
  check digit (computed without the car flag: a car WMI with a letter at 13
  is never clean, so never stored) and not a check-digit exception are
  probed; only results with codes exactly `[0]` are stored.
- **Admission.** Per worker, as are the WMI depth memo and the hit counters.
  A store costs about a fifth of a decode. While fewer than a quarter of the
  worker's recent stores paid off, only every 64th VIN is probed. Sharing
  needed no retuning: a worker's hits count entries any worker stored, so the
  ratio tracks the shared table, and on shuffled NY at 12 workers and 16,384
  entries the policy hits 49.5% against 50.7% with it disabled.
- **Correctness.** Debug builds re-decode every hit and assert equality, so
  the Rust suite is a differential test (unit tests stream cover VINs and
  serial siblings at 1, 2 and 16,384 entries, four workers overwrite a
  1- and 2-entry table sixteen times over, and recycled rows alternate
  uncopied hits, copies, errors and malformed input). Release whole-corpus equality
  (`examples/result_cache_check.rs`) against uncached `decode_full`: zero
  mismatches over sorted and shuffled NY and 5M realistic VINs at 1 and 12
  workers and 1, 2, 64 and 16,384 entries, plus the admission policy
  disabled at 12 workers and 2 entries (constant overwrite under readers:
  10.2M hits on sorted NY) and 65,536 entries (8.97M hits on shuffled NY).
  Fingerprints unchanged.
- **Results.** New York registrations, one run per cell. "Per worker" rows
  are the previous design (64 entries, or 65,536, per worker), rerun back to
  back with the shared table; "none" rows are the build before the cache:

  | workers | input | entries | VIN/s | cycles/VIN | instr/VIN | hits | peak RSS |
  |---:|---|---:|---:|---:|---:|---:|---:|
  | 1 | sorted | none | 159,869 | 21.0k | | | 1.2 GB |
  | 1 | sorted | 64 per worker | 1,132,301 | 3.0k | 10.3k | 94.0% | 1.2 GB |
  | 1 | sorted | 1,024 | 1,155,605 | 3.0k | 10.0k | 94.7% | 1.2 GB |
  | 1 | sorted | **16,384** | 1,130,883 | 3.0k | 9.9k | 94.8% | 1.4 GB |
  | 1 | sorted | 65,536 | 1,109,827 | 3.1k | 10.0k | 94.8% | 2.3 GB |
  | 1 | shuffled | none | 102,890 | 32.5k | | | 1.2 GB |
  | 1 | shuffled | 1,024 | 102,445 | 32.6k | 79.4k | 0.4% | 1.2 GB |
  | 1 | shuffled | **16,384** | 150,186 | 22.3k | 45.1k | 50.1% | 1.2 GB |
  | 1 | shuffled | 65,536 | 242,569 | 13.8k | 25.5k | 74.4% | 1.8 GB |
  | 12 | sorted | none | 1,310,992 | 25.4k | 79.5k | | 1.3 GB |
  | 12 | sorted | 64 per worker | 4,061,914 | 7.7k | 13.5k | 89.9% | 1.4 GB |
  | 12 | sorted | 1,024 | 4,320,316 | 7.3k | 10.8k | 94.0% | 1.4 GB |
  | 12 | sorted | **16,384** | 4,169,419 | 7.4k | 10.7k | 94.1% | 1.9 GB |
  | 12 | sorted | 65,536 | 4,151,626 | 7.3k | 10.7k | 94.1% | 2.8 GB |
  | 12 | shuffled | none | 790,892 | 40.5k | | | 1.3 GB |
  | 12 | shuffled | 65,536 per worker | 1,249,963 | 26.3k | 39.1k | 55.8% | 11.6 GB |
  | 12 | shuffled | 1,024 | 830,023 | 40.3k | 80.0k | 0.2% | 1.4 GB |
  | 12 | shuffled | **16,384** | 887,614 | 34.6k | 46.7k | 49.5% | 1.7 GB |
  | 12 | shuffled | 65,536 | 1,535,066 | 21.6k | 26.3k | 73.6% | 2.3 GB |

  Sharing lifts twelve workers on sorted input from 90% to 94% hits, the
  one-worker rate, and cuts 20% of instructions; cycles fall only 5%, because
  at twelve workers a hit is bound by copying 14 KB through the memory
  system (7.4k cycles per VIN against 3.0k on one worker). On shuffled input
  one shared table of 65,536 entries beats twelve private ones of 65,536 at a
  fifth of the memory. The default is 16,384 entries: sorted input saturates
  by 1,024, shuffled input needs tens of thousands, and 16,384 caps the table
  near 230 MB while taking a third off shuffled cycles on one worker and a
  seventh on twelve. On the synthetic 2M slice, where nothing repeats, 16,384
  entries cost +1.9% cycles (four alternating pairs, +1.4% to +2.6%) and
  1,024 about +1%.
- **Memory.** About 13-15 KB per entry (56-66 elements of 224 bytes; their
  owned text is interned, see below): 16,384 entries ≈ 230 MB per stream, 65,536 ≈ 0.9 GB,
  whatever the worker count. Peak RSS runs above that at twelve workers
  (+0.5 GB at 16,384 on sorted NY): entries are freed on threads other than
  the one that allocated them, and each pass of the benchmark is a new stream
  with new threads, so the allocator holds some freed pages.
- **Hit cost.** About 2k cycles on one worker: element clone ~50%, dropping
  the slot's old elements ~15%, probe and bookkeeping ~10%, allocation ~5%,
  the `arc-swap` load under 1%. Both copies (into the output slot and into a
  new entry) must go through `extend_from_slice`: building the entry with
  `Vec::clone` made the copy loop call `DecodedElement::clone` per element
  (+1.4k instructions per VIN). The next two bullets take out most of the
  rest.
- **Uncopied hits.** Output slots are recycled, so a row often already holds
  the very result a hit would copy into it. Each entry gets an id that is
  never reused, and each row of a worker's slots remembers the id it was last
  filled from (or stored as, after a miss); a hit on that id writes only
  `vin`. Recycling leaves such rows intact, and every other write (a decode,
  a cleared slot, a fresh `None` row) forgets the id. The id is compared, not
  a pointer, so nothing holds or bumps a shared refcount. Debug builds still
  compare every hit's whole row with a full decode. On sorted NY 35% of hits
  skip the copy at one worker but only 11% at twelve: a worker's three slots
  come back after about 36 batches (1,152 VINs) instead of 3 (96), so a row
  has usually moved on to another key. Same host, one run per cell, back to
  back with the build before:

  | workers | input | VIN/s | cycles/VIN | instr/VIN | uncopied share of hits |
  |---:|---|---:|---:|---:|---:|
  | 1 | sorted | 1,138,308 → 1,453,707 | 3,006 → 2,390 (−20%) | 9.93k → 7.62k | 35.0% |
  | 12 | sorted | 4,251,676 → 4,196,910 | 7,370 → 7,372 | 10.72k → 10.01k | 10.9% |
  | 1 | shuffled | 151,416 → 152,295 | 22.1k → 22.0k | 45.1k → 45.1k | 0.02% |
  | 12 | shuffled | 945,892 → 1,012,612 | 34.1k → 33.3k | 46.6k → 46.4k | 0.02% |
  | 12 | shuffled, 65,536 | 1,475,617 → 1,543,946 | 21.8k → 21.5k | 26.5k → 26.3k | 0.01% |

  Twelve sorted workers save 7% of instructions but no cycles: what remains
  there is still the copy's memory traffic. The no-repeat overhead on the
  synthetic 2M slice is unchanged within noise (+13 instructions per VIN;
  two alternating pairs +2.6%/+2.3% cycles against +2.1%/+0.8% before).
- **Bitwise entries.** A stored result owned 8-11 strings, so every copy
  allocated and every recycled row walked its elements to free them. A store
  now swaps each owned string in the decoded row for an equal interned one
  (a process-wide, append-only, deduplicated set of leaked strings, like the
  default records' scrubbed values, behind a per-worker memo so the lock is
  taken once per new text per worker), then copies the row into the entry
  bitwise. A hit copies the entry's elements with one `memcpy`, and a row
  holding such a copy (marked in its remembered id) empties with
  `set_len(0)` instead of a drop walk ([replaced the next day](#november-13-cache-hits-share-the-element-list)
  by sharing the list, which needs neither). Two small `unsafe` blocks in
  `result_cache.rs`, both relying on one invariant: every text of an entry
  is borrowed. The interner caps at 32 MB counting 32 bytes of overhead per
  text; at the cap a result needing new text is simply not stored. Nearly
  all interned text is one descriptor value per key: sorted NY interns
  233,693 texts (217,970 of 11 bytes, 11,373 of 14), 2.7 MB of text, 10.2 MB
  with overhead; shuffled NY the same; the synthetic 2M slice, where nothing
  repeats, 26k texts per 60k stores. Building the entry element by element
  while interning cost +1.4k instructions per VIN on shuffled input (more
  than the hits saved); interning in place and copying the row bitwise made
  stores cheaper than before. Same host, one run per cell, back to back with
  the previous step:

  | workers | input | VIN/s | cycles/VIN | instr/VIN |
  |---:|---|---:|---:|---:|
  | 1 | sorted | 1,454,146 → 1,901,587 | 2,393 → 1,903 (−20%) | 7.62k → 5.30k |
  | 12 | sorted | 4,139,484 → 4,099,105 | 7,401 → 6,942 (−6%) | 10.02k → 7.26k |
  | 1 | shuffled | 153,023 → 155,624 | 21.9k → 21.5k | 45.1k → 43.4k |
  | 12 | shuffled | 921,897 → 1,082,756 | 34.1k → 30.6k | 46.5k → 44.7k |
  | 12 | shuffled, 65,536 | 1,490,184 → 1,774,319 | 21.6k → 18.5k | 26.4k → 23.6k |

  Against the shared table before both steps: one sorted worker 3.0k → 1.9k
  cycles per VIN (−37%), twelve 7.4k → 6.9k. The no-repeat overhead on the
  synthetic 2M slice: +2.4%/+2.2% cycles in two alternating pairs (+102-108
  instructions per VIN, against +85-88 before either step).
- **Slot plans.** Hits made a 24-row batch about 15 µs, so the per-batch
  hand-off (a worker waking and sleeping; kernel time) became the cost: at
  twelve workers on sorted NY the September plan (24 rows × 4 slots) spent
  11 s of system CPU per pass and 6.7k cycles per VIN. A grid at 1, 4, 8 and
  12 workers, cache hitting (sorted NY) and missing (a synthetic slice that
  never repeats), two alternating rounds per cell
  ([raw runs](scripts/bench/native_plan_grid_2026_11_12.json)), found no
  plan within 3% of the best in both regimes: small batches lose up to half
  the hit rate, large ones up to 13% of the miss rate (their in-flight
  results fall out of cache). 64 rows × 4 slots has the best worst case
  (95-96% of each regime's peak at 8 and 12 workers, within about 1% of the
  best at 1 and 4), so the predictor now uses it at every worker count
  ([predictor.rs](crates/ultravin/src/predictor.rs)); the September plan
  table and its interpolation are gone, and the memory budget only drops
  slots, then rows (at 17,408 bytes per row 64×4 fits 512 MiB up to 120
  workers). The trade: up to ~5-6% slower on never-repeating synthetic input
  at twelve workers in exchange for ~1.7× on repeating input (1.9× at eight
  in the grid). `throughput ... full auto` (calibration and prediction
  included), one run each, back to back with 0c212a2:

  | workers | input | VIN/s | cycles/VIN | busy cores | system CPU s |
  |---:|---|---:|---:|---:|---:|
  | 12 | NY sorted | 4,262,010 → **7,292,891** | 6,787 → 4,302 | 9.5 → 10.4 | 45.9 → 13.1 |
  | 12 | NY shuffled | 1,105,344 → 1,097,599 | 30.6k → 30.8k | 11.3 → 11.3 | 2.7 → 2.2 |
  | 12 | synthetic 2M | 1,959,442 → 1,836,453 | 16,962 → 18,136 | 11.1 → 11.1 | 3.0 → 2.1 |
  | 1 | NY sorted | 1,978,981 → 1,864,874 | 1,787 → 1,894 | 1.1 → 1.1 | 1.6 → 1.4 |
  | 1 | NY shuffled | 155,495 → 156,584 | 21.5k → 21.4k | 1.0 → 1.0 | 2.4 → 1.7 |
  | 1 | synthetic 2M | 276,316 → 277,082 | 12,108 → 12,101 | 1.0 → 1.0 | 0.5 → 0.4 |

  One sorted worker is the open question: two more pairs gave 1.97M/1.94M
  for 32×3 against 1.84M/1.91M for 64×4 (32×3 has more uncopied hits, 35%
  against 25%, because its slots come back sooner), while the grid had 64×4
  ahead by 1-3%. One plan everywhere was kept. Peak RSS is unchanged at one
  worker and +0.06-0.12 GB at twelve on NY.

## November 13: native stream wakes only the worker that can go on

- **Bottleneck.** With cache hits a 64-row batch takes ~100 µs, so twelve
  sorted-NY workers hand off ~100k batches a second, and every hand-off
  went through one mutex: a claim, a cancel check, the permit drop (with a
  `notify_all`), and the consumer's per-batch `notify_all`. A `sample`
  profile put 16% of worker wall time in `psynch_mutexwait`/`cvwait`/
  `mutexdrop` and 17% of the consumer's in `mutexwait`; the process spent
  12-13 s of system CPU per 10 s pass. Temporary counters over one run
  (1.06M batches): workers slept 182k times on their own four full slots,
  0 times on the row cap, and the consumer slept in `recv` on 9.4% of
  batches; each sleeper woke about once, but on any event, not its own.
- **Mechanism** ([native_stream.rs](crates/ultravin/src/native_stream.rs)).
  Each worker has its own wake token (a `Mutex<bool>` + `Condvar`, padded to
  a cache line, `park`-like: a wake sent before the wait makes the wait
  return), and waits only on that. The consumer marking a slot reusable wakes
  that slot's owner; cancellation wakes all. `cancelled` is an `AtomicBool`,
  in-flight rows an `AtomicUsize` that only grows under the claim lock (so
  claims still reserve rows in batch order and the cap holds) and shrinks
  lock-free when a permit drops. A worker blocked on the cap registers in a
  waiter count and re-checks before it sleeps; a dropped permit wakes workers
  only while that count is non-zero. The claim lock is the one shared lock
  left, taken once per batch.
- **Correctness.** `result_cache_check` at 12 workers: 0 mismatches over
  all of sorted and shuffled NY (11.9M rows each). A new test has the
  consumer panic while the other workers are asleep on full slots and on
  the row cap (it hangs if cancellation does not wake everyone). The
  native-stream tests passed 200 consecutive runs at 12 test threads.
- **Results.** `throughput ... full auto`, Apple Silicon 8P+4E, shared host,
  back to back with b784805 (12-worker sorted: three alternating pairs, all
  candidate runs above all baseline runs; shuffled 12: four pairs):

  | workers | input | VIN/s | cycles/VIN | busy cores | system CPU s |
  |---:|---|---:|---:|---:|---:|
  | 12 | NY sorted 2M | 6.47-6.51M → **7.05-7.16M** (+10%) | 4,578-4,597 → 4,198-4,206 | 9.8-9.9 → 9.9-10.0 | 12.4-13.0 → 4.7-5.1 |
  | 12 | NY shuffled | 1.095-1.106M → 1.088-1.102M | 30.7k → 30.7k | 11.2-11.3 → 11.1-11.3 | 2.1-2.2 → 2.1 |
  | 12 | synthetic 2M | 1,869,512 → 1,864,204 | 17,903 → 17,954 | 11.2 → 11.2 | 1.9 → 1.8 |
  | 1 | NY sorted 2M | 1,735,654/1,734,533 → 1,724,701/1,733,160 | 1,974 → 1,960-1,971 | 1.0 | 0.8 |
  | 1 | NY shuffled | 157,088 → 156,790 | 21.3k → 21.4k | 1.0 | 1.6 → 1.8 |
  | 1 | synthetic 2M | 277,489/280,191 → 274,854/281,190 | 12.0-12.1k → 12.0-12.2k | 1.0 | 0.3-0.5 |

  One shuffled 12-worker candidate run under a load spike (1.02M) is left
  out of the range; its three reruns matched the baseline.
- **Shared slot pool (rejected).** After the change workers still slept on
  their own full slots after 9% of batches while about 11 of the other 44
  slots were free, so one pool of the same 48 slots that any worker takes
  from was tried on top. Workers got busier (10.1-10.2 cores) and the
  uncopied-hit share held (4.4% against 4.2%), but cycles rose to ~4,520
  per VIN and throughput fell to 6.72-6.75M, below targeted wakes alone:
  a slot last written on another core (often the other cluster) costs more
  than the stalls it fills.

## November 13: cache hits share the element list

- **Bottleneck.** A result-cache hit still copied the entry's 56-66
  elements (13-15 KB) into the output row. At twelve sorted-NY workers that
  is ~100 GB/s of copying: 4.2k cycles per VIN against 2.0k on one worker,
  and eight workers already as fast as twelve (7.2M VIN/s).
- **Mechanism.** `DecodeResult::elements` is now an `Elements` list
  ([elements.rs](crates/ultravin/src/elements.rs)): an owned `Vec`, or an
  `Arc<Vec>` shared with other results. It derefs to the element slice and
  compares, prints and serializes exactly like the `Vec` it replaces;
  `to_mut`/`into_vec` copy a shared list first, like `Cow`. A store moves
  the decoded row's list into an `Arc` the row and the entry both hold (into
  an exact-size list first when the row's reused buffer has more than an
  eighth to spare, so a full table holds no slack); a hit clones that `Arc`
  into the row and patches the header. A row whose list is the entry's
  (`Arc::ptr_eq`) already holds the whole result and gets only `vin`, which
  replaces the per-row entry ids. Entries own their text again, so the
  process-wide interner (32 MB cap, leaked), the bitwise copy, the
  `set_len(0)` emptying and both `unsafe` blocks in `result_cache.rs` are
  gone, as is `result_cache_interned()`. A list lives as long as any row,
  entry or caller clone holding it.
- **Buffer reuse.** A decode into a row whose list is shared takes the list
  back if the row is its last holder (`Arc::into_inner`), else a spare from
  the worker's pool (at most 8 lists of at most 256 elements), else
  allocates. A hit parks the row's displaced list there, as does a store
  that moved its row into an exact-size list.
- **Correctness.** Debug builds still re-decode every hit and assert
  equality. `result_cache_check` at 12 workers against uncached
  `decode_full`, holding up to 32k cloned rows past their batch: 0 mismatches
  over all of sorted and shuffled NY (11.9M rows each, 11.2M and 6.0M hits),
  again at 2 entries (9.1M hits on sorted NY with entries evicted under rows
  that still share them), and over the synthetic 2M slice. The
  native-stream tests passed 100 consecutive runs at 12 test threads.
- **Results.** `throughput ... full auto`, back to back with c6cd0d3,
  alternating, ranges over pairs (12-worker sorted: three pairs; 12-worker
  synthetic: three; shuffled 12 and 1-worker sorted and synthetic: two):

  | workers | input | VIN/s | cycles/VIN | busy cores | system CPU s |
  |---:|---|---:|---:|---:|---:|
  | 12 | NY sorted 2M | 7.14-7.21M → **10.55-10.56M** (+47%) | 4,186-4,196 → 2,630-2,641 | 9.9-10.0 → 9.2 | 4.8-5.2 → 5.2-5.4 |
  | 8 | NY sorted 2M | 7.17M → 9.83M (+37%) | 3,330 → 2,268 | 7.4 → 7.0 | 5.3 → 5.9 |
  | 1 | NY sorted 2M | 1.730-1.733M → **2.833-2.839M** (+64%) | 1,962-1,966 → 1,224-1,226 | 1.0 → 1.1 | 0.8 → 1.2 |
  | 12 | NY shuffled | 1.091-1.100M → 1.219-1.267M (+11-16%) | 30.7k → 26.7-26.8k | 11.2 → 10.9-11.3 | 2.1-2.2 → 1.8-3.1 |
  | 1 | NY shuffled | 156,979 → 177,334 (+13%) | 21.3k → 18.9k | 1.0 | 1.8 → 1.7 |
  | 12 | synthetic 2M | 1.869-1.875M → 1.827-1.859M (−1.5%) | 17.86-17.92k → 18.04-18.20k | 11.2 → 11.1-11.2 | 1.8-1.9 → 1.8-2.0 |
  | 1 | synthetic 2M | 276,555-281,331 → 272,726-276,004 (−1.5%) | 11.97-12.12k → 12.15-12.30k | 1.0 | 0.3-0.4 → 0.4-0.5 |

  Instructions per VIN fell everywhere (sorted 12: 6.72k → 4.65k; synthetic:
  −90). The synthetic slice, where nothing repeats, pays ~1% more cycles:
  with the cache disabled the two builds match within 0.5%, and a `sample`
  diff shows the same decode work, inlined differently, plus 0.4% freeing
  evicted entries' text, which the interner used to leak. Moving every
  stored row into an exact-size list (so no later miss allocates) measured
  the same. Peak RSS falls at twelve workers on NY (sorted 0.98-1.01 →
  0.70-0.72 GB, shuffled 1.83 → 1.64-1.68 GB) and holds within about
  0.08 GB on synthetic input. Twelve workers now keep only ~9.2 cores busy
  on sorted input: the per-batch hand-off, not the hit, is what they wait on.
- **A consumer that reads the data.** The benchmark consumer only
  `black_box`es each batch. With a temporary harness change that sums every
  element's value length and id, twelve sorted workers go from 3.50-3.52M
  (5,232 cycles/VIN, 5.8 busy cores) to 9.42-9.43M VIN/s (2,825-2,829, 8.8
  cores), two pairs. Most likely the consumer thread on master read 13 KB
  of elements freshly written on another core per VIN, while now
  consecutive sorted rows mostly point at the same few lists, which stay
  in its cache.
- **Refcount traffic.** A hit that changes the row's list increments the
  entry list's count and decrements the old one, counters every worker on
  that key shares. A `sample` at twelve sorted workers puts
  `DecodeWorkspace::park_elements` (where the displaced list's decrement
  lands) at 4.7% of busy samples against 0.4% on one worker, and the
  worker loop, where the clone is inlined, at 19.9% against 16.5%: roughly
  5-8% of twelve-worker hit time, measurable but not the limit. The fix, if
  wanted, is a per-worker handle: each worker wraps an entry's list once in
  an `Arc` only it touches and hands its rows that, so the shared count
  moves once per worker per entry instead of once per hit.

## November 13: batch calls run on the native stream

- **What changed.** `decode_batch(_at)`, `decode_batch_json(_at)` and full
  JSON lines (so Python's `decode_batch(full=True)`, `decode_batch_json(full=True)`
  and the JSON-lines CLI) no longer have rayon drivers of their own. They are
  thin consumers of the native stream and its result reuse cache
  ([native_stream.rs](crates/ultravin/src/native_stream.rs)):
  `Db::decode_batch_stream_at` lends batches, `decode_batch_at` moves each
  result to its input position, JSON writes each row's text on the workers
  (a per-batch converter whose output travels with the batch) and the caller
  joins them once, and Python builds each batch's dicts as it arrives, taking
  the GIL per batch while the workers go on. The stream gained a caller year
  per row, an optional visiting order and the converter. The full JSON
  encoder now reads a `DecodeResult`, so single and batch JSON share it.
  `decode_batch_managed(_at)` and its `Db` methods are gone; the flat and
  columnar paths are unchanged.
- **Three things the first version got wrong.** Measured on the synthetic
  2M slice, one worker, before they were fixed:
  - *Locality.* The old drivers sorted the whole call by VIN prefix; the
    stream sorted only within 64-row batches. Never-repeating input decoded
    at 205k VIN/s in input order against 276k presorted (cache off). Batch calls now visit their inputs in the old drivers'
    `locality_order` and place results by `NativeBatch::input_indices`.
    On shuffled NY this also makes repeats adjacent: 2.5× at twelve workers.
  - *Thread lifetime.* The stream spawned fresh threads per call. Results
    that outlive the call (a returned `Vec`) were then freed into the
    abandoned mimalloc heaps of exited threads, so every call faulted in
    fresh pages: 171k against 203k VIN/s with `MIMALLOC_PURGE_DELAY=-1`.
    Workers now run on the pid-aware decoder pool (`batch_pool`, the same
    `RAYON_NUM_THREADS` threads as before), which also keeps them fork-safe.
    `NativeStreamError::Spawn` is gone with the spawning.
  - *Short calls.* A 100-VIN call built and tore down a 16,384-slot table
    (each slot's drop walks `arc-swap`'s debt list) and used two of twelve
    workers. The table is now capped at the input length and short calls
    split their rows evenly across the workers.
  The handed-over JSON was first one string per batch, appended by the
  caller as it arrived; that left the output in input order only without
  the global sort, and the caller's growing string copied each batch again
  on every doubling (12-worker synthetic −34%). One string per row and one
  exact-size join, as the old driver did, matches it.
- **Correctness.** Output hashed per 200k-VIN call (full results as
  `serde_json` per row, JSON and JSON lines as text) equals 0f4fe7d over all
  of sorted and shuffled NY (11.9M rows each), the synthetic 2M slice with
  and without per-row years (every third row `None`), 2M shuffled NY with
  years, sorted NY in 777-row calls with years, and shuffled NY in 1-, 7- and
  50-row calls; JSON lines on synthetic with years and sorted NY. Python:
  identical `repr` of the dicts and identical JSON, JSON-lines and flat JSON
  strings between the two wheels for empty input, one VIN with and without a
  year, invalid, non-ASCII, lowercase and padded VINs, and 5,000 shuffled NY
  VINs with mixed years. New tests: batch calls against single decodes over
  serial siblings, errors and messy input with per-row years and a short year
  list; a reversed visiting order with a converter; converter panics at the
  first, a middle and the last batch propagate and the stream runs again;
  the result-cache differential tests now stream mixed per-row years.
- **Results.** Rust, `ULTRAVIN_NOW_MICROS` fixed, 200k-VIN calls over 2M
  VINs, back to back with 0f4fe7d, two alternating rounds (JSON one at one
  worker); peak RSS from `time -l`:

  | call | workers | input | VIN/s before → after | peak RSS GB |
  |---|---:|---|---:|---:|
  | `decode_batch_at` | 12 | NY sorted | 623-625k → **5.29-5.37M** (8.5×) | 3.6 → 0.7 |
  | `decode_batch_at` | 1 | NY sorted | 124-125k → **2.09-2.11M** (17×) | 3.2 → 0.6 |
  | `decode_batch_at` | 12 | NY shuffled | 553-554k → **1.41M** (2.5×) | 3.9 → 1.2 |
  | `decode_batch_at` | 1 | NY shuffled | 118-119k → 121-122k | 3.2 → 3.1 |
  | `decode_batch_at` | 12 | synthetic | 699-708k → 694-696k (−0.4%, −2.0%) | 5.1 → 4.5 |
  | `decode_batch_at` | 1 | synthetic | 240-243k → 251k (+4%) | 4.1 → 4.0 |
  | `decode_batch_json_at` | 12 | NY sorted | 357-358k → **487-494k** (+37%) | 9.7 → 9.7 |
  | `decode_batch_json_at` | 1 | NY sorted | 76k → **167k** (2.2×) | 9.4 → 9.5 |
  | `decode_batch_json_at` | 12 | NY shuffled | 302k → 345k (+14%) | 9.2 → 9.6 |
  | `decode_batch_json_at` | 1 | NY shuffled | 72.5k → 76.1k (+5%) | 8.9 → 8.9 |
  | `decode_batch_json_at` | 12 | synthetic | 318-322k → 315-319k (−0.9%) | 11.0 → 11.3 |
  | `decode_batch_json_at` | 1 | synthetic | 106k → 121k (+14%) | 10.3 → 10.4 |

  Twelve-worker JSON is now bound by the one serial join of ~3.5 GB of text
  per call (~8 GB/s); one worker by serialization (about 6 µs a VIN against
  0.5 µs for a cached decode on sorted NY). Caching each entry's encoded body
  would only help the second, so it was not built.

  Python, release wheels, 200k VINs, 12 cores, best of three in each of two
  processes per wheel:

  | call | input | VIN/s before → after |
  |---|---|---:|
  | `decode_batch(full=True)` | NY sorted | 61.6-61.9k → 68.6-70.0k (+12%) |
  | `decode_batch(full=True)` | NY shuffled | 53.4-53.6k → 59.3-59.9k (+11%) |
  | `decode_batch_json(full=True)` | NY sorted | 241k → 266-280k (+10-16%) |
  | `decode_batch_json(full=True)` | NY shuffled | 108-118k → 110-124k (noisy) |

  Dict building under the GIL still dominates the dict call, and the
  conversion of the Rust string to a Python one the JSON call.
- **Short calls decode directly.** On the stream, 12-worker calls of 100
  and 1,000 never-repeating VINs ran 9-42% slower than before, 4,096 VINs 5%
  and 8,192 VINs 3%: worker start-up and per-batch hand-offs dominate a call
  of a millisecond, and in a short call nearly every row is a store (each
  worker may store 16 before hits must pay). Tried and dropped: scaling the
  free stores to the call's length, fewer workers, a serial sort, chained
  worker starts, workers leaving without waiting for their last slots; the
  best combination still left 100-VIN calls 11% behind. Calls under
  `STREAM_MIN_ROWS` (8,192) now take the old driver's path (rayon over the
  uncached full decode in prefix order, JSON likewise), and only the longer
  ones stream. A stream's table also now makes a slot's `ArcSwapOption` on
  its first store: dropping one walks every thread's `arc-swap` debt list,
  and a 16,384-slot table paid that per slot on every call (8,192-VIN calls
  −8% → −3%, 10,000-VIN sorted NY 2.6M → 5.2M VIN/s). One run per cell, 12
  workers, before (0f4fe7d) / streaming every call (568e5ba) / now:

  | VINs per call | synthetic | NY shuffled | NY sorted |
  |---:|---:|---:|---:|
  | 100 | 515k / 299k / 529k | 354k / 228k / 337k | 492k / 520k / 491k |
  | 1,000 | 743k / 630k / 773k | 513k / 427k / 514k | 736k / 2.19M / 731k |
  | 10,000 | 649k / 558k / 620k | 509k / 483k / 527k | 644k / 2.65M / 5.21M |
  | 200,000 | 706k / 697k / 702k | 562k / 1.43M / 1.42M | 652k / 5.27M / 5.54M |

  One worker, 1,000-VIN synthetic calls: 240k / 237k / 235k.
- **Flat derived from full results (measured, not built).** A temporary
  converter built each `FlatResult` from the stream's full result
  (`flatten_unique_variables` over its elements) on the workers. Its
  `serde_json` output equals `decode_batch_flat_at`'s on the synthetic and
  sorted NY 2M slices, with and without per-row years. 200k-VIN calls, two
  alternating rounds:

  | workers | input | `decode_batch_flat_at` today → from full | 
  |---:|---|---:|
  | 12 | NY sorted | 748-769k → 1.56-1.57M (2.1×) |
  | 1 | NY sorted | 121k → 631-636k (5.2×) |
  | 12 | synthetic | 448-484k → 471-506k (+3%) |
  | 1 | synthetic | 136-139k → 186k (+35%) |

  Today's flat path is slower than the full one on never-repeating input
  (139k against 251k VIN/s on one worker): it allocates every value and
  never reuses buffers, which costs more than the projection it skips.

## November 13: native batches lengthen while they hit

- **Bottleneck.** A cache hit costs ~1.2k cycles, so a 64-row batch of
  hits is ~25 µs of work and each hand-off costs a large share of it: on
  sorted NY at twelve workers only ~9.4 cores were busy at 2.67k cycles per
  VIN. Fixed longer batches helped there (1024×8: ~15M VIN/s) but cost ~10%
  on never-repeating input, where every in-flight row owns ~15 KB.
- **Mechanism** ([native_stream.rs](crates/ultravin/src/native_stream.rs)).
  Workers claim a row range from a cursor instead of a fixed-size batch
  number, and the consumer delivers by claim number. Each worker reads its
  result-cache hit count around a batch: if at most a quarter of the rows
  missed, its next batch doubles, up to `batch_size + max_inflight_rows /
  workers` (64 → 320 rows for the shipped plan); otherwise it is back to
  `batch_size`. The in-flight cap now counts misses: a claim reserves at
  most `batch_size` rows, and once decoded the permit settles to the batch's
  miss count (a hit shares its element list with the cache). Settling up
  never waits, so the claim-order argument for deadlock freedom stands.
  Slots grow to a long batch on first use; on recycle, rows past
  `batch_size` keep their small strings but drop their element list, owned
  or shared, so retained buffers are what fixed batches retained.
- **Memory.** Input that misses never lengthens a batch: same rows, same
  cap, same slots as before. Worst case for mixed input: every worker's long
  batch, claimed after one that mostly hit, misses throughout, so missed
  rows held can reach `max_inflight_rows + workers × (max − base)` =
  `2 × max_inflight_rows` (6,144 rows, ~107 MB at 17 KB a row, at twelve
  workers). Hit rows in flight (≤ 48 × 320) add their own strings, an
  estimated ~0.3 KB each, and keep alive a shared list the table may have
  evicted meanwhile, as hit rows already did. Measured peak RSS:
  +35-45 MB on the sorted 2M run, unchanged elsewhere.
- **Correctness.** `result_cache_check` at 12 workers: 0 mismatches over all
  of sorted and shuffled NY (11.9M rows each). New tests: exact lengths
  growing and falling back on one worker, order and results across four
  workers on input that alternates hits and misses, all-miss input never
  lengthening a batch, the cap's reserve-and-settle accounting, and consumer
  and mid-decode panics inside long batches. Native-stream tests passed 100
  consecutive runs at 12 test threads.
- **Visible change.** `decode_native_stream*` batches are no longer all
  `batch_size` long; `start_index` and `batch_index` still place them. No
  caller in the repo assumed fixed lengths.
- **Results.** `throughput ... full auto`, back to back with 7d33d19,
  alternating pairs, shared host (other agents compiling during the
  shuffled pairs):

  | workers | input | VIN/s | cycles/VIN | busy cores | peak RSS MB |
  |---:|---|---:|---:|---:|---:|
  | 12 | NY sorted 2M | 10.46-10.81M → **12.66-13.22M** (+21-23%) | 2,667-2,678 → 2,042-2,107 | 9.3-9.6 → 8.8-8.9 | 639-653 → 677-686 |
  | 1 | NY sorted 2M | 2.59-2.60M → **2.83-2.84M** (+9%) | 1,268-1,272 → 1,150-1,155 | 1.0 | 557-559 → 557-559 |
  | 12 | NY shuffled | 1.24-1.27M → 1.20-1.26M | 26.5-26.8k → 26.6-26.9k | 11.2-11.3 → 11.0-11.3 | 1,272-1,289 → 1,278-1,290 |
  | 1 | NY shuffled | 0.18-0.19M → 0.18M | 18.4-18.7k → 18.7-18.8k | 1.0 | 1,212 → 1,212 |
  | 12 | synthetic 2M | 1.83-1.85M → 1.79-1.86M | 18.0-18.2k → 18.1-18.3k | 11.0-11.1 → 10.9-11.2 | 636-641 → 633-638 |
  | 1 | synthetic 2M | 0.27-0.28M → 0.28M | 12.17-12.20k → 12.11-12.12k | 1.0 | 519 → 519-520 |

  Shuffled NY (50% hits) keeps 99.99% of its rows in 64-row batches and
  synthetic 100%, so their plan is unchanged; the spread is host noise.
  200k-VIN batch calls, 12 workers, three pairs: `decode_batch_at` sorted
  5.45-5.59M → 5.92-6.56M (+15%, RSS 755-761 → 763-771 MB), synthetic
  687-700k → 698-699k; `decode_batch_json_at` sorted 480-482k → 479-488k,
  synthetic 315-318k → 315-320k.
- **Tried and dropped.** Dropping rows past `batch_size` entirely on
  recycle cost the one-worker sorted run 4.5% (2.53M against 2.66M); keeping
  their strings recovers it. Longer maxima (64 + 3 or 7 × 256 rows) reached
  13.5-13.7M but added 90-190 MB of peak RSS and a 4-8× worst case. A stricter
  growth rule (one miss in eight) gained less (12.5M). Eight slots per worker
  (13.5M at 832 rows) would have needed a bigger cap or no buffer retention
  for input that misses.

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
| Compact per-schema pattern rows (September 25) | Copying each row's id, element, date and text spans into the lazy index kept hits out of the pattern table but added instructions and gained nothing on shuffled input. |
| Bracket-key position masks for the unused-position check (September 25) | No instruction change: the regex-backed bracket keys are not where that check spends its time. |
| Drop recycled elements as they are overwritten (September 25) | Cost 2.6k more instructions per VIN than dropping them in one pass. |
| O(1) indexes for engine-model and vehicle-spec joins (September 25) | About 1%, below the bar for the extra indexes. |
| Warm upcoming batch rows to overlap cache misses (November 11) | Lookahead of 1-8 rows and staged passes across the batch, with real loads down the WMI, schema and pattern-index chain. Warming cost as many cycles (0.5-0.8k per VIN, mostly the lookups needed to find the index) as the misses it hid; pattern-index misses are only about a sixth of the ~6k-cycle shuffled penalty. Best variant: +0.3% cycles at one worker, -1.6% at four, +5% on sorted input. |
| Memoize results by structure instead of by VIN (November 11, measured, not built) | The headline corpus fills descriptor positions at random, so choices multiply. Over 20M VINs a result keyed by positions 1-8, 10, 11 or by the set of matched patterns repeats only 22-26% of the time late in the run. Splitting off independent per-pattern records, keyed by pattern and WMI row (exact, 0 mismatches in 5M), leaves a remainder that still misses 35% on regular WMIs and 12% on small-volume ones and needs 4.4 GB at 5M VINs. Only the WMI-and-year context (99.6%) and single output records repeat. Input with repeated descriptors, such as a real fleet, is a different case and was not measured. |
| Batch JSON handed over as one string per stream batch (November 13) | The caller appended each batch as it arrived, so the text stayed in input order only without the global locality sort, and the growing output was copied again on every doubling: −34% at twelve workers on never-repeating input. One string per row and one exact-size join replaced it. |
| Native stream workers on threads spawned per call (November 13) | Results that outlive the call were freed into the abandoned allocator heaps of exited threads, so each call faulted in fresh pages: −16% for a one-worker batch call. Workers now run on the decoder pool. |

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
- **Run the repository checks.** The documented rounds passed `make check checku` in their
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
| [THROUGHPUT_2026_09_10_JSON.md](docs/THROUGHPUT_2026_09_10_JSON.md) | Direct full JSON encoding, controls, correctness and resource measurements |
| [throughput_2026_09_10_json.json](scripts/bench/throughput_2026_09_10_json.json) | Full JSON raw samples, input/build identities, controls and correctness digests |
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
