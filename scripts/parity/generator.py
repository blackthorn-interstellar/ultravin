"""Synthesize valid VINs that exercise WMI -> VinSchema -> Pattern coverage.

For each WMI, its year-applicable VinSchemas, and a sample of each schema's
Patterns, build a 17-char VIN whose positions 4-8 and 10-17 satisfy the pattern
Keys (wildcards/bracket-classes filled with a valid member), with a model-year
char at position 10 inside the schema's year range and a CORRECT check digit at
position 9. Also emits targeted error-case VINs (short, bad char, unknown WMI,
no-pattern). The oracle is authoritative; the generator only aims the corpus.
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import asdict, dataclass
from typing import Any

from scripts.parity import oracle

# Model-year char for years 2010..2039 (I/O/Q excluded; 30-year cycle).
_MY_CHARS = "ABCDEFGHJKLMNPRSTVWXY123456789"
# Default fill: VDS/plant get a benign letter; the serial gets digits so the
# computed check digit lands on an otherwise valid VIN.
_FILL_VDS = "A"
_FILL_SERIAL = "1"

# Check-digit transliteration (I/O/Q excluded) and position weights.
_TRANSLIT = {c: (ord(c) - ord("0")) for c in "0123456789"}
for _grp, _val in [
    ("AJ", 1),
    ("BKS", 2),
    ("CLT", 3),
    ("DMU", 4),
    ("ENV", 5),
    ("FW", 6),
    ("GPX", 7),
    ("HY", 8),
    ("RZ", 9),
]:
    for _ch in _grp:
        _TRANSLIT[_ch] = _val
_WEIGHTS = [8, 7, 6, 5, 4, 3, 2, 10, 0, 9, 8, 7, 6, 5, 4, 3, 2]


@dataclass
class VinCase:
    vin: str
    kind: str  # "pattern" | "error"
    wmi: str | None = None
    vin_schema_id: int | None = None
    pattern_id: int | None = None
    note: str | None = None


def year_char(year: int) -> str:
    return _MY_CHARS[(year - 2010) % 30]


def choose_year(year_from: int, year_to: int | None, current_year: int) -> int:
    """Pick a representative model year inside the schema's range (prefer recent, >=2010)."""
    cap = current_year + 2
    hi = min(year_to if year_to is not None else 9999, cap, 2039)
    lo = max(year_from, 2010)
    if lo <= hi:
        return hi
    # Range is entirely below 2010 (or above cap): fall back to its edge.
    if year_to is not None and year_to < 2010:
        return year_to
    return year_from


def _parse_keys(spec: str) -> list[str | None]:
    """A keys segment -> per-position concrete chars (None = wildcard/unconstrained)."""
    out: list[str | None] = []
    i = 0
    while i < len(spec):
        c = spec[i]
        if c in "*_":
            out.append(None)
            i += 1
        elif c == "[":
            j = spec.index("]", i)
            members = spec[i + 1 : j].replace("-", "")
            out.append(members[0] if members else None)
            i = j + 1
        else:
            out.append(c)
            i += 1
    return out


def check_digit(vin: list[str]) -> str:
    total = 0
    for idx, ch in enumerate(vin):
        v = _TRANSLIT.get(ch)
        if v is None:
            return "?"
        total += v * _WEIGHTS[idx]
    r = total % 11
    return "X" if r == 10 else str(r)


def build_vin(wmi: str, keys: str, year: int) -> str:
    """Materialize a 17-char VIN satisfying `keys` for `wmi` at model year `year`."""
    vin = [_FILL_VDS] * 17
    for k in range(11, 17):  # serial positions 12-17 (0-based 11..16)
        vin[k] = _FILL_SERIAL
    # WMI positions 1-3 (and 12-14 for 6-char low-volume WMIs).
    for k, ch in enumerate(wmi[:3]):
        vin[k] = ch
    if len(wmi) == 6:
        for k, ch in enumerate(wmi[3:6]):
            vin[11 + k] = ch
    vin[9] = year_char(year)  # position 10
    parts = keys.split("|")
    for k, ch in enumerate(_parse_keys(parts[0])):  # positions 4-8
        if ch is not None and k < 5:
            vin[3 + k] = ch
    if len(parts) > 1:
        for k, ch in enumerate(_parse_keys(parts[1])):  # positions 10-17
            if ch is not None and 9 + k < 17:
                vin[9 + k] = ch
    vin[8] = "0"  # placeholder; check digit computed next
    vin[8] = check_digit(vin)
    return "".join(vin)


def _fetch_schema_patterns(conn: Any, shard: int, shards: int, sample: int) -> list[VinCase]:
    cur_year = oracle.current_year(conn)
    cases: list[VinCase] = []
    with conn.cursor() as cur:
        cur.execute("select id, wmi from vpic.wmi order by id")
        wmis = cur.fetchall()
        for idx, w in enumerate(wmis):
            if idx % shards != shard:
                continue
            cur.execute(
                "select vinschemaid, yearfrom, yearto from vpic.wmi_vinschema where wmiid=%s",
                (w["id"],),
            )
            for link in cur.fetchall():
                year = choose_year(link["yearfrom"], link["yearto"], cur_year)
                cur.execute(
                    "select distinct keys from vpic.pattern where vinschemaid=%s order by keys limit %s",
                    (link["vinschemaid"], sample),
                )
                seen: set[str] = set()
                for prow in cur.fetchall():
                    vin = build_vin(w["wmi"], prow["keys"], year)
                    if vin in seen:
                        continue
                    seen.add(vin)
                    cases.append(
                        VinCase(
                            vin=vin,
                            kind="pattern",
                            wmi=w["wmi"],
                            vin_schema_id=link["vinschemaid"],
                            note=prow["keys"],
                        )
                    )
    return cases


def error_cases(conn: Any) -> list[VinCase]:
    """A small fixed set of error-path VINs (short, bad char, unknown WMI, no pattern)."""
    cases: list[VinCase] = []
    with conn.cursor() as cur:
        cur.execute("select wmi from vpic.wmi where length(wmi)=3 order by id limit 1")
        row = cur.fetchone()
    real = row["wmi"] if row else "1HG"
    base = build_vin(real, "*****", 2020)
    cases.append(VinCase(vin=base[:11], kind="error", note="short-vin"))
    cases.append(VinCase(vin=base[:8] + "I" + base[9:], kind="error", note="bad-char-I"))
    cases.append(VinCase(vin="ZZZ" + base[3:], kind="error", note="unknown-wmi"))
    cases.append(VinCase(vin=real + "00000000000000", kind="error", note="no-pattern"))
    return cases


def generate(shard: int, shards: int, sample: int, with_errors: bool) -> list[VinCase]:
    with oracle.connect() as conn:
        cases = _fetch_schema_patterns(conn, shard, shards, sample)
        if with_errors and shard == 0:
            cases.extend(error_cases(conn))
    return cases


def _parse_shard(text: str) -> tuple[int, int]:
    i, n = text.split("/")
    return int(i), int(n)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="Generate parity-coverage VINs (JSONL on stdout).")
    ap.add_argument("--shard", default="0/1", help="i/n: only WMIs where index %% n == i")
    ap.add_argument("--sample", type=int, default=2, help="patterns (distinct keys) per schema")
    ap.add_argument("--limit", type=int, default=0, help="cap total VINs (0 = no cap)")
    ap.add_argument("--no-errors", action="store_true", help="skip the error-case VINs")
    ap.add_argument("--out", default="-", help="output file (default stdout)")
    args = ap.parse_args(argv)

    shard, shards = _parse_shard(args.shard)
    cases = generate(shard, shards, args.sample, not args.no_errors)
    if args.limit:
        cases = cases[: args.limit]

    sink = sys.stdout if args.out == "-" else open(args.out, "w")  # noqa: SIM115
    try:
        for c in cases:
            sink.write(json.dumps(asdict(c)) + "\n")
    finally:
        if sink is not sys.stdout:
            sink.close()
    print(f"generated {len(cases)} VINs (shard {shard}/{shards})", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
