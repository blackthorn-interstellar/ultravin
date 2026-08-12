# Known scanner findings

An appendix for anyone reading an automated security report against this
repository. Every item below is something a SAST, secret, or SCA scanner
reliably flags, that we have looked at and accepted. Each entry says where it
is, what flags it, and why it is benign. Findings not on this list have not
been triaged — treat them as real.

Two framing facts do most of the work here. First, Ultravin ships as a Rust
library plus a Python wheel that embeds a data artifact; it opens no sockets,
spawns no processes, and reads no files at runtime. Second, everything under
`scripts/`, `docker-compose.yml`, and the `bench`/`oracle`/`campaign` targets is
developer tooling for reproducing NHTSA parity locally. None of it is published
to PyPI or crates.io, and none of it runs in CI against anything but disposable
containers. The published wheel contains `python/ultravin/` and the compiled
extension — nothing from `scripts/`.

The CI suite that produces most of these findings is
[`.github/workflows/security.yaml`](../.github/workflows/security.yaml).

## a. Hard-coded MSSQL password `Ultravin!2026`

`scripts/bench/mssql_setup.py:10` and `:23`, `scripts/bench/throughput.py:89`,
`docs/BENCHMARKS.md:202`. Flagged by gitleaks, TruffleHog, Trivy secret mode,
and most commercial secret scanners as a hard-coded credential. It is the SA
password an operator sets on a throwaway local `azure-sql-edge` container
started by hand to hold a public NHTSA `.bak`, on a port bound to their own
machine, so the value is a fixed literal on purpose — it is a magic constant
that has to match between the `docker run` line and the client, not a secret.
No account anywhere accepts it. `throughput.py` reads `ULTRAVIN_MSSQL_DSN`
first, so a real deployment never uses the literal.

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
already installed. CI never uses it: every workflow installs `uv` through
`astral-sh/setup-uv@v6`, a pinned action.

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
