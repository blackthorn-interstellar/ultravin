# Known deviations from the oracle

ultravin is **byte-for-byte identical** to the official Postgres
`vpic.spvindecode` except where the reference itself is defective: the VINs
registered in `scripts/known_problems.json` and the cache cells enumerated in
`scripts/stale_cache_cells.json`. On those, and only those, ultravin
deliberately does not reproduce the defect.

**This file is the evidence companion to that registry, and both halves are
mandatory.** `scripts/known_problems.json` says which VINs and why, one line
each; the section here that an entry's `doc` anchor names carries the proof —
the defective upstream artifact, named, and how it is shown to be defective. A
registry entry with no section, or a section no entry points at, is a defect in
the list itself, and `tests/test_known_problems.py` fails on either. An output
diff is never evidence: it is the observation being explained, not the
explanation.

Each entry records a `scope`. `error-fields` means the defect is confined to the
oracle's error-correction machinery — `spvindecode_errorcode` and the five
elements only it produces (142/143/144/156/191) — including the case where that
machinery aborts and takes the whole call down with it. `clean-decode` means the
divergence changes what a clean full-VIN decode resolves to: a different vehicle,
not a different possible-values string. Both are admissible; scope is recorded so
the blast radius is visible, not as a bar to clear — the bar is the evidence. It
is load-bearing in exactly one place: the machine-enumerated cell list of §2 may
excuse only `error-fields` divergences, so that class's `clean-decode` members
stay registered per VIN (`tests/test_known_problems.py`).

The policy that governs *how* a divergence earns a place here, and how the gates
adjudicate one, is `docs/ACCEPTANCE.md`.

**Entries expire.** Every data refresh re-decodes every registered VIN against
the new oracle (`scripts/parity/known_problems.py`) and the **known-problems**
gate fails the run if one stopped reproducing — a crash VIN the oracle now
answers, or a deviation VIN ultravin now matches. Upstream does fix things, and
a stale excuse is worse than no excuse: it silently forgives the next real
regression on that VIN. When the gate names one, verify it against the section's
evidence below, then retire it from `scripts/known_problems.json`, and retire
the section here once its last VIN is gone.

---

<a id="regex-crash-7t0"></a>

## 1. Oracle crashes on a malformed pattern regex — WMI `7T0`, MY 2023-2025

The oracle aborts with a Postgres error inside `vpic.fvalidcharsinregex`:

```
ERROR:  invalid regular expression: invalid character range
CONTEXT:  PL/pgSQL function vpic.fvalidcharsinregex(character varying) line 22 at IF
          PL/pgSQL function vpic.fvalidcharsinkey(character varying) line 68 at assignment
          SQL statement "insert into tbl_spVinDecode_ErrorCode1
                         select * from vpic.fValidCharsInKey(key) where return_chr <> '|'"
          PL/pgSQL function vpic.spvindecode_errorcode(character varying,integer) line 165
```

`spvindecode` raises and returns **nothing** — not a partial row set, no rows at
all — so there is no oracle answer to have parity *with*. ultravin decodes the
VIN normally.

**The offending datum.** Two rows of the `pattern` table (1,674,161 rows in
dump 2026_08, `vpic/manifest.json`) carry a character class Postgres cannot
compile:

```sql
select id, vinschemaid, keys, elementid, attributeid
  from vpic.pattern where keys like '%[1-A-JT]%';
```

```
  id    | vinschemaid |      keys       | elementid | attributeid
--------+-------------+-----------------+-----------+-------------
1827686 |       24522 | *****|*[1-A-JT] |        77 | INDIANA
1827685 |       24522 | *****|*[1-A-JT] |        75 | 6
```

`vinschema` 24522 is *BRINKLEY RV, LLC Trailer Schema for 7T0 MY(2023-2025)*, so
the blast radius is WMI `7T0`, model years 2023-2025 (`wmi_vinschema.yearfrom /
yearto`) — the year codes `P`, `R`, `S`. A 7T0 VIN outside that range decodes
fine; it matches schema 28060, whose keys are well-formed.

**The mechanism.** `spvindecode_errorcode` feeds every matched item's `Keys` to
`fValidCharsInKey`, which hands each bracket group to `fValidCharsInRegEx`, which
builds `'^' || str || '$'` and evaluates `s ~ pattern` in its loop
(`vpic/procs/fvalidcharsinregex.sql`). For this key that is `^[1-A-JT]$`, and
Postgres rejects the class outright:

```
vpic=# select '1' ~ '^[1-A-JT]$';
ERROR:  invalid regular expression: invalid character range
```

The failure is in the *error-correction* path, not the matching path — which is
what makes it a defect rather than a rule. **The dump contradicts itself here.**
`vpic.sqlwild_to_regex`, which the matching path uses on the same key text, ends
with a hand-written escape hatch for this exact string:

```sql
  out = REPLACE(out, '1-A', '1A');
```

So the translation to Postgres already knew `1-A` was hostile to its regex engine
and patched it in one of the two places the key is compiled. `fValidCharsInRegEx`
never got the same treatment, so half the proc tolerates the key and the other
half aborts the entire decode.

The abort is unconditional for a schema-24522 match: a synthesized
check-digit-valid 7T0 MY2024 VIN that ultravin decodes with error code 0 aborts
the oracle just the same, so clean decodes are lost too, not only the error
path's outputs — these entries are `error-fields` because the defect sits in the
error-correction machinery, not because the damage stops there. The same VIN
moved to MY2026 (schema 28060) decodes on the oracle in exact field-for-field
parity with ultravin.

**Closure is checkable, not assumed.** Registering a crash VIN on resemblance
would defeat the gate, so the question "can any other datum in this dump raise
that error?" is answered by compiling every distinct bracket group the `pattern`
table carries with the engine that decodes:

```sql
do $$
declare g text;
begin
  for g in select distinct (regexp_matches(keys, '\[[^]]*\]', 'g'))[1] from vpic.pattern
  loop
    begin
      perform 'A' ~ ('^' || g || '$');
    exception when others then
      raise notice 'uncompilable: %', g;
    end;
  end loop;
end $$;
```

That scan returns exactly one group, `[1-A-JT]`, carried by exactly the two rows
above. Nothing takes that on faith, though: the refresh gate
below fails on any crash VIN that is not already registered, so a second
uncompilable group would surface as a gate failure, not as a silent pass.

**What ultravin does.** `errors.rs::valid_chars_in_regex` compiles the class with
the Rust `regex` crate, which accepts it — `1-A` is an ascending range, and the
trailing `-` before `J` is a literal — and yields the valid characters
`AJT123456789`. That is also what the class means to the SQL Server engine vPIC
is authored on, where `LIKE '[1-A-JT]'` reads the same range, literal `-`, `J`,
`T`. Nothing about the VIN is special; only Postgres' stricter class parser is.
ultravin therefore decodes and returns an answer where the oracle returns none.
`errors.rs` pins the tolerated expansion in a unit test, so if the class ever
starts resolving to something else, that is a change in ultravin, not a
rediscovery of this defect.

**How it is handled.** You cannot snapshot a crash, so these VINs are **excluded**
from the regression corpus: `freeze.py` skips any VIN the oracle errors on and
surfaces new skips in the refresh report. `scripts/parity/sweep.py` records them
under `oracle_errors`, and `refresh.sweep_gate` **fails** on any crash VIN not
registered in `scripts/known_problems.json` under kind `oracle-crash` — the
VINs registered there. That list is a sample of an unbounded class, so a new 7T0
MY2023-2025 VIN reaching a sweep fails that gate until a human re-verifies it
against this section.

**Machine-classified at covfuzz intake.** The gate is not where the fuzzer meets
this class. Every night covfuzz re-finds fresh members — exact-VIN dedupe cannot
see that two 7T0 VINs are one defect — and each one used to cost an agent PR that
appended one more sample of an unbounded class. `scripts/parity/regex_crash.py`
adjudicates them instead, and nightly.yaml drops a covfuzz failure record only
when all three hold: it is a **crash** record (an `error`, no `field_diffs` or
`fingerprint`), that error carries both the `InvalidRegularExpression` and the
`vpic.fvalidcharsinregex` markers this defect raises, and ultravin's own full
decode of the VIN actually selects the defective pattern — some element's `keys`
contains the literal `[1-A-JT]`. It keys on the defect, not on WMI `7T0` and not
on `vin_schema_id = 24522`, so it survives the renumbering each monthly dump
brings. Fail any axis and the record is filed; fail to be judged at all and it is
filed with a `::warning::`.

**Intake suppression is not gate suppression.** Nothing above changes what the
gates do: `refresh.sweep_gate` still fails on any crash VIN that reaches a sweep
unregistered, `freeze.py` still excludes them from the corpus, and
`scripts/known_problems.json` remains the human-verified record of this class.
What the predicate buys is only that the fuzzer stops re-filing work already
explained here. A genuine ultravin decoder bug cannot hide behind it: a decoder
bug moves ultravin's *output values* on VINs the oracle answers, which surfaces
as a divergence record carrying `field_diffs` — the first condition refuses that
shape outright, whatever the VIN.

<a id="stale-wmiyearvalidchars-cache"></a>

## 2. Stale `WMIYearValidChars` cache — the dump contradicts itself

`spvindecode_errorcode` does not compute the per-position valid characters that
drive its suggested-VIN / error-byte / unused-position logic. It **reads them from
the precomputed `WMIYearValidChars` cache**, and computes them only when the cache
has no row at all for that WMI-year:

```sql
INSERT INTO tbl_spVinDecode_ErrorCode(p, c)
    SELECT DISTINCT position, "char" FROM vpic.WMIYearValidChars
    WHERE wmi = var_wmi AND year = modelYear
      AND var_wmi NOT IN (SELECT DISTINCT wmi FROM vpic.WMIYearValidChars_CacheExceptions);

SELECT COUNT(*) INTO tmpRowCount FROM tbl_spVinDecode_ErrorCode;
if tmpRowCount = 0 then
    insert into tbl_spVinDecode_ErrorCode(p, c)
        select distinct p, c from vpic.fExtractValidCharsPerWmiYear(var_wmi, ...);
end if;
```

The two branches are supposed to be the same answer: the cache *is* a materialised
`fExtractValidCharsPerWmiYear`, which is itself `SELECT DISTINCT p.Keys` over the
`pattern` rows of every schema covering that WMI-year, expanded through
`fValidCharsInKey`. ultravin computes the second (`errors.rs::valid_charset`, a
port verified byte-equal to that function); the oracle reads the first. **When
they disagree, the dump contradicts itself** — and only one of the two answers is
consistent with the `pattern` rows the same file ships.

The disagreement takes **three shapes**, and every registered member of this class
is one of them. They differ only in which side of the diff the deciding position
sits on, so the cell list enumerates all three identically — but the direction
decides *which* year's cell is the defective one, so an entry's evidence has to
name its shape:

| shape | at the deciding position | effect on the oracle | defective cell keyed by |
|---|---|---|---|
| **permissive** | cache lists a character the recompute does not | accepts a character the pattern rows reject, so that pass takes no correction code | the oracle's model year |
| **restrictive** | cache is missing a character the recompute has | penalizes a pass the pattern rows say is clean, so that pass loses | ultravin's model year |
| **omitted-row** | cache has no row for the position at all | validates nothing there — an absent charset is not an empty one | the oracle's model year |

The omitted-row shape is the one worth pausing on: `spvindecode_errorcode`'s scan
does `if allowed then ... end if`, so a position the cache never wrote is not
checked, which makes an absent row strictly *more* permissive than any extra
character. The cell list records those positions on its `only_in_recompute` side,
so they are enumerated exactly like the other two.

(The exceptions subquery does not do what it reads like. The only WMI-named
column of `WMIYearValidChars_CacheExceptions` is the quoted, upper-case `"WMI"`,
so the unquoted `wmi` in the subquery matches nothing in its own `FROM` and
resolves outward to `WMIYearValidChars.wmi` — and that column is array-typed
(`character varying(6)[]`, `vpic/schema/tables/wmiyearvalidchars_cacheexceptions.sql`),
a second independent reason the `NOT IN` cannot mean what it reads like. A single
row in the table would therefore switch *every* WMI to the fallback. It ships
empty. The scan below always runs: it skips the cells of any listed WMI and
records how many WMIs it saw in `summary.cache_exception_wmis`, and the refresh
gate — control 3 further down — rejects the month when that counter is non-zero.)

**The class is enumerated, not sampled.** `vpic-import --stale-cache-report`
(`crates/ultravin-build/src/stalecache.rs`) diffs every `(wmi, year)` cell of the
dump's cache against the recompute from that same dump's pattern rows — the
decoder's own `ultravin::recompute_valid_chars` — and **`scripts/stale_cache_cells.json`
is the resulting `[wmi, year, positions]` list**: **4,967 stale cells in dump
2026_08** (`summary.stale_cells`). That file is the authoritative membership list
for this class. Every monthly refresh reruns the scan and rewrites the file from
it — `scripts/refresh.py` invokes `vpic-import --stale-cache-report` and hands the
report to `stale_cache.write_cells`, and the `stale-cache` gate then validates the
result — so the list is always this month's dump; the full report rides out as the
run's `data-refresh-report` artifact. A month-over-month change in the list is the
expected signal, not a failure — it self-documents in the refresh PR diff.

Two cells worked through, the ones the first two registered `clean-decode` VINs
land on:

| cell (`wmi`, `year`) | position | cache | recomputed from `pattern` | stale extras |
|---|---:|---|---|---|
| `MLH`, 2019 | 11 | `1345ACDFKMRY` | `5KY` | `134ACDFMR` |
| `JH2`, 2024 | 11 | `1345ACDEFJKMRY` | `345EJKRY` | `1ACDFM` |

Both columns come out of the same loaded dump, one query apart:

```sql
select position, string_agg("char", '' order by "char")
  from vpic.wmiyearvalidchars where wmi = 'MLH' and year = 2019 group by position;
select p, string_agg(distinct c, '' order by c)
  from vpic.fextractvalidcharsperwmiyear('MLH', 2019::smallint) group by p;
```

**The cache is what makes the oracle's answer, demonstrably.** Deleting a stale
cell inside a transaction takes `tmpRowCount` to 0, so the proc runs its own
`fExtractValidCharsPerWmiYear` fallback over the same dump — and the oracle then
reproduces ultravin **byte-for-byte** on every probe VIN reaching that cell,
including the two whose whole decode changes:

```
MLHAE041XKA111111  delete 81 cache rows for (MLH,2019) -> oracle MY=1989 codes='0,14'  parity_now=True
JH2RD1613RA111111  delete 80 cache rows for (JH2,2024) -> oracle MY=1994 codes='0,14'  parity_now=True
```

Rolled back afterwards; the cache is left at its shipped 8,809,229 rows
(`vpic/manifest.json`).

**How far the defect reaches** depends on which position the stale characters sit
at, and the registry records it as each entry's `scope`:

1. **Element 144 only** (`error-fields`, the common case). The position is
   already in error for another reason, so the only difference is the possible-
   values list printed for it: the oracle prints the cache's characters for that
   position, ultravin the recompute's. `(SCF, 2025)`, listed stale at position 7,
   is one such cell.

2. **The correction ladder** (`error-fields`). A probe VIN on `(JH2, 2024)` with
   `A` at position 11: the cache lists `A`, so the oracle sees nothing wrong and
   returns codes `0,14` with no SuggestedVIN; the pattern source does not, so
   ultravin flags one error, lets the check digit pick the single surviving
   candidate `J`, and returns codes `3,14` with error bytes `(11:J)`. When the
   cache's extra characters and the pattern source's singleton then collapse to
   the same surviving character after the check-digit filter, Suggested VIN and
   Possible Values match and only the error-code number differs (2 vs 3). That
   observation names no VIN position, so the cell list cannot excuse it; those
   VINs are registered individually (see §4).

3. **The whole decode** (`clean-decode`, 184 VINs registered individually; the
   two worked through here are `MLHAE041XKA111111` and
   `JH2RD1613RA111111`). Same cells, but these VINs have an *inconclusive* model
   year — `fVinModelYear2` cannot choose between the two halves of position 10's
   30-year cycle, so both years get a decode pass and the best-of scoring picks
   one. Running the oracle's own `spvindecode_errorcode` per candidate year shows
   the cache deciding that race:

   ```
   MLHAE041XKA111111  year 2019: cache present -> codes='14'     bytes=''
                      year 2019: cell removed  -> codes='4 14'   bytes='(11:5KY)'
                      year 1989: either way    -> codes='14'     bytes=''
   ```

   With the stale cache the 2019 pass carries no correction code, so it ties the
   1989 pass on `ErrorValue` and takes the tiebreakers below it (elements weight,
   matched patterns, then the higher model year): the oracle answers *2019 Honda
   CRF50F*, schema 22155. With the pattern source's charset that same pass takes
   code 4 — weight −200 against the 1989 pass's −30 — and loses outright, which
   is why ultravin answers the 1989-schema vehicle (schema 4005).
   `JH2RD1613RA111111` is the same story one cell over (code 3, 2024 → 1994).
   Note what is *not* different: both engines consider exactly the same two
   candidate years and run a pass for each. Only the error code one of those
   passes earns differs, and that comes from the charset.

   The 2026_08 answer key made the size of this sub-class visible for the first
   time: **182 further `clean-decode` VINs**, spread over **19 cells**, every one
   of them a position-10 30-year ambiguity resolved by the cache. All three
   shapes are represented:

   - **permissive**, 159 VINs over 16 cells — `(JH2, 2019…2026)`, `(MLH, 2025)`,
     `(MLH, 2026)`, `(10T, 2024)`, `(2HJ, 2025)`, `(3H1, 2025)`, `(5J7, 2025)`,
     `(JH1, 2025)`, `(JYA, 2027)`. The MLH/JH2 walkthrough above is this shape.
   - **restrictive**, 18 VINs over `(JKA, 2025)` and `(JKB, 2025)`, both stale at
     position 11 with cache `ABJT` against recompute `ABDJT`. Each VIN carries
     `D` there, so the oracle penalizes its own 2025 pass and falls back to 1995
     while ultravin keeps 2025 — the flip runs the *other* way, and the
     defective cell is the one keyed by ultravin's year.
   - **omitted-row**, 5 VINs on `(JKA, 2021)`, whose 69 rows cover only positions
     4–8. The recompute yields `ABDJ` at position 11; each VIN carries `C`, which
     ultravin rejects and the oracle never looks at.

   Each was admitted only after a machine check appropriate to its shape — the
   deciding position's character on the expected side of the cache/recompute
   diff, and the cell on `scripts/stale_cache_cells.json` — and each entry's
   `evidence` names its shape, its cell, its position, and the charsets. The
   decisive test is shape-independent and was run on every one: emptying the
   cell in a rolled-back transaction made the oracle reproduce ultravin
   byte-for-byte on **182/182**, cache restored to its shipped 8,809,229 rows.

**Per-VIN registration for this class covers only the `clean-decode` members.** A
divergence is adjudicated against the cell list by `scripts/parity/stale_cache.py`
and counts as this defect only when **all three** hold: its field diffs touch
**only** the error/correction elements (142/143/144/156/191 — all
`spvindecode_errorcode`, the cache's sole consumer, can reach); the `(wmi, year)`
cell that decode reads — `fVinWMI` plus the model year the decode chose — is on
the list; **and** the difference points at a VIN position that cell is actually
stale at. Each entry is `[wmi, year, positions]` for exactly that reason — a cell
stale at position 11 explains nothing about a wrong charset printed for position
5. The positions come from the difference itself: element 144 renders one
`(position:charset)` group per flagged position, and element 142 is the whole VIN
with the flagged positions rewritten, so the two SuggestedVINs compare character
by character. Elements 143/156/191 are per-decode summaries with no position of
their own and can only ride along. A divergence whose evidence names no position
at all is **not** this class. The corpus and sweep gates count the ones that
qualify as the known class; the nightly covfuzz intake drops them instead of
filing backlog work. Everything else fails exactly as before.

**What keeps a decoder bug out of the list — and what does not.** The list is
computed from the dump alone, never from an observed ultravin-vs-oracle
difference, so no output the decoder prints can put a cell on it. That is *not*
the same as independence from the decoder: the recompute half of the diff is
`ultravin::recompute_valid_chars`, the decoder's own charset code, so a bug
there would move the list and the decode together and the cell would look
legitimately stale. Three controls stand against that, none of them this list:

1. **`answerkey verify`** re-checks the *whole* decode of every VIN in
   `tests/answerkey.json` against the frozen oracle answer. It never consults the
   cell list, so a charset bug shows up there as a plain mismatch. What that gate
   is and when it runs is `docs/ACCEPTANCE.md`.
2. **The frozen unit tests in `crates/ultravin/src/errors.rs`** pin the charset
   extraction and the element-144 rendering directly, independent of any dump.
3. **The refresh's `stale-cache` jump gate** fails a month whose newly-stale
   cell count exceeds `refresh.STALE_CACHE_JUMP_LIMIT` (500). Upstream churn
   moves tens of cells; a charset regression re-lists thousands, because every
   cell whose recompute moved now contradicts a cache that did not.

What the list itself contributes is narrowness, not proof: the excuse it buys is
bounded to the five elements the cache feeds, on the one cell that VIN's decode
reads, at a VIN position that cell is actually stale at. A defect that reaches
further is outside the class by construction. That is why the `clean-decode`
members stay registered individually: their whole decode changes, which no cell
list may excuse.

**Decision: keep ultravin's source-consistent computation.** Matching the oracle
here would mean shipping the 8.8M-row cache — or its delta — purely to reproduce
characters the dump's own `pattern` table contradicts, on a defect that
self-heals whenever NHTSA rebuilds the cache.

---

## 3. The Postgres dump mis-collates element 144

Element 144 ("possible values") prints a position's valid-character set as a
string, e.g. `(6:_123456789)`. That string is a *sort*, so its order is decided by
the collation of whatever server produced it — and the reference exists on two
different servers that disagree.

NHTSA authors vPIC on **SQL Server**. Its database collation is
`SQL_Latin1_General_CP1_CI_AS`, read directly out of `RESTORE HEADERONLY` on
`VPICList_lite_2026_06.bak`. Under that collation `_` sorts *before* the digits,
so NHTSA's own service returns `(6:_123456789)`. The Postgres dump we test against
is a translation, and no Postgres collation reproduces SQL Server on both of the
places `spvindecode` sorts text:

| | element 143 key tiebreak | element 144 charset |
|---|---|---|
| SQL Server (`SQL_Latin1_General_CP1_CI_AS`) | reference | reference |
| Postgres `C` | ✅ matches | ❌ `_` sorts last |
| Postgres `en_US.utf8` | ❌ key order diverges | ✅ matches |

We pin the oracle to `C` (`docker-compose.yml`) because the tiebreak governs
*which rows come back* while the charset governs only how one field prints, and
because every key pair in the data orders identically under SQL Server and `C`
(`docs/ACCEPTANCE.md`). ultravin then emits SQL Server's order at both sites, so
it matches NHTSA and differs from our own Postgres oracle on element 144 alone.

The order can only differ where a charset mixes `_` with alphanumerics.
Everything else is alphanumeric-only, where `C` and SQL Server agree. That the
byte order is the *host's* and not the *data's* is directly demonstrable on the
oracle itself — same rows, same server, two collations:

```sql
select string_agg(c,'' order by c collate "C")           -- HJKLMNPRSTVWX_  (the oracle)
     , string_agg(c,'' order by c collate "en_US.utf8")  -- _HJKLMNPRSTVWX  (ultravin)
from unnest(string_to_array('H,J,K,L,M,N,P,R,S,T,V,W,X,_', ',')) as c;
```

`spvindecode_errorcode` builds the payload with `ORDER BY c` over
`tbl_spVinDecode_ErrorCode` (`vpic/procs/spvindecode_errorcode.sql`), so the sort
happens at decode time on the oracle host — it is not a stored string that came
from NHTSA.

**ultravin's print order is enforced by a type, not a call site.** `errors.rs`
holds valid-character sets in `ValidChars`, whose inner `BTreeSet` is private and
which implements only `Display`, in reference order. There is no `.iter()` to
reach, so `set.iter().collect::<String>()` — the codepoint-ordered version, which
is the bug — cannot be written. The `collation_tests` in `errors.rs` pin the order
against the full vPIC alphabet.

**The deviation is neutralized on both comparison paths, so it needs no registry
entries and carries no anchor.** Any VIN reaching one of those mixed charsets
diverges the same way, so enumerating today's members would only invite
tomorrow's. Instead: the answer key hashes element 144 as a character *set*
(`normalize.collation_agnostic`), and the differential runners (`sweep.py`,
`campaign.py`, `brutal.py`, `freeze.py`) inherit the same rule because
`normalize.diff_rows` applies it before comparing. The neutralization is narrow —
charset *contents*, the position each charset is bound to, and the group order are
all still compared byte-for-byte, so a genuine element-144 regression still
diverges (`tests/test_normalize.py` pins exactly that boundary).

---

<a id="stale-cache-code-2-vs-3"></a>

## 4. Stale cache picks the check-digit correction rung — WMI `ZDM`, MY 2018

The same defective artifact as §2 — the shipped `vpic.WMIYearValidChars` cell —
can move the correction *rung* without moving Suggested VIN or Possible Values.
`spvindecode_errorcode` chooses error code 2 when the flagged position has
exactly one replacement character, and error code 3 when it has several and the
check digit leaves exactly one. When the cache is a strict superset of the
pattern table at that position and the extra characters fail the check digit,
both engines correct to the same VIN and print the same `(pos:char)` group; only
elements 143 and 191 differ. The machine-enumerated class in §2 fails closed on
that observation — `stale_cache.diff_positions` is empty, so
`is_expected_divergence` is false — and the VIN has to be registered here.

**The offending datum.** Cell `(ZDM, 2018)` is already on the §2 list, stale at
positions 5, 6, and 7 (`scripts/stale_cache_cells.json`). Position 5 is the one
this VIN trips. Both columns come out of the same loaded dump:

```sql
select position, string_agg("char", '' order by "char")
  from vpic.wmiyearvalidchars where wmi = 'ZDM' and year = 2018
  and position = 5 group by position;
select p, string_agg(distinct c, '' order by c)
  from vpic.fextractvalidcharsperwmiyear('ZDM', 2018::smallint)
  where p = 5 group by p;
```

```
 source  | position | chars
---------+----------+-------
 cache   |        5 | AB
 extract |        5 | A
```

The extract is the pattern table speaking. Schema 20037 (*Ducati Motorcycle
Schema for ZDM/ML0 (2018)*) is the only schema covering that cell; the only key
that writes a character into key index 2 (VIN position 5) is `[ABDGHKMV]A`,
which contributes `A`. No 2018 key contributes `B` there. `B` is a leftover in
the cache, not a current rule.

**The mechanism.** `ZDMA1ENT2JB011111` is invalid at position 5 (`1` is in
neither charset). The rest of the VDS is on-charset, so `cntErrors = 1` and the
rung is decided by `length(lastReplacements)`:

| charset read | `lastReplacements` | rung | codes |
|---|---|---|---|
| cache `AB` | length 2 | check-digit filter (`fVINCheckDigit`) | 3, then 14 |
| extract `A` | length 1 | auto-correct | 2, then 14 |

The filter is what makes 142/144 agree. Position 9 of the input is `2`:

```
vpic=# select vpic.fvincheckdigit(<same VIN with pos 5 = A>);  -- 2  (kept)
vpic=# select vpic.fvincheckdigit(<same VIN with pos 5 = B>);  -- 6  (dropped)
```

So the oracle's `NewReplacements` collapses to `A`, `err_errorbytes` becomes
`(5:A)`, and `err_correctedvin` is the input with position 5 rewritten to `A` —
the same pair ultravin emits from the single-candidate rung. Elements 143/191
then print "3 - VIN corrected, error in one position (assuming Check Digit is
correct)" versus "2 - VIN corrected, error in one position".

**The cache is what makes the oracle's answer, demonstrably.** Deleting the
cell's 50 rows inside a transaction takes `tmpRowCount` to 0, so the proc runs
its own `fExtractValidCharsPerWmiYear` fallback — and the oracle then returns
codes `2,14` with that same Suggested VIN and Possible Values `(5:A)`,
byte-for-byte with ultravin. Rolled back afterwards; the cache is left at its
shipped 8,809,229 rows.

**Why this is not §2's enumerated class.** `scripts/parity/stale_cache.py`
excuses a divergence only when the difference *points at* a VIN position the
cell is stale at, and the only elements that name a position are 142 and 144.
A 143/191-only fingerprint has `diff_positions == {}`, and an empty set is a
verdict of *not* that class, not a free pass — otherwise any code-only bug
that happened to land on a stale cell would be laundered. The defect is still
the stale cell; the observation is just one the cell list is forbidden to
forgive. Matching the oracle here would mean teaching ultravin to read that
cell, which §2 already rejected.

**What ultravin does.** `errors.rs::valid_charset` recomputes from the pattern
rows, sees `{A}` at position 5, and takes the `cntErrors == 1 &&
last_replacements.len() == 1` branch (code 2). That is the source-consistent
answer, and it is what the oracle itself produces once the stale cell is gone.

---

## How the answer key carries these deviations

The monthly answer key (`scripts/parity/answerkey.py`, policy in
`docs/ACCEPTANCE.md`) freezes one hash per VIN and re-checks it on every PR with
no Postgres anywhere. That works only if the key has something to say about the
VINs on this page, where the oracle's answer is the defect. It says it with two
prefixes, neither of them a hex digit:

| marker | meaning | compared by `verify`? |
|---|---|---|
| `!` | the oracle raised instead of answering, so there is no answer to freeze | no — nothing to compare against |
| `~` | the two disagreed and the disagreement was proven to be the stale-cache class, so the frozen hash is **ultravin's own** | yes |

**A `~` is a pin, not a skip.** It is the easiest thing on this page to
misread. The marker does not mean "stop checking this VIN"; it means "stop
asserting the oracle was right about it". The frozen hash is ultravin's answer,
`verify` compares it like any other, and a later change to that VIN's decode
fails exactly as a plain mismatch would — the only difference is the report,
which says ultravin moved on a documented deviation rather than that it
disagrees with the oracle. A `~` is compared **even when the VIN is also
registered** in `scripts/known_problems.json`: the registry means "do not trust
the oracle here", and ultravin's own pinned answer is not the oracle's, so
skipping it would leave those VINs with no regression cover at all. Build time
is the only place the marker can be decided, because the classification needs
the field-level diff and `verify` deliberately has no oracle to diff against.

**Every `~` is earned by counterfactual, not by argument.** `answerkey.classify`
asks two questions in order, and a VIN must pass both:

1. *Is it caused by the stale cache?* Put the stale cells back to what the
   dump's own `pattern` rows say they hold — `stale_cache.counterfactual_rows`,
   the same rolled-back freshening the sections above use as evidence — ask the
   oracle again, and require its answer to be ultravin's byte for byte. Nothing
   that fails this is ever excused, whatever else is true of it.
2. *Is it in scope for a machine excuse?* Policy rather than physics. A
   divergence confined to the error/correction elements the cache feeds may be
   excused by machine, because that is the blast radius the class is defined
   over. One that reaches the **vehicle** may not.

The six verdicts that fall out, three of which earn a `~`:

| verdict | what it means |
|---|---|
| `error-fields, cache-caused` | the diff stays inside elements 142/143/144/156/191 — the machine excuse, and the common case |
| `year flip, collapses on the oracle's year` | the vehicle is not really in dispute; once both sides agree on the model year the residue sits inside one stale cell (`stale_cache.repin_verdict`) |
| `clean-decode, registered per VIN` | a different vehicle, argued for this VIN in `scripts/known_problems.json` |
| `clean-decode, cache-caused, NOT registered` | the same thing with nobody's name on it — fails `verify`, and says so by name |
| `not reproduced by a freshened cache` | the counterfactual did not hold, so this is not the class at all — fails `verify` |
| `registered, not pinned (not compared)` | registered, but the counterfactual did not hold, so there is no defensible answer to pin — kept out of the comparison rather than silently forgiven |

**The policy line is the third row.** A clean-decode change is excused by a
*human*, one VIN at a time, never by the machine — the cell list in
`scripts/stale_cache_cells.json` may forgive only what stays inside the five
elements the cache feeds (§2). The single exception is the second row, and it
is narrow for a reason: a flip that dissolves the moment both engines agree on
the year is not a disagreement about the car, only about which year's cell was
read. The registry is read live from `scripts/known_problems.json`, never
hardcoded, so registering a VIN is what moves it from the fourth row to the
third — and retiring one moves it back.
