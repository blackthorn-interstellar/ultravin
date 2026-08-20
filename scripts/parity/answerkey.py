"""Freeze the oracle's answers once a month so every later test can skip it.

The oracle is the source of truth but it is slow (~35 VIN/s stock) and needs
Docker and an 11M-row load. That is fine once a month and hopeless on every
commit — so once a month we ask it for its answer to a large, data-derived VIN
corpus and keep a hash of each. From then until the next release, `verify` checks
ultravin against those hashes with no Postgres anywhere.

The corpus must come from NHTSA's data, never from ultravin's own behaviour.
`ultravin.seeded()` qualifies: it enumerates the rules in the dump and the
character classes those rules distinguish. `cover_vins()` does **not** — it is
chosen by decoding with ultravin and covering ultravin's own notion of what
matters, so as an answer key it could only ever confirm what we already thought
to test.

Hashes are taken over the *canonical* response: `spvindecode`'s final ORDER BY
does not determine the order of rows inside a group, so the raw rows genuinely
vary between runs and a naive hash would be flaky. `normalize` already excludes
that ordering.

    make oracle-up && make oracle-load DUMP=downloads/vPICList_lite_2026_07.plain.zip
    uv run -- python -m scripts.parity.answerkey build --out target/answerkey.jsonl
    uv run -- python -m scripts.parity.answerkey verify --key target/answerkey.jsonl
"""

from __future__ import annotations

import hashlib
import json
import multiprocessing as mp
import subprocess
import time
from collections import Counter
from pathlib import Path
from typing import Any

import typer
import ultravin

from scripts.parity import normalize, oracle
from scripts.refresh import KNOWN_DEVIATION_VINS, ORACLE_CRASH_VINS

REPO = Path(__file__).resolve().parents[2]
MANIFEST = REPO / "vpic" / "manifest.json"
PIN = REPO / "tests" / "answerkey.json"
# The oracle is defective on every VIN in scripts/known_problems.json and ultravin
# is deliberately more correct (docs/KNOWN_DEVIATIONS.md), whether the oracle
# crashed or merely answered wrongly — the key records those VINs, it does not
# compare them. Both kinds, because a key built here must not re-freeze either.
KNOWN_DEVIATIONS = ORACLE_CRASH_VINS | KNOWN_DEVIATION_VINS

app = typer.Typer(add_completion=False, help="Build and check the frozen oracle answer key.")


def canonical_hash(rows: list[dict[str, Any]]) -> str:
    """A stable digest of one decode, order-insensitive within a group."""
    norm = normalize.collation_agnostic([normalize.from_oracle(r) for r in rows])
    flat = sorted(json.dumps(r, sort_keys=True, default=str) for r in norm)
    return hashlib.blake2b("\n".join(flat).encode(), digest_size=8).hexdigest()


def ultravin_hashes(vins: list[str]) -> list[str]:
    """The same digest shape, computed from ultravin's own answers.

    Batched: `decode` one VIN at a time spends most of its time crossing the
    Python boundary, and the key is millions of rows long.
    """
    results: Any = ultravin.decode_batch(vins)
    out = []
    for result in results:
        rows = normalize.collation_agnostic(normalize.ultravin_rows(result))
        flat = sorted(json.dumps(r, sort_keys=True, default=str) for r in rows)
        out.append(hashlib.blake2b("\n".join(flat).encode(), digest_size=8).hexdigest())
    return out


def _check_chunk(chunk: list[tuple[str, str]]) -> list[str]:
    """VINs in this chunk whose answer differs from the frozen one."""
    vins = [v for v, _ in chunk]
    return [v for (v, expected), got in zip(chunk, ultravin_hashes(vins), strict=True) if got != expected]


_conn: Any = None


def _init() -> None:
    global _conn
    _conn = oracle.connect()


def _ask(vin: str) -> tuple[str, str]:
    """The oracle's answer for one VIN. An oracle crash is recorded, not raised —
    it means the reference implementation cannot answer, which is itself the fact
    worth freezing."""
    try:
        return vin, canonical_hash(oracle.decode(_conn, vin))
    except Exception as e:  # noqa: BLE001 — a VIN the oracle dies on must not stop the build
        return vin, f"!{type(e).__name__}"


def sample_selected(vin: str, mod: int) -> bool:
    """Whether this VIN falls in the stable 1/mod sample of the corpus.

    A STABLE hash, deliberately not Python's built-in hash(): str hashing is
    randomized per process by PYTHONHASHSEED, and the stock and fast keys the
    equivalence gate compares are frozen in separate jobs — a per-process hash
    would pick different VINs on each side and make `compare` fail spuriously.
    """
    return int.from_bytes(hashlib.blake2b(vin.encode(), digest_size=8).digest(), "big") % mod == 0


def corpus(limit: int, shard: int, shards: int, sample_mod: int = 0) -> list[str]:
    """The data-derived corpus, optionally one shard of it or a stable sample.

    Sharding is by position, so N shards partition the corpus exactly and each
    can be built by a separate machine against its own oracle. `sample_mod` is a
    different cut, for the equivalence gate: the stable-hash-selected 1/N of the
    whole corpus, spread across every manufacturer. A contiguous stride aliases
    with the schema-grouped corpus and permanently misses ~half the makes; the
    hash sample does not. When set it takes precedence over shard/shards.
    """
    vins = ultravin.seeded(limit=limit)
    if sample_mod > 0:
        return [v for v in vins if sample_selected(v, sample_mod)]
    return vins[shard::shards] if shards > 1 else vins


@app.command()
def build(
    out: str = typer.Option(..., help="where to write the key (JSONL)"),
    limit: int = typer.Option(
        0,
        help="stop after roughly N VINs (0 = all ~1.8M). Cuts at a schema boundary, so it is "
        "the first whole schemas by id — a plumbing smoke test, not a representative sample.",
    ),
    shard: int = typer.Option(0, help="build only shard i of n"),
    shards: int = typer.Option(1, help="total shards"),
    sample_mod: int = typer.Option(
        0,
        help="instead of a shard slice, freeze the stable-hash 1/N sample of the whole corpus "
        "(0 = off). Spread across every manufacturer; takes precedence over --shard/--shards.",
    ),
    workers: int = typer.Option(4, help="oracle connections; it saturates around 4 per 10 cores"),
) -> None:
    """Ask the oracle for its answer to every VIN in the corpus and record it."""
    vins = corpus(limit, shard, shards, sample_mod)
    where = f"1/{sample_mod} hash sample" if sample_mod else f"shard {shard + 1}/{shards}"
    typer.echo(f"corpus: {len(vins):,} VINs ({where})", err=True)

    manifest = json.loads(MANIFEST.read_text())
    started = time.time()
    path = Path(out)
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w") as fh, mp.Pool(workers, initializer=_init) as pool:
        fh.write(
            json.dumps(
                {
                    "_about": "Frozen oracle answers. Each line is [vin, hash of the canonical response].",
                    "month": manifest["month"],
                    "dump_sha256": manifest["dump_sha256"],
                    "artifact_blake3": manifest["artifact_blake3"],
                    "shard": shard,
                    "shards": shards,
                    "sample_mod": sample_mod,
                    "count": len(vins),
                }
            )
            + "\n"
        )
        for done, (vin, digest) in enumerate(pool.imap(_ask, vins, chunksize=16), start=1):
            fh.write(json.dumps([vin, digest]) + "\n")
            if done % 20_000 == 0:
                rate = done / (time.time() - started)
                typer.echo(f"  {done:,}/{len(vins):,} at {rate:.0f} VIN/s", err=True)
    unanswered = sum(1 for _, h in read_key(path)[1] if h.startswith("!"))
    share = unanswered / max(len(vins), 1)
    typer.echo(
        f"wrote {path} in {(time.time() - started) / 60:.1f} min "
        f"({len(vins) - unanswered:,} answered, {unanswered:,} not)",
        err=True,
    )
    # A handful of VINs genuinely kill the oracle (see KNOWN_DEVIATIONS). A large
    # share means the oracle fell over — disk, a dropped connection — and the key
    # is mostly holes. Publishing that would be worse than publishing nothing,
    # because `verify` would pass on the strength of what little it could compare.
    if share > 0.01:
        typer.echo(f"{share:.1%} of this shard has no oracle answer — refusing it", err=True)
        raise typer.Exit(1)


def read_key(path: Path) -> tuple[dict[str, Any], list[tuple[str, str]]]:
    """Headers and answers out of a key file.

    A published key is the shards concatenated, so headers appear throughout it,
    not just on line one. Objects are headers, arrays are answers.
    """
    header: dict[str, Any] = {}
    entries: list[tuple[str, str]] = []
    with path.open() as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            row = json.loads(line)
            if isinstance(row, dict):
                if header and (row["month"], row["artifact_blake3"]) != (
                    header["month"],
                    header["artifact_blake3"],
                ):
                    msg = f"{path.name} mixes shards from different builds"
                    raise ValueError(msg)
                header = row
            else:
                entries.append((row[0], row[1]))
    return header, entries


@app.command()
def verify(
    key: str = typer.Option(..., help="key file (JSONL), or a directory of shards"),
    strict_artifact: bool = typer.Option(True, help="fail when the key was built against a different artifact"),
    workers: int = typer.Option(0, help="parallel checkers (0 = one per core)"),
) -> None:
    """Check ultravin against the frozen answers. No oracle, no Docker."""
    workers = workers or mp.cpu_count()
    path = Path(key)
    files = sorted(path.glob("*.jsonl")) if path.is_dir() else [path]
    manifest = json.loads(MANIFEST.read_text())

    entries: list[tuple[str, str]] = []
    for f in files:
        header, part = read_key(f)
        if header["month"] != manifest["month"]:
            typer.echo(f"key is for {header['month']}, data is {manifest['month']}", err=True)
            raise typer.Exit(2)
        if strict_artifact and header["artifact_blake3"] != manifest["artifact_blake3"]:
            typer.echo("key was built against a different artifact — rebuild it or pass --no-strict-artifact", err=True)
            raise typer.Exit(2)
        entries.extend(part)

    started = time.time()
    oracle_failures = sum(1 for _, h in entries if h.startswith("!"))
    comparable = [(v, h) for v, h in entries if v not in KNOWN_DEVIATIONS and not h.startswith("!")]
    chunks = [comparable[i : i + 20_000] for i in range(0, len(comparable), 20_000)]

    mismatches: list[str] = []
    if workers > 1 and len(chunks) > 1:
        with mp.Pool(workers) as pool:
            for bad in pool.imap_unordered(_check_chunk, chunks):
                mismatches.extend(bad)
    else:
        for chunk in chunks:
            mismatches.extend(_check_chunk(chunk))
    mismatches.sort()
    elapsed = time.time() - started
    typer.echo(
        f"{len(entries):,} frozen answers checked in {elapsed:.1f}s ({len(entries) / max(elapsed, 1e-9):,.0f} VIN/s)"
    )
    if oracle_failures:
        share = oracle_failures / max(len(entries), 1)
        typer.echo(f"{oracle_failures:,} ({share:.1%}) the oracle itself could not answer, so not compared")
        if share > 0.01:
            typer.echo(
                f"this key is {share:.0%} holes — it was built against a failing oracle and proves little",
                err=True,
            )
            raise typer.Exit(2)
    if mismatches:
        typer.echo(f"{len(mismatches):,} MISMATCH against the oracle's frozen answer:", err=True)
        for vin in mismatches[:20]:
            typer.echo(f"  {vin}", err=True)
        raise typer.Exit(1)
    typer.echo("every answer matches")


def _duplicate_vins(entries: list[tuple[str, str]]) -> list[str]:
    """VINs that appear more than once (a malformed key: dict() would hide the loss)."""
    counts = Counter(vin for vin, _ in entries)
    return sorted(vin for vin, n in counts.items() if n > 1)


@app.command()
def compare(
    a: str = typer.Option(..., help="one key"),
    b: str = typer.Option(..., help="the other"),
) -> None:
    """Assert two keys agree, VIN for VIN.

    Used to hold the rewritten stored procedures to account: the same slice is
    frozen from the dump's procedures untouched and from the rewrite, and any
    difference means the rewrite is not the equivalence it claims to be — on this
    month's data, not on the sample someone measured once.

    Fail-closed. Both keys cover the same corpus slice by construction, so a
    duplicate VIN (a malformed key) or differing VIN sets (the rewrite dropped or
    added rows) is itself a failure — not something to collapse with dict() or
    print and pass. Exit 1 means the two genuinely disagree (a VIN's hash differs,
    or one side is missing VINs the other has); exit 2 means the comparison could
    not be trusted (a malformed key, or no VINs in common at all).
    """
    _, left = read_key(Path(a))
    _, right = read_key(Path(b))

    for side, entries in (("a", left), ("b", right)):
        dups = _duplicate_vins(entries)
        if dups:
            typer.echo(f"key {side} has {len(dups):,} duplicate VIN(s): {dups[:20]}", err=True)
            raise typer.Exit(2)

    lmap, rmap = dict(left), dict(right)
    only_a = sorted(lmap.keys() - rmap.keys())
    only_b = sorted(rmap.keys() - lmap.keys())
    shared = lmap.keys() & rmap.keys()
    if not shared:
        typer.echo("the two keys share no VINs — nothing was compared", err=True)
        raise typer.Exit(2)
    if only_a or only_b:
        typer.echo(f"the keys cover different VINs: {len(only_a):,} only in a, {len(only_b):,} only in b", err=True)
        if only_a:
            typer.echo(f"  only in a: {only_a[:10]}", err=True)
        if only_b:
            typer.echo(f"  only in b: {only_b[:10]}", err=True)
        raise typer.Exit(1)

    differing = sorted(v for v in shared if lmap[v] != rmap[v])
    typer.echo(f"{len(shared):,} VINs in both; {len(differing):,} differ")
    if differing:
        for vin in differing[:20]:
            typer.echo(f"  {vin}: {lmap[vin]} != {rmap[vin]}", err=True)
        raise typer.Exit(1)
    typer.echo("the two agree on every VIN")


@app.command()
def fetch(
    dest: str = typer.Option("target/answerkey", help="where to put the key"),
    tag: str = typer.Option("", help="release tag (default: data-<month> from the pin)"),
) -> None:
    """Download the published key named by `tests/answerkey.json` and check it.

    The key is tens of megabytes and changes monthly, so it lives on the release
    rather than in git; what git holds is the pin. A download that does not match
    the pinned checksum is refused — an answer key you cannot trust the integrity
    of is worse than none.
    """
    if not PIN.exists():
        typer.echo(f"no pin at {PIN} — nothing has been published yet", err=True)
        raise typer.Exit(2)
    pinned = json.loads(PIN.read_text())
    manifest = json.loads(MANIFEST.read_text())
    if pinned["month"] != manifest["month"]:
        typer.echo(f"pin is for {pinned['month']}, data is {manifest['month']} — rebuild the key", err=True)
        raise typer.Exit(2)

    out = Path(dest)
    out.mkdir(parents=True, exist_ok=True)
    asset = out / pinned["asset"]
    release = tag or f"data-{pinned['month']}"
    typer.echo(f"fetching {pinned['asset']} from {release}", err=True)
    proc = subprocess.run(
        ["gh", "release", "download", release, "--pattern", pinned["asset"], "--dir", str(out), "--clobber"],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        typer.echo(f"gh release download failed:\n{proc.stderr[-800:]}", err=True)
        raise typer.Exit(1)

    digest = hashlib.sha256(asset.read_bytes()).hexdigest()
    if digest != pinned["sha256"]:
        typer.echo(f"checksum mismatch: got {digest}, pinned {pinned['sha256']}", err=True)
        raise typer.Exit(1)
    if asset.suffix == ".zst":
        subprocess.run(["zstd", "-df", str(asset), "-o", str(asset.with_suffix(""))], check=True)
    typer.echo(f"{pinned['count']:,} answers ready in {out}")


@app.command()
def pin(
    key: str = typer.Option(..., help="the published key asset"),
    sha256: str = typer.Option(..., help="its checksum, as published"),
    count: int = typer.Option(..., help="how many answers it holds"),
) -> None:
    """Record where the key lives and what it should hash to.

    The key itself is tens of megabytes and changes every month, so it lives on
    the release rather than in git. What git keeps is this pin: enough to fetch
    the right file, prove it arrived intact, and refuse a key built for another
    month's data.
    """
    manifest = json.loads(MANIFEST.read_text())
    PIN.write_text(
        json.dumps(
            {
                "_about": "Pointer to the frozen oracle answer key. `make answerkey-verify` fetches and checks it.",
                "month": manifest["month"],
                "dump_sha256": manifest["dump_sha256"],
                "artifact_blake3": manifest["artifact_blake3"],
                "asset": Path(key).name,
                "sha256": sha256,
                "count": count,
            },
            indent=2,
        )
        + "\n"
    )
    typer.echo(f"pinned {Path(key).name} ({count:,} answers) in {PIN.name}")


if __name__ == "__main__":
    app()
