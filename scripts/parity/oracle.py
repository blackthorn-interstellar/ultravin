"""Thin psycopg client over the Postgres parity oracle.

The oracle is the source of truth: `vpic.spvindecode` run against the pinned
.plain dump (deterministic — dedup tiebreak ends in id ASC), unmodified unless it
was loaded with ULTRAVIN_ORACLE_FAST_PROCS=1. Connection defaults match
docker-compose.yml (localhost:55432, db=vpic, user/pass=postgres).
"""

from __future__ import annotations

import os
from typing import Any

import psycopg
from psycopg.rows import dict_row

DSN = os.environ.get(
    "ULTRAVIN_ORACLE_DSN",
    "host=localhost port=55432 dbname=vpic user=postgres password=postgres",
)

# Decodes per transaction (ORACLE_TUNING step 6a). 1 = autocommit, the safe default.
# Grouping decodes amortizes per-commit overhead; 10 was the measured sweet spot and
# 100 was worse. Only sound against a fast-procs oracle (ULTRAVIN_ORACLE_FAST_PROCS=1
# at load time), whose procs `delete from` their temp tables instead of dropping them
# — that is what keeps decodes independent of each other inside one transaction.
BATCH = int(os.environ.get("ULTRAVIN_ORACLE_BATCH", "1"))


# NB: psycopg's dict_row overload confuses the type checker, so the connection
# is typed as Any here; rows are real dicts at runtime (row_factory=dict_row).
def connect(batch: int | None = None) -> Any:
    n = BATCH if batch is None else batch
    # autocommit: stock spvindecode creates/drops temp tables per call; without
    # per-call commits, lock objects accumulate and exhaust max_locks_per_transaction.
    conn: Any = psycopg.connect(DSN, row_factory=dict_row, autocommit=n <= 1)  # ty: ignore[invalid-argument-type]
    conn.uv_batch = n
    conn.uv_pending = 0
    return conn


def decode(conn: Any, vin: str) -> list[dict[str, Any]]:
    """Raw spvindecode rows for one VIN, ordered as the proc emits them."""
    with conn.cursor() as cur:
        cur.execute("select * from vpic.spvindecode(%s)", (vin,))
        rows = cur.fetchall()
    # No-op on the default autocommit connection, so one-VIN-at-a-time callers are
    # untouched; batched callers get a commit every `uv_batch` decodes. A partial
    # tail batch needs no explicit flush: decodes write nothing durable (temp tables
    # only), so it makes no difference whether the trailing transaction commits on
    # `with` exit or is rolled back at close.
    if not conn.autocommit:
        conn.uv_pending += 1
        if conn.uv_pending >= conn.uv_batch:
            conn.commit()
            conn.uv_pending = 0
    return rows


def current_year(conn: Any) -> int:
    with conn.cursor() as cur:
        cur.execute("select extract(year from now())::int as y")
        row = cur.fetchone()
        assert row is not None
        return int(row["y"])
