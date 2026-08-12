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
mounted, and the model reviewer is confined to
`Read,Grep,Glob,Bash(git:*),Bash(gh pr view:*)` with `Edit`, `Write`,
`WebFetch`, and `WebSearch` explicitly denied. The publish step fails closed:
anything short of an explicit approval leaves the required `review-verdict`
check red.

## d. `pickle` load and dump

`scripts/parity/campaign.py:241` and `:250`
(`python.lang.security.deserialization.pickle.avoid-pickle`). This is a
coverage-fuzzer cache that the campaign script writes and reads back on the
operator's own machine, in `campaign/` — a gitignored runtime directory
(`.gitignore:120`). It is never transmitted, never committed, never shipped in
a package, and has no producer other than the same script that consumes it, so
there is no trust boundary for a malicious payload to cross. Both sites carry
`nosemgrep` annotations with this justification inline.

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

`crates/ultravin-core/src/db.rs` — eight `unsafe` sites: two `unsafe impl`
(`Send`/`Sync`, lines 84-85), an `unsafe fn` declaration (138), and five blocks
(118, 195, 224, 233, 247). The load path for **untrusted** artifacts
(`Db::from_bytes` → `Db::build`) validates before it trusts: checked
`rkyv::access` proves layout and alignment, `validate_arena` proves every
interned arena slice is valid UTF-8, and `check_element_ids` bounds element ids
so a crafted artifact cannot drive a huge index allocation. Only after all three
pass does line 118 take the unchecked pointer. The validation-skipping path is
`unsafe fn build_trusted`, reachable only from `embedded_raw()` for the blob
compiled into the binary — an embedder cannot reach it with their own file. The
`from_utf8_unchecked` at 247 sits on the hottest call in a decode and is covered
by the `validate_arena` proof; re-validating per call measured ~13% of decode
self-time. Line 224's `Mmap::map` and its documented TOCTOU caveat live behind
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
and a customer pointing it at the repo will hit the same exit code. To get a
real Python result, export the locked closure into a format Snyk understands —
`uv export --frozen --format requirements-txt` — and scan that; it resolves to
exactly what `uv.lock` pins, and it is how Snyk would surface the `psycopg`
advisories described in (h). The Rust closure has no Snyk equivalent at all
(Snyk has no Cargo support), and is instead covered by cargo-audit, cargo-deny,
OSV-Scanner, and Trivy — four scanners that do parse `Cargo.lock`, all of which
hard-fail this repo's CI on a known CVE.
