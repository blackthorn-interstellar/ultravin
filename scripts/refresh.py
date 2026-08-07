"""Monthly NHTSA vPIC data refresh: detect a newer dump, integrate it, gate on parity.

Stdlib-only (runnable as `python3 scripts/refresh.py ...` with no environment):

  detect            probe NHTSA for a dump newer than the vpic/manifest.json pin
  run --month M     integrate month M end-to-end and enforce the parity gates

NHTSA publishes `vPICList_lite_YYYY_MM.plain.zip` mid-month (observed day 9-18)
where YYYY_MM is the month the release lands in, keeps ~8 months online, and
occasionally re-touches a published file — so `detect` probes every month from
the pin (exclusive) through the current month and also flags a re-issued pin.

`run` shells out to the same tools a human uses, in order: vpic-import
(rewrites vpic/ + the embedded artifact), uv sync + maturin develop, the docker
Postgres oracle (scripts/oracle.sh — WIPES any loaded oracle), re-freeze of
tests/parity_corpus.json, a 500-VIN live sweep, pytest, cargo test. Nothing is
hand-edited; the gates then decide:

  corpus    every diverging re-frozen VIN is a documented known deviation
  sweep     a fresh live sweep diverges only on known deviations, and the oracle
            crashed only on VINs documented as crashing it
  coverage  the decode path still reaches 100% of its reachable regions, and no
            allowance went stale (a stale one means this month's data reaches a
            branch the old data could not)
  pytest    the offline suite passes
  cargo     the Rust suite passes

freeze.py and sweep.py deliberately exit 0 no matter what they observe; the
gates here are what turn their reports into a verdict. Exit codes: 0 success or
nothing-to-do, 1 mechanical failure, 2 gate failure. A report is written to
target/refresh/report.{md,json} either way, and GitHub Actions outputs/summary
are appended when $GITHUB_OUTPUT/$GITHUB_STEP_SUMMARY are set.
"""

from __future__ import annotations

import argparse
import datetime as dt
import email.utils
import hashlib
import io
import json
import os
import re
import subprocess
import urllib.error
import urllib.request
import zipfile
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "vpic" / "manifest.json"
LOOKUPS = ROOT / "vpic" / "lookups.json"
CORPUS = ROOT / "tests" / "parity_corpus.json"
REPORT_DIR = ROOT / "target" / "refresh"
URL_TEMPLATE = "https://vpic.nhtsa.dot.gov/Downloads/vPICList_lite_{month}.plain.zip"
MONTH_RE = re.compile(r"^\d{4}_(0[1-9]|1[0-2])$")
SWEEP_LIMIT = 500
LOOKUP_MAX_ROWS = 512  # tables above this aren't lookups; the parity gates still validate them
LOOKUP_REPORT_CAP = 20

# Documented, deliberate ultravin-vs-oracle deviations (docs/KNOWN_DEVIATIONS.md).
# A diverging VIN outside this set fails the refresh, and so does a VIN the live
# sweep saw the oracle *crash* on (KNOWN_DEVIATIONS.md #1). The two kinds are
# listed together because both mean the same thing: the oracle cannot be used as
# the answer for this VIN, and a human already signed off on why.
#
# The crash VINs below are the 62 WMI-7T0 VINs the 2026_07 campaign hit. They are
# a *sample* of an unbounded class — any 7T0 VIN of model year 2023-2025 whose
# decode matches vinschema 24522 aborts the same way — so a future sweep may find
# a 7T0 VIN that is not listed here and fail the gate. That failure is correct:
# it should be re-verified against §1's evidence and then added, not assumed.
# freeze.py needs none of this: it skips oracle-erroring VINs before they ever
# reach the corpus, and surfaces new skips in the report as follow-ups.
ORACLE_CRASH_VINS = frozenset(
    {
        "7T0M6TGCURDSNZTHF",  # the original 2026_06 report (KNOWN_DEVIATIONS.md #1)
        "7T0A1AAA0SA111111",
        "7T0A1AAA1PA111111",
        "7T0A1AAA8RA111111",
        "7T0AA##A?PA111111",
        "7T0AA##A?RA111111",
        "7T0AA##A?SA111111",
        "7T0AAAA#?PA111111",
        "7T0AAAA#?RA111111",
        "7T0AAAA#?SA111111",
        "7T0AAAAA0PE111111",
        "7T0AAAAA0S1111111",
        "7T0AAAAA0SA111111",
        "7T0AAAAA0SJ111111",
        "7T0AAAAA1P1111111",
        "7T0AAAAA1PA111111",
        "7T0AAAAA1PJ111111",
        "7T0AAAAA1RG111111",
        "7T0AAAAA1SH111111",
        "7T0AAAAA2PH111111",
        "7T0AAAAA2RC111111",
        "7T0AAAAA2RT111111",
        "7T0AAAAA2SD111111",
        "7T0AAAAA3PD111111",
        "7T0AAAAA4RF111111",
        "7T0AAAAA4SG111111",
        "7T0AAAAA5PG111111",
        "7T0AAAAA5RB111111",
        "7T0AAAAA5SC111111",
        "7T0AAAAA5ST111111",
        "7T0AAAAA6PC111111",
        "7T0AAAAA6PT111111",
        "7T0AAAAA7RE111111",
        "7T0AAAAA7SF111111",
        "7T0AAAAA8PF111111",
        "7T0AAAAA8R1111111",
        "7T0AAAAA8RA111111",
        "7T0AAAAA8RJ111111",
        "7T0AAAAA8SB111111",
        "7T0AAAAA9PB111111",
        "7T0AAAAA9RH111111",
        "7T0AAAAAXRD111111",
        "7T0AAAAAXSE111111",
        "7T0AAF#0?SA111111",
        "7T0AAL#A?SA111111",
        "7T0AGAAA2SA111111",
        "7T0AGAAA3PA111111",
        "7T0AGAAAXRA111111",
        "7T0AH##A?SA111111",
        "7T0ARAAA0PA111111",
        "7T0ARAAA7RA111111",
        "7T0ARAAAXSA111111",
        "7T0AZAAA0PA111111",
        "7T0AZAAA7RA111111",
        "7T0AZAAAXSA111111",
        "7T0FAAAA0RA111111",
        "7T0FAAAA3SA111111",
        "7T0FAAAA4PA111111",
        "7T0TA##1?SA111111",
        "7T0TA#71?SA111111",
        "7T0TAAAA0PA111111",
        "7T0TAAAA7RA111111",
        "7T0TAAAAXSA111111",
    }
)
KNOWN_DEVIATION_VINS = frozenset({"W1LSB0L72VEJV2EPX"}) | ORACLE_CRASH_VINS


# --------------------------------------------------------------------------- util


def log(msg: str) -> None:
    print(f"[refresh] {msg}", flush=True)


def sh(cmd: list[str], *, check: bool = True, capture: bool = False) -> subprocess.CompletedProcess[str]:
    log("$ " + " ".join(cmd))
    return subprocess.run(cmd, cwd=ROOT, check=check, capture_output=capture, text=True)


def gh_output(pairs: dict[str, str]) -> None:
    """Expose key=value pairs to GitHub Actions (and humans)."""
    for k, v in pairs.items():
        log(f"output {k}={v}")
    path = os.environ.get("GITHUB_OUTPUT")
    if path:
        with open(path, "a") as f:
            f.writelines(f"{k}={v}\n" for k, v in pairs.items())


def gh_summary(markdown: str) -> None:
    path = os.environ.get("GITHUB_STEP_SUMMARY")
    if path:
        with open(path, "a") as f:
            f.write(markdown + "\n")


# --------------------------------------------------------------------------- detect


@dataclass
class Probe:
    """HEAD result for one dump URL."""

    url: str
    exists: bool
    last_modified: dt.datetime | None = None
    etag: str = ""
    content_length: int = 0


def head(url: str, timeout: float = 30.0) -> Probe:
    req = urllib.request.Request(url, method="HEAD")
    try:
        resp = urllib.request.urlopen(req, timeout=timeout)
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return Probe(url=url, exists=False)
        raise
    with resp:
        lm = resp.headers.get("Last-Modified")
        return Probe(
            url=url,
            exists=True,
            last_modified=email.utils.parsedate_to_datetime(lm) if lm else None,
            etag=(resp.headers.get("ETag") or "").strip('"'),
            content_length=int(resp.headers.get("Content-Length") or 0),
        )


def url_for(month: str) -> str:
    return URL_TEMPLATE.format(month=month)


def next_month(month: str) -> str:
    y, m = (int(p) for p in month.split("_"))
    y, m = (y + 1, 1) if m == 12 else (y, m + 1)
    return f"{y:04d}_{m:02d}"


def month_candidates(pinned: str, today: dt.date) -> list[str]:
    """Months strictly after the pin through the current month, newest first."""
    current = f"{today.year:04d}_{today.month:02d}"
    out: list[str] = []
    m = next_month(pinned)
    while m <= current:  # zero-padded YYYY_MM compares correctly as text
        out.append(m)
        m = next_month(m)
    return list(reversed(out))


def pinned_manifest() -> dict:
    """The committed pin. The working-tree manifest is NOT the pin: a crashed or
    partial run leaves it rewritten to the new month, and trusting it would make
    a re-run false-noop ("byte-identical to the pin") without ever gating."""
    p = sh(["git", "show", "HEAD:vpic/manifest.json"], check=False, capture=True)
    if p.returncode == 0 and (p.stdout or "").strip():
        return json.loads(p.stdout)
    return json.loads(MANIFEST.read_text())  # not a git checkout — best effort


@dataclass
class Detection:
    month: str
    url: str
    reason: str  # "new" | "reissue" | "forced"


def _utc_today() -> dt.date:
    return dt.datetime.now(dt.timezone.utc).date()  # NHTSA timestamps are GMT


def detect(pinned: dict, today: dt.date, probe: Callable[[str], Probe] = head) -> Detection | None:
    """Newest unintegrated month if one exists, else a re-issued pin, else None.

    Re-issue = the pinned month's file no longer has the pinned byte size
    (manifests since 2026_07 record dump_bytes). Comparing sizes is stateless
    and cannot retrigger daily on a byte-identical re-touch, unlike mtime.
    """
    month = pinned["month"]
    for candidate in month_candidates(month, today):
        p = probe(url_for(candidate))
        if p.exists:
            return Detection(month=candidate, url=p.url, reason="new")
    dump_bytes = pinned.get("dump_bytes")
    if dump_bytes:
        p = probe(url_for(month))
        if p.exists and p.content_length and p.content_length != dump_bytes:
            return Detection(month=month, url=p.url, reason="reissue")
    return None


def cmd_detect(args: argparse.Namespace) -> int:
    pinned = pinned_manifest()
    if args.month:  # manual override: skip probing, trust the caller
        if not MONTH_RE.match(args.month):
            log(f"invalid month {args.month!r} (want YYYY_MM)")
            return 1
        gh_output({"month": args.month, "url": url_for(args.month), "reason": "forced"})
        return 0
    found = detect(pinned, _utc_today())
    if found is None:
        months_behind = len(month_candidates(pinned["month"], _utc_today()))
        log(f"no dump newer than pinned {pinned['month']}")
        if months_behind >= 2:
            log(f"warning: pin is {months_behind} months behind with nothing published — investigate")
        gh_output({"month": "", "stale": str(months_behind >= 2).lower()})
        return 0
    log(f"detected {found.reason} dump for {found.month}: {found.url}")
    gh_output({"month": found.month, "url": found.url, "reason": found.reason})
    return 0


# --------------------------------------------------------------------------- gates


@dataclass
class Gate:
    name: str
    ok: bool
    detail: str


def _exact(fp: dict) -> bool:
    return not fp["field_diffs"] and not fp["missing"] and not fp["extra"] and fp["order_ok"]


def corpus_gate(corpus: dict) -> Gate:
    diverging = sorted(e["vin"] for e in corpus["entries"] if not _exact(e["expected_diff"]))
    unexpected = [v for v in diverging if v not in KNOWN_DEVIATION_VINS]
    detail = f"{len(corpus['entries'])} VINs re-frozen; diverging: {diverging or 'none'}"
    if unexpected:
        detail += f"; NOT documented deviations: {unexpected}"
    return Gate("corpus", not unexpected, detail)


def sweep_gate(report: dict) -> Gate:
    total, exact = report["total"], report["exact_parity"]
    # A VIN the oracle crashed on produced no answer, so it is neither parity nor
    # a diff. sweep.py used to die on the first one; now it reports them and the
    # verdict happens here, so an *undocumented* crash can never pass unnoticed.
    crashed = sorted({e["vin"] for e in report.get("oracle_errors", [])})
    undocumented_crashes = [v for v in crashed if v not in KNOWN_DEVIATION_VINS]
    crash_detail = ""
    if crashed:
        crash_detail = f"; oracle crashed on {len(crashed)} VIN(s) (documented: {not undocumented_crashes})"
        if undocumented_crashes:
            crash_detail += f"; NOT documented: {undocumented_crashes}"
    if report["diverged"] == 0:
        return Gate("sweep", not undocumented_crashes, f"{exact}/{total} exact{crash_detail}")
    vins = sorted({ex["vin"] for ex in report["examples"]})
    unexpected = [v for v in vins if v not in KNOWN_DEVIATION_VINS]
    unlisted = report["diverged"] - len(report["examples"])  # only if --examples < limit
    ok = not unexpected and unlisted <= 0 and not undocumented_crashes
    detail = f"{exact}/{total} exact; diverging: {vins}{crash_detail}"
    if unlisted > 0:
        detail += f"; {unlisted} further diffs not enumerated"
    return Gate("sweep", ok, detail)


# --------------------------------------------------------------------------- classify


@dataclass
class Classification:
    kind: str  # "data-only" | "schema-change"
    changed_files: list[str]
    tables_added: list[str]
    tables_dropped: list[str]
    functions_added: list[str]
    functions_dropped: list[str]
    row_moves: list[tuple[str, int, int]]  # (table, old, new) sorted by |delta|
    total_rows: tuple[int, int]
    artifact_bytes: tuple[int, int]


def changed_schema_files() -> list[str]:
    p = sh(["git", "status", "--porcelain", "--", "vpic/schema", "vpic/procs"], capture=True)
    return sorted(line[3:] for line in p.stdout.splitlines() if line.strip())


def classify(old: dict, new: dict, changed: list[str]) -> Classification:
    old_t, new_t = old["tables"], new["tables"]
    moves = sorted(
        ((t, old_t.get(t, 0), new_t.get(t, 0)) for t in set(old_t) | set(new_t)),
        key=lambda x: -abs(x[2] - x[1]),
    )
    tables_added = sorted(set(new_t) - set(old_t))
    tables_dropped = sorted(set(old_t) - set(new_t))
    functions_added = sorted(set(new["functions"]) - set(old["functions"]))
    functions_dropped = sorted(set(old["functions"]) - set(new["functions"]))
    structural = bool(changed or tables_added or tables_dropped or functions_added or functions_dropped)
    return Classification(
        kind="schema-change" if structural else "data-only",
        changed_files=changed,
        tables_added=tables_added,
        tables_dropped=tables_dropped,
        functions_added=functions_added,
        functions_dropped=functions_dropped,
        row_moves=[(t, a, b) for t, a, b in moves if a != b][:10],
        total_rows=(old["total_rows"], new["total_rows"]),
        artifact_bytes=(old["artifact_bytes"], new["artifact_bytes"]),
    )


# --------------------------------------------------------------------------- lookups
#
# In-place value edits in small lookup tables (e.g. 2026_07 renamed bodystyle 7
# "...(SUV)..." to "...[SUV]...") change decode output for huge VIN populations
# yet are invisible to row-count deltas. Freezing those tables into
# vpic/lookups.json makes the rename reviewable in the PR diff, and the report
# names each changed value below.


def freeze_lookups(dump: Path) -> dict[str, dict[str, str]]:
    """{table: {first-column: rest-of-row}} for every vpic table with <= LOOKUP_MAX_ROWS rows.

    Raw COPY text, untouched: the file is a diff surface, not a parser."""
    copy_re = re.compile(r"^COPY vpic\.(\w+) \(")
    tables: dict[str, dict[str, str]] = {}
    name = ""
    rows: dict[str, str] | None = None
    with zipfile.ZipFile(dump) as z:
        member = next(n for n in z.namelist() if n.endswith(".sql"))
        with z.open(member) as raw:
            for line in io.TextIOWrapper(raw, encoding="utf-8"):
                if rows is None:
                    m = copy_re.match(line)
                    if m:
                        name, rows = m.group(1), {}
                elif line.startswith("\\."):
                    if len(rows) <= LOOKUP_MAX_ROWS:
                        tables[name] = rows
                    rows = None
                elif len(rows) <= LOOKUP_MAX_ROWS:
                    key, _, rest = line.rstrip("\n").partition("\t")
                    rows[key] = rest
    return tables


def pinned_lookups() -> dict | None:
    """The committed freeze, or None before the first refresh ships one."""
    p = sh(["git", "show", "HEAD:vpic/lookups.json"], check=False, capture=True)
    if p.returncode == 0 and (p.stdout or "").strip():
        return json.loads(p.stdout)
    return None


@dataclass
class LookupDiff:
    changed: list[tuple[str, str, str, str]] = field(default_factory=list)  # (table, id, old, new)
    added: list[tuple[str, int]] = field(default_factory=list)
    removed: list[tuple[str, int]] = field(default_factory=list)
    baseline: bool = False  # first refresh: nothing committed to diff against


def diff_lookups(old: dict, new: dict) -> LookupDiff:
    d = LookupDiff()
    order = lambda k: (len(k), k)  # noqa: E731 — numeric-string ids sort numerically
    for t in sorted(set(old) | set(new)):
        o, n = old.get(t, {}), new.get(t, {})
        d.changed += ((t, k, o[k], n[k]) for k in sorted(o.keys() & n.keys(), key=order) if o[k] != n[k])
        if len(n.keys() - o.keys()):
            d.added.append((t, len(n.keys() - o.keys())))
        if len(o.keys() - n.keys()):
            d.removed.append((t, len(o.keys() - n.keys())))
    return d


def render_lookups(d: LookupDiff) -> list[str]:
    lines = ["## Lookup value changes", ""]
    if d.baseline:
        return [*lines, "`vpic/lookups.json` baseline frozen — value diffs appear from the next refresh.", ""]
    shown = d.changed[:LOOKUP_REPORT_CAP]
    lines += [f"- `{t}[{k}]`: “{a}” → “{b}”" for t, k, a, b in shown]
    if len(d.changed) > len(shown):
        lines.append(f"- …{len(d.changed) - len(shown)} more — see the `vpic/lookups.json` diff")
    if d.added:
        lines.append("- rows added: " + ", ".join(f"`{t}` +{n}" for t, n in d.added))
    if d.removed:
        lines.append("- rows removed: " + ", ".join(f"`{t}` -{n}" for t, n in d.removed))
    if not (d.changed or d.added or d.removed):
        lines.append("none")
    return [*lines, "", f"_(tables ≤ {LOOKUP_MAX_ROWS} rows; larger tables are gate-validated but not enumerated)_", ""]


# --------------------------------------------------------------------------- report


@dataclass
class Report:
    old_month: str
    month: str
    source: Probe
    sha256: str
    classification: Classification
    gates: list[Gate]
    lookups: LookupDiff | None = None
    skipped_vins: list[str] = field(default_factory=list)
    followups: list[str] = field(default_factory=list)

    @property
    def ok(self) -> bool:
        return all(g.ok for g in self.gates)


def render_report(r: Report) -> str:
    c = r.classification
    lines = [f"# vPIC data refresh: {r.old_month} → {r.month}", ""]
    if c.kind == "data-only":
        lines.append("**Classification: data-only** — `vpic/schema/` and `vpic/procs/` are unchanged.")
    else:
        lines.append("**Classification: SCHEMA/PROC CHANGE** — review the `vpic/` diff carefully:")
        lines += [f"- `{f}`" for f in c.changed_files]
        for label, items in [
            ("tables added", c.tables_added),
            ("tables dropped", c.tables_dropped),
            ("functions added", c.functions_added),
            ("functions dropped", c.functions_dropped),
        ]:
            if items:
                lines.append(f"- {label}: {', '.join(items)}")
    lines += [
        "",
        f"| | {r.old_month} | {r.month} | Δ |",
        "|---|---:|---:|---:|",
        f"| total rows | {c.total_rows[0]:,} | {c.total_rows[1]:,} | {c.total_rows[1] - c.total_rows[0]:+,} |",
        f"| artifact bytes | {c.artifact_bytes[0]:,} | {c.artifact_bytes[1]:,} | {c.artifact_bytes[1] - c.artifact_bytes[0]:+,} |",
        "",
        "Largest table changes: " + (", ".join(f"{t} {b - a:+,}" for t, a, b in c.row_moves) or "none"),
        "",
    ]
    if r.lookups is not None:
        lines += render_lookups(r.lookups)
    lines += ["## Gates", ""]
    lines += [f"- {'✅' if g.ok else '❌'} **{g.name}** — {g.detail}" for g in r.gates]
    if r.followups:
        lines += ["", "## Follow-ups", ""] + [f"- {f}" for f in r.followups]
    lm = r.source.last_modified.isoformat() if r.source.last_modified else "?"
    lines += [
        "",
        f"Source: {r.source.url} ({r.source.content_length:,} bytes, last-modified {lm})",
        f"sha256: `{r.sha256}`",
        "",
    ]
    return "\n".join(lines)


def write_report(r: Report) -> None:
    REPORT_DIR.mkdir(parents=True, exist_ok=True)
    md = render_report(r)
    (REPORT_DIR / "report.md").write_text(md)
    payload = {
        "month": r.month,
        "old_month": r.old_month,
        "classification": r.classification.kind,
        "ok": r.ok,
        "gates": [{"name": g.name, "ok": g.ok, "detail": g.detail} for g in r.gates],
        "skipped_vins": r.skipped_vins,
        "followups": r.followups,
        "sha256": r.sha256,
    }
    (REPORT_DIR / "report.json").write_text(json.dumps(payload, indent=2) + "\n")
    gh_summary(md)
    print("\n" + md)


# --------------------------------------------------------------------------- run


def download(month: str) -> tuple[Path, str, Probe]:
    """Fetch the dump fresh (published files get re-touched), return path + sha256."""
    url = url_for(month)
    probe = head(url)
    if not probe.exists:
        msg = f"[refresh] no dump published for {month} at {url}"
        raise SystemExit(msg)
    dest = ROOT / "downloads" / f"vPICList_lite_{month}.plain.zip"
    dest.parent.mkdir(exist_ok=True)
    tmp = dest.with_suffix(".zip.part")
    sh(["curl", "-fSL", "--retry", "3", "--max-time", "900", url, "-o", str(tmp)])
    tmp.replace(dest)
    digest = hashlib.sha256()
    with open(dest, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            digest.update(chunk)
    return dest, digest.hexdigest(), probe


def parse_freeze_skips(stdout: str) -> list[str]:
    return re.findall(r"skipped \(oracle error[^)]*\): (\S+):", stdout)


def cmd_run(args: argparse.Namespace) -> int:
    month = args.month
    if not MONTH_RE.match(month):
        log(f"invalid month {month!r} (want YYYY_MM)")
        return 1
    old_manifest = pinned_manifest()

    dump, sha, probe = download(month)
    if sha == old_manifest["dump_sha256"]:
        log(f"dump for {month} is byte-identical to the pin — nothing to do")
        gh_output({"changed": "false", "month": month})
        return 0

    # Rebuild vpic/ + the embedded artifact, then the Python extension on top of it.
    sh(
        [
            "cargo",
            "run",
            "-p",
            "ultravin-build",
            "--release",
            "--locked",
            "--",
            "--dump",
            str(dump),
            "--month",
            month,
            "--out",
            "vpic",
        ]
    )
    new_manifest = json.loads(MANIFEST.read_text())

    # Freeze small lookup tables so in-place label renames surface in the PR
    # diff and the report (row counts can't show them).
    old_lookups = pinned_lookups()
    new_lookups = freeze_lookups(dump)
    LOOKUPS.write_text(json.dumps(new_lookups, indent=1, sort_keys=True, ensure_ascii=False) + "\n")
    lookups = LookupDiff(baseline=True) if old_lookups is None else diff_lookups(old_lookups, new_lookups)

    sh(["uv", "sync", "--frozen", "--all-extras"])
    sh(["uv", "run", "--frozen", "--", "maturin", "develop", "--uv"])

    # Cycle the oracle onto the new dump (wipes whatever was loaded — by design).
    sh(["bash", "scripts/oracle.sh", "down"], check=False)
    sh(["bash", "scripts/oracle.sh", "up"])
    sh(["bash", "scripts/oracle.sh", "load", str(dump)])

    # Re-freeze the regression corpus from the new oracle, then gate everything.
    freeze = sh(
        [
            "uv",
            "run",
            "--frozen",
            "--",
            "python",
            "-m",
            "scripts.parity.freeze",
            "--target",
            "220",
            "--add-vins",
            "tests/brutal_repros.json",
        ],
        capture=True,
    )
    print(freeze.stdout, end="")
    skipped = parse_freeze_skips(freeze.stdout)
    gates = [corpus_gate(json.loads(CORPUS.read_text()))]

    REPORT_DIR.mkdir(parents=True, exist_ok=True)
    sweep_path = REPORT_DIR / "sweep.json"
    sh(
        [
            "uv",
            "run",
            "--frozen",
            "--",
            "python",
            "-m",
            "scripts.parity.sweep",
            "--sample",
            "2",
            "--limit",
            str(SWEEP_LIMIT),
            "--examples",
            str(SWEEP_LIMIT),
            "--out",
            str(sweep_path),
        ]
    )
    gates.append(sweep_gate(json.loads(sweep_path.read_text())))

    pytest = sh(["uv", "run", "--frozen", "--", "pytest", "-q"], check=False, capture=True)
    print(pytest.stdout[-4000:], pytest.stderr[-2000:])
    gates.append(
        Gate(
            "pytest",
            pytest.returncode == 0,
            "passed" if pytest.returncode == 0 else (pytest.stdout.strip().splitlines() or ["failed (see log)"])[-1],
        )
    )
    # The cover is rebuilt from the new dump by vpic-import above, but *what it
    # looks for* is code: the token grammar, the sweep dimensions, the candidate
    # families. A month whose data reaches a branch the old data could not shows
    # up here as a stale allowance — the reason for excluding it has expired.
    coverage = sh(["uv", "run", "--frozen", "--", "python", "-m", "scripts.coverage"], check=False, capture=True)
    print(coverage.stdout[-3000:], coverage.stderr[-2000:])
    cov_detail = (coverage.stdout.strip().splitlines() or ["failed (see log)"])[-1]
    if coverage.returncode != 0:
        cov_detail = (
            "; ".join(
                ln.strip()
                for ln in (coverage.stdout + coverage.stderr).splitlines()
                if ln.strip().startswith(("NEW GAP", "STALE", "WIDENED", "NARROWED"))
            )
            or cov_detail
        )
    gates.append(Gate("coverage", coverage.returncode == 0, cov_detail))

    cargo = sh(["cargo", "test", "--workspace", "--exclude", "ultravin-py"], check=False, capture=True)
    print(cargo.stdout[-2000:], cargo.stderr[-2000:])
    gates.append(
        Gate("cargo", cargo.returncode == 0, "passed" if cargo.returncode == 0 else "cargo test failed (see log)")
    )

    classification = classify(old_manifest, new_manifest, changed_schema_files())
    report = Report(
        old_month=old_manifest["month"],
        month=month,
        source=probe,
        sha256=sha,
        classification=classification,
        gates=gates,
        lookups=lookups,
        skipped_vins=skipped,
    )
    healed = sorted(
        KNOWN_DEVIATION_VINS
        - {e["vin"] for e in json.loads(CORPUS.read_text())["entries"] if not _exact(e["expected_diff"])}
    )
    if healed:
        report.followups.append(
            f"known deviation(s) {healed} no longer reproduce — update docs/KNOWN_DEVIATIONS.md "
            "and drop them from KNOWN_DEVIATION_VINS in scripts/refresh.py"
        )
    if skipped:
        report.followups.append(
            f"freeze skipped oracle-erroring VIN(s) {skipped} — if new, document per docs/KNOWN_DEVIATIONS.md"
        )
    if classification.kind == "schema-change":
        report.followups.append("schema/proc text changed — diff vpic/ against the decoder before merging")
    write_report(report)
    gh_output(
        {
            "changed": "true",
            "month": month,
            "classification": classification.kind,
            "gates": "pass" if report.ok else "fail",
            "report": str((REPORT_DIR / "report.md").relative_to(ROOT)),
        }
    )
    if not report.ok:
        log("gate failure: " + "; ".join(f"{g.name}: {g.detail}" for g in gates if not g.ok))
        return 2
    log("all gates green")
    return 0


# --------------------------------------------------------------------------- main


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description="Monthly NHTSA vPIC data refresh: detect a newer dump, integrate it, gate on parity."
    )
    sub = ap.add_subparsers(dest="cmd", required=True)
    d = sub.add_parser("detect", help="probe NHTSA for a dump newer than the pin")
    d.add_argument("--month", default="", help="force this YYYY_MM instead of probing")
    d.set_defaults(fn=cmd_detect)
    r = sub.add_parser("run", help="integrate a month end-to-end with parity gates")
    r.add_argument("--month", required=True, help="YYYY_MM to integrate")
    r.set_defaults(fn=cmd_run)
    args = ap.parse_args(argv)
    try:
        return args.fn(args)
    except subprocess.CalledProcessError as e:
        # Mechanical failure: leave a machine-readable trace for the fix agent
        # (the gate-failure path writes a full report; this path otherwise
        # leaves only the workflow log).
        REPORT_DIR.mkdir(parents=True, exist_ok=True)
        (REPORT_DIR / "failure.json").write_text(
            json.dumps({"failed_command": e.cmd, "returncode": e.returncode}, indent=2) + "\n"
        )
        log(f"mechanical failure: {e.cmd} exited {e.returncode} (context in {REPORT_DIR / 'failure.json'})")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
