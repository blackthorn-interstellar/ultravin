# Known deviations from the oracle

ultravin targets **byte-for-byte parity** with the official Postgres `vpic.spvindecode`.
The brutal multi-approach campaign (random + full systematic + coverage-guided
covfuzz, 134,661 divergences → 35 signatures) drove that to **exact parity on every
case except three signatures — and in all three, the Postgres reference itself is
defective.**
These are intentional, documented deviations where ultravin is *more correct*: the
first two where the oracle contradicts its own sources, the third where the dump
contradicts the SQL Server database NHTSA actually publishes from.

All three are error/partial-VIN-only (they affect the error-correction outputs
142/143/144/156/191); clean full-VIN decode is byte-identical to the oracle.

The policy that governs *how* a divergence earns a place on this list — the bar
of evidence, the bounded scope, the freeze — is `docs/ACCEPTANCE.md`. This file
is the list it refers to.

**Entries expire.** Every data refresh re-decodes every VIN named here against
the new oracle (`scripts/parity/known_problems.py`) and the **known-problems**
gate fails the run if one stopped reproducing — a crash VIN the oracle now
answers, or a deviation VIN ultravin now matches. Upstream does fix things, and
a stale excuse is worse than no excuse: it silently forgives the next real
regression on that VIN. When the gate names one, verify it against the section's
evidence below, then drop it from `ORACLE_CRASH_VINS` / `KNOWN_DEVIATION_VINS`
in `scripts/refresh.py` and retire the section here once its last VIN is gone.

---

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
campaign hit it 62 more times, which was enough data to name the exact cause.

**The offending datum.** Exactly two rows in the 1,667,711-row `pattern` table
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
**fails** on any crash VIN that is not in `ORACLE_CRASH_VINS` in `scripts/refresh.py`
— the sample of 63 VINs observed so far. That list is a sample of an unbounded
class, so a new 7T0 MY2023-2025 VIN will fail the gate until a human re-verifies
it against this section. That is deliberate: a crash must never pass silently
just because a similar one was once explained.

`errors.rs` pins the tolerated expansion in a unit test, so if the class ever
starts resolving to something else, that is a change in ultravin, not a rediscovery
of this defect.

## 2. Stale `WMIYearValidChars` cache — `W1LSB0L72VEJV2EPX`

`spvindecode_errorcode` reads the precomputed `WMIYearValidChars` **cache** for the
per-position valid characters used in suggested-VIN / error-byte / unused-position
logic. That cache is a *derived snapshot* of the `pattern` source — and in the
2026_06 dump it is **stale**: it was built mid-edit, before a schema was added to
the same dump.

Proof (W1L / model year 2027):

```
cache (wmiyearvalidchars):    positions {9, 11}
computed from pattern source: positions {8, 9, 11}

W1L schemas applicable to 2027:
  29239  created 2026-05-01 14:41  updated 14:53   ← in the cache
  29240  created 2026-05-01 15:15                  ← position 8; NOT in the cache
```

The cache was frozen between 14:53 and 15:15; schema 29240 (which constrains
position 8) landed at 15:15. **The same dump's `pattern` table contains both
schemas.** So the oracle's *decode* matches schema 29240's patterns, but its
*error-correction* valid-chars (from the stale cache) don't know 29240 exists —
the oracle contradicts itself. ultravin computes valid-chars from `pattern` (the
source of truth), so it is **self-consistent** and reflects the dump's actual data:
it flags position 8 and emits error code 4 + possible-values where the oracle
(stale) emits code 14.

**Decision: keep ultravin's fresh, source-consistent computation.** Matching the
oracle here would mean embedding the stale cache (or its delta) purely to
reproduce a defect that self-heals the next time NHTSA rebuilds the cache. We do
not import the 8.8M-row `WMIYearValidChars` table. This deviation is frozen in the
parity corpus as a documented expectation so any *unexpected* change to it is caught.

---

## 3. The Postgres dump mis-collates element 144 — 172 VINs in the 2026_07 corpus

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
with alphanumerics: **95 of 1,682,382** `(wmi, year, position)` charsets do, across
**16 WMIs**. Everything else is alphanumeric-only, where C and SQL Server agree.

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

These VINs are **not** listed in `KNOWN_DEVIATION_VINS`. That list is for
individual upstream defects; this class is unbounded (any VIN reaching one of the
95 mixed charsets), so enumerating tonight's four would only invite the next four.
