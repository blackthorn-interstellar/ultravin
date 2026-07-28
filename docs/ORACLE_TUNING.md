# Oracle tuning: measured optimization ladders for Postgres and MSSQL

**Goal:** make the vPIC oracle (`vpic.spvindecode` / `dbo.spVinDecode`) fast enough for
bulk differential testing (10M–100M VINs), without touching decode logic on the
reference side.

**Method:** identical to `docs/BENCHMARKS.md` — `scripts/bench/corpus.txt` (5,000 VINs),
~10s warmup, 60-second timed runs, measured single-connection and at 8 parallel worker
processes (aggregate). One change per step, stacking the winners. Local reference
numbers for comparison: **Postgres 19.5 VIN/s**, **MSSQL 22.5 VIN/s** (single connection,
Apple Silicon; MSSQL under Rosetta amd64 emulation).

**Hardware:** one `c7a.2xlarge` (8 vCPU AMD EPYC, 16 GB, EBS gp3) per engine, us-east-1,
Ubuntu 24.04, engines in Docker. Total spend for the entire exercise: **~$1.24**
(PG $0.64 + $0.10 replacement, MSSQL $0.50).

**Data:** Postgres loaded vPIC `2026_07` (11.0M rows); MSSQL restored
`vPICList_lite_2026_06.bak`. The one-month skew is irrelevant for throughput
(near-identical row counts).

**Headline:**

| Engine | Stock (single / 8-conn) | Optimized (single / 8-conn) | Gain |
|---|---|---|---|
| Postgres 16 | 13.1 / 76.9 VIN/s | **52.4 / 413.6 VIN/s** | 4.0× / 5.4× |
| SQL Server 2022 | 38.9 / 194.2 VIN/s | **40.3 / 285.2 VIN/s** | 1.04× / 1.5× |

Optimized Postgres delivers **~51.7 VIN/s per vCPU** vs MSSQL's ~35.6 — Postgres wins by
1.45× once the temp-table rewrite hands it the proc-temp-table caching SQL Server has
natively.

---

## Postgres ladder

Every step below stacks on the previous kept step.

| # | Change | Single | 8-conn | Verdict |
|---|---|---|---|---|
| 0 | Baseline: `postgres:16-alpine`, stock config, EBS volume, TCP | 13.1 | 76.9 | — |
| 1 | Image swap → `postgres:16` (Debian/glibc) | 22.5 | 129.6 | **kept, +72%** |
| 2 | Reckless config (see below) | 23.9 | 141.9 | kept, +6% |
| 3 | PGDATA on tmpfs | 25.0 | 148.3 | kept, +5% |
| 4 | Unix-domain socket instead of TCP | 25.0 | 148.5 | kept, ~0% |
| 5 | **Temp-table rewrite** (see below) | 43.9 | 332.7 | **kept, +75% / +124%** |
| 6a | Transaction batching, 10 decodes/txn | 52.0 | 415.3 | **kept, +18% / +25%** |
| 6b | Transaction batching, 100 decodes/txn | 44.0 | 346.0 | reverted — worse than 10 |
| — | Final stacked recipe, re-measured fresh (batch=10) | 52.4 | 413.6 | reproduced: 50.1 / 403.3 |

### Step 1 — the musl tax (+72%)

The single largest one-line win. `postgres:16-alpine` links musl libc, whose allocator
is much slower than glibc's for allocation-heavy work — and interpreted plpgsql with
per-call regex evaluation allocates constantly. Swapping to the Debian-based image
(identical Postgres 16.14) took 13.1 → 22.5 VIN/s. The local docker-compose oracle uses
the alpine image and is paying this tax today.

### Step 2 — reckless config (+6%)

All durability and background-work costs removed (valid only for a disposable,
reloadable oracle — never for anything that must survive a crash):

```
fsync = off
synchronous_commit = off
full_page_writes = off
wal_level = minimal            # + max_wal_senders = 0
autovacuum = off
shared_buffers = 4GB
work_mem = 64MB
temp_buffers = 64MB
checkpoint_timeout = 1d
max_wal_size = 16GB
jit = off
```

Only +6%: the dataset (~3 GB) is fully cache-resident and the workload is CPU in the
plpgsql interpreter, so removing I/O safety barely registers. The main WAL producer was
the per-decode temp-table catalog churn — attacked directly in step 5.

### Steps 3–4 — tmpfs (+5%) and unix sockets (~0%)

tmpfs removes the file create/unlink syscalls for per-decode temp-table relfilenodes and
any residual page writeback. Sockets vs TCP was a wash — the ~40ms/decode of server CPU
dwarfs localhost protocol overhead.

### Step 5 is not optional at scale (measured 2026-07-28)

The ladder presents the rewrite as a throughput win. Building a 1.8M-VIN answer
key showed it is a correctness prerequisite for bulk runs, twice:

- With `autovacuum = off` (step 2), stock procs filled an 8 GB volume about 40k
  decodes into a 113k-VIN shard: 438k `DiskFull`, then 963k `OperationalError`
  as connections died behind it. The key published as 77% holes and still passed
  verification, because only what it had was compared.
- With autovacuum back **on**, stock procs still only reached ~33k decodes of a
  57k slice before the same failure. Autovacuum does not keep up with 9-20 temp
  table drops per decode; it races the churn and loses.

So `autovacuum=off` is dropped from the local recipe, and the answer-key build
runs with `ULTRAVIN_ORACLE_FAST_PROCS=1`. The equivalence job that gates it is
sized to ~14k VINs precisely because the stock side cannot go further.

### Step 5 — the temp-table rewrite (+75% single, +124% aggregate)

The big lever. Stock procs create and drop **9–20 temp tables per decode** (`spvindecode`
2, `spvindecode_core` 4 × 1–3 passes, `spvindecode_errorcode` 3, `fvalidcharsinkey` 1 per
call). Each CREATE/DROP writes and deletes rows in `pg_class`, `pg_attribute`, `pg_type`,
`pg_depend`, WAL-logs those catalog changes, commits them per call, broadcasts cache
invalidations — and, because the table OIDs change every call, invalidates plpgsql's
cached plans for every statement touching them, forcing per-call replans.

The transform is mechanical across `vpic/procs/*.sql` (the procs already use
`create temporary table IF NOT EXISTS`):

- `on commit drop` → `on commit delete rows` (empties in place at commit via
  `heap_truncate`: no catalog write, no new relfilenode, no WAL)
- every `drop table X;` → `delete from X;` (zero catalog churn; preserves semantics if
  a caller ever runs multiple decodes in one transaction)
- connections set `client_min_messages = warning` (suppresses the per-call
  "relation already exists, skipping" notice)

After the first call on a connection: zero catalog writes, zero DDL WAL, zero file
churn, stable OIDs → plans cached across calls.

**Equivalence gate** (rewritten vs stock procs, same data, 5,292 VINs = 5,000 corpus +
272 parity-corpus + 20 invalid/short/bad-char):

- **Content mismatches: 0.**
- Order-only mismatches: 9 — identical row multisets, different order among tie rows
  (dead-tuple physics changes physical scan order where the proc's ORDER BY doesn't
  fully determine it). Under batch=100 this rises to 2,560. The parity framework
  already treats intra-group row order as non-semantic (`scripts/parity/normalize`
  excludes it; `freeze` tiebreaks by id), so canonical-form comparisons are unaffected —
  but a strict byte-ordered comparison would flag these. Concentrated on
  malformed-pattern VINs (`#`/`?` characters).

### Step 6 — transaction batching (+18% at 10/txn)

With the rewrite in place (explicit `delete from` keeps calls independent within a
transaction), grouping 10 decodes per `BEGIN…COMMIT` amortizes per-commit overhead:
52.0/415 VIN/s. Requires `max_locks_per_transaction = 4096`. Batch=100 is *worse*
(44/346) and multiplies order-only tie diffs — use 10.

### Connection-count sweep — oversubscription never helps

On the final recipe, aggregate VIN/s by worker count (60s per point):

| 8 workers | 12 | 16 | 24 |
|---|---|---|---|
| **403.3** | 377.9 | 368.2 | 361.2 |

Monotonically decreasing. One connection per core is exactly right: during runs Postgres
consumed 790–794% of 800% CPU while the bench clients used 3–6% — there are no idle gaps
for extra connections to fill, only context-switch overhead to add.

---

## MSSQL ladder

SQL Server runs the **unmodified original** `dbo.spVinDecode` — it is the true reference,
so only environment changes were allowed. Decode output was hash-verified identical for
3 canonical VINs after every restart-requiring step (8 recordings, identical throughout).

| # | Change | Single | 8-conn | Verdict |
|---|---|---|---|---|
| 0 | Baseline: stock SQL Server 2022 container (CU26), EBS | 38.9 | 194.2 | — |
| 1 | `DELAYED_DURABILITY = FORCED` | 40.5 | 232.6 | kept |
| 2 | tempdb → tmpfs (8 data files 256MB + log 128MB) | 38.8 | 269.6 | kept |
| 3 | `MEMORY_OPTIMIZED TEMPDB_METADATA = ON` | 37.5 | 264.6 | **reverted — regression** |
| 4 | `max server memory` = 12 GB + full warm pass | 40.8 | 263.7 | kept |
| 5 | Whole `/var/opt/mssql` on tmpfs | 40.3 | 232.2 | reverted — no gain, 8-conn worse |
| 6 | `--network host` (drop docker NAT hop) | 40.8 | 265.3 | dropped — no effect |
| 6b | Trace flag 8008 (least-loaded scheduler assignment) | 40.3 | 280.0 | **kept — kills run variance** |
| 6c | `COMPATIBILITY_LEVEL = 160` (shipped at 150) | 40.3 | 286.2 | kept |
| — | Final stacked recipe, re-measured | 40.3 | 285.2 | reproduced: 39.9 / 284.6 |

### Notable findings

- **The local 22.5 VIN/s was emulation tax, not SQL Server's speed.** Native x86 stock
  is 38.9 VIN/s — +73% before any tuning.
- **The temp-table thesis, confirmed from the other side.** `MEMORY_OPTIMIZED
  TEMPDB_METADATA` — a feature built precisely for temp-table metadata contention —
  measurably *regressed* (−3% single). SQL Server's stored-proc temp-table caching
  already eliminates the churn Postgres suffers; there was nothing left for the feature
  to fix. The Postgres rewrite (step 5 above) hand-implements what MSSQL does natively.
- **The scheduler lottery.** Before TF8008, 8-conn runs swung 194–270 VIN/s because
  connections sometimes landed two-to-a-SQLOS-scheduler: doubled-up workers ran at
  exactly half speed (~19.5 vs ~39 solo) — a clean natural experiment showing
  per-scheduler oversubscription is zero-sum. TF8008 (assign to least-loaded scheduler)
  made per-worker rates uniform (33–36 each) and the aggregate stable at 280–286.
- **Nothing storage-side matters once cached.** The 1.2 GB database fits in the buffer
  pool; whole-DB tmpfs and host networking were both no-ops. Single-connection
  throughput was essentially untunable (38–41 across every config): ~25ms/call is
  `spVinDecode`'s own CPU.
- The 2022 container already ships 8 tempdb data files by default; step 2's gain came
  from tmpfs relocation + sizing, not file count.

---

## Cross-engine conclusions

1. **The decode cost is interpreter CPU, not I/O and not safety.** All
  durability/storage tricks combined: +11% (PG), ~+20% mostly-variance (MSSQL). Both
  boxes ran pinned at ~100% server CPU. Cores are the only scaling axis that matters.
2. **Temp-table lifecycle was the dominant removable overhead in Postgres** (+75%
  single / +124% aggregate), and its natural absence in MSSQL explains why MSSQL had so
  little headroom. Postgres also pays a large allocator tax on alpine/musl (+72% from
  the image swap alone).
3. **Connections = cores, exactly.** Measured monotonic decline past 1/core on Postgres;
  zero-sum scheduler doubling on MSSQL.
4. **Reference integrity held:** MSSQL procs untouched; Postgres rewrite is
  content-equivalent over 5,292 VINs (0 mismatches) with a documented order-only tie
  caveat that the parity framework's canonical form already absorbs.

**Bulk-testing economics at ~51.7 VIN/s per vCPU (optimized Postgres, batch=10):**

| Volume | 48 vCPU (Hetzner CCX63, ~$1.10/hr) | 20× 4-vCPU GH Actions jobs ($0) | 192-vCPU spot |
|---|---|---|---|
| 10M VINs | ~67 min, ~$1.30 | ~40 min, one wave | ~17 min, ~$1–2 |
| 100M VINs | ~11 h, ~$12 | ~7 h (multi-wave, heavy) | ~2.8 h, ~$8–15 |

## Applied to this repo

The Postgres ladder is now landed in the local oracle. What went where:

| Step | Where | Default |
|---|---|---|
| 1 image swap → `postgres:16` (Debian/glibc) | `docker-compose.yml` | always on |
| 2 reckless config | `docker-compose.yml` `command:` | always on |
| 3 PGDATA on tmpfs | `docker-compose.yml`, **primary `oracle` only** | always on |
| 5 temp-table rewrite | `scripts/oracle.sh` `fast_procs()` | **off** — `ULTRAVIN_ORACLE_FAST_PROCS=1` |
| 6a batching, 10/txn | `scripts/parity/oracle.py` | **off** — `ULTRAVIN_ORACLE_BATCH=10` |

Step 4 (unix sockets) was skipped: measured a wash above, and it would cost a bind mount.

Two sizing deviations, both forced by the Docker VM (~11.7 GB) having to hold five
oracles where the EC2 box held one: `shared_buffers` is 1 GB rather than 4 GB (the DB
is 1.4 GB and cache-resident either way — step 2's gain was the durability barriers,
not buffer size), and tmpfs is on the primary `oracle` service only, with the pool left
on disk. Note the primary now loses its data on *any* restart, not just `down -v`.

### Local re-measurement (10-core Apple Silicon, Docker Desktop VM)

Same method, 8s warmup + 30s timed, corpus VINs, 1 and 4 connections. This machine
degrades past 4 connections, so 4 is the aggregate point rather than the doc's 8.

| Config | Single | 4-conn |
|---|---|---|
| Baseline: `postgres:16-alpine`, stock config, disk, stock procs | 10.0 | 25.5 |
| + steps 1–3 (glibc image, reckless config, tmpfs) | 13.1 | 29.5 |
| + step 5 (temp-table rewrite) | 17.4 | 49.2 |
| + step 6a (batch=10) | **17.6** | **52.3** |
| | **1.76×** | **2.05×** |

The shape reproduces — the rewrite is the dominant lever (+33% single, +67% aggregate)
— but the magnitudes are smaller than EC2's. Two honest caveats:

- **Steps 1–3 gave +31%/+16% here, not +72%.** This is aarch64 under a Docker Desktop
  VM, not x86 EPYC on bare Ubuntu; the musl allocator penalty is real but nothing like
  the x86 measurement. Kept anyway: it is free, and it is strictly positive.
- **Batching gave +1% single / +6% at 4 conns, not +18%/+25%.** Single-connection is
  inside the noise. Kept because it is opt-in, never negative, and the aggregate gain
  is where bulk runs live. Do not expect it to matter on one connection here.

Load time also dropped from minutes to ~35s (stock procs) / ~52s (rewritten), a
side-effect of the reckless config plus tmpfs.

### Equivalence, re-verified locally

`vpic.spvindecode` under the rewritten procs vs. under the stock procs, same data, 399
VINs (350 spread across the bench corpus + 49 malformed: short, bad-char, broken check
digit, empty, and 40 `#`/`?` pattern VINs — the shapes the EC2 run found order-only
diffs on): **0 content mismatches, 0 order-only mismatches**, under
`scripts/parity/normalize.py`'s canonical comparison. The same 399 VINs also pass the
full ultravin-vs-oracle parity sweep against the rewritten oracle, batched and
unbatched.

### The trust-story cost of step 5

`docs/ACCEPTANCE.md` defines the parity oracle as the dump's `spvindecode` run
**unmodified**. With `ULTRAVIN_ORACLE_FAST_PROCS=1` it is not literally unmodified —
it is a mechanically rewritten oracle whose equivalence is *measured* rather than
*definitional*. That is a real, if narrow, weakening: parity would no longer be proven
against NHTSA's exact text, only against a transform of it that has never yet produced
a differing answer.

Hence the default is off. Correctness runs (`make check`, the frozen corpus, answer-key
builds, anything whose output becomes a committed baseline) get the untouched oracle.
Bulk differential runs, where the alternative is fewer VINs tested, can opt in. The
committed `vpic/procs/*.sql` is never edited — the transform is applied to the dump
stream at load time — so the `vpic-import` byte-reproduction gate is unaffected.

## Reproduction

**Postgres:** `postgres:16` (Debian) on tmpfs, reckless config above, procs transformed
per step 5, `client_min_messages=warning`, `max_locks_per_transaction=4096`, 10
decodes/txn, workers = vCPUs. Rewritten procs: session scratchpad `pg-fast-procs/`
(regenerable mechanically from `vpic/procs/` with the two substitutions).

**MSSQL:** `mcr.microsoft.com/mssql/server:2022-latest` (Developer), restore bak, then:
`DELAYED_DURABILITY=FORCED`; relocate 8 tempdb files + log to tmpfs (256MB/128MB);
`max server memory` 12288; `mssql.conf` `[traceflag] traceflag0=8008`;
`COMPATIBILITY_LEVEL=160`; restart; one warm corpus pass. Leave
`MEMORY_OPTIMIZED TEMPDB_METADATA` **off**.

*Measured July 2026 on c7a.2xlarge (EC2). Raw logs: `pg-bench-summary.json`,
`results.jsonl`, per-step logs (session scratchpad).*
