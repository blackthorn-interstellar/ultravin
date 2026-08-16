# Automated Monthly Data Refresh

NHTSA publishes a new standalone vPIC database roughly monthly
(`vPICList_lite_YYYY_MM.plain.zip`, observed day 9–18, kept online ~8 months,
occasionally re-touched after publication). `.github/workflows/data-refresh.yaml`
integrates each release with no human in the loop until PR review:

```
daily cron ──▶ detect ──▶ refresh (mechanical + gates) ──▶ PR: data/YYYY_MM
                              │ failure
                              ▼
                     report artifact + manual remediation
```

## The mechanical path

`scripts/refresh.py` is the whole pipeline and runs identically locally and in
CI (stdlib-only; everything heavy is a subprocess of the existing tools):

```bash
make refresh-detect            # is there a newer dump than vpic/manifest.json?
make refresh MONTH=2026_08     # integrate it (wipes + reloads the docker oracle)
```

`run` downloads the dump, rebuilds `vpic/` + the embedded artifact
(`vpic-import`), rebuilds the Python extension, cycles the docker Postgres
oracle onto the new dump, re-freezes `tests/parity_corpus.json`, and gates:

- **corpus** — every diverging re-frozen VIN is in `KNOWN_DEVIATION_VINS`
  (`scripts/refresh.py`), the machine-readable face of
  `docs/KNOWN_DEVIATIONS.md`. `freeze.py` happily snapshots *any* current diff
  as the new baseline, so this gate is what stops a regression from being
  laundered into a green test suite.
- **sweep** — 500 freshly generated VINs decoded live against the new oracle,
  zero undocumented divergence.
- **coverage** — `make coverage`: the decode path still reaches 100% of the
  regions a VIN can reach, and no allowance in
  `scripts/coverage_allowances.json` went **stale**. A stale one is the
  interesting failure: it means this month's data reaches a branch the previous
  month's could not. The allowances rest on facts about the dump — 0 `tobeqced`
  schemas, 0 models with two makes, no reversed character classes — and a month
  that breaks one of those assumptions lights up exactly that entry instead of
  passing silently.

- **pytest / cargo** — the offline suites.

The behavioural cover baked into the artifact is recomputed from the *new* dump
by `vpic-import`, so it is never carried over. What does not update itself is the
code that decides *what to look for*: the token grammar, the sweep dimensions and
the constructed candidate families. The coverage gate is what notices when the
new data outgrows them.

Exit 0 = integrated and green; exit 2 = gates failed (report still written to
`target/refresh/report.{md,json}`); exit 1 = mechanical breakage. The PR body
is the report: data-only vs schema-change classification (`git status` on
`vpic/schema` + `vpic/procs` — the importer strips volatile dump noise, so a
data-only month diffs clean there), manifest row/table/function deltas, lookup
value changes, gate results, and follow-ups (e.g. a healed known deviation).

Row counts can't show an in-place label edit (2026_07 silently renamed
`bodystyle` 7 from `...(SUV)...` to `...[SUV]...` — 71 rows before and after),
so `run` also freezes every vpic table with ≤ 512 rows into `vpic/lookups.json`:
the rename becomes a one-line PR diff, and the report names each changed value
against the committed freeze.

## Failure remediation

If `refresh` fails — schema drift, a proc change, bad data, or a new oracle
defect — the workflow uploads `target/refresh/` as the `data-refresh-report`
artifact for a trusted maintainer to inspect. Run `make refresh MONTH=YYYY_MM`
locally, follow the parity and known-deviation policies, and open or update the
`data/YYYY_MM` pull request after all gates pass. Refresh-derived reports, logs,
and generated files originate in the third-party NHTSA dump and must remain
untrusted input; they are never supplied to an autonomous agent with repository
credentials.

## The review gate

`data-review.yaml` publishes a `review-verdict` check run on the PR head —
a **required status check** on master, so nothing merges without it. Non-data
PRs pass in seconds; data PRs whose diff is pure regeneration (`vpic/**` + the
corpus) pass a deterministic allowlist for $0; any data PR carrying code or
doc changes must be approved by an adversarial
Claude reviewer (read-only, verdict-only) that checks diff scope, that every
decoder edit is justified by an upstream `vpic/` hunk, gate integrity (a new
`KNOWN_DEVIATION_VINS` entry needs documented evidence of an upstream defect),
and injection artifacts. Human-created PRs trigger it via
`pull_request_target` (the gate always runs master's copy of itself and never
executes PR code); pipeline-created PRs emit no events, so the refresh job
dispatches it explicitly. Uncertainty fails closed with findings posted as
a PR comment.

## Setup

| what | why |
|---|---|
| secret `ANTHROPIC_API_KEY` | enables the model leg of the read-only review gate |
| var `DATA_REFRESH_AUTOMERGE=true` | merge **data-only** PRs: the workflow waits for the required checks (CI + `review-verdict`) and squash-merges synchronously, then tags `data-YYYY_MM` on the merge commit |
| var `DATA_REFRESH_AUTOMERGE_SCHEMA=true` | extend merging to **schema-change** PRs too; leave off to keep a human on the merge button when decoder code changed |
| var `DATA_REFRESH_AUTORELEASE=true` | after merging, also push the next patch `v` tag and dispatch `release.yaml` on it to ship PyPI |

No GitHub App and no PAT anywhere: `GITHUB_TOKEN` *events* never trigger
workflows, but explicit `workflow_dispatch` calls are exempt — so the pipeline
kicks `ci.yaml` and `data-review.yaml` itself after creating a PR, and the
merge/tag/release chain runs synchronously in-job
(`.github/actions/merge-data-pr`). One side effect: the merge commit lands on
master without a redundant master-push CI run — the identical tree was just
fully checked on the branch. If you merge a data PR by hand, tag the month
yourself (`git tag data-YYYY_MM && git push origin data-YYYY_MM`).

Defaults with zero setup: PRs are opened and checks run; merging and releasing
are off, and failed refreshes require manual remediation.

**One GitHub setting is load-bearing:** "Allow GitHub Actions to create and
approve pull requests" (Settings → Actions → General → Workflow permissions)
must be enabled at **both** the org and repo level, or `gh pr create` from any
workflow 403s regardless of job permissions — the July 2026 live run hit
exactly this.

**Selfcheck:** `gh workflow run data-refresh-selfcheck.yaml` checks the optional
review service's prerequisites in under a minute with zero API spend — `detect`
runs on a bare runner, the environment mounts a valid `ANTHROPIC_API_KEY`, and
`GITHUB_TOKEN` can create a PR (via a `[skip ci]` canary that is closed and
deleted immediately). Run it after changing the settings above or rotating the
key.

## Failure playbook

- **build-data sha256 mismatch on an old PR/branch** — NHTSA re-touched the
  pinned month's file. The daily `detect` also catches this (`reason=reissue`,
  via URL `Last-Modified` vs the manifest's commit time) and opens a refresh PR
  for the *same* month with the new hash.
- **corpus/sweep gate names an unknown VIN** — either the decoder no longer
  matches the new data (fix Rust) or the dump itself is defective (oracle
  crash, stale cache table — precedent in `docs/KNOWN_DEVIATIONS.md`). A trusted
  maintainer triages; the gate only accepts VINs that are documented deviations.
- **freeze skips new oracle-crash VINs** — reported as a follow-up, not a
  failure: those are upstream defects (malformed `Pattern.keys` regexes). If a
  crash VIN lands inside freeze's *sampled* corpus or the sweep (not just the
  `--add-vins` list), the run fails mechanically with a psycopg traceback
  naming the VIN — document it per `KNOWN_DEVIATIONS.md` and exclude it during
  manual remediation.
- **re-issued pin detection** — `detect` compares the pinned URL's
  `Content-Length` against `dump_bytes` in the manifest (recorded since
  2026_07); a same-size re-issue is invisible to HEAD probes and surfaces as a
  build-data sha mismatch instead.
- **nothing detected for 2+ months** — `detect` warns (`stale=true`); NHTSA
  prunes files after ~8 months, so investigate before the window closes.
- **January runs** — the corpus records `oracle_current_year`; a fresh freeze
  always matches the current year, so the year-skip guard in
  `tests/test_parity_corpus.py` only bites stale corpora, not refreshed ones.

Manual override: `gh workflow run data-refresh.yaml -f month=2026_08` (also the
way to re-run after closing a failed PR — the daily cron won't stack a second
run while a `data/<month>` PR is open).
