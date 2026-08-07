# VIN corpora — how to hit everything, and how to hit it with the fewest VINs

Generation ships **in the library**, so anyone testing a VIN decoder can use it:

```python
import ultravin

ultravin.generate(100, seed=42)  # deterministic valid VINs
ultravin.generate(50, make="HONDA", year=2020)  # filtered
ultravin.cover_vins()  # ~164: every behaviour, once
ultravin.sweep(["pattern"])  # ~545k: every pattern row
```

No database, no network — it all comes from the artifact already baked into the
wheel. `scripts/parity/coverage.py` keeps only the part that genuinely needs
Postgres: auditing a corpus against the vPIC row counts.

| | call | VINs | what it is for |
| --- | --- | --- | --- |
| **generate** | `ultravin.generate(n, seed=…)` | any | fixtures, fuzzing, load tests |
| **cover** | `ultravin.cover_vins()` | ~164 | CI, regressions, bisecting |
| **sweep** | `ultravin.sweep()` | ~584k | every data row exercised once |
| **pairwise** | `ultravin.pairwise()` | ~1.74M | every 2-way descriptor interaction |

## Why there is no "enumerate everything"

The decoder's output space does not have a size you can reach. Two facts:

- **The decoder echoes its input.** "Vehicle Descriptor" (element 196), Suggested
  VIN, and the error text all quote the VIN back, so counting them makes the
  decoder injective — one distinct output per distinct input, ~10^11.9
  distinguishable descriptors. Exclude them and you are counting what the decoder
  says about the *vehicle*, which is the only useful question.
- **What is left still factorizes.** In the largest schema, 120 of 300 element
  pairs are driven by disjoint descriptor positions, so their values vary
  independently and multiply. The population is a product, not a list.

So exhaustiveness has to be defined by a *strength*, not by enumeration:

| corpus | VINs | distinct vehicle-outputs |
| --- | ---: | ---: |
| sweep — every row once (1-wise over rows) | 584,019 | 464,142 |
| pairwise — every 2-way class interaction | 1,736,895 | 882,955 |
| **union** | **~2.3M** | **1,314,621** |

The two are complementary, and the overlap is only 32,476. The sweep matches each
pattern by satisfying all of its positions at once — a 13-way conjunction that
pairwise never has to construct. Pairwise reaches combinations of *co-matching*
patterns that the sweep, holding every other position at a fill character, never
produces. Neither subsumes the other.

Building pairwise takes ~200s (the sweep takes 1.7s), so it takes a `limit=`.

The cover and the sweep are one pipeline, not two ideas: the sweep is the
candidate pool and the cover is the greedy minimisation of it. Anything less as a
pool is a smaller universe wearing the word "everything" — sampling three keys
per schema instead of enumerating all 545k patterns silently costs 34 tokens.

The cover is computed when the artifact is built (from the data month's own year,
never the system clock, so the artifact stays reproducible) and stored in it —
about 3 KB — so callers get the cover for their exact data month at no cost.

---

## Why one VIN per thing is the wrong instinct

The obvious strategies — one VIN per WMI, one per pattern, one per engine model,
one per conversion — each produce a list sized by *its own* dimension, and they
overlap almost completely. A single decode returns dozens of elements, each
resolved through its own rung of the source ladder, so **one VIN is evidence for
dozens of behaviours at once**. The 2026_07 data:

| dimension | rows | one-VIN-per-row cost |
| --- | ---: | ---: |
| distinct (schema, keys) patterns | 545,485 | 545,485 |
| WMIs | 12,925 | 12,925 |
| models referenced by patterns | 31,213 | 31,213 |
| vehicle-spec schemas | 6,586 | 6,586 |
| engine models | 346 | 346 |
| conversions | 6 | 6 |
| error codes | 15 | 15 |

Add them up and you get the sweep. Ask instead "which behaviours does this VIN
prove?" and the same ground is covered by 117.

## The min corpus: set cover over behaviour tokens

1. **Build a candidate pool** — the whole sweep, plus deliberate dedup-tie,
   year-edge and error-code VINs that no table enumeration can produce.
2. **Score each candidate** with `token_signature`: the set of behaviours it
   demonstrates. Tokens are read straight off `ultravin.decode()` —
   `(element, id, source_rung)`, `(keys, syntax, length)`, `(error, code)`,
   `(conversion, id, number_shape)`, `(year, kind, conclusive)`,
   `(check_digit, …)`, `(specs, bucket)` — plus one that is *not* visible in the
   output: a VIN matching two element-18 patterns, the only thing that exercises
   `pick_element18`'s tiebreak.
3. **Greedy set cover** (lazy/CELF): repeatedly take the candidate with the
   largest number of uncovered tokens. Greedy is the best a polynomial algorithm
   can do here — set cover has no better than a `ln n` approximation unless
   P = NP — and ties break on index, so the output is deterministic.

Tokens have to be *precise* or greedy silently skips the case they were meant to
buy. Two worth knowing about:

- `(year, kind, conclusive)` — three separate rules can move the model year off
  what position 10 names. Predict the two arithmetic ones (carLT position 7, the
  future-year pull-back) and flag `conclusive`, so `swap` means only the ±30
  schema retry. Without both refinements an ordinary VIN covers the token and the
  retry in `year.rs:84-96` never runs.
- `(tie, element18)` — needs the schema's other element-18 keys to know a second
  pattern also matched. Nothing in the output says so.
- `(tie, dedup)` — `dedup_cmp` only reaches `cmp_keys_no_brackets` when two
  patterns for one element agree on priority, `CreatedOn` and star-free key
  length. Constructing that needs `merge_keys`: intersect the two specs per
  position and pick the overlapping character. Building from either pattern's own
  first character excludes the other, which is why the naive attempt found none
  in 79,294 candidate groups. One VIN buys 28 regions.

One branch resists this entirely: the vehicle-spec dedup tiebreak fires only when
two spec rows survive key elimination for the same element. Modelling that in
Python means reimplementing engine internals that would then rot, so it stays
measured rather than asserted.

### The coverage gate

`cargo llvm-cov` reports ~95% of the decode modules and always will, because some
of that code is unreachable by construction. A permanent 5% gap hides regressions
inside it, so the gap is written down instead: every uncovered region carries a
reason in `scripts/coverage_allowances.json`, and `make coverage` fails on
anything else.

```
decode path: 2599/2737 regions (94.96%)
reachable:   2599/2599 (100.00%) after 39 allowances
```

It fails in both directions, which is the point:

- an uncovered region with **no allowance** is a new gap — a corpus that used to
  reach that code no longer does;
- an allowance whose regions are **now covered** is stale, and the usual cause is
  a monthly data refresh making the branch reachable. A dump that ships a
  `tobeqced` schema, a reversed character class, or a model with two makes would
  light up exactly those entries.

The 138 allowed regions, by mechanism:

| | regions | why no VIN reaches it |
| --- | ---: | --- |
| `sqlwild_to_regex` | 29 | runs in the artifact builder (`artifact.rs:456`), which precomputes `keys_regex`; decode reads the stored result |
| conversion arithmetic | 35 | negative operands, zero denominators, all-nines rounding carries — the six vPIC formulas are positive multiply/divide over positive displacements |
| matcher regex fallback | 18 | needs a negated, nested, unterminated or reversed character class; 2026_07 contains none |
| decode-path guards | 26 | dump shapes the data does not have: 0 `tobeqced` schemas, 0 orphan `wmi_vinschema` rows, 0 patterns on elements 26/27/29/39, 0 duplicate (schema, element, keys) rows, 0 models with two makes, 0 WMIs double-linked to a schema |
| error-path arms | 14 | corrections and charset arms for VIN shapes the corpus does not construct |
| check-digit + misc | 16 | the public `check_digit` wrapper decode never calls, plus closure arms |

### What "covers everything" does and does not mean

The token universe is defined as the union over the pool, so the cover always
reports 100% of it — that is a tautology, not a proof. The independent check is
code coverage of the Rust engine, measured over the same VIN list:

```bash
cargo llvm-cov run --example covrun --summary-only -- vins.txt
```

Measured on 2026_07 (`ultravin-core`, region coverage):

| corpus | VINs | regions |
| --- | ---: | ---: |
| one clean VIN | 1 | 69.08% |
| clean + malformed | 2 | 73.94% |
| frozen parity corpus (before) | 272 | 88.91% |
| **baked cover** | **164** | **90.30%** |
| stacked bulk strategies | 39,790 | 91.09% |

Less than half the size of the frozen corpus and ahead of a systematic pool 173×
larger. What is left is overwhelmingly not corpus-addressable:

| the remaining 393 regions | | |
| --- | ---: | --- |
| functions no VIN calls | ~250 | artifact serialization, blake3, the `external-data` memmap backend, `sqlwild_to_regex` (build time), `multi_valued_variables` / `Db::elements` |
| unreachable with this data | ~45 | `apply_sign`'s negative arm and `div`'s zero denominator (all six vPIC formulas are positive); the regex fallback in `matcher.rs` (2026_07 has no negated, nested, unterminated or reversed classes) |
| behind API parameters | ~80 | `decode_full`'s `include_private`, the caller-supplied model year (also why error 12 is unreachable) |
| genuinely open | ~15 | the vehicle-spec dedup tiebreak |

Per bulk strategy, measured the same way:

| strategy | VINs | regions | tokens |
| --- | ---: | ---: | ---: |
| error codes only | 14 | 80.38% | 192 |
| engine models only | 335 | 86.18% | 326 |
| vehicle specs only | 6,516 | 87.93% | 556 |
| every WMI | 12,925 | 88.62% | 485 |
| patterns (20k sample) | 20,412 | 90.16% | 636 |
| every pattern | 545,383 | — | 697 |
| all of the above stacked | 39,790 | 91.09% | 657 |
| **min cover over the sweep** | **163** | **91.07%** | **708** |

Each bulk strategy alone is broad in one dimension and blind in the others. The
full pattern enumeration is the strongest single one and still misses ten tokens
— every one of them a constructed case (error codes 6 and 11, multi-error
combinations, the dedup tie, an unmapped year), because those are not rows in any
table and no amount of enumeration produces them.

The probe drives every public entry point — `decode`, `decode_batch`, the flat
and JSON wrappers — not just `decode`. It matters: measuring through `decode`
alone hides a third of `lib.rs` behind the probe rather than the corpus, and
makes every corpus look identical.

## The sweep corpus: brute force, explainable

`coverage sweep` emits one VIN per row of every dimension that can change a
decode: every WMI, every distinct (schema, keys) pattern, every engine model,
every vehicle-spec schema, every error code. No cleverness — it is the list you
hand someone who asks whether every make is covered.

Check any list, either corpus or someone else's, by decoding it:

```bash
uv run -- python -m scripts.parity.coverage report vins.txt
```

Every number is measured, not assumed: a pattern counts only if it won an
element, a model only if it was emitted, and hits are counted in the same unit
as the totals (row ids, not display names). The full sweep on 2026_07:

| dimension | hit | total | |
| --- | ---: | ---: | ---: |
| WMIs | 12,925 | 12,925 | 100.0% |
| makes | 12,155 | 12,172 | 99.9% |
| models | 30,956 | 31,213 | 99.2% |
| VIN schemas | 24,816 | 25,033 | 99.1% |
| series | 15,152 | 15,531 | 97.6% |
| engine models | 320 | 346 | 92.5% |
| vehicle-spec schemas | 5,530 | 6,586 | 84.0% |
| elements | 137 | 160 | 85.6% |
| error codes | 14 | 15 | 93.3% |
| conversions | 6 | 6 | 100.0% |
| pattern rows | 1,249,363 | 1,667,711 | 74.9% |

The shortfalls are the interesting part, and none of them are fixable by adding
VINs:

- **pattern rows 74.9%** — a pattern only surfaces if it *wins* its element. The
  other 418k rows are outranked by a sibling for every VIN that matches them.
- **error code 12** — "model year entered does not match" needs a caller-supplied
  year, and the decode API takes only a VIN.
- **elements 85.6%** — 23 elements have no pattern, default or spec row that any
  VIN can reach in this data month.
- **makes / models / series / schemas** — the remainder are rows no WMI links to,
  or whose schema is `tobeqced`.

## Relationship to the other harnesses

- `scripts/parity/generator.py` — builds the VINs (`build_vin`, `check_digit`,
  `choose_year`); this module aims them.
- `scripts/parity/brutal.py` — the long-running campaign (random + systematic +
  coverage-guided fuzzing) that hunts for oracle divergences. Its
  `ultravin_coverage` is a coarser cousin of `token_signature`, kept separate so
  the campaign's saturation behaviour does not shift underneath it.
- `scripts/parity/freeze.py` — snapshots a chosen corpus into
  `tests/parity_corpus.json` for the offline regression test.
