"""Gate the decode path at 100% of the regions a VIN can actually reach.

`cargo llvm-cov` reports ~95% for the decode modules and always will: some of
that code is unreachable by construction (the artifact builder's own helpers, the
alternate storage backend) and some is unreachable with the data NHTSA currently
ships (arithmetic arms six positive formulas cannot take, a regex fallback for
character classes no key contains). A permanent 5% gap hides regressions inside
it, so the gap is written down instead — every uncovered region carries a reason
in `coverage_allowances.json`, and this check fails on anything else.

It fails in both directions:

- an uncovered region with no allowance is a **new gap** — a corpus that used to
  reach that code no longer does;
- an allowance whose regions are now all covered is **stale** — usually because a
  monthly data refresh made the branch reachable, which is worth knowing.

    make coverage
"""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path
from typing import Any

import typer
import ultravin

REPO = Path(__file__).resolve().parents[1]
ALLOWANCES = Path(__file__).parent / "coverage_allowances.json"
# The decode path proper. The generator, the builder and the loader live in the
# same crate but no VIN exercises them, so they are not part of this gate.
DECODE_FILES = {
    "decode.rs",
    "errors.rs",
    "matcher.rs",
    "year.rs",
    "checkdigit.rs",
    "wmi.rs",
    "conversion.rs",
    "resolve.rs",
}

app = typer.Typer(add_completion=False, help="Decode-path coverage gate.")


def demangle(name: str) -> str:
    """The bare function name out of a Rust symbol, e.g. `decode::decode_core`."""
    tail = name.rsplit("13ultravin_core", 1)[-1]
    out, i = [], 0
    while i < len(tail):
        digits = ""
        while i < len(tail) and tail[i].isdigit():
            digits += tail[i]
            i += 1
        if not digits:
            i += 1
            continue
        n = int(digits)
        out.append(tail[i : i + n])
        i += n
    return "::".join(out) if out else tail


def measure(vins: Path, json_out: Path) -> dict[str, Any]:
    """Run the corpus through `covrun` under llvm-cov and return the report."""
    proc = subprocess.run(
        [
            "cargo",
            "llvm-cov",
            "run",
            "--example",
            "covrun",
            "--json",
            "--output-path",
            str(json_out),
            "--",
            str(vins),
        ],
        cwd=REPO,
        capture_output=True,
        text=True,
        # Its own target dir: an ordinary `cargo build` running beside this would
        # otherwise wipe the instrumented objects mid-run.
        env={**os.environ, "CARGO_TARGET_DIR": str(REPO / "target" / "corpus-cov")},
        check=False,
    )
    if proc.returncode != 0:
        msg = f"cargo llvm-cov failed ({proc.returncode}):\n{proc.stderr[-2000:]}"
        raise RuntimeError(msg)
    return json.loads(json_out.read_text())


def uncovered_by_function(report: dict[str, Any]) -> dict[tuple[str, str], int]:
    """Uncovered region count per (file, function) across the decode path."""
    out: dict[tuple[str, str], int] = {}
    for fn in report["data"][0]["functions"]:
        file = fn["filenames"][0].split("/")[-1]
        if file not in DECODE_FILES:
            continue
        missed = sum(1 for r in fn["regions"] if r[4] == 0)
        if missed:
            key = (file, demangle(fn["name"]))
            out[key] = out.get(key, 0) + missed
    return out


def totals(report: dict[str, Any]) -> tuple[int, int]:
    covered = total = 0
    for fn in report["data"][0]["functions"]:
        if fn["filenames"][0].split("/")[-1] not in DECODE_FILES:
            continue
        for r in fn["regions"]:
            total += 1
            covered += 1 if r[4] > 0 else 0
    return covered, total


def load_allowances() -> dict[tuple[str, str], dict[str, Any]]:
    data = json.loads(ALLOWANCES.read_text())
    return {(a["file"], a["function"]): a for a in data["allow"]}


@app.command()
def check(
    vins: str = typer.Option("", help="VIN list to measure (default: the artifact's own cover)"),
    update: bool = typer.Option(False, "--update", help="rewrite the region counts, keeping reasons"),
) -> None:
    """Measure the decode path and compare it to the written-down allowances."""
    work = REPO / "target" / "corpus-cov"
    work.mkdir(parents=True, exist_ok=True)
    vin_file = Path(vins) if vins else work / "cover.txt"
    if not vins:
        vin_file.write_text("\n".join(ultravin.cover_vins()) + "\n")

    report = measure(vin_file, work / "coverage.json")
    covered, total = totals(report)
    found = uncovered_by_function(report)
    allowed = load_allowances()

    new_gaps = {k: v for k, v in found.items() if k not in allowed}
    grown = {k: (v, allowed[k]["regions"]) for k, v in found.items() if k in allowed and v > allowed[k]["regions"]}
    stale = {k: a["regions"] for k, a in allowed.items() if k not in found}
    shrunk = {k: (found[k], allowed[k]["regions"]) for k in found if k in allowed and found[k] < allowed[k]["regions"]}

    reachable = total - sum(a["regions"] for a in allowed.values())
    typer.echo(f"decode path: {covered}/{total} regions ({100 * covered / total:.2f}%)")
    typer.echo(f"reachable:   {covered}/{reachable} ({100 * covered / reachable:.2f}%) after {len(allowed)} allowances")

    if update:
        _rewrite(allowed, found)
        typer.echo(f"updated {ALLOWANCES.name}")
        return

    ok = True
    for (file, fn), n in sorted(new_gaps.items()):
        typer.echo(f"  NEW GAP      {file}::{fn} — {n} uncovered regions, no allowance", err=True)
        ok = False
    for (file, fn), (now, was) in sorted(grown.items()):
        typer.echo(f"  WIDENED      {file}::{fn} — {now} uncovered, allowed {was}", err=True)
        ok = False
    for (file, fn), was in sorted(stale.items()):
        typer.echo(f"  STALE        {file}::{fn} — now fully covered, allowed {was}; drop the allowance", err=True)
        ok = False
    for (file, fn), (now, was) in sorted(shrunk.items()):
        typer.echo(f"  NARROWED     {file}::{fn} — {now} uncovered, allowed {was}; lower it", err=True)
        ok = False

    if ok:
        typer.echo("every uncovered region is accounted for")
    raise typer.Exit(0 if ok else 1)


def _rewrite(allowed: dict[tuple[str, str], dict[str, Any]], found: dict[tuple[str, str], int]) -> None:
    """Refresh counts in place, keeping every reason and dropping what is covered."""
    about = json.loads(ALLOWANCES.read_text())["_about"]
    entries = [
        {
            "file": key[0],
            "function": key[1],
            "reason": allowed.get(key, {}).get("reason", "TODO: explain why no VIN can reach this"),
            "regions": found[key],
        }
        for key in sorted(found)
    ]
    lines = ["{", f'  "_about": {json.dumps(about)},', '  "allow": [']
    for i, e in enumerate(entries):
        lines.append("    " + json.dumps(e, sort_keys=True) + ("," if i < len(entries) - 1 else ""))
    lines += ["  ]", "}"]
    ALLOWANCES.write_text("\n".join(lines) + "\n")


if __name__ == "__main__":
    app()
