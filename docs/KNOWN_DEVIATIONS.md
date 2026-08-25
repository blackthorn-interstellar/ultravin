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
registered in `scripts/known_problems.json` under kind `oracle-crash` — the 65
VINs registered there. That list is a sample of an unbounded class, so a new 7T0
MY2023-2025 VIN fails the gate until a human re-verifies it against this section.
That is deliberate: a crash must never pass silently just because a similar one
was once explained.

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

Two cells worked through, both of them the ones the registered `clean-decode`
VINs land on:

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
   candidate `J`, and returns codes `3,14` with error bytes `(11:J)`.

3. **The whole decode** (`clean-decode`, `MLHAE041XKA111111` and
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
further is outside the class by construction. That is why the two `clean-decode`
VINs above stay registered individually: their whole decode changes, which no
cell list may excuse.

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
