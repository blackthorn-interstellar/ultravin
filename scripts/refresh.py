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
  stale-cache
            the regenerated scripts/stale_cache_cells.json agrees with its own
            summary, did not gain implausibly many cells, and still faces an
            empty cacheexceptions table (the list *changing* month to month is
            the point, not a failure — see scripts/parity/stale_cache.py)
  known-problems
            every documented oracle problem still reproduces — the converse of
            the two above, which only ever *excuse* those VINs — and each
            registered deviation still diverges the *same way* HEAD froze it
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
import sys
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
STALE_CACHE_CELLS = ROOT / "scripts" / "stale_cache_cells.json"
REPORT_DIR = ROOT / "target" / "refresh"
URL_TEMPLATE = "https://vpic.nhtsa.dot.gov/Downloads/vPICList_lite_{month}.plain.zip"
MONTH_RE = re.compile(r"^\d{4}_(0[1-9]|1[0-2])$")
SWEEP_LIMIT = 500
LOOKUP_MAX_ROWS = 512  # tables above this aren't lookups; the parity gates still validate them
LOOKUP_REPORT_CAP = 20

# The documented vPIC defects ultravin deliberately does not reproduce live in
# one registry, scripts/known_problems.json — one entry per VIN, each carrying
# the upstream root cause, the reproducible evidence, and the
# docs/KNOWN_DEVIATIONS.md section that argues it. The registry is split here
# into two frozensets because its two kinds excuse two different observations.
# `oracle-crash` excuses a *crash*: the oracle aborted and gave no answer at all.
# `deviation` excuses a *divergence*: the oracle answered and ultravin disagreed,
# for a documented reason. Neither kind excuses the other's condition — a
# crash-listed VIN that suddenly diverges is new information (the oracle now
# answers, and we are wrong about what it says), and a crash on a deviation VIN
# is an undocumented crash. Both must fail their gate. Every entry is also
# re-probed each refresh (known_problems_gate): an excuse that stopped
# reproducing is stale and fails too.
#
# The 65 crash VINs are the original 2026_06 report, the 62 more the 2026_07
# campaign hit, and the two the 2026-08-16 backlog probe hit; all 65 are WMI
# 7T0. They are a *sample* of an unbounded class —
# any 7T0 VIN of model year 2023-2025 whose decode matches vinschema 24522 aborts
# the same way — so a future sweep may find a 7T0 VIN that is not registered and
# fail the gate. That failure is correct: it should be re-verified against the
# entry's evidence and then added, not assumed.
# freeze.py needs none of this: it skips oracle-erroring VINs before they ever
# reach the corpus, and surfaces new skips in the report as follow-ups.
KNOWN_PROBLEMS = ROOT / "scripts" / "known_problems.json"
PROBLEM_KINDS = ("oracle-crash", "deviation")
PROBLEM_SCOPES = ("error-fields", "clean-decode")


def load_known_problems(path: Path = KNOWN_PROBLEMS) -> list[dict[str, str]]:
    """Every registry entry, in file order."""
    return json.loads(path.read_text())["entries"]


_KNOWN_PROBLEMS = load_known_problems()
ORACLE_CRASH_VINS = frozenset(e["vin"] for e in _KNOWN_PROBLEMS if e["kind"] == "oracle-crash")
KNOWN_DEVIATION_VINS = frozenset(e["vin"] for e in _KNOWN_PROBLEMS if e["kind"] == "deviation")


# --------------------------------------------------------------------------- util


def log(msg: str) -> None:
    print(f"[refresh] {msg}", flush=True)


def sh(cmd: list[str], *, check: bool = True, capture: bool = False) -> subprocess.CompletedProcess[str]:
    log("$ " + " ".join(cmd))
    return subprocess.run(cmd, cwd=ROOT, check=check, capture_output=capture, text=True)


def head_json(path: str) -> dict | None:
    """`git show HEAD:<path>` parsed, or None when HEAD has no such file.

    The *committed* copy, never the working tree: a refresh rewrites its inputs
    in place, so anything comparing "before" against "after" has to read before
    from git. None also covers "not a git checkout at all", where there is no
    before to compare with."""
    p = sh(["git", "show", f"HEAD:{path}"], check=False, capture=True)
    if p.returncode == 0 and (p.stdout or "").strip():
        return json.loads(p.stdout)
    return None


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
        # Fixed https://vpic.nhtsa.dot.gov URL_TEMPLATE; only the YYYY_MM month
        # varies. Scheme and host are not caller-controlled.
        # nosemgrep: dynamic-urllib-use-detected
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


# One documented class is enumerated by machine rather than by VIN: every
# (wmi, year) cell where the dump's vpic.wmiyearvalidchars cache contradicts the
# recompute from that same dump's pattern rows. The oracle reads the cache and
# ultravin recomputes, so a divergence that touches only the error/correction
# elements *and* lands on a listed cell is that defect and nothing else — see
# scripts/parity/stale_cache.py and docs/KNOWN_DEVIATIONS.md. The gates below
# take that set as an argument (computed by a subprocess that has the extension;
# this module runs under a bare python3) and default to empty, so nothing is
# excused unless the classification actually ran.


def corpus_gate(corpus: dict, stale_cache_vins: frozenset[str] = frozenset()) -> Gate:
    diverging = sorted(e["vin"] for e in corpus["entries"] if not _exact(e["expected_diff"]))
    stale = [v for v in diverging if v in stale_cache_vins and v not in KNOWN_DEVIATION_VINS]
    unexpected = [v for v in diverging if v not in KNOWN_DEVIATION_VINS and v not in stale_cache_vins]
    detail = f"{len(corpus['entries'])} VINs re-frozen; diverging: {diverging or 'none'}"
    if stale:
        detail += f"; {len(stale)} in the stale-wmiyearvalidchars-cache class: {stale}"
    if unexpected:
        detail += f"; NOT documented deviations: {unexpected}"
    return Gate("corpus", not unexpected, detail)


# Legitimate month-over-month churn in the stale-cell list is tens of cells: a
# rebuild drops some pattern rows and the cache lags on the handful of WMI-years
# they covered. A charset regression in the decoder looks nothing like that — it
# re-lists thousands at once, because every cell whose recompute moved now
# contradicts a cache that did not. So a jump this large stops the refresh: the
# answer key must be green and a human must read the scan report before a month
# that big is accepted.
STALE_CACHE_JUMP_LIMIT = 500


def stale_cache_gate(doc: dict, head_doc: dict | None, problems: list[str]) -> Gate:
    """The regenerated stale-cell list must be self-consistent and plausible.

    A month-over-month *change* in the list is expected — the cache is stale in
    different places every rebuild — and is reported, never failed: the list is
    a fact about the dump and the refresh PR diff is where it documents itself.
    Three things do fail. An internally inconsistent list means the file was
    hand-edited or its two halves came from different scans, and every divergence
    excused on the strength of it is then unfounded. A newly-stale count past
    `STALE_CACHE_JUMP_LIMIT` is the shape a decoder charset regression takes, not
    the shape upstream churn takes. And a non-empty
    `wmiyearvalidchars_cacheexceptions` means the proc no longer reads the cache
    the way this whole scan assumes it does."""
    cells = {(wmi, year) for wmi, year, *_ in doc["cells"]}
    detail = f"{len(cells):,} stale cells in {doc['dump']}"
    implausible: list[str] = []
    if head_doc is not None:
        was = {(wmi, year) for wmi, year, *_ in head_doc.get("cells", [])}
        newly, healed = cells - was, was - cells
        detail += f"; vs {head_doc.get('dump')}: {len(newly):+,} newly stale, {len(healed):,} healed"
        if len(newly) > STALE_CACHE_JUMP_LIMIT:
            implausible.append(
                f"{len(newly):,} newly stale cells exceeds the {STALE_CACHE_JUMP_LIMIT:,} jump limit — "
                "upstream churn is tens of cells, a decoder charset regression re-lists thousands; "
                "confirm `answerkey verify` is green and read target/refresh/stale_cache.json before accepting"
            )
    exceptions = doc.get("summary", {}).get("cache_exception_wmis", 0)
    if exceptions:
        implausible.append(
            f"vpic.wmiyearvalidchars_cacheexceptions carries {exceptions:,} WMI(s) — it has always been "
            "empty, so the proc's cache read now behaves differently and the scan's assumptions need re-deriving"
        )
    if problems:
        detail += "; INCONSISTENT (the list contradicts its own summary): " + "; ".join(problems)
    if implausible:
        detail += "; REJECTED: " + "; ".join(implausible)
    return Gate("stale-cache", not problems and not implausible, detail)


def sweep_gate(report: dict, stale_cache_vins: frozenset[str] = frozenset()) -> Gate:
    total, exact = report["total"], report["exact_parity"]
    # A VIN the oracle crashed on produced no answer, so it is neither parity nor
    # a diff. sweep.py used to die on the first one; now it reports them and the
    # verdict happens here, so an *undocumented* crash can never pass unnoticed.
    crashed = sorted({e["vin"] for e in report.get("oracle_errors", [])})
    undocumented_crashes = [v for v in crashed if v not in ORACLE_CRASH_VINS]
    crash_detail = ""
    if crashed:
        crash_detail = f"; oracle crashed on {len(crashed)} VIN(s) (documented: {not undocumented_crashes})"
        if undocumented_crashes:
            crash_detail += f"; NOT documented: {undocumented_crashes}"
    if report["diverged"] == 0:
        return Gate("sweep", not undocumented_crashes, f"{exact}/{total} exact{crash_detail}")
    vins = sorted({ex["vin"] for ex in report["examples"]})
    stale = [v for v in vins if v in stale_cache_vins and v not in KNOWN_DEVIATION_VINS]
    unexpected = [v for v in vins if v not in KNOWN_DEVIATION_VINS and v not in stale_cache_vins]
    unlisted = report["diverged"] - len(report["examples"])  # only if --examples < limit
    ok = not unexpected and unlisted <= 0 and not undocumented_crashes
    detail = f"{exact}/{total} exact; diverging: {vins}{crash_detail}"
    if stale:
        detail += f"; {len(stale)} in the stale-wmiyearvalidchars-cache class: {stale}"
    if unlisted > 0:
        detail += f"; {unlisted} further diffs not enumerated"
    return Gate("sweep", ok, detail)


def registered_deviations(entries: list[dict[str, str]]) -> frozenset[str]:
    """The `deviation` VINs of a registry snapshot (HEAD's or the current one)."""
    return frozenset(e["vin"] for e in entries if e.get("kind") == "deviation")


def deviation_shape_changes(
    head_corpus: dict | None,
    head_registry: list[dict[str, str]] | None,
    corpus: dict,
    registry: list[dict[str, str]],
) -> list[str]:
    """Registered deviations whose *expected difference* is not the one HEAD froze.

    docs/ACCEPTANCE.md requires a deviation to be frozen in the corpus so the
    difference is locked and an unexpected change to it fails a gate. The
    known-problems probe only asks whether the VIN still diverges *at all*, and
    `freeze.py` will happily snapshot a new shape as the new baseline — so
    without this, a documented deviation that quietly started diverging some
    other way is laundered green by the very file that is supposed to pin it.

    Compared only for VINs that are deviations in **both** registries: a
    registration added this cycle has no baseline to differ from (this run is
    what establishes its shape), and a retired one is no longer ours to hold
    still. When HEAD carries no corpus or no registry — the first refresh ever,
    or not a git checkout — there is nothing to compare and nothing to fail on.

    HEAD moves when the refresh PR merges, so a deliberate change fires this
    once, gets re-investigated, and becomes the new baseline."""
    if head_corpus is None or head_registry is None:
        return []
    both = registered_deviations(registry) & registered_deviations(head_registry)
    was = {e["vin"]: e["expected_diff"] for e in head_corpus.get("entries", [])}
    now = {e["vin"]: e["expected_diff"] for e in corpus.get("entries", [])}
    return sorted(v for v in both if v in was and v in now and was[v] != now[v])


def known_problems_gate(
    probe: dict,
    crash_vins: frozenset[str] = ORACLE_CRASH_VINS,
    deviation_vins: frozenset[str] = KNOWN_DEVIATION_VINS,
    shape_changes: list[str] | None = None,
) -> Gate:
    """Every documented oracle problem must still reproduce on the new dump.

    The other gates only *excuse* these VINs, so a healed one keeps passing
    forever — 2026_08 fixed the stale-year-cache deviation and nothing noticed.
    scripts/parity/known_problems.py decodes each of them against the new oracle;
    this judges the report, in the same spirit as the coverage gate failing on a
    stale allowance: an expired excuse is a gate failure, not a footnote.

    An infra error (dead socket) is not evidence of either outcome, so it neither
    passes nor fails an entry on the merits — it fails the gate as *unverifiable*,
    because a run that could not check is not a run that confirmed."""
    healed: dict[str, list[str]] = {"oracle-crash": [], "deviation": []}
    unverifiable: list[str] = []
    for vins, name, expected in (
        (crash_vins, "oracle-crash", "crash"),
        (deviation_vins, "deviation", "diverged"),
    ):
        for vin in sorted(vins):
            outcome = probe.get(vin, {}).get("outcome", "not probed")
            if outcome == expected:
                continue
            if outcome in ("infra-error", "not probed"):
                unverifiable.append(f"{vin} ({outcome})")
            else:
                healed[name].append(f"{vin} (now {outcome})")
    total = len(crash_vins) + len(deviation_vins)
    stale = sum(len(v) for v in healed.values())
    detail = f"{total - stale - len(unverifiable)}/{total} documented problems still reproduce"
    for name, stale_vins in healed.items():
        if stale_vins:
            detail += (
                f"; {name} entries no longer reproduce — re-verify against docs/KNOWN_DEVIATIONS.md, "
                f"then retire them from scripts/known_problems.json: {stale_vins}"
            )
    if unverifiable:
        detail += f"; UNVERIFIABLE (oracle unreachable, so nothing was confirmed — re-run): {unverifiable}"
    if shape_changes:
        detail += (
            "; documented deviation changed shape — re-investigate and update the evidence "
            f"deliberately (docs/KNOWN_DEVIATIONS.md), then re-freeze: {shape_changes}"
        )
    return Gate("known-problems", not stale and not unverifiable and not shape_changes, detail)


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


def freeze_lookups(dump: Path) -> dict[str, list[list[str]]]:
    """{table: sorted list of full rows} for every vpic table with <= LOOKUP_MAX_ROWS rows.

    Rows are raw COPY text split on tabs — otherwise untouched: the file is a diff
    surface, not a parser. Whole rows are kept (not just {first-column: rest}) so
    composite-key tables (e.g. defs_model, keyed by make+id+from_year+mode) don't
    collapse when a later row shares a first column, and the cap counts real rows
    rather than distinct first columns."""
    copy_re = re.compile(r"^COPY vpic\.(\w+) \(")
    tables: dict[str, list[list[str]]] = {}
    name = ""
    rows: list[list[str]] | None = None
    capped = False  # this table exceeded the cap; consume to \. but don't keep it
    with zipfile.ZipFile(dump) as z:
        member = next(n for n in z.namelist() if n.endswith(".sql"))
        with z.open(member) as raw:
            for line in io.TextIOWrapper(raw, encoding="utf-8"):
                if rows is None:
                    m = copy_re.match(line)
                    if m:
                        name, rows, capped = m.group(1), [], False
                elif line.startswith("\\."):
                    if not capped:
                        tables[name] = sorted(rows)
                    rows = None
                elif not capped:
                    rows.append(line.rstrip("\n").split("\t"))
                    capped = len(rows) > LOOKUP_MAX_ROWS
    return tables


def pinned_lookups() -> dict | None:
    """The committed freeze, or None before the first refresh ships one."""
    return head_json("vpic/lookups.json")


@dataclass
class LookupDiff:
    changed: list[tuple[str, str, str, str]] = field(default_factory=list)  # (table, id, old, new)
    added: list[tuple[str, int]] = field(default_factory=list)
    removed: list[tuple[str, int]] = field(default_factory=list)
    baseline: bool = False  # first refresh: nothing committed to diff against
    migrated: bool = False  # pinned freeze predates full-row storage; per-row diff skipped


def _keyed_rows(rows: list[list[str]]) -> dict[str, str] | None:
    """{first-column: rest-of-row} when the first column is a unique id, else None.

    Single-column-keyed lookups (the label tables where a rename matters) get a
    readable id→value diff. Composite-key tables have no such id, so their diff
    falls back to counting whole rows added/removed."""
    ids = [r[0] for r in rows if r]
    if len(set(ids)) != len(ids):
        return None
    return {r[0]: "\t".join(r[1:]) for r in rows if r}


def _is_keyed_shape(snapshot: dict) -> bool:
    """True for the pre-migration {table: {id: rest}} freeze (table values are dicts)."""
    return any(isinstance(v, dict) for v in snapshot.values())


def diff_lookups(old: dict, new: dict) -> LookupDiff:
    if _is_keyed_shape(old) != _is_keyed_shape(new):
        # The pinned freeze predates full-row storage; a per-row diff across the
        # two shapes would be a wall of noise, so skip it for this one cycle.
        return LookupDiff(migrated=True)
    d = LookupDiff()
    order = lambda k: (len(k), k)  # noqa: E731 — numeric-string ids sort numerically
    for t in sorted(set(old) | set(new)):
        o_rows, n_rows = old.get(t, []), new.get(t, [])
        o, n = _keyed_rows(o_rows), _keyed_rows(n_rows)
        if o is not None and n is not None:
            d.changed += ((t, k, o[k], n[k]) for k in sorted(o.keys() & n.keys(), key=order) if o[k] != n[k])
            added, removed = len(n.keys() - o.keys()), len(o.keys() - n.keys())
        else:  # composite key: no stable id, so diff whole rows as a set
            o_set, n_set = {tuple(r) for r in o_rows}, {tuple(r) for r in n_rows}
            added, removed = len(n_set - o_set), len(o_set - n_set)
        if added:
            d.added.append((t, added))
        if removed:
            d.removed.append((t, removed))
    return d


def render_lookups(d: LookupDiff) -> list[str]:
    lines = ["## Lookup value changes", ""]
    if d.baseline:
        return [*lines, "`vpic/lookups.json` baseline frozen — value diffs appear from the next refresh.", ""]
    if d.migrated:
        return [
            *lines,
            (
                "Lookup snapshot format migrated to full-row storage — per-row value diff "
                "unavailable this cycle; compare the `vpic/lookups.json` diff directly."
            ),
            "",
        ]
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


def followups(r: Report) -> list[str]:
    """Human actions the report should surface. None of them fail a gate.

    A healed known deviation used to be reported here; the known-problems gate
    fails on it now, and one signal beats a warning next to a verdict.

    Pure (cmd_run does the reading) so every condition stays testable."""
    out: list[str] = []
    if r.skipped_vins:
        out.append(
            f"freeze skipped oracle-erroring VIN(s) {r.skipped_vins} — if new, document per docs/KNOWN_DEVIATIONS.md"
        )
    if r.classification.kind == "schema-change":
        out.append("schema/proc text changed — diff vpic/ against the decoder before merging")
    # render_lookups says so too, but only inside the lookup section — a reader who
    # skims to Follow-ups would otherwise take an empty value diff as "nothing changed".
    if r.lookups is not None and r.lookups.migrated:
        out.append(
            "lookup value review unavailable this cycle (snapshot format migrated) — "
            "verify label changes in the vpic/lookups.json diff by hand"
        )
    return out


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


def corpus_vins_file(corpus: Path = CORPUS, out: Path | None = None) -> Path | None:
    """The committed corpus's VIN list, written out for freeze.py's `--vins`.

    freeze.py's default is a *fresh* manufacturer-diverse sample, so an unpinned
    monthly re-freeze silently replaces the curated corpus rather than updating
    it: master's 399 coverage-union VINs would come back as ~272 mostly-different
    ones, retiring 342 VINs someone chose on purpose and making the PR diff
    unreadable. Re-freezing the *existing* set keeps the curation and makes the
    monthly diff mean "same VINs, new expectations" — which is what the corpus
    gate is written to judge. Returns None before the first corpus exists, where
    a fresh sample is the only option.

    Every registered `deviation` VIN is unioned in, because docs/ACCEPTANCE.md
    puts one in the corpus so its expected difference is *frozen*. Being in the
    corpus is what makes that lock checkable: `deviation_shape_changes` compares
    each one against the shape HEAD committed, and the known-problems gate fails
    on a difference — the probe alone only asks whether the VIN still diverges at
    all, which a changed shape still satisfies. Crash VINs are deliberately left
    out: freeze skips whatever the oracle errors on, so listing them would buy a
    skipped-VIN follow-up every month and nothing else.
    """
    if not corpus.exists():
        return None
    vins = [e["vin"] for e in json.loads(corpus.read_text())["entries"]]
    if not vins:
        return None
    vins += sorted(KNOWN_DEVIATION_VINS.difference(vins))
    out = out or REPORT_DIR / "corpus_vins.txt"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text("\n".join(vins) + "\n")
    return out


def _stale_cache():  # noqa: ANN202 — the module, whose import is deferred on purpose
    """`scripts.parity.stale_cache`, importable however this file was started.

    `python3 scripts/refresh.py` puts `scripts/` on sys.path rather than the repo
    root, so the package import needs the root put back. Deferred rather than a
    top-level import only so this module's own import stays as cheap as its
    docstring promises; the target is stdlib-only too."""
    if str(ROOT) not in sys.path:
        sys.path.insert(0, str(ROOT))
    from scripts.parity import stale_cache  # noqa: PLC0415

    return stale_cache


def stale_cache_expected(corpus: Path, sweep: Path) -> dict[str, frozenset[str]]:
    """Diverging VINs whose difference is the machine-enumerated stale-cache class.

    Shelled out, like every other parity step: deciding it needs a live ultravin
    decode for the model year the cell is keyed by, and this file runs under a
    bare `python3` with no extension installed. A run that produced nothing
    returns empty sets — nothing is excused by a classification that did not
    happen."""
    out = REPORT_DIR / "stale_cache_expected.json"
    out.unlink(missing_ok=True)
    sh(
        [
            "uv",
            "run",
            "--frozen",
            "--",
            "python",
            "-m",
            "scripts.parity.stale_cache",
            "expected",
            "--corpus",
            str(corpus),
            "--sweep",
            str(sweep),
            "--out",
            str(out),
        ],
        check=False,
    )
    found = json.loads(out.read_text()) if out.exists() else {}
    return {k: frozenset(found.get(k, ())) for k in ("corpus", "sweep")}


def freeze_command(vins_file: Path | None) -> list[str]:
    """`scripts.parity.freeze` argv: re-freeze the existing corpus, or sample a new one."""
    cmd = ["uv", "run", "--frozen", "--", "python", "-m", "scripts.parity.freeze"]
    cmd += ["--vins", str(vins_file)] if vins_file else ["--target", "220"]
    return [*cmd, "--add-vins", "tests/brutal_repros.json"]


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
    # The same pass scans vpic.wmiyearvalidchars against its own pattern rows
    # (--stale-cache-report), so the month's stale cells cost no second parse.
    REPORT_DIR.mkdir(parents=True, exist_ok=True)
    stale_report = REPORT_DIR / "stale_cache.json"
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
            "--stale-cache-report",
            str(stale_report),
        ]
    )
    new_manifest = json.loads(MANIFEST.read_text())

    # Regenerate the committed cell list so the refresh PR carries this month's.
    # The full report stays in target/refresh/ and rides out as the workflow's
    # data-refresh-report artifact; only the compact list is committed.
    stale_cache = _stale_cache()
    head_cells = head_json("scripts/stale_cache_cells.json")
    cells_doc = stale_cache.write_cells(json.loads(stale_report.read_text()), STALE_CACHE_CELLS)
    cells_problems = stale_cache.consistency_errors(cells_doc)

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
    # Read the corpus's VIN list — and HEAD's frozen shapes — *before* freeze
    # overwrites the file.
    head_corpus = head_json("tests/parity_corpus.json")
    head_registry = (head_json("scripts/known_problems.json") or {}).get("entries")
    freeze = sh(freeze_command(corpus_vins_file()), capture=True)
    print(freeze.stdout, end="")
    skipped = parse_freeze_skips(freeze.stdout)
    corpus = json.loads(CORPUS.read_text())
    shape_changes = deviation_shape_changes(head_corpus, head_registry, corpus, _KNOWN_PROBLEMS)

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
    # Both gates judge the same divergences, so both consult the same class.
    expected = stale_cache_expected(CORPUS, sweep_path)
    gates = [
        corpus_gate(corpus, expected["corpus"]),
        sweep_gate(json.loads(sweep_path.read_text()), expected["sweep"]),
        stale_cache_gate(cells_doc, head_cells, cells_problems),
    ]

    # Re-probe the documented problems themselves. The gates above only excuse
    # these VINs, and freeze's sample and the sweep almost never contain a crash
    # VIN, so without this a fixed upstream defect keeps buying an excuse forever.
    # Soft-run: a probe that dies leaves no report, which the gate reads as
    # "nothing confirmed" and fails on — more useful than a mechanical exit 1.
    known_path = REPORT_DIR / "known_problems.json"
    known_path.unlink(missing_ok=True)  # never judge this month by a stale local report
    sh(
        [
            "uv",
            "run",
            "--frozen",
            "--",
            "python",
            "-m",
            "scripts.parity.known_problems",
            "--out",
            str(known_path),
        ],
        check=False,
    )
    gates.append(
        known_problems_gate(
            json.loads(known_path.read_text()) if known_path.exists() else {},
            shape_changes=shape_changes,
        )
    )

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
    report.followups = followups(report)
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
