"""Differential runner: decode each VIN with ultravin AND the oracle, diff, aggregate.

Reads VIN cases (JSONL from generator.py, or generates a fresh sample), decodes
each via the installed `ultravin` module and `vpic.spvindecode`, diffs all 15
columns per element plus the error/suggested-VIN/error-text elements, and writes
a structured JSON report categorizing divergences by element id and by feature.

This needs the live oracle. The self-contained regression test (no oracle) is
tests/test_parity_corpus.py, frozen by freeze.py.
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import Counter
from typing import Any

import ultravin as uv

from scripts.parity import generator, normalize, oracle


def _load_cases(path: str) -> list[dict[str, Any]]:
    lines = sys.stdin if path == "-" else open(path)  # noqa: SIM115
    try:
        return [json.loads(ln) for ln in lines if ln.strip()]
    finally:
        if lines is not sys.stdin:
            lines.close()


def run(cases: list[dict[str, Any]], examples: int) -> dict[str, Any]:
    elem_counter: Counter[int] = Counter()
    feature_counter: Counter[str] = Counter()
    field_counter: Counter[str] = Counter()
    order_mismatches = 0
    n_ok = 0
    sample_diffs: list[dict[str, Any]] = []

    with oracle.connect() as conn:
        for case in cases:
            vin = case["vin"]
            oracle_rows = [normalize.from_oracle(r) for r in oracle.decode(conn, vin)]
            mine = normalize.ultravin_rows(uv.decode(vin))
            d = normalize.diff_rows(oracle_rows, mine)
            if d["ok"]:
                n_ok += 1
                continue
            if not d["order_ok"]:
                order_mismatches += 1
            for fd in d["field_diffs"]:
                elem_counter[fd["element_id"]] += 1
                feature_counter[fd["feature"]] += 1
                field_counter[fd["field"]] += 1
            for row in d["missing"]:
                elem_counter[row["element_id"]] += 1
                feature_counter["missing"] += 1
            for row in d["extra"]:
                elem_counter[row["element_id"]] += 1
                feature_counter["extra"] += 1
            if len(sample_diffs) < examples:
                sample_diffs.append(
                    {
                        "vin": vin,
                        "note": case.get("note"),
                        "order_ok": d["order_ok"],
                        "field_diffs": d["field_diffs"][:20],
                        "missing": [{"element_id": r["element_id"], "variable": r["variable"]} for r in d["missing"]],
                        "extra": [{"element_id": r["element_id"], "variable": r["variable"]} for r in d["extra"]],
                    }
                )

    total = len(cases)
    return {
        "total": total,
        "exact_parity": n_ok,
        "diverged": total - n_ok,
        "order_mismatches": order_mismatches,
        "by_feature": dict(feature_counter.most_common()),
        "by_field": dict(field_counter.most_common()),
        "by_element": dict(sorted(elem_counter.items(), key=lambda kv: (-kv[1], kv[0]))),
        "examples": sample_diffs,
    }


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="Differential ultravin-vs-oracle parity sweep.")
    ap.add_argument("--cases", default="", help="JSONL of VIN cases (default: generate a sample)")
    ap.add_argument("--shard", default="0/1", help="when generating: i/n WMI shard")
    ap.add_argument("--sample", type=int, default=2, help="when generating: patterns per schema")
    ap.add_argument("--limit", type=int, default=0, help="cap total VINs (0 = no cap)")
    ap.add_argument("--examples", type=int, default=15, help="per-VIN diff examples to include")
    ap.add_argument("--out", default="-", help="report JSON output (default stdout)")
    args = ap.parse_args(argv)

    if args.cases:
        cases = _load_cases(args.cases)
    else:
        shard, shards = generator._parse_shard(args.shard)
        cases = [
            {"vin": c.vin, "kind": c.kind, "note": c.note}
            for c in generator.generate(shard, shards, args.sample, with_errors=True)
        ]
    if args.limit:
        cases = cases[: args.limit]

    report = run(cases, args.examples)
    text = json.dumps(report, indent=2, default=str)
    if args.out == "-":
        print(text)
    else:
        with open(args.out, "w") as f:
            f.write(text)
    print(
        f"parity: {report['exact_parity']}/{report['total']} exact; "
        f"diverged {report['diverged']}; features {report['by_feature']}",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
