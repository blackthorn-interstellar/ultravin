"""Comprehensive, resumable, detached parity campaign across the 5-oracle pool.

Two engines, run as separate chunked processes (one `run --engine` each):
  - systematic: backward model-year march YEAR_HI -> YEAR_LO, 3 years in flight,
    exhaustive per year (every WMI x schema x distinct keys). Hard-complete.
  - covfuzz:    coverage-guided fuzzer (ultravin is the coverage signal, includes
    truncation so it subsumes plain fuzz); only coverage-expanding VINs reach the
    oracle; runs until coverage saturates.

Each `run` works for --max-seconds, then checkpoints and exits: 0 = more to do,
42 = engine done. A supervisor restarts it, so it resumes after any kill. State,
per-engine fail logs (JSONL), and status-<engine>.json live under --dir.
"""

from __future__ import annotations

import argparse
import json
import pickle
import random
import time
from multiprocessing import Pool, Queue
from pathlib import Path
from typing import Any

import psycopg
import ultravin
from psycopg.rows import dict_row

from scripts.parity import brutal, generator, normalize, oracle

YEAR_HI = 2027  # current model year + 2 (decode cap)
YEAR_LO = 1980  # earliest yearfrom in the data
COV_SATURATE = 2_000_000  # covfuzz done after this many consecutive no-new-coverage tries

# --------------------------------------------------------------------------- #
# Worker: each holds one oracle connection (round-robin over the engine's ports)
# --------------------------------------------------------------------------- #
_conn: Any = None


def _winit(q: Queue) -> None:
    global _conn
    _conn = psycopg.connect(q.get(), row_factory=dict_row, autocommit=True)  # ty: ignore[invalid-argument-type]


def _check(case: dict[str, Any]) -> tuple[str, dict[str, Any]] | None:
    vin, eng = case["vin"], case["engine"]
    try:
        o = [normalize.from_oracle(r) for r in oracle.decode(_conn, vin)]
        m = normalize.ultravin_rows(ultravin.decode(vin))
        d = normalize.diff_rows(o, m)
    except Exception as e:  # noqa: BLE001
        return (eng, {"vin": vin, "engine": eng, "error": repr(e)[:200]})
    if d["ok"]:
        return None
    return (
        eng,
        {
            "vin": vin,
            "engine": eng,
            "fingerprint": normalize.fingerprint(d),
            "field_diffs": d["field_diffs"][:30],
            "missing": [r["element_id"] for r in d["missing"]],
            "extra": [r["element_id"] for r in d["extra"]],
        },
    )


def _pool(ports: list[int], workers: int):
    q: Queue = Queue()
    for i in range(workers):
        q.put(f"host=localhost port={ports[i % len(ports)]} dbname=vpic user=postgres password=postgres")
    return Pool(workers, initializer=_winit, initargs=(q,))


# --------------------------------------------------------------------------- #
# systematic engine: 3-year-concurrent backward march
# --------------------------------------------------------------------------- #
def _year_rows(conn: Any, year: int) -> list[dict[str, Any]]:
    """All (wmi, keys) for a model year, deterministic order. ~80-230k rows."""
    with conn.cursor() as cur:
        cur.execute(
            "select w.wmi as wmi, p.keys as keys from vpic.wmi w "
            "join vpic.wmi_vinschema wvs on wvs.wmiid=w.id "
            "  and %s between wvs.yearfrom and coalesce(wvs.yearto,2999) "
            "join (select distinct vinschemaid, keys from vpic.pattern) p "
            "  on p.vinschemaid=wvs.vinschemaid "
            "order by w.id, wvs.vinschemaid, p.keys",
            (year,),
        )
        return cur.fetchall()


def systematic_producer(state: dict[str, Any], enum_conn: Any, deadline: float):
    active = state["active"]

    def ensure() -> None:
        while len(active) < 3 and state["next_year"] >= YEAR_LO:
            active.append({"year": state["next_year"], "offset": 0})
            state["next_year"] -= 1

    ensure()
    rows = {a["year"]: _year_rows(enum_conn, a["year"]) for a in active}
    while active:
        if time.time() > deadline:
            return
        for a in list(active):
            r = rows[a["year"]]
            if a["offset"] >= len(r):
                state["done_years"].append(a["year"])
                active.remove(a)
                rows.pop(a["year"], None)
                ensure()
                for na in active:
                    if na["year"] not in rows:
                        rows[na["year"]] = _year_rows(enum_conn, na["year"])
                continue
            row = r[a["offset"]]
            a["offset"] += 1
            yield {"vin": generator.build_vin(row["wmi"], row["keys"], a["year"]), "engine": "systematic"}


# --------------------------------------------------------------------------- #
# covfuzz engine: coverage-guided (with truncation), persists coverage to resume
# --------------------------------------------------------------------------- #
def covfuzz_producer(state: dict[str, Any], cov: dict[str, Any], deadline: float):
    rng = random.Random(98765 + state["counter"])
    if not cov["corpus"]:
        cov["corpus"] = brutal._seed_corpus(800)
    vds = [3, 4, 5, 6, 7]
    while True:
        if time.time() > deadline or state["saturated"]:
            return
        state["counter"] += 1
        base = rng.choice(cov["corpus"])
        if rng.random() < 0.15:
            cand = base[: rng.choice([8, 11, 14, 16])]  # truncation -> subsumes plain fuzz
        else:
            vinl = list(base)
            for _ in range(rng.choice([1, 1, 2])):
                vinl[rng.choice([*vds, 9])] = rng.choice(brutal.VIN_ALPHABET)
            if rng.random() < 0.2:
                vinl[rng.randrange(17)] = rng.choice(brutal.ALL_CHARS)
            elif rng.random() < 0.5:
                vinl[8] = "0"
                vinl[8] = generator.check_digit(vinl)
            cand = "".join(vinl)
        try:
            edges = brutal.ultravin_coverage(cand)
        except Exception:  # noqa: BLE001
            continue
        new = {hash(e) & 0xFFFFFFFFFFFFFFFF for e in edges} - cov["seen"]
        if new:
            cov["seen"] |= new
            if len(cand) == 17:
                cov["corpus"].append(cand)
                if len(cov["corpus"]) > 60000:
                    cov["corpus"].pop(rng.randrange(len(cov["corpus"])))
            state["since_new"] = 0
            yield {"vin": cand, "engine": "covfuzz"}
        else:
            state["since_new"] += 1
            if state["since_new"] > COV_SATURATE:
                state["saturated"] = True
                return


# --------------------------------------------------------------------------- #
def _status(
    eng: str, state: dict[str, Any], cov: dict[str, Any] | None, chunk_tested: int, elapsed: float
) -> dict[str, Any]:
    base = {
        "engine": eng,
        "total_tested": state["tested"],
        "total_failures": state["failures"],
        "chunk_tested": chunk_tested,
        "chunk_rate_per_s": round(chunk_tested / elapsed, 1) if elapsed > 0 else 0,
    }
    if eng == "systematic":
        base |= {
            "years_done": len(state["done_years"]),
            "active_years": [a["year"] for a in state["active"]],
            "next_year": state["next_year"],
        }
    else:
        base |= {
            "candidates_tried": state["counter"],
            "since_new_coverage": state["since_new"],
            "coverage_edges": len(cov["seen"]) if cov else 0,
            "saturated": state["saturated"],
        }
    return base


def cmd_run(args: argparse.Namespace) -> int:
    d = Path(args.dir)
    d.mkdir(parents=True, exist_ok=True)
    eng = args.engine
    statef = d / f"state-{eng}.json"
    faill = d / f"fails-{eng}.jsonl"
    ports = [int(p) for p in args.ports.split(",")]
    deadline = time.time() + args.max_seconds

    cov: dict[str, Any] | None = None
    state: dict[str, Any]
    if eng == "systematic":
        state = (
            json.loads(statef.read_text())
            if statef.exists()
            else {"next_year": YEAR_HI, "active": [], "done_years": [], "tested": 0, "failures": 0}
        )
        enum_conn = oracle.connect()
        prod = systematic_producer(state, enum_conn, deadline)
        done_fn = lambda: state["next_year"] < YEAR_LO and not state["active"]  # noqa: E731
    else:
        state = (
            json.loads(statef.read_text())
            if statef.exists()
            else {"counter": 0, "since_new": 0, "saturated": False, "tested": 0, "failures": 0}
        )
        covf = d / "coverage.pkl"
        cov = pickle.loads(covf.read_bytes()) if covf.exists() else {"seen": set(), "corpus": []}
        prod = covfuzz_producer(state, cov, deadline)
        done_fn = lambda: state["saturated"]  # noqa: E731

    def persist() -> None:
        statef.write_text(json.dumps(state))
        if cov is not None:
            (d / "coverage.pkl").write_bytes(pickle.dumps(cov))
        (d / f"status-{eng}.json").write_text(json.dumps(_status(eng, state, cov, tested, time.time() - t0)))

    fh = open(faill, "a")  # noqa: SIM115
    t0 = last = time.time()
    tested = 0
    try:
        with _pool(ports, args.workers) as pool:
            for res in pool.imap_unordered(_check, prod, chunksize=4):
                tested += 1
                state["tested"] += 1
                if res is not None:
                    state["failures"] += 1
                    fh.write(json.dumps(res[1]) + "\n")
                now = time.time()
                if now - last > 30:
                    fh.flush()
                    persist()
                    last = now
                if now > deadline:
                    break
    finally:
        fh.flush()
        fh.close()
        persist()
    done = done_fn()
    print(
        f"[campaign:{eng}] chunk tested={tested} total={state['tested']} fails={state['failures']} done={done}",
        flush=True,
    )
    return 42 if done else 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="Comprehensive resumable parity campaign.")
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run", help="run one chunk of one engine")
    r.add_argument("--engine", required=True, choices=["systematic", "covfuzz"])
    r.add_argument("--ports", required=True, help="comma list of oracle ports for this engine")
    r.add_argument("--workers", type=int, default=6)
    r.add_argument("--max-seconds", type=int, default=1800, dest="max_seconds")
    r.add_argument("--dir", default="campaign")
    args = ap.parse_args(argv)
    if args.cmd == "run":
        return cmd_run(args)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
