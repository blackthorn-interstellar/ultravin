"""Nightly dependency bump with a publication cooldown (default 7 days).

Freshly published releases are where supply-chain compromises live; an
unattended nightly bump should never be the first installer. `uv lock
--exclude-newer` enforces the cooldown natively for Python, but cargo has no
equivalent, so `cooldown` post-processes a `cargo update`: every crate whose
new version was published inside the window is reverted to the version HEAD
pins (`cargo update -p crate@<new> --precise <old>`). Anything the rule cannot
decide — a crate new to the tree, a multi-version shuffle, a version crates.io
does not report — fails closed: Cargo.lock is restored wholesale and the bump
waits for a cleaner night.

The written report covers both ecosystems and becomes the PR body.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
import urllib.request
from datetime import datetime, timedelta, timezone
from pathlib import Path

import tomllib  # ty: ignore[unresolved-import]  (3.11+; CI-only tooling, never shipped to the 3.10 floor)

ROOT = Path(__file__).resolve().parent.parent

Move = tuple[str, str, str]  # (name, old_version, new_version)


def log(msg: str) -> None:
    print(f"[deps] {msg}", flush=True)


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


def _young(key: tuple[str, str], ages: dict[tuple[str, str], datetime | None], cutoff: datetime) -> bool:
    ts = ages.get(key)
    return ts is None or ts > cutoff  # unknown age fails closed


def plan(
    clean: list[Move],
    unclean: list[tuple[str, set[str]]],
    ages: dict[tuple[str, str], datetime | None],
    cutoff: datetime,
) -> tuple[list[Move], list[tuple[str, str]]]:
    """Decide reverts (young or unknown-age clean moves) and name the blockers.

    A blocker is an unclean arrival that is young or of unknown age: it cannot
    be individually reverted, so the whole Cargo.lock bump is abandoned tonight.
    """
    reverts = [m for m in clean if _young((m[0], m[2]), ages, cutoff)]
    blockers = [(name, v) for name, added in unclean for v in sorted(added) if _young((name, v), ages, cutoff)]
    return reverts, blockers


def crates_io_ages(pairs: list[tuple[str, str]]) -> dict[tuple[str, str], datetime | None]:
    """Publication times from crates.io, one request per crate name."""
    ages: dict[tuple[str, str], datetime | None] = {}
    for name in sorted({name for name, _ in pairs}):
        url = f"https://crates.io/api/v1/crates/{name}/versions"
        req = urllib.request.Request(url, headers={"User-Agent": "ultravin-nightly-deps (CI cooldown check)"})
        published: dict[str, str] = {}
        for attempt in (1, 2):
            try:
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


def lock_moves(path: str) -> tuple[list[Move], list[tuple[str, set[str]]]]:
    old_text = sh(["git", "show", f"HEAD:{path}"], capture=True).stdout
    new_text = (ROOT / path).read_text()
    return moves(versions(old_text), versions(new_text))


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

    # Up to two passes: a precise revert can itself shuffle transitive pins.
    for _ in range(2):
        clean, unclean = lock_moves("Cargo.lock")
        if not clean and not unclean:
            break
        pairs = [(n, v) for n, _, v in clean] + [(n, v) for n, added in unclean for v in added]
        reverts, blockers = plan(clean, unclean, crates_io_ages(pairs), cutoff)
        last_young = [f"`{n} {v}`" for n, _, v in reverts] + [f"`{n} {v}`" for n, v in blockers]
        if blockers:
            sh(["git", "checkout", "--", "Cargo.lock"])
            named = ", ".join(f"`{n} {v}`" for n, v in blockers)
            cargo_notes = [
                (
                    f"**Cargo bumps abandoned tonight**: {named} arrived inside the {args.days}-day "
                    "cooldown (or with unknown publish age) and has no safe revert target. "
                    "Tomorrow's run retries."
                )
            ]
            break
        if not reverts:
            break
        for name, old_v, new_v in reverts:
            # Lockstep pairs (foo + foo_derive pinned `=`) refuse single-crate
            # reverts; reverting the parent drags the sibling back, so tolerate
            # individual failures — pass 2 re-checks, and the for-else abandons
            # wholesale if anything young survives both passes.
            try:
                sh(["cargo", "update", "-p", f"{name}@{new_v}", "--precise", old_v])
            except subprocess.CalledProcessError:
                log(f"could not revert {name} {new_v} -> {old_v} individually — will re-check after this pass")
                continue
            cargo_notes.append(f"reverted `{name}` {new_v} → {old_v}: published inside the {args.days}-day cooldown")
    else:
        # Two passes did not converge — typically a young transitive dep whose
        # kept parent requires it (unrevertable without solving the graph).
        # The cooldown cannot be guaranteed piecemeal, so none of it ships.
        sh(["git", "checkout", "--", "Cargo.lock"])
        cargo_notes = [
            "**Cargo bumps abandoned tonight**: "
            + ", ".join(last_young)
            + f" stayed inside the {args.days}-day cooldown after reverts. Tomorrow's run retries."
        ]

    py_clean, py_unclean = lock_moves("uv.lock")
    cargo_clean, cargo_unclean = lock_moves("Cargo.lock")
    report = "\n".join(
        [
            f"## deps: nightly bump {datetime.now(timezone.utc):%Y-%m-%d}",
            "",
            f"### Python (uv — cooldown enforced natively via `--exclude-newer`, {args.days} days)",
            "",
            markdown_moves(py_clean, py_unclean),
            f"### Cargo (cooldown {args.days} days, enforced against crates.io publish dates)",
            "",
            markdown_moves(cargo_clean, cargo_unclean),
            *(f"- {n}" for n in cargo_notes),
            "",
        ]
    )
    out = Path(args.report)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(report)
    log(f"report written to {args.report}")
    return 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("cooldown", help="revert cargo bumps published inside the window; write the PR report")
    c.add_argument("--days", type=int, default=7)
    c.add_argument("--report", default="target/deps/report.md")
    c.set_defaults(fn=cmd_cooldown)
    args = ap.parse_args(argv)
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())
