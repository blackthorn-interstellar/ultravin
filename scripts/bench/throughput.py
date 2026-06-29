"""60-second throughput benchmark: how many VINs can each engine decode?

Every engine decodes the same fixed corpus (scripts/bench/corpus.txt), cycling
through it until the wall-clock budget (default 60s) is spent, and reports the
total count and VIN/s. This is the single-stream, one-sequential-caller number;
ultravin additionally exposes a parallel batch path.

Engines:
  ultravin        in-process Python decode(), one VIN at a time
  ultravin-batch  in-process Python decode_batch(), parallel over the corpus
  postgres        NHTSA spvindecode over the live Postgres oracle (psycopg)
  mssql           NHTSA spVinDecode over SQL Server (pymssql)

Usage: python -m scripts.bench.throughput <engine> [--seconds 60] [--json]
"""

from __future__ import annotations

import argparse
import json
import os
import time
from pathlib import Path

CORPUS = Path(__file__).parent / "corpus.txt"


def load_corpus() -> list[str]:
    return [v for v in CORPUS.read_text().splitlines() if v]


def _result(engine: str, n: int, dt: float, extra: dict | None = None) -> dict:
    return {
        "engine": engine,
        "vins": n,
        "seconds": round(dt, 3),
        "vins_per_s": round(n / dt, 1),
        "vins_per_60s": round(n / dt * 60),
        **(extra or {}),
    }


def run_ultravin(vins: list[str], seconds: float) -> dict:
    import ultravin  # noqa: PLC0415  (lazy: keep optional deps optional)

    deadline = time.perf_counter() + seconds
    n, i, m = 0, 0, len(vins)
    t0 = time.perf_counter()
    while time.perf_counter() < deadline:
        ultravin.decode(vins[i % m])
        i += 1
        n += 1
    return _result("ultravin", n, time.perf_counter() - t0)


def run_ultravin_batch(vins: list[str], seconds: float) -> dict:
    import ultravin  # noqa: PLC0415  (lazy: keep optional deps optional)

    ultravin.decode_batch(vins)  # warm per-thread caches
    deadline = time.perf_counter() + seconds
    n = 0
    t0 = time.perf_counter()
    while time.perf_counter() < deadline:
        ultravin.decode_batch(vins)
        n += len(vins)
    return _result("ultravin-batch", n, time.perf_counter() - t0, {"cores": os.cpu_count()})


def run_postgres(vins: list[str], seconds: float) -> dict:
    from scripts.parity import oracle  # noqa: PLC0415  (lazy: keep optional deps optional)

    deadline = time.perf_counter() + seconds
    n, i, m = 0, 0, len(vins)
    with oracle.connect() as conn, conn.cursor() as cur:
        t0 = time.perf_counter()
        while time.perf_counter() < deadline:
            cur.execute("select * from vpic.spvindecode(%s)", (vins[i % m],))
            cur.fetchall()
            i += 1
            n += 1
    return _result("postgres", n, time.perf_counter() - t0)


def run_mssql(vins: list[str], seconds: float) -> dict:
    import pymssql  # noqa: PLC0415  (lazy: keep optional deps optional)

    dsn = os.environ.get(
        "ULTRAVIN_MSSQL_DSN",
        "server=localhost;port=1433;user=sa;password=Ultravin!2026;database=vPICList_lite",
    )
    kv = dict(p.split("=", 1) for p in dsn.split(";") if p)
    conn = pymssql.connect(
        server=kv["server"],
        port=kv.get("port", "1433"),
        user=kv["user"],
        password=kv["password"],
        database=kv["database"],
    )
    deadline = time.perf_counter() + seconds
    n, i, m = 0, 0, len(vins)
    cur = conn.cursor()
    t0 = time.perf_counter()
    while time.perf_counter() < deadline:
        cur.execute("EXEC dbo.spVinDecode @v=%s", (vins[i % m],))
        cur.fetchall()
        i += 1
        n += 1
    conn.close()
    return _result("mssql", n, time.perf_counter() - t0)


ENGINES = {
    "ultravin": run_ultravin,
    "ultravin-batch": run_ultravin_batch,
    "postgres": run_postgres,
    "mssql": run_mssql,
}


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("engine", choices=ENGINES)
    ap.add_argument("--seconds", type=float, default=60.0)
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args(argv)

    vins = load_corpus()
    res = ENGINES[args.engine](vins, args.seconds)
    if args.json:
        print(json.dumps(res))
    else:
        print(
            f"{res['engine']}: {res['vins']:,} VINs in {res['seconds']}s "
            f"= {res['vins_per_s']:,} VIN/s ({res['vins_per_60s']:,} in 60s)"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
