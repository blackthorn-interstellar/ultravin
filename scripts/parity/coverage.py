"""Audit what a VIN list actually covers, against the oracle's own row counts.

Generation itself now lives in the library — `ultravin.generate`,
`ultravin.sweep` and `ultravin.cover_vins` build VINs from the embedded artifact
with no database at all, so anyone testing a VIN decoder can use them. What is
left here is the part that genuinely needs Postgres: checking a corpus against
the totals in the vPIC tables, so "this hits every make and model" is a measured
claim rather than a construction argument.

    make oracle-up && make oracle-load DUMP=downloads/vPICList_lite_2026_07.plain.zip
    uv run -- python -m scripts.parity.coverage report vins.txt

Coverage of the *code* (as opposed to the data) is a different instrument:

    cargo llvm-cov run --example covrun --summary-only -- vins.txt
"""

from __future__ import annotations

import json
import sys
from typing import Any

import typer
import ultravin

from scripts.parity import oracle

app = typer.Typer(add_completion=False, help="Audit VIN corpus coverage against the oracle.")

# Hits are counted in the same unit as the totals — row ids, not display names —
# so the percentages mean something.
_TOTALS_SQL = {
    "wmis": "select count(*) n from vpic.wmi",
    "makes": "select count(distinct makeid) n from vpic.wmi_make",
    "models": "select count(distinct attributeid) n from vpic.pattern where elementid = 28",
    "series": "select count(distinct attributeid) n from vpic.pattern where elementid = 34",
    "schemas": "select count(*) n from vpic.vinschema",
    "patterns": "select count(*) n from vpic.pattern",
    "engine_models": "select count(distinct lower(trim(name))) n from vpic.enginemodel",
    "vspec_schemas": "select count(*) n from vpic.vehiclespecschema",
    "elements": "select count(*) n from vpic.element",
    "error_codes": "select count(*) n from vpic.errorcode",
    "conversions": "select count(*) n from vpic.conversion",
}
# Elements whose attribute id IS the row id of the dimension being counted.
_ID_ELEMENTS = {26: "makes", 28: "models", 34: "series"}


def coverage_report(vins: list[str], conn: Any, batch: int = 20_000) -> dict[str, Any]:
    """Decode a VIN list and report what it actually touched, per dimension.

    Every number is measured, not assumed: a pattern counts only if it won an
    element, a model only if it was emitted.
    """
    with conn.cursor() as cur:
        cur.execute("select wmi from vpic.wmi")
        known_wmis = {r["wmi"] for r in cur.fetchall()}
        cur.execute("select lower(trim(name)) name from vpic.enginemodel")
        known_engines = {r["name"] for r in cur.fetchall()}

    seen: dict[str, set[Any]] = {k: set() for k in _TOTALS_SQL}
    for i in range(0, len(vins), batch):
        results: Any = ultravin.decode_batch(vins[i : i + batch], full=True)
        for r in results:
            if r["wmi"] in known_wmis:  # a made-up WMI is not coverage
                seen["wmis"].add(r["wmi"])
            for e in r["elements"]:
                src = e.get("source") or ""
                attr = e.get("attribute_id") or ""
                seen["elements"].add(e["element_id"])
                if e.get("pattern_id"):
                    seen["patterns"].add(e["pattern_id"])
                if e.get("vin_schema_id"):
                    # Vehicle-spec rows carry a VehicleSpecSchema id here, which is
                    # a different namespace from VinSchema — do not mix them.
                    seen["vspec_schemas" if src == "Vehicle Specs" else "schemas"].add(e["vin_schema_id"])
                if (dim := _ID_ELEMENTS.get(e["element_id"])) and attr:
                    seen[dim].add(attr)
                if e["element_id"] == 18 and (name := attr.strip().lower()) in known_engines:
                    seen["engine_models"].add(name)  # only names that reach an EngineModel row
                if src.startswith("Conversion "):
                    seen["conversions"].add(src.split(":", 1)[0])
            seen["error_codes"].update(r["error_codes"] or [])

    with conn.cursor() as cur:
        totals = {}
        for dim, sql in _TOTALS_SQL.items():
            cur.execute(sql)
            totals[dim] = cur.fetchone()["n"]
    return {
        "vins": len(vins),
        "dimensions": {
            dim: {
                "hit": len(seen[dim]),
                "total": totals[dim],
                "pct": round(100 * len(seen[dim]) / totals[dim], 1) if totals[dim] else 0.0,
            }
            for dim in _TOTALS_SQL
        },
    }


def format_report(report: dict[str, Any]) -> str:
    """The report as a table a non-engineer can read."""
    lines = [f"{report['vins']} VINs", "", f"{'dimension':16}{'hit':>10}{'total':>10}{'':>3}coverage"]
    for name, d in report["dimensions"].items():
        lines.append(f"{name:16}{d['hit']:>10,}{d['total']:>10,}{'':>3}{d['pct']:>5}%")
    return "\n".join(lines)


def read_vins(path: str) -> list[str]:
    """VINs from a plain list, or from JSONL rows carrying a `vin` key."""
    with open(path) as f:
        lines = [ln.strip() for ln in f if ln.strip()]
    return [json.loads(ln)["vin"] if ln.startswith("{") else ln for ln in lines]


@app.command("report")
def cmd_report(
    vins: str = typer.Argument(..., help="file of VINs (one per line, or JSONL with a `vin` key)"),
    as_json: bool = typer.Option(False, "--json", help="emit the raw report JSON"),
) -> None:
    """Decode a VIN list and show what it covers, dimension by dimension."""
    parsed = read_vins(vins)
    with oracle.connect() as conn:
        report = coverage_report(parsed, conn)
    typer.echo(json.dumps(report, indent=2) if as_json else format_report(report))


@app.command("emit")
def cmd_emit(
    kind: str = typer.Argument("cover", help="cover | sweep | random"),
    out: str = typer.Option("-", help="output file (default stdout)"),
    n: int = typer.Option(100, help="how many, for `random`"),
    seed: int = typer.Option(0, help="seed, for `random`"),
    dimensions: str = typer.Option("", help="comma list for `sweep` (default all)"),
) -> None:
    """Write a corpus from the library. No oracle needed for this one."""
    if kind == "cover":
        vins = ultravin.cover_vins()
    elif kind == "sweep":
        vins = ultravin.sweep([d.strip() for d in dimensions.split(",")] if dimensions else None)
    elif kind == "random":
        vins = ultravin.generate(n, seed=seed)
    else:
        msg = f"unknown kind: {kind}"
        raise typer.BadParameter(msg)

    sink = sys.stdout if out == "-" else open(out, "w")  # noqa: SIM115
    try:
        sink.write("\n".join(vins) + "\n")
    finally:
        if sink is not sys.stdout:
            sink.close()
    typer.echo(f"{len(vins)} VINs", err=True)


if __name__ == "__main__":
    app()
