# Known deviations from the oracle

ultravin is **byte-for-byte identical** to the official Postgres
`vpic.spvindecode` **except on the VINs registered in
`scripts/known_problems.json`**, where the reference itself is defective and
ultravin deliberately does not reproduce the defect. The brutal multi-approach
campaign (random + full systematic + coverage-guided covfuzz, 134,661
divergences → 35 signatures) drove everything else to exact parity.

**This file is the evidence companion to that registry, and both halves are
mandatory.** `scripts/known_problems.json` says which VINs and why, one line
each; the section here that an entry's `doc` anchor names carries the proof —
the defective upstream artifact, named, and how it was shown to be defective. A
registry entry with no section, or a section no entry points at, is a defect in
the list itself, and `tests/test_known_problems.py` fails on either. An output
diff is never evidence: it is the observation being explained, not the
explanation.

Each entry records a `scope`: `error-fields` when the defect only reaches the
error-correction outputs (142/143/144/156/191), `clean-decode` when it reaches a
clean full-VIN decode. Both are admissible. Scope is recorded so the blast
radius is visible, not as a bar to clear — the bar is the evidence.

The policy that governs *how* a divergence earns a place here is
`docs/ACCEPTANCE.md`.

**Entries expire.** Every data refresh re-decodes every registered VIN against
the new oracle (`scripts/parity/known_problems.py`) and the **known-problems**
gate fails the run if one stopped reproducing — a crash VIN the oracle now
answers, or a deviation VIN ultravin now matches. Upstream does fix things, and
a stale excuse is worse than no excuse: it silently forgives the next real
regression on that VIN. When the gate names one, verify it against the section's
evidence below, then retire it from `scripts/known_problems.json` and retire the
section here once its last VIN is gone.

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
VIN normally. This was first reported for one VIN in 2026_06; the 2026_07 parity
campaign hit it 62 more times, which was enough data to name the exact cause. The
2026-08-16 backlog probe hit two more (`7T03ZWKM9RA111111`, `7T0FRAYX7RA111111`,
both MY code `R` = 2024), which were accepted by exhaustion rather than
resemblance — see below.

**The offending datum.** Exactly two rows in the 1,674,161-row `pattern` table
carry a character class Postgres cannot compile:

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
builds `'^' || str || '$'` and evaluates `s ~ pattern` (line 22). For this key
that is `^[1-A-JT]$`, and Postgres rejects the class outright:

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

**The class is closed — verified by exhaustion, not sampling.** Registering a
new crash VIN on resemblance would defeat the gate, so for the 2026_08 pair the
resemblance was checked instead of assumed. Every distinct bracket group in the
2026_07 dump's 1,667,711-row `pattern` table was compiled against Postgres' own
regex engine: of 9,087 distinct groups, 1,670 take the regex path and exactly
**one** is uncompilable — `[1-A-JT]`, carried by exactly the two rows above. An
`invalid character range` abort out of `fValidCharsInRegEx` therefore cannot
come from any other datum in that dump. Two boundary checks pin the blast
radius: the abort is unconditional for a schema-24522 match — a synthesized
check-digit-valid 7T0 MY2024 VIN that ultravin decodes with error code 0 aborts
the oracle just the same, so clean decodes are lost too, not only the error
path's outputs — and the same VIN moved to MY2026 (schema 28060) decodes on the
oracle in exact field-for-field parity with ultravin.

**What ultravin does.** `errors.rs::valid_chars_in_regex` compiles the class with
the Rust `regex` crate, which accepts it — `1-A` is an ascending range, and the
trailing `-` before `J` is a literal — and yields the valid characters
`AJT123456789`. That is also what the class means to the SQL Server engine vPIC
is authored on, where `LIKE '[1-A-JT]'` reads the same range, literal `-`, `J`,
`T`. Nothing about the VIN is special; only Postgres' stricter class parser is.
ultravin therefore decodes and returns an answer where the oracle returns none.

**How it is handled.** You cannot snapshot a crash, so these VINs are **excluded**
from the regression corpus: `freeze.py` skips any VIN the oracle errors on and
surfaces new skips in the refresh report. `scripts/parity/sweep.py` records them
under `oracle_errors` (it used to die on the first one), and `refresh.sweep_gate`
**fails** on any crash VIN not registered in `scripts/known_problems.json` under
kind `oracle-crash` — the sample of 65 VINs observed so far. That list is a sample of an unbounded
class, so a new 7T0 MY2023-2025 VIN will fail the gate until a human re-verifies
it against this section. That is deliberate: a crash must never pass silently
just because a similar one was once explained.

`errors.rs` pins the tolerated expansion in a unit test, so if the class ever
starts resolving to something else, that is a change in ultravin, not a rediscovery
of this defect.

<a id="stale-wmiyearvalidchars-cache"></a>

## 2. Stale `WMIYearValidChars` cache — the dump contradicts itself

`spvindecode_errorcode` does not compute the per-position valid characters that
drive its suggested-VIN / error-byte / unused-position logic. It **reads them from
the precomputed `WMIYearValidChars` cache**, and computes them only when the cache
has no row at all for that WMI-year:

```sql
INSERT INTO tbl_spVinDecode_ErrorCode(p, c)
    SELECT DISTINCT position, "char" FROM vpic.WMIYearValidChars
    WHERE wmi = var_wmi AND year = modelYear ...;

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

**What happened in 2026_08.** The refresh moved `pattern` +6,450 rows and
`wmiyearvalidchars` −3,318. The cache was rebuilt, but not from the `pattern`
table this dump ships: nine WMI-year cells still carry characters that no key of
any schema covering that year allows. (The 2026_06 entry in this section was the
same defect in the opposite direction — a cache frozen mid-edit, missing a schema
the dump already contained. It healed in 2026_08 and is retired below.)

| cell (`wmi`, `year`) | position | cache | recomputed from `pattern` | stale extras |
|---|---:|---|---|---|
| `3GN`, 2023 | 4 | `AFK` | `AK` | `F` |
| `3GN`, 2023 | 5 | `BLX` | `BX` | `L` |
| `3GN`, 2023 | 6 | `5789BCDEFGHJKLMNSTUVWX` | `589BCDEFGHJKLMNSTUVWX` | `7` |
| `3GN`, 2023 | 8 | `45GJKSV` | `4GJSV` | `5K` |
| `JM1`, 2025 | 4, 5, 8 | `BDN`, `DPR`, `7BMY` | `BN`, `DP`, `7MY` | `D`, `R`, `B` |
| `1V2`, 2025 | 11 | `CEMPW` | `C` | `EMPW` |
| `YV4`, 2024 | 11 | `12BJP` | `12BP` | `J` |
| `SCF`, 2025 | 7 | `EFGKL` | `EFGL` | `K` |
| `SCF`, 2026 | 7 | `DEFGHJKLMN` | `DEFGHJLMN` | `K` |
| `1ZV`, 2014 | 4, 8 | `BH`, `FHMNSZ` | `B`, `FMZ` | `H`, `HNS` |
| `JH2`, 2024 | 11 | `1345ACDEFJKMRY` | `345EJKRY` | `1ACDFM` |
| `MLH`, 2019 | 11 | `1345ACDFKMRY` | `5KY` | `134ACDFMR` |

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
reproduces ultravin **byte-for-byte** on all ten VINs, including the two whose
whole decode changes. Rolled back afterwards; the cache is left at its shipped
8,809,229 rows.

```
MLHAE041XKA111111  delete 81 cache rows for (MLH,2019) -> oracle MY=1989 codes='0,14'  parity_now=True
JH2RD1613RA111111  delete 80 cache rows for (JH2,2024) -> oracle MY=1994 codes='0,14'  parity_now=True
JH2SC7752RA111111  delete 80 cache rows for (JH2,2024) -> oracle MY=2024 codes='3,14'  parity_now=True
3GNAAAAA2PS111111  delete 41 cache rows for (3GN,2023) -> oracle MY=2023 codes='5,14'  parity_now=True
JM1AAAAA1S0111111  delete 27 cache rows for (JM1,2025) -> oracle MY=2025 codes='5,14'  parity_now=True
1V2AAAE81SA111111  delete 51 cache rows for (1V2,2025) -> oracle MY=2025 codes='5,14'  parity_now=True
YV4AAABE8RA111111  delete 40 cache rows for (YV4,2024) -> oracle MY=2024 codes='5,14'  parity_now=True
SCFAAAAA0SG111111  delete 17 cache rows for (SCF,2025) -> oracle MY=2025 codes='5,14'  parity_now=True
SCFAAAAA9TG111111  delete 25 cache rows for (SCF,2026) -> oracle MY=2026 codes='5,14'  parity_now=True
1ZVAAAAA6E5111111  delete 18 cache rows for (1ZV,2014) -> oracle MY=2014 codes='5,14'  parity_now=True
```

**2026-08-24 backlog probe — 25 more stale cells, same artifact.** Tonight's
covfuzz probe logged 71 VINs, every one an element-142/144 (sometimes 143/156/191)
disagreement. None were stale: all 71 still diverged against the live 2026_08
oracle. For each VIN, `vpic.fvinwmi` + the oracle's chosen model year named a
`(wmi, year)` cell; comparing that cell's `wmiyearvalidchars` rows to
`vpic.fextractvalidcharsperwmiyear` on the same dump showed a mismatch; deleting
those rows inside a rolled-back transaction made `spvindecode` take the
`fExtractValidCharsPerWmiYear` fallback and reproduce ultravin **byte-for-byte**
on all 71. The defective upstream artifact is still the shipped cache, not the
decoder. Some cells are the extras-only shape already in the table above; some
are the opposite (cache missing characters the pattern still allows — the
2026_06 direction); a few are mixed. All 71 stay `error-fields` — none flip the
best-of year the way `MLHAE041XKA111111` did. Eight probe VINs contain I, O, or Q
and cannot enter the registry; each of those cells is represented by a sibling
or, for the 6-char trailer WMI `1Z9599` MY2026, by the well-formed stand-in
`1Z9AAEA80TA599111` (same cell, same delete-cache proof).

| cell (`wmi`, `year`) | cache rows | vs `fExtractValidCharsPerWmiYear` |
|---|---:|---|
| `1AC`, 1981 | 24 | pos 5 missing `G` |
| `1AC`, 1983 | 23 | pos 8 missing `9` |
| `1BN`, 1985 | 40 | pos 4/5 missing `3`/`FMNV`; pos 11 extra `Z`, missing `ABMNSTVX` |
| `1BN`, 1990 | 40 | same shape as `(1BN, 1985)` |
| `1G6`, 2018 | 44 | extras `R`/`7`/`E`/`4` at pos 4/5/7/8 |
| `1GB`, 2026 | 79 | extras at pos 4/5/6/7/8/11 |
| `1GC`, 2025 | 69 | extras `WY`/`HT` at pos 5/8 |
| `1GD`, 2023 | 64 | extras `FH` at pos 8 |
| `1GD`, 2024 | 68 | extras `FHT` at pos 8 |
| `1GD`, 2025 | 63 | extras `89`/`FHT` at pos 5/8 |
| `1GT`, 2023 | 68 | extras `FH` at pos 8 |
| `1GT`, 2027 | 69 | extras `89`/`FT` at pos 5/8 |
| `1Z9599`, 2026 | 19 | pos 5/7 missing `G`/`GHJK` (6-char trailer WMI) |
| `3GB`, 2023 | 30 | extras `WY`/`FH` at pos 5/8 |
| `3GB`, 2024 | 31 | extras `WY`/`FH` at pos 5/8 |
| `3GB`, 2025 | 31 | extras `WY`/`FHT` at pos 5/8 |
| `3GB`, 2026 | 32 | extras `WY`/`FHT` at pos 5/8 |
| `3GN`, 2019 | 59 | extras `F`/`L`/`5K` at pos 4/5/8 (sibling of the documented 2023 cell) |
| `3GR`, 2027 | 64 | pos 4 missing `B` |
| `7PD`, 2027 | 8 | pos 4–8 missing `S`/`G`/`BCD`/`BC`/`DEF` |
| `JTN`, 2023 | 28 | extras at pos 4/5/6/7/8/11 |
| `SCC`, 2024 | 21 | extras `M`/`D`/`V`/`DN` at pos 5/6/7/8 |
| `SCC`, 2025 | 24 | extras `M`/`D`/`V`/`DN` at pos 5/6/7/8 |
| `SCC`, 2027 | 29 | extras `M`/`D`/`V`/`DN` at pos 5/6/7/8 |
| `ZDM`, 2018 | 50 | pos 5 extra `B`; pos 6 missing `AHJ`; pos 7 extras `PRSTUVWXYZ` |

One representative per cell, same delete-and-rollback:

```
1ACUV57AXBAM11111  delete 24 cache rows for (1AC,1981)   -> parity_now=True
1ACAV5EA7DAM11111  delete 23 cache rows for (1AC,1983)   -> parity_now=True
1BNAUAYXXFB111111  delete 40 cache rows for (1BN,1985)   -> parity_now=True
1BNAM9002LB11S111  delete 40 cache rows for (1BN,1990)   -> parity_now=True
1G6KW4GY?JA077111  delete 44 cache rows for (1G6,2018)   -> parity_now=True
1GB6GUAB0TE111111  delete 79 cache rows for (1GB,2026)   -> parity_now=True
1GC0AL1U1SA232111  delete 69 cache rows for (1GC,2025)   -> parity_now=True
1GDRH4EEPPA077111  delete 64 cache rows for (1GD,2023)   -> parity_now=True
1GDXHA0A2RA077111  delete 68 cache rows for (1GD,2024)   -> parity_now=True
1GDK7AH62SA077111  delete 63 cache rows for (1GD,2025)   -> parity_now=True
1GTC7MTR4PA077131  delete 68 cache rows for (1GT,2023)   -> parity_now=True
1GTHCLAV5VU077111  delete 69 cache rows for (1GT,2027)   -> parity_now=True
1Z9AAEA80TA599111  delete 19 cache rows for (1Z9599,2026) -> parity_now=True
3GBAAFZ57PA112111  delete 30 cache rows for (3GB,2023)   -> parity_now=True
3GBAABAPXRA111111  delete 31 cache rows for (3GB,2024)   -> parity_now=True
3GBAAAPA?SA111111  delete 31 cache rows for (3GB,2025)   -> parity_now=True
3GB5KG3A?TA111B11  delete 32 cache rows for (3GB,2026)   -> parity_now=True
3GNYJ8AD2KA111111  delete 59 cache rows for (3GN,2019)   -> parity_now=True
3GR0CLS05VA111111  delete 64 cache rows for (3GR,2027)   -> parity_now=True
7PDAGAAA3VN111111  delete  8 cache rows for (7PD,2027)   -> parity_now=True
JTNKU22G?PA111111  delete 28 cache rows for (JTN,2023)   -> parity_now=True
SCCACCEX6RH1G1111  delete 21 cache rows for (SCC,2024)   -> parity_now=True
SCCAACAA2SH111111  delete 24 cache rows for (SCC,2025)   -> parity_now=True
SCCAACAE8VH111111  delete 29 cache rows for (SCC,2027)   -> parity_now=True
ZDMNH8RD7JB111111  delete 50 cache rows for (ZDM,2018)   -> parity_now=True
```

**How far the defect reaches** depends on which position the stale characters sit
at, and the registry records it as each entry's `scope`:

1. **Element 144 only** (`error-fields`, seven VINs). The position is already in
   error for another reason, so the only difference is the possible-values list
   printed for it — the oracle offers characters the data no longer allows, e.g.
   `(7:EFGKL)` against ultravin's `(7:EFGL)` for `SCFAAAAA0SG111111`.

2. **The correction ladder** (`error-fields`, `JH2SC7752RA111111`). Position 11
   of that VIN is `A`. The cache lists `A`, so the oracle sees nothing wrong and
   returns codes `0,14` with no SuggestedVIN; the pattern source does not, so
   ultravin flags one error, lets the check digit pick the single surviving
   candidate `J`, and returns codes `3,14` with error bytes `(11:J)`.

3. **The whole decode** (`clean-decode`, `MLHAE041XKA111111` and
   `JH2RD1613RA111111`). Same cell, but these VINs have an *inconclusive* model
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

**Decision: keep ultravin's source-consistent computation.** Matching the oracle
here would mean shipping the 8.8M-row cache — or its delta — purely to reproduce
characters the dump's own `pattern` table contradicts, on a defect that
self-heals the next time NHTSA rebuilds the cache (as the 2026_06 one did). The
registered VINs are a *sample* of an unbounded class: any VIN reaching one of
these cells, and any cell the next rebuild leaves stale, diverges the same way.
A new one therefore fails the corpus or sweep gate until a human re-verifies it
against this section and registers it — deliberately, so a fresh divergence is
never waved through on the strength of an old explanation.

---

## 3. The Postgres dump mis-collates element 144 — 95 charsets, 16 WMIs

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
| Postgres `en_US.utf8` | ❌ 1,071 VINs differ | ✅ matches |

We pin the oracle to `C` (`docker-compose.yml`) because the tiebreak governs
*which rows come back* while the charset governs only how one field prints, and
because all 58 key pairs in the data were verified to order identically under
SQL Server and C. ultravin then emits SQL Server's order at both sites, so it
matches NHTSA and differs from our own Postgres oracle on element 144 alone.

**Scope is small and bounded.** The order only differs when a charset mixes `_`
with alphanumerics: **95 of 1,681,352** `(wmi, year, position)` charsets do, across
**16 WMIs**. Everything else is alphanumeric-only, where C and SQL Server agree.
How many *corpus* VINs reach one is a property of that month's corpus and not of
the defect — it was 172 when this section was written and is 0 in the current
403-VIN corpus — which is the argument for neutralizing the class structurally
instead of enumerating today's members.

**How it is enforced, structurally.** The fix is not a call site — it is a type.
`errors.rs` holds valid-character sets in `ValidChars`, whose inner `BTreeSet` is
private and which implements only `Display`, in reference order. There is no
`.iter()` to reach, so `set.iter().collect::<String>()` — the codepoint-ordered
version, which is the bug — cannot be written. Unit tests pin the order against
the full vPIC alphabet.

Because the oracle's order here is a property of the dump host rather than of
NHTSA's rules, the answer key hashes element 144 as a character *set*
(`normalize.collation_agnostic`): its contents are compared, its byte order is
not. That keeps the key from re-freezing one host's collation and from listing
172 VINs that a data refresh would invalidate.

### 3a. The differential runners did not apply that rule (fixed 2026-08-12)

`collation_agnostic` was called by `answerkey.py` and by nothing else. The
*differential* path — `sweep.py`, `campaign.py`, `brutal.py`, `freeze.py`, all of
which compare through `normalize.diff_rows` — went straight from `from_oracle` to
a byte comparison. So the answer key knew this deviation was expected and the
parity campaign did not: every covfuzz VIN whose charset mixes `_` with
alphanumerics was logged to `tests/parity_backlog.jsonl` as a fresh divergence,
and re-logged on the next run, because there was no decoder change that could ever
retire it. Four such entries had accumulated (WMI `1HD`, MY1999, position 7):

```
1HD2TW980XA084111  1HDCSM716XA084111  1HDCFPAP3XA084111  1HDCFG2P3XA084111

oracle    (4:148)(5:BCDEFGRS)(6:ACDEFGHJKLMNPRST)(7:HJKLMNPRSTVWX_)(11:JKTY)
ultravin  (4:148)(5:BCDEFGRS)(6:ACDEFGHJKLMNPRST)(7:_HJKLMNPRSTVWX)(11:JKTY)
```

Identical characters at identical positions; only the place of `_` differs, and
only in the one charset that contains it. That the byte order is the *host's* and
not the *data's* is directly demonstrable on the oracle itself — same rows, same
server, two collations:

```sql
select string_agg(c,'' order by c collate "C")           -- HJKLMNPRSTVWX_  (the oracle)
     , string_agg(c,'' order by c collate "en_US.utf8")  -- _HJKLMNPRSTVWX  (ultravin)
from unnest(string_to_array('H,J,K,L,M,N,P,R,S,T,V,W,X,_', ',')) as c;
```

`spvindecode_errorcode` builds the payload with `ORDER BY c` over
`tbl_spVinDecode_ErrorCode` (L79-86), so the sort happens at decode time on the
oracle host — it is not a stored string that came from NHTSA.

**Fix:** `diff_rows` now neutralizes element 144's within-charset order itself, so
every comparison site inherits the one definition of semantic equality instead of
each caller having to remember it. The neutralization is unchanged and still
narrow — charset *contents*, the position each charset is bound to, and the group
order are all still compared byte-for-byte, so a genuine element-144 regression
still diverges (`tests/test_normalize.py` pins exactly that boundary). ultravin's
own print order remains pinned by the `collation_tests` in `errors.rs`.

These VINs are **not** in `scripts/known_problems.json`, and this section
carries no anchor for one to point at. The registry names individual VINs whose
defect is re-probed one VIN at a time; this class is unbounded (any VIN reaching
one of the 95 mixed charsets) and is neutralized structurally in
`normalize.diff_rows` instead, so enumerating tonight's four would only invite
the next four.

---

## 4. Retired entries

An entry that stopped reproducing is retired, not kept — a stale excuse silently
forgives the next real regression on that VIN. The **known-problems** gate is
what catches one: it re-decodes every registered VIN against each new dump and
fails the refresh when one heals. This section is the log, so a reader who finds
a VIN missing from the registry can see it was checked rather than lost.

Retired sections carry no anchor: `scripts/known_problems.json` has no entry left
to point at one, and `tests/test_known_problems.py` requires every anchor to be
claimed.

- **W1LSB0L72VEJV2EPX** — stale `WMIYearValidChars` cache, registered 2026_06,
  **retired 2026-08-20 (dump 2026_08)**. The 2026_06 cache had been frozen
  mid-edit, between W1L schema 29239 (created 2026-05-01 14:41, updated 14:53)
  and schema 29240 (created 15:15), so its valid-character positions were
  `{9, 11}` where the same dump's `pattern` table gave `{8, 9, 11}`: the oracle
  missed position 8 and emitted code 14 where ultravin emitted code 4 with
  possible-values. The
  2026_08 rebuild picked schema 29240 up, the cell now agrees with the pattern
  source, and ultravin decodes the VIN **exactly**. The class itself did not go
  away — see section 2, where the same table is stale in the other direction.
