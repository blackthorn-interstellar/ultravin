"""Brutal parity campaign: generate VINs many ways, diff each vs the oracle, log failures.

Three generators:
  - random:     random strings over the VIN alphabet (+ some with I/O/Q and odd
                lengths) — hammers WMI-miss / no-pattern / bad-char / suggested-VIN.
  - systematic: db-driven, exhaustive — every WMI x every Wmi_VinSchema x every
                distinct Pattern.keys x representative model years. This covers
                every make/model/trim/feature decode path the data can express.
  - fuzz:       coverage-targeted mutations of valid systematic VINs (corrupt
                check digit, inject I/O/Q, truncate, flip VDS/year chars).

Each VIN is decoded by the installed `ultravin` (output is identical in debug or
release) AND by `vpic.spvindecode`, then diffed field-for-field with
`normalize.diff_rows` (intra-group row order already excluded). Failures stream
to a JSONL log. `dedupe` clusters failures by bug signature into a minimal set.

Parallelism is a process pool (each worker holds its own oracle connection); the
oracle (~60 ms/decode) is the bottleneck, so throughput ~= workers x ~16/s.

    uv run -- python -m scripts.parity.brutal run --modes all --workers 8 \
        --random 200000 --max-seconds 7200 --fail-log fails.jsonl
    uv run -- python -m scripts.parity.brutal dedupe --fail-log fails.jsonl --out reduced.json
"""

from __future__ import annotations

import argparse
import itertools
import json
import random as _random
import sys
import time
from collections.abc import Iterator
from multiprocessing import Pool
from typing import Any

import ultravin

from scripts.parity import generator, normalize, oracle

VIN_ALPHABET = "ABCDEFGHJKLMNPRSTUVWXYZ0123456789"  # valid: no I, O, Q
ALL_CHARS = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"  # includes I/O/Q for invalids
_YEAR_CHARS = "ABCDEFGHJKLMNPRSTVWXY123456789"


# --------------------------------------------------------------------------- #
# Generators (stream small dicts: {"vin", "mode", "note"})
# --------------------------------------------------------------------------- #
def gen_random(n: int, seed: int) -> Iterator[dict[str, Any]]:
    rng = _random.Random(seed)
    for i in range(n):
        roll = rng.random()
        if roll < 0.80:
            vin = "".join(rng.choice(VIN_ALPHABET) for _ in range(17))
        elif roll < 0.92:
            vin = "".join(rng.choice(ALL_CHARS) for _ in range(17))  # may contain I/O/Q
        else:
            # <=16 only: spvindecode's vin is varchar(17), so >17 raises
            # StringDataRightTruncation in the oracle and can't be compared.
            ln = rng.choice([0, 3, 8, 11, 14, 16])
            vin = "".join(rng.choice(ALL_CHARS) for _ in range(ln))
        yield {"vin": vin, "mode": "random", "note": f"r{i}"}


def _sample_years(year_from: int, year_to: int | None, cur_year: int, k: int) -> list[int]:
    cap = cur_year + 2
    hi = min(year_to if year_to is not None else 9999, cap, 2039)
    lo = max(year_from, 2010)
    if lo > hi:
        edge = year_to if (year_to is not None and year_to < 2010) else year_from
        return [edge]
    if k <= 1 or lo == hi:
        return sorted({lo, hi})
    mid = (lo + hi) // 2
    return sorted({lo, mid, hi})


def gen_systematic(shard: int, shards: int, years: int) -> Iterator[dict[str, Any]]:
    """Every WMI x schema x distinct keys x representative years (db-driven)."""
    with oracle.connect() as conn:
        cur_year = oracle.current_year(conn)
        with conn.cursor() as cur:
            cur.execute("select id, wmi from vpic.wmi order by id")
            wmis = cur.fetchall()
        for idx, w in enumerate(wmis):
            if idx % shards != shard:
                continue
            with conn.cursor() as cur:
                cur.execute(
                    "select vinschemaid, yearfrom, yearto from vpic.wmi_vinschema where wmiid=%s",
                    (w["id"],),
                )
                links = cur.fetchall()
            for link in links:
                yrs = _sample_years(link["yearfrom"], link["yearto"], cur_year, years)
                with conn.cursor() as cur:
                    cur.execute(
                        "select distinct keys from vpic.pattern where vinschemaid=%s order by keys",
                        (link["vinschemaid"],),
                    )
                    keyrows = cur.fetchall()
                for kr in keyrows:
                    for y in yrs:
                        vin = generator.build_vin(w["wmi"], kr["keys"], y)
                        yield {
                            "vin": vin,
                            "mode": "systematic",
                            "note": f"{w['wmi']}:{link['vinschemaid']}:{y}:{kr['keys']}",
                        }


def gen_fuzz(shard: int, shards: int, per_base: int, seed: int) -> Iterator[dict[str, Any]]:
    rng = _random.Random(seed)
    for base in gen_systematic(shard, shards, years=1):
        vin = base["vin"]
        if len(vin) != 17:
            continue
        muts = [
            vin[:8] + ("0" if vin[8] != "0" else "1") + vin[9:],  # corrupt check digit
            vin[: (p := rng.randrange(17))] + rng.choice("IOQ") + vin[p + 1 :],  # invalid char
            vin[: rng.choice([8, 11, 14, 16])],  # truncate
            vin[: (q := rng.choice([3, 4, 5, 6, 7]))] + rng.choice(VIN_ALPHABET) + vin[q + 1 :],  # flip VDS
            vin[:9] + rng.choice(_YEAR_CHARS) + vin[10:],  # change year char
        ]
        for m in muts[:per_base]:
            yield {"vin": m, "mode": "fuzz", "note": "mut:" + base["note"][:48]}


# --------------------------------------------------------------------------- #
# Coverage-seeking fuzzer: ultravin's fast decode is the coverage feedback;
# only VINs that EXPAND decode coverage are forwarded to the slow oracle.
# --------------------------------------------------------------------------- #
def ultravin_coverage(vin: str) -> set[tuple]:
    """Decode features a VIN exercises (ultravin only, ~200us): each
    (vin_schema, pattern) matched, each (element, source) resolved, and the
    error-code combination. The union of these across VINs is 'coverage'."""
    r: Any = ultravin.decode(vin)
    edges: set[tuple] = set()
    for e in r.get("elements", []):
        pid = e.get("pattern_id")
        if pid:
            edges.add(("p", e.get("vin_schema_id"), pid))
        edges.add(("e", e.get("element_id"), e.get("source")))
    edges.add(("c", tuple(sorted(r.get("error_codes", []) or []))))
    return edges


def _seed_corpus(n_seeds: int) -> list[str]:
    """Diverse valid seeds: one VIN per sampled WMI (first key, recent year)."""
    seeds: list[str] = []
    with oracle.connect() as conn:
        cur_year = oracle.current_year(conn)
        with conn.cursor() as cur:
            cur.execute("select id, wmi from vpic.wmi order by id")
            wmis = cur.fetchall()
        step = max(1, len(wmis) // max(1, n_seeds))
        for w in wmis[::step][:n_seeds]:
            with conn.cursor() as cur:
                cur.execute(
                    "select vinschemaid, yearfrom, yearto from vpic.wmi_vinschema where wmiid=%s limit 1",
                    (w["id"],),
                )
                link = cur.fetchone()
            if link is None:
                continue
            with conn.cursor() as cur:
                cur.execute(
                    "select keys from vpic.pattern where vinschemaid=%s order by keys limit 1",
                    (link["vinschemaid"],),
                )
                pr = cur.fetchone()
            if pr is None:
                continue
            y = generator.choose_year(link["yearfrom"], link["yearto"], cur_year)
            seeds.append(generator.build_vin(w["wmi"], pr["keys"], y))
    return seeds


def gen_covfuzz(n_seeds: int, budget: int, seed: int) -> Iterator[dict[str, Any]]:
    """Coverage-guided: mutate corpus VINs and yield only those that expand
    ultravin decode coverage (those then get oracle-checked downstream). Stops
    early once coverage saturates."""
    rng = _random.Random(seed)
    corpus = _seed_corpus(n_seeds)
    if not corpus:
        return
    seen: set[tuple] = set()
    vds = [3, 4, 5, 6, 7]
    for v in corpus[:]:
        new = ultravin_coverage(v) - seen
        if new:
            seen |= new
            yield {"vin": v, "mode": "covfuzz", "note": "seed"}
    tried = since_new = 0
    saturate = max(50_000, budget // 10)
    while tried < budget:
        tried += 1
        vin = list(rng.choice(corpus))
        for _ in range(rng.choice([1, 1, 2])):
            vin[rng.choice([*vds, 9])] = rng.choice(VIN_ALPHABET)
        if rng.random() < 0.2:  # explore error paths
            vin[rng.randrange(17)] = rng.choice(ALL_CHARS)
        elif rng.random() < 0.5:  # keep some VINs clean (valid check digit)
            vin[8] = "0"
            vin[8] = generator.check_digit(vin)
        cand = "".join(vin)
        try:
            new = ultravin_coverage(cand) - seen
        except Exception:  # noqa: BLE001 — a bad candidate must not kill the loop
            continue
        if new:
            seen |= new
            corpus.append(cand)
            since_new = 0
            yield {"vin": cand, "mode": "covfuzz", "note": f"cov+{len(new)}"}
        else:
            since_new += 1
            if since_new > saturate:
                break  # coverage saturated


# --------------------------------------------------------------------------- #
# Worker: decode with both engines and diff
# --------------------------------------------------------------------------- #
_conn: Any = None


def _init() -> None:
    global _conn
    _conn = oracle.connect()


def _check(case: dict[str, Any]) -> dict[str, Any] | None:
    vin = case["vin"]
    try:
        o_rows = [normalize.from_oracle(r) for r in oracle.decode(_conn, vin)]
        mine = normalize.ultravin_rows(ultravin.decode(vin))
        d = normalize.diff_rows(o_rows, mine)
    except Exception as e:  # noqa: BLE001 — a failing VIN must not kill the campaign
        return {"vin": vin, "mode": case["mode"], "note": case.get("note"), "error": repr(e)[:300]}
    if d["ok"]:
        return None
    return {
        "vin": vin,
        "mode": case["mode"],
        "note": case.get("note"),
        "fingerprint": normalize.fingerprint(d),
        "field_diffs": d["field_diffs"][:30],
        "missing": [r["element_id"] for r in d["missing"]],
        "extra": [r["element_id"] for r in d["extra"]],
    }


def _stream(args: argparse.Namespace) -> Iterator[dict[str, Any]]:
    modes = args.modes.split(",") if args.modes != "all" else ["random", "systematic", "fuzz", "covfuzz"]
    if "random" in modes:
        yield from gen_random(args.random, args.seed)
    if "systematic" in modes:
        yield from gen_systematic(args.shard_i, args.shard_n, args.years)
    if "fuzz" in modes:
        yield from gen_fuzz(args.shard_i, args.shard_n, args.fuzz_per, args.seed)
    if "covfuzz" in modes:
        yield from gen_covfuzz(args.covfuzz_seeds, args.covfuzz_budget, args.seed)


def cmd_run(args: argparse.Namespace) -> int:
    cases = _stream(args)
    if args.limit:
        cases = itertools.islice(cases, args.limit)
    tested = fails = 0
    t0 = time.time()
    deadline = t0 + args.max_seconds if args.max_seconds else None
    with open(args.fail_log, "a") as flog, Pool(args.workers, initializer=_init) as pool:
        for res in pool.imap_unordered(_check, cases, chunksize=8):
            tested += 1
            if res is not None:
                fails += 1
                flog.write(json.dumps(res) + "\n")
                flog.flush()
            if tested % args.progress_every == 0:
                rate = tested / max(time.time() - t0, 1e-9)
                print(f"[brutal] tested={tested} failures={fails} rate={rate:.0f}/s", file=sys.stderr, flush=True)
            if deadline and time.time() > deadline:
                print(f"[brutal] max-seconds reached at tested={tested}", file=sys.stderr, flush=True)
                pool.terminate()
                break
    print(f"[brutal] DONE tested={tested} failures={fails} elapsed={time.time() - t0:.0f}s", file=sys.stderr)
    return 0


# --------------------------------------------------------------------------- #
# Dedupe failures by bug signature -> minimal repro set
# --------------------------------------------------------------------------- #
def _signature(rec: dict[str, Any]) -> str:
    if "error" in rec:
        return "error:" + rec["error"].split("(")[0]
    fields = sorted({(fd["element_id"], fd["field"]) for fd in rec.get("field_diffs", [])})
    return json.dumps(
        {"f": fields, "m": sorted(set(rec.get("missing", []))), "x": sorted(set(rec.get("extra", [])))},
        sort_keys=True,
    )


def cmd_dedupe(args: argparse.Namespace) -> int:
    best: dict[str, dict[str, Any]] = {}
    counts: dict[str, int] = {}
    total = 0
    with open(args.fail_log) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            rec = json.loads(line)
            total += 1
            sig = _signature(rec)
            counts[sig] = counts.get(sig, 0) + 1
            cur = best.get(sig)
            if cur is None or len(rec["vin"]) < len(cur["vin"]):
                best[sig] = rec
    reduced = sorted(best.values(), key=lambda r: (len(r["vin"]), r["vin"]))
    for r in reduced:
        r["hit_count"] = counts[_signature(r)]
    out = {"total_failures": total, "distinct_signatures": len(best), "vins": reduced}
    with open(args.out, "w") as fh:
        json.dump(out, fh, indent=2)
    print(f"[dedupe] {total} failures -> {len(best)} distinct bug signatures -> {args.out}", file=sys.stderr)
    for sig, c in sorted(counts.items(), key=lambda kv: -kv[1])[:25]:
        print(f"  x{c:<7} {sig[:160]}", file=sys.stderr)
    return 0


def cmd_check(args: argparse.Namespace) -> int:
    """Decode a fixed VIN list vs the oracle; nonzero exit if any still diverge.
    The success fence for the fix pass (repro set 35 -> 0)."""
    with open(args.vins) as f:
        data = json.load(f)
    vins = [v["vin"] for v in data["vins"]] if isinstance(data, dict) else list(data)
    fails: list[tuple[str, str]] = []
    with oracle.connect() as conn:
        for vin in vins:
            try:
                o = [normalize.from_oracle(r) for r in oracle.decode(conn, vin)]
                m = normalize.ultravin_rows(ultravin.decode(vin))
                d = normalize.diff_rows(o, m)
            except Exception as e:  # noqa: BLE001
                fails.append((vin, f"error:{e!r}"[:90]))
                continue
            if not d["ok"]:
                fields = sorted({(fd["element_id"], fd["field"]) for fd in d["field_diffs"]})
                miss = sorted({r["element_id"] for r in d["missing"]})
                extra = sorted({r["element_id"] for r in d["extra"]})
                fails.append((vin, f"fields={fields} missing={miss} extra={extra}"))
    print(f"[check] {len(vins)} VINs, {len(fails)} still failing", file=sys.stderr)
    for vin, info in fails[:60]:
        print(f"  FAIL {vin}: {info}", file=sys.stderr)
    return 1 if fails else 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="Brutal VIN parity campaign vs the oracle.")
    sub = ap.add_subparsers(dest="cmd", required=True)

    c = sub.add_parser("check", help="decode a VIN list vs oracle; nonzero exit if any diverge")
    c.add_argument("--vins", required=True, help="JSON: {vins:[{vin}]} or a plain list of VINs")

    r = sub.add_parser("run", help="generate + diff vs oracle + log failures")
    r.add_argument("--modes", default="all", help="comma list of random,systematic,fuzz,covfuzz (or 'all')")
    r.add_argument("--random", type=int, default=100_000, help="number of random VINs")
    r.add_argument(
        "--covfuzz-budget", type=int, default=500_000, dest="covfuzz_budget", help="covfuzz candidate VINs to try"
    )
    r.add_argument(
        "--covfuzz-seeds", type=int, default=800, dest="covfuzz_seeds", help="covfuzz seed VINs (1 per sampled WMI)"
    )
    r.add_argument("--fuzz-per", type=int, default=3, dest="fuzz_per", help="mutations per systematic base")
    r.add_argument("--years", type=int, default=3, help="model years sampled per schema (systematic)")
    r.add_argument("--shard", default="0/1", help="i/n WMI shard for systematic/fuzz")
    r.add_argument("--workers", type=int, default=8)
    r.add_argument("--limit", type=int, default=0, help="cap total VINs (0 = no cap)")
    r.add_argument("--max-seconds", type=int, default=0, dest="max_seconds", help="time budget (0 = none)")
    r.add_argument("--seed", type=int, default=1)
    r.add_argument("--fail-log", default="brutal_fails.jsonl", dest="fail_log")
    r.add_argument("--progress-every", type=int, default=2000, dest="progress_every")

    d = sub.add_parser("dedupe", help="cluster failures by bug signature -> minimal set")
    d.add_argument("--fail-log", required=True, dest="fail_log")
    d.add_argument("--out", default="brutal_reduced.json")

    args = ap.parse_args(argv)
    if args.cmd == "run":
        args.shard_i, args.shard_n = (int(x) for x in args.shard.split("/"))
        return cmd_run(args)
    if args.cmd == "dedupe":
        return cmd_dedupe(args)
    if args.cmd == "check":
        return cmd_check(args)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
