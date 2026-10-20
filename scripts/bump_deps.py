"""Nightly dependency bump with a publication cooldown (default 7 days).

Freshly published releases are where supply-chain compromises live; an
unattended nightly bump should never be the first installer. pyproject's
`exclude-newer` enforces the cooldown natively for Python, but cargo has no
equivalent, so `cooldown` post-processes a `cargo update`: every crate whose
new version was published inside the window is reverted to the version HEAD
pins (`cargo update -p crate@<new> --precise <old>`), and so is any freshly
bumped dependent that would just pin it forward again. Anything the rule
cannot decide — a crate new to the tree with no bumped dependent to withdraw
it, a multi-version shuffle, a version crates.io does not report — fails
closed: Cargo.lock is restored wholesale and the bump waits for a cleaner
night.

The one exception is a security fix: a package whose current pin has an open
OSV advisory (RustSec, GHSA, PyPI) skips the cooldown. Waiting out the window
would keep a known vulnerability to dodge a hypothetical one, and every PR's
security scan stays red until the fix lands. `uv-fixes` prints the uv flags
for that; `cooldown` exempts the cargo moves itself.

The written report covers both ecosystems and becomes the PR body.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
import urllib.request
from collections.abc import Collection
from datetime import datetime, timedelta, timezone
from pathlib import Path

import tomllib  # ty: ignore[unresolved-import]  (3.11+; CI-only tooling, never shipped to the 3.10 floor)

ROOT = Path(__file__).resolve().parent.parent

Move = tuple[str, str, str]  # (name, old_version, new_version)


def log(msg: str) -> None:
    print(f"[deps] {msg}", file=sys.stderr, flush=True)


def sh(cmd: list[str], *, capture: bool = False) -> subprocess.CompletedProcess[str]:
    log("$ " + " ".join(cmd))
    return subprocess.run(cmd, cwd=ROOT, check=True, capture_output=capture, text=True)


def versions(lock_text: str) -> dict[str, set[str]]:
    """name -> pinned versions in a Cargo.lock or uv.lock (both TOML [[package]])."""
    out: dict[str, set[str]] = {}
    for pkg in tomllib.loads(lock_text).get("package", []):
        if "version" in pkg:  # the editable project root has none
            out.setdefault(pkg["name"], set()).add(pkg["version"])
    return out


def moves(old: dict[str, set[str]], new: dict[str, set[str]]) -> tuple[list[Move], list[tuple[str, set[str]]]]:
    """Split lockfile changes into clean one-for-one moves and everything else.

    Clean: exactly one version left and one arrived for the name — revertible.
    Unclean (returned with the arriving versions): a crate new to the tree or a
    multi-version shuffle — there is nothing safe to revert *to*.
    """
    clean: list[Move] = []
    unclean: list[tuple[str, set[str]]] = []
    for name, newvs in sorted(new.items()):
        added = newvs - old.get(name, set())
        removed = old.get(name, set()) - newvs
        if not added:
            continue
        if len(added) == 1 and len(removed) == 1:
            clean.append((name, next(iter(removed)), next(iter(added))))
        else:
            unclean.append((name, added))
    return clean, unclean


def deps(lock_text: str) -> dict[str, set[str]]:
    """name -> the names it depends on, from Cargo.lock's own `dependencies` lists.

    An entry is `"name"`, or `"name version"` where the tree holds several
    versions; the version is dropped and every version of a name shares one
    edge set. That can only over-report an edge, and an over-reported edge
    costs one extra revert — never a missed one.
    """
    out: dict[str, set[str]] = {}
    for pkg in tomllib.loads(lock_text).get("package", []):
        out.setdefault(pkg["name"], set()).update(d.split()[0] for d in pkg.get("dependencies", []))
    return out


def _young(key: tuple[str, str], ages: dict[tuple[str, str], datetime | None], cutoff: datetime) -> bool:
    ts = ages.get(key)
    return ts is None or ts > cutoff  # unknown age fails closed


def _reaches(graph: dict[str, set[str]], name: str, moved: set[str]) -> set[str]:
    """Every name `name` can force a new pin on: its deps, and theirs only through
    crates that moved too (cycle-safe). An unmoved crate keeps the requirements
    HEAD already satisfied, so the walk stops there — `ring` holding still means
    `rustls-webpki` cannot be what drags in a young `cc`."""
    seen: set[str] = set()
    stack = list(graph.get(name, ()))
    while stack:
        n = stack.pop()
        if n not in seen:
            seen.add(n)
            if n in moved:
                stack += graph.get(n, ())
    return seen


def plan(
    clean: list[Move],
    unclean: list[tuple[str, set[str]]],
    ages: dict[tuple[str, str], datetime | None],
    cutoff: datetime,
    graph: dict[str, set[str]],
    fixes: Collection[str] = (),
) -> tuple[list[Move], dict[str, list[str]], list[tuple[str, str]]]:
    """Decide the reverts, say which of them are dependents, and name the blockers.

    Young (or unknown-age) clean moves revert to HEAD's pin — but a young crate
    that a freshly bumped dependent *requires* will not stay reverted, so every
    eligible mover reaching a young crate in `graph` is reverted alongside it.
    The returned map (mover -> the young pins that earned it a revert) is the
    report's explanation. Only a mover can be at fault: an untouched pin
    resolved against the old version at HEAD, so it still permits it.

    A blocker is a young unclean arrival — new to the tree, or a multi-version
    shuffle — that no revert reaches, so nothing can withdraw it and nothing
    can revert it in place. The whole Cargo.lock bump is abandoned tonight.

    A clean move named in `fixes` (its old pin has an open advisory) is never
    young: it ships tonight, and so does anything bumped only to pin it.
    """
    young_clean = [m for m in clean if m[0] not in fixes and _young((m[0], m[2]), ages, cutoff)]
    young_unclean = [(n, v) for n, added in unclean for v in sorted(added) if _young((n, v), ages, cutoff)]
    young = {n: f"{n} {v}" for n, _, v in young_clean} | {n: f"{n} {v}" for n, v in young_unclean}

    moved = {n for n, *_ in clean} | {n for n, _ in unclean}
    reach = {name: _reaches(graph, name, moved) for name, _, _ in clean}
    pinned: dict[str, list[str]] = {}
    for m in clean:
        pins = sorted(young[n] for n in reach[m[0]] & young.keys())
        if m not in young_clean and pins:
            pinned[m[0]] = pins

    # Dependents first: reverting one usually drags its young pin back down
    # with it, leaving the young crate's own revert a no-op rather than a fight.
    reverts = [m for m in clean if m[0] in pinned] + young_clean
    withdrawn = {n for m in reverts for n in reach[m[0]]}
    blockers = [(n, v) for n, v in young_unclean if n not in withdrawn]
    return reverts, pinned, blockers


def crates_io_ages(pairs: list[tuple[str, str]]) -> dict[tuple[str, str], datetime | None]:
    """Publication times from crates.io, one request per crate name."""
    ages: dict[tuple[str, str], datetime | None] = {}
    for name in sorted({name for name, _ in pairs}):
        url = f"https://crates.io/api/v1/crates/{name}/versions"
        req = urllib.request.Request(url, headers={"User-Agent": "ultravin-nightly-deps (CI cooldown check)"})
        published: dict[str, str] = {}
        for attempt in (1, 2):
            try:
                # Fixed https://crates.io registry endpoint; only the crate name,
                # read from our own Cargo.lock, varies.
                # nosemgrep: dynamic-urllib-use-detected
                with urllib.request.urlopen(req, timeout=30) as resp:
                    published = {v["num"]: v["created_at"] for v in json.load(resp)["versions"]}
                break
            except (OSError, TimeoutError, KeyError, json.JSONDecodeError) as e:
                if attempt == 2:
                    log(f"crates.io lookup failed for {name}: {e} — treating its bumps as unknown age")
                else:
                    time.sleep(2)
        for pname, ver in pairs:
            if pname == name:
                raw = published.get(ver)
                ages[(pname, ver)] = datetime.fromisoformat(raw) if raw else None
        time.sleep(0.2)  # crates.io crawler courtesy
    return ages


def advisories(ecosystem: str, pairs: list[tuple[str, str]]) -> dict[str, list[str]]:
    """name -> open OSV advisory ids, for each pinned (name, version) that has any.

    A failed lookup returns {}: no exemptions, the cooldown holds.
    """
    if not pairs:
        return {}
    queries = [{"package": {"name": n, "ecosystem": ecosystem}, "version": v} for n, v in pairs]
    req = urllib.request.Request(
        "https://api.osv.dev/v1/querybatch",
        data=json.dumps({"queries": queries}).encode(),
        headers={"Content-Type": "application/json"},
    )
    try:
        # Fixed https://api.osv.dev endpoint; only the body varies.
        # nosemgrep: dynamic-urllib-use-detected
        with urllib.request.urlopen(req, timeout=30) as resp:
            results = json.load(resp)["results"]
    except (OSError, TimeoutError, KeyError, json.JSONDecodeError) as e:
        log(f"OSV lookup failed: {e} — no advisory exemptions tonight")
        return {}
    found: dict[str, list[str]] = {}
    for (name, _), r in zip(pairs, results, strict=True):
        found.setdefault(name, []).extend(v["id"] for v in r.get("vulns", []))
    return {n: ids for n, ids in found.items() if ids}


def fix_notes(fixes: dict[str, list[str]], clean: list[Move]) -> list[str]:
    return [f"`{n}` {o} → {v} skipped the cooldown: {o} has {', '.join(fixes[n])}" for n, o, v in clean if n in fixes]


def lock_moves(path: str) -> tuple[list[Move], list[tuple[str, set[str]]]]:
    old_text = sh(["git", "show", f"HEAD:{path}"], capture=True).stdout
    new_text = (ROOT / path).read_text()
    return moves(versions(old_text), versions(new_text))


def only_metadata_changed(old_text: str, new_text: str) -> bool:
    """True when a lockfile's text moved but not one package pin did.

    `uv lock --exclude-newer` stamps the cutoff into uv.lock's `[options]` table
    even on a night that resolved nothing new. That stanza is provenance, not a
    dependency change — and the next plain `uv run` re-resolves and strips it
    back out — but while it sits there the lockfile differs from HEAD, which
    every downstream "did anything bump?" check reads as a real bump.
    """
    return new_text != old_text and versions(old_text) == versions(new_text)


def markdown_moves(clean: list[Move], unclean: list[tuple[str, set[str]]]) -> str:
    if not clean and not unclean:
        return "nothing to bump\n"
    lines = ["| package | old | new |", "|---|---|---|"]
    lines += [f"| {n} | {o} | {v} |" for n, o, v in clean]
    lines += [f"| {n} | — | {', '.join(sorted(vs))} |" for n, vs in unclean]
    return "\n".join(lines) + "\n"


def cmd_cooldown(args: argparse.Namespace) -> int:
    cutoff = datetime.now(timezone.utc) - timedelta(days=args.days)
    cargo_notes: list[str] = []
    last_young: list[str] = []
    fixes = advisories("crates.io", [(n, o) for n, o, _ in lock_moves("Cargo.lock")[0]])

    # Up to four passes: a precise revert shuffles transitive pins of its own,
    # and a dependent revert can uncover the next young crate above it.
    for _ in range(4):
        clean, unclean = lock_moves("Cargo.lock")
        if not clean and not unclean:
            break
        pairs = [(n, v) for n, _, v in clean] + [(n, v) for n, added in unclean for v in added]
        graph = deps((ROOT / "Cargo.lock").read_text())
        reverts, pinned, blockers = plan(clean, unclean, crates_io_ages(pairs), cutoff, graph, fixes)
        last_young = [f"`{n} {v}`" for n, _, v in reverts if n not in pinned]
        last_young += [f"`{n} {v}`" for n, v in blockers]
        if blockers:
            sh(["git", "checkout", "--", "Cargo.lock"])
            named = ", ".join(f"`{n} {v}`" for n, v in blockers)
            cargo_notes = [
                (
                    f"**Cargo bumps abandoned tonight**: {named} arrived inside the {args.days}-day "
                    "cooldown (or with unknown publish age), with no safe revert target and no bumped "
                    "dependent to withdraw it. Tomorrow's run retries."
                )
            ]
            break
        if not reverts:
            break
        for name, old_v, new_v in reverts:
            # Lockstep pairs (foo + foo_derive pinned `=`) refuse single-crate
            # reverts; reverting the parent drags the sibling back, so tolerate
            # individual failures — a later pass re-checks, and the for-else
            # abandons wholesale if anything young survives all of them.
            try:
                sh(["cargo", "update", "-p", f"{name}@{new_v}", "--precise", old_v])
            except subprocess.CalledProcessError:
                log(f"could not revert {name} {new_v} -> {old_v} individually — will re-check after this pass")
                continue
            why = pinned.get(name)
            reason = (
                "pins young " + ", ".join(f"`{p}`" for p in why)
                if why
                else f"published inside the {args.days}-day cooldown"
            )
            cargo_notes.append(f"reverted `{name}` {new_v} → {old_v}: {reason}")
    else:
        # Even reverting the dependents did not converge — a young crate is
        # held up by something the name-level graph cannot reach or cannot
        # move. The cooldown cannot be guaranteed piecemeal, so none of it ships.
        sh(["git", "checkout", "--", "Cargo.lock"])
        cargo_notes = [
            "**Cargo bumps abandoned tonight**: "
            + ", ".join(last_young)
            + f" stayed inside the {args.days}-day cooldown after reverts. Tomorrow's run retries."
        ]

    # An empty night has to *look* empty: the caller decides whether to open a PR
    # by asking git whether the lockfiles moved, so a leftover `[options]` stanza
    # would send it off to branch and commit a bump that does not exist.
    if only_metadata_changed(sh(["git", "show", "HEAD:uv.lock"], capture=True).stdout, (ROOT / "uv.lock").read_text()):
        sh(["git", "checkout", "--", "uv.lock"])
        log("uv.lock moved outside its package pins only — restored HEAD's copy")

    py_clean, py_unclean = lock_moves("uv.lock")
    cargo_clean, cargo_unclean = lock_moves("Cargo.lock")
    py_fixes = advisories("PyPI", [(n, o) for n, o, _ in py_clean])
    report = "\n".join(
        [
            f"## deps: nightly bump {datetime.now(timezone.utc):%Y-%m-%d}",
            "",
            "### Python (uv — cooldown enforced natively via pyproject's `exclude-newer`)",
            "",
            markdown_moves(py_clean, py_unclean),
            *(f"- {n}" for n in fix_notes(py_fixes, py_clean)),
            f"### Cargo (cooldown {args.days} days, enforced against crates.io publish dates)",
            "",
            markdown_moves(cargo_clean, cargo_unclean),
            *(f"- {n}" for n in fix_notes(fixes, cargo_clean) + cargo_notes),
            "",
        ]
    )
    out = Path(args.report)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(report)
    log(f"report written to {args.report}")
    return 0


def cmd_uv_fixes(args: argparse.Namespace) -> int:
    """Print an --exclude-newer-package flag for every uv.lock pin with an open advisory."""
    pins = sorted((n, v) for n, vs in versions((ROOT / "uv.lock").read_text()).items() for v in vs)
    now = f"{datetime.now(timezone.utc):%Y-%m-%dT%H:%M:%SZ}"
    for name in sorted(advisories("PyPI", pins)):
        print(f"--exclude-newer-package={name}={now}")
    return 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("cooldown", help="revert cargo bumps published inside the window; write the PR report")
    c.add_argument("--days", type=int, default=7)
    c.add_argument("--report", default="target/deps/report.md")
    c.set_defaults(fn=cmd_cooldown)
    u = sub.add_parser("uv-fixes", help="print uv flags exempting vulnerable pins from the cooldown")
    u.set_defaults(fn=cmd_uv_fixes)
    args = ap.parse_args(argv)
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())
