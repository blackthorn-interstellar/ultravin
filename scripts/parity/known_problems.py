"""Re-probe every documented oracle problem against the live oracle.

`ORACLE_CRASH_VINS` and `KNOWN_DEVIATION_VINS` (scripts/refresh.py, derived
from the scripts/known_problems.json registry) are *excuses*: the corpus and
sweep gates use them to forgive an observation. Nothing made an excuse prove
itself still true, and the 65 crash VINs are in neither the re-frozen corpus
nor the 500-VIN sweep, so most months they are never decoded at all — the
2026_08 dump healed W1LSB0L72VEJV2EPX and the refresh stayed green.

This decodes every listed VIN against the new oracle and writes one record per
VIN, `{vin: {"outcome": "crash"|"infra-error"|"diverged"|"exact", ...}}`.
Like freeze.py and sweep.py it exits 0 whatever it sees;
`refresh.known_problems_gate` is what turns the report into a verdict.
"""

from __future__ import annotations

import argparse
import json
from collections import Counter
from pathlib import Path
from typing import Any

import ultravin as uv

from scripts.parity import campaign, normalize, oracle
from scripts.refresh import KNOWN_DEVIATION_VINS, ORACLE_CRASH_VINS

OUT = Path(__file__).resolve().parents[2] / "target" / "refresh" / "known_problems.json"


def probe(conn: Any, vin: str) -> dict[str, Any]:
    """Decode one VIN both ways and classify what the oracle did with it."""
    # Lazy like scripts.parity.oracle: importing this module must not need psycopg.
    import psycopg  # noqa: PLC0415  (lazy: keep optional deps optional)

    try:
        oracle_rows = [normalize.from_oracle(r) for r in oracle.decode(conn, vin)]
    except psycopg.Error as e:
        record = {"error": repr(e)[:200]}
        if not conn.autocommit:  # a failed decode poisons the transaction
            conn.rollback()
            conn.uv_pending = 0
        # A dead socket says nothing about the VIN. Reporting it as "crash" would
        # let an oracle outage silently re-certify every documented problem, so it
        # gets its own outcome and the gate treats it as unverifiable.
        return {"outcome": "infra-error" if campaign.is_infra_error(record) else "crash", **record}
    d = normalize.diff_rows(oracle_rows, normalize.ultravin_rows(uv.decode(vin)))
    if d["ok"]:
        return {"outcome": "exact"}
    return {"outcome": "diverged", "fingerprint": normalize.fingerprint(d)}


def run(conn: Any, vins: list[str]) -> dict[str, dict[str, Any]]:
    return {vin: probe(conn, vin) for vin in vins}


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="Re-probe every documented oracle problem.")
    ap.add_argument("--out", default=str(OUT), help="report JSON output path")
    args = ap.parse_args(argv)
    with oracle.connect() as conn:
        report = run(conn, sorted(ORACLE_CRASH_VINS | KNOWN_DEVIATION_VINS))
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2, sort_keys=True, default=str) + "\n")
    tally = dict(Counter(r["outcome"] for r in report.values()).most_common())
    print(f"probed {len(report)} documented problem VIN(s): {tally} -> {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
