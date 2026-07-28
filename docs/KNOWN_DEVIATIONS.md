# Known deviations from the oracle

ultravin targets **byte-for-byte parity** with the official Postgres `vpic.spvindecode`.
The brutal multi-approach campaign (random + full systematic + coverage-guided
covfuzz, 134,661 divergences → 35 signatures) drove that to **exact parity on every
case except three — and in all three, the Postgres reference itself is defective.**
These are intentional, documented deviations where ultravin is *more correct*: the
first two where the oracle contradicts its own sources, the third where the dump
contradicts the SQL Server database NHTSA actually publishes from.

All three are error/partial-VIN-only (they affect the error-correction outputs
142/143/144/156/191); clean full-VIN decode is byte-identical to the oracle.

---

## 1. Oracle crashes on a malformed pattern regex — `7T0M6TGCURDSNZTHF`

The oracle aborts with a Postgres error inside `vpic.fvalidcharsinregex`:

```
invalid regular expression: invalid character range
```

A matched `Pattern.keys` contains a reversed bracket range (e.g. `[Z-A]`) that
Postgres' regex engine rejects, so `spvindecode` raises and returns **nothing**.
ultravin decodes the VIN normally (it tolerates the malformed class). You cannot
have parity with a crash, so this VIN is **excluded** from the regression corpus
(it can't be snapshotted). ultravin is strictly more robust here.

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
