"""Thin psycopg client over the unmodified Postgres parity oracle.

The oracle is the source of truth: `vpic.spvindecode` run against the pinned
.plain dump (deterministic — dedup tiebreak ends in id ASC). Connection defaults
match docker-compose.yml (localhost:55432, db=vpic, user/pass=postgres).
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


# NB: psycopg's dict_row overload confuses the type checker, so the connection
# is typed as Any here; rows are real dicts at runtime (row_factory=dict_row).
def connect() -> Any:
    # autocommit: spvindecode creates/drops temp tables per call; without
    # per-call commits, lock objects accumulate and exhaust max_locks_per_transaction.
    return psycopg.connect(DSN, row_factory=dict_row, autocommit=True)  # ty: ignore[invalid-argument-type]


def decode(conn: Any, vin: str) -> list[dict[str, Any]]:
    """Raw spvindecode rows for one VIN, ordered as the proc emits them."""
    with conn.cursor() as cur:
        cur.execute("select * from vpic.spvindecode(%s)", (vin,))
        return cur.fetchall()


def current_year(conn: Any) -> int:
    with conn.cursor() as cur:
        cur.execute("select extract(year from now())::int as y")
        row = cur.fetchone()
        assert row is not None
        return int(row["y"])
