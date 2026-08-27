# Known scanner findings

An appendix for anyone reading an automated security report against this
repository. Every item below is something a SAST, secret, or SCA scanner
reliably flags, that we have looked at and accepted. Each entry says where it
is, what flags it, and why it is benign. Findings not on this list have not
been triaged — treat them as real.

Two framing facts do most of the work here. First, Ultravin ships as a Rust
library plus a Python wheel that embeds a data artifact; it opens no sockets,
spawns no processes, and reads no files at runtime. Second, everything under
`scripts/` and `docker-compose.yml` — the benchmark scripts, plus the `oracle-*`
and `campaign-*` Makefile targets that drive the containers they measure — is
developer tooling for reproducing NHTSA parity locally. None of it is published
to PyPI or crates.io, and none of it runs in CI against anything but disposable
containers. The published wheel contains `python/ultravin/` and the compiled
extension — nothing from `scripts/`.

The CI suite that produces most of these findings is
[`.github/workflows/security.yaml`](../.github/workflows/security.yaml).

## a. Hard-coded MSSQL password `Ultravin!2026`

`scripts/bench/mssql_setup.py:10` and `:23`, `scripts/bench/throughput.py:89`,
`docs/BENCHMARKS.md:202`. This repository's own secret scanners do **not** flag
it: gitleaks' default ruleset has no rule that matches, and TruffleHog runs
`--only-verified`, which an unverifiable fake credential can never satisfy
(`.gitleaks.toml` records why we chose not to allowlist these strings — doing so
would also mask a real leak that reused them). Commercial scanners matching on
entropy or on keywords like `PASSWORD=` will flag it. It is the SA password an
operator sets on a throwaway local SQL Server container started by hand to hold
a public NHTSA `.bak`; the value is a fixed literal because the `docker run`
line and the client have to agree on it, and no account anywhere accepts it.
One caveat worth stating plainly: unlike the Postgres pool, the documented
`docker run` publishes `-p 1433:1433` on all interfaces, not loopback — fine on
a laptop, worth changing to `127.0.0.1:1433:1433` on a shared host.
`throughput.py` prefers `ULTRAVIN_MSSQL_DSN` when it is set.

## b. Hard-coded Postgres credentials `postgres`/`postgres`

`docker-compose.yml:14`, `scripts/parity/oracle.py:16`,
`scripts/parity/campaign.py:87`. Same class of finding, same reason: this is
the parity oracle, a pool of five disposable Postgres containers loaded from
the public NHTSA dump. Every port is published to loopback only
(`127.0.0.1:55432:5432` through `55436`), the containers are configured with
durability disabled because they hold nothing worth keeping, and
`make oracle-load` rebuilds the whole thing from scratch in minutes.
`oracle.py` honours `ULTRAVIN_ORACLE_DSN` for anyone pointing it elsewhere.

## c. `pull_request_target` trigger

`.github/workflows/data-review.yaml:14`. Scanners (zizmor, CodeQL's Actions
pack, Snyk) flag `pull_request_target` because the classic failure mode is
checking out and executing the PR's code with the elevated token. This workflow
does the opposite: `actions/checkout` takes the base repo at `master` with no
`ref:` override, so it always runs the trusted copy of the gate — a PR cannot
edit its own judge — and it never executes PR code, only diffs it via `git` and
`gh`. PRs from forks are diverted to `human-review` before any credential is
mounted. The gate is split three ways so no job both judges untrusted content
and can write: the adversarial reviewer runs in a `contents: read`-only job with
no write token and no file-write or network tools (a hard Grok `--tools`
allowlist of `read_file,grep,list_dir,run_terminal_command`), working from a
pre-fetched head SHA and the PR body on disk rather than the API, and nothing
runs after it in that job. The `checks: write` token that publishes
`review-verdict` lives in a separate job that reads the verdict as data and
checks out nothing the reviewer produced — so a prompt-injected reviewer cannot
forge its own required check. The publish step fails closed: anything short of
an explicit approval leaves the required `review-verdict` check red.

## d. `pickle` load and dump

`scripts/parity/campaign.py:241` and `:250`
(`python.lang.security.deserialization.pickle.avoid-pickle`). This is a
coverage-fuzzer cache that the campaign script writes and reads back on the
operator's own machine, in `campaign/` — a gitignored runtime directory
(`.gitignore:120`). It is never transmitted, never committed, never shipped in
a package, and has no producer other than the same script that consumes it, so
there is no trust boundary for a malicious payload to cross. Both sites carried
`nosemgrep` annotations with this justification inline.

**Resolved** in `dba6ebe`: the checkpoint is now `campaign/coverage.json`, keyed by
blake2b digests instead of per-process-randomized `hash()`. Both `pickle` sites
and their `nosemgrep` annotations are gone; the rule no longer fires here.

## e. f-string-interpolated SQL

`scripts/bench/mssql_setup.py:53` and `:65`
(`formatted-sql-query`, `sqlalchemy-execute-raw-query`). These build
`RESTORE FILELISTONLY` and `RESTORE DATABASE ... MOVE` statements by string
formatting. The interpolated values are the operator's own `--bak` path and the
logical file names SQL Server itself just returned from `FILELISTONLY`; there is
no external input, no user, and no data at risk on a container that exists to be
thrown away. T-SQL `RESTORE` also does not accept bind parameters for these
clauses. Both sites carry `nosemgrep` annotations.

## f. `unsafe` in the Rust core

`crates/ultravin/src/db.rs` — eight `unsafe` sites: two `unsafe impl`
(`Send`/`Sync`, lines 80-81), an `unsafe fn` declaration (112), and five blocks
(92, 169, 198, 207, 221). The load path for **untrusted** artifacts
(`Db::from_bytes` → `Db::build`) validates before it trusts, through
`tables::validate_body`: checked `rkyv::access` proves layout and alignment,
`validate_arena` proves every interned arena slice is valid UTF-8, and
`check_element_ids` bounds element ids so a crafted artifact cannot drive a
huge index allocation. Only after all three pass does line 92 take the unchecked
pointer. The validation-skipping path is `unsafe fn build_trusted`, reachable
only from `embedded_raw()` for the blob compiled into the binary. That blob is
either our importer's output from the checkout (`data/vpic.rkyv`) or a file an
embedder names with `ULTRAVIN_DATA` at build time — and `build.rs` runs the same
`validate_body` proofs on that file before it becomes `include_bytes!`, so no
unvalidated bytes reach `build_trusted` by either route. The
`from_utf8_unchecked` at 221 sits on the hottest call in a decode and is covered
by the `validate_arena` proof; re-validating per call measured ~13% of decode
self-time. Line 198's `Mmap::map` and its documented TOCTOU caveat live behind
the non-default `external-data` cargo feature, which is not compiled into the
shipped Python wheel and has no in-repo caller.

## g. Piping a remote script to a shell

`Makefile:117` — `wget -qO- https://astral.sh/uv/install.sh | sh`, inside the
`install-uv` convenience target. Flagged as remote code execution / unpinned
supply chain. It is a developer convenience for bootstrapping `uv` on a fresh
checkout and runs only when someone invokes it interactively without `uv`
already installed. No workflow shells out to `make install-uv`, so it never runs
in CI: the five workflows that need `uv` (`ci`, `security`, `answer-key`,
`data-refresh`, `nightly`) install it through `astral-sh/setup-uv`, pinned to a
commit SHA.

*(Note: earlier internal notes described this line as `curl`-to-shell; the
Makefile uses `wget`. Same finding, same reasoning.)*

## h. CVEs in `psycopg[binary]`'s bundled libpq

`pyproject.toml:26`. SCA tools (pip-audit, Trivy, OSV, Snyk) periodically report
libpq or OpenSSL advisories against `psycopg-binary`, because its wheels bundle
their own copies of those libraries rather than linking the system ones, and the
bundled copies lag distro patching. The dependency is in the `dev` dependency
group, used only by the parity tooling to talk to the local oracle. It is not a
runtime dependency of the `ultravin` package — `[project].dependencies` is
`typer` alone — so it is absent from the published wheel and from any
environment that merely installs Ultravin.

## i. VINs in `tests/parity_corpus.json`

Flagged occasionally as PII or as leaked vehicle identifiers. The VINs are
synthetic. They are generated by `scripts/parity/generator.py`, which walks the
public vPIC WMI → VinSchema → Pattern tables and constructs 17-character strings
that satisfy a pattern's key constraints, fills the remaining positions with
constant filler (`A` for the VDS, `1` for the serial), and computes a correct
check digit. They correspond to no registration, no owner, and no
manufactured vehicle — they are pattern coordinates that happen to be
well-formed VINs.

## j. Snyk Open Source cannot scan this repository directly

`snyk test --all-projects` does not return a report here — it exits 3 with
`SNYK-CLI-0008`, "No supported files found". Snyk parses neither `uv.lock` nor
`Cargo.lock`, and this tree carries no `requirements.txt`, `setup.py`, or
`poetry.lock` for it to fall back on, so it finds nothing to analyse. Read that
error as a tooling gap, not a clean bill of health: it means Snyk never looked,
and a customer pointing it at the repo will hit the same exit code. Which is why
our own CI does not invoke it that way: the Snyk job first exports the locked
closure into a format Snyk does read,

```
uv export --frozen --no-hashes --no-emit-project --format requirements-txt \
  -o requirements-snyk.txt
```

then scans that file with `--file=requirements-snyk.txt --package-manager=pip`,
so Snyk's proprietary database does see every pinned Python dependency —
including the `psycopg` advisories described in (h). A customer wanting the same
coverage should export the same way. `--frozen` is the part that matters: an
unfrozen export re-resolves and drops `uv.lock`'s cooldown pin, quietly changing
what gets scanned.

The Rust closure has no Snyk equivalent at all (Snyk has no Cargo support), and
is instead covered by cargo-audit, cargo-deny, OSV-Scanner, and Trivy — four
scanners that do parse `Cargo.lock`, all of which hard-fail this repo's CI on a
known CVE.

## k. Snyk Code's 19 `scripts/`-only findings

Snyk's Git-integration dashboard scans without a severity threshold, so it
reported 19 SAST issues our CI never showed (the job gated at `high`). All 19
are under `scripts/`, and all 19 are the same category error: the rules assume
untrusted input where the only input is an operator's own command line.

- **SQL injection and the hard-coded password** (`scripts/bench/mssql_setup.py`)
  — the interpolated `--bak` path and the logical file names the server itself
  just reported, aimed at the throwaway local container of (a) and (e).
- **Deserialization of untrusted data** (`scripts/parity/campaign.py`) —
  `coverage.pkl` is the tool's own resume state, written and read by that one
  script in the gitignored `campaign/` dir. Already covered by (d), and since
  fixed outright: the checkpoint is now JSON with stable blake2b edge keys.
- **Path traversal** (`brutal.py`, `campaign.py`, `freeze.py`, `generator.py`,
  `sweep.py`, `refresh.py`) — every "tainted" path is an argparse flag the
  operator typed, except `refresh.py`'s two, which are `$GITHUB_OUTPUT` and
  `$GITHUB_STEP_SUMMARY` set by the Actions runner. Nothing downloaded from
  NHTSA reaches a path.

Rather than annotate 19 lines, `.snyk` excludes `scripts/**` from Snyk Code:
none of it is published (see the framing above), and it has no untrusted input
to protect. With the noise gone, both Snyk steps in `security.yaml` now gate at
`--severity-threshold=medium`, so a genuine medium finding in shipped code
fails CI instead of sitting on a dashboard. That job is ref-gated to `master`
— its `SNYK_TOKEN` must not share a runner with the `uv pip install` of an
untrusted lockfile — so it gates post-merge, not on the pull request.

## l. Codex cloud-scan batch, 2026-08-23 (five findings)

Codex's per-commit scans read one commit in isolation, so two of the five were
stale by the time they were triaged:

- **Deps re-wait lacks `actions: read`** — real when scanned, already fixed:
  the nightly deps-merge job was granted `actions: read` (commit `dadbb73`,
  PR #54) before the finding was read.
- **Deleted docs leave dangling policy references** — mostly false positive:
  `docs/ACCEPTANCE.md` was deleted in `7b70555` and restored the next day in
  `d21d0df`; every reference to it resolves. The one genuinely dangling
  reference (`docs/PLAN.md` in a `year.rs` doc comment) now points at
  `vpic/procs/spvindecode.sql` instead.
- **Parquet decode loads unused columns** — valid; fixed by applying a
  `ProjectionMask` in `FileState::open` so only the vin/year columns are
  decoded (autodetect sniffs one full-width batch, then re-reads projected).
- **Coverage gate can miss within-function region swaps** — accurate and
  already accepted: the count-keyed allowance design and its exact blind spot
  are annotated in `scripts/coverage.py` (see the docstring there). Spans are
  available in the coverage JSON; counts were chosen because line-keyed
  allowances would need re-baselining on most decode-path edits, and blind
  re-baselining launders regressions more easily than the count does.
- **Thread-local `PyString` cache crosses subinterpreters** — half right. The
  UB variant (per-interpreter GIL) is unreachable: the module does not declare
  `Py_mod_multiple_interpreters`, so CPython refuses the import (verified by
  execution). Legacy shared-GIL subinterpreters do import it and do share
  cached strings — an isolation-contract violation accepted knowingly, since
  the GIL serializes refcounting, the strings are immutable, and pyo3's own
  `intern!` shares strings process-wide the same way. The `META_CACHE`
  annotation in `crates/ultravin-py/src/lib.rs` records the full reasoning.

## m. Network access in `build.rs`

`crates/ultravin/build.rs` downloads the data artifact when a *released* crate
version is built with no artifact supplied (the default `download-data`
feature). Scanners flag build-script downloads as a supply-chain vector. Here
the fetch is HTTPS to this project's own GitHub release for the exact crate
version (`CARGO_PKG_REPOSITORY` + `v$CARGO_PKG_VERSION`), and the bytes are
never trusted on arrival: they must hash to the blake3 pinned inside the crate
(`data/manifest.json`, committed and released with the code) and then pass the
same `validate_body` proofs as any untrusted artifact before they become
`include_bytes!`. A mismatch fails the build. There is no download in the
repo's own builds (version 0.0.0 skips it, the checkout has `data/vpic.rkyv`)
nor on docs.rs; embedders who forbid build-time network set `ULTRAVIN_DATA` or
build with `--no-default-features`.
