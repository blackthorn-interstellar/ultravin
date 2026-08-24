# Automated Monthly Data Refresh

NHTSA publishes a new standalone vPIC database roughly monthly
(`vPICList_lite_YYYY_MM.plain.zip`, observed day 9–18, kept online ~8 months,
occasionally re-touched after publication). `.github/workflows/data-refresh.yaml`
integrates each release with no human in the loop until PR review:

```
daily cron ──▶ detect ──▶ refresh (mechanical + gates) ──▶ PR: data/YYYY_MM
                              │ failure
                              ▼
                          Grok agent fixes on the runner ──▶ same PR
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

- **corpus** — every diverging re-frozen VIN is registered in
  `scripts/known_problems.json` (kind `deviation`), the single source of truth
  for the defects ultravin deliberately does not reproduce; the evidence for
  each is in `docs/KNOWN_DEVIATIONS.md`. `freeze.py` happily snapshots *any*
  current diff as the new baseline, so this gate is what stops a regression from
  being laundered into a green test suite. The re-freeze re-freezes the
  *existing* corpus VIN set (`--vins`), so the diff always reads "same VINs, new
  expectations" rather than a fresh sample replacing the curated one — plus
  every registered `deviation` VIN, which belongs in the corpus so its expected
  difference is frozen too (the known-problems gate only asks whether it still
  diverges, not whether it still diverges the *same way*).
- **sweep** — 500 freshly generated VINs decoded live against the new oracle,
  zero undocumented divergence.
- **stale-cache** — `scripts/stale_cache_cells.json`, regenerated from this
  month's dump by the same `vpic-import` pass, is internally consistent, did not
  gain implausibly many cells, and still faces an empty
  `vpic.wmiyearvalidchars_cacheexceptions`. This is the one documented deviation
  class enumerated by machine rather than by VIN: every `(wmi, year)` cell whose
  `WMIYearValidChars` contents contradict the recompute from the dump's own
  pattern rows, with the VIN positions they disagree at. The list *changing*
  month to month is the point, not a failure — it self-documents in the PR diff.
  What fails is a list that contradicts its own summary, a jump past 500 newly
  stale cells (upstream churn moves tens; a decoder charset regression re-lists
  thousands), or a non-empty cacheexceptions table, which would mean the proc no
  longer reads the cache the way the scan assumes. Policy: `docs/ACCEPTANCE.md`;
  evidence: `docs/KNOWN_DEVIATIONS.md#stale-wmiyearvalidchars-cache`.
- **known-problems** — the converse of the three above: every registered VIN
  still *reproduces* its documented problem. See below.
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
value changes, gate results, and follow-ups (e.g. a new oracle-crash VIN freeze
had to skip).

## Documented problems must keep reproducing

Every other gate uses `scripts/known_problems.json` to *excuse* an observation,
so nothing ever made an
excuse prove it was still true. Upstream fixes things: 2026_08 healed the
stale-year-cache deviation and the refresh stayed green on that axis, because the
gates only ever ask "is this divergence forgiven?". Worse, the 63 crash VINs land
in neither freeze's sample nor the 500-VIN sweep, so most months they were never
decoded at all.

`scripts/parity/known_problems.py` therefore decodes every listed VIN against the
new oracle and writes `target/refresh/known_problems.json`
(`{vin: {"outcome": "crash"|"diverged"|"exact"|"infra-error"}}`); the
**known-problems** gate requires each `oracle-crash` VIN to still crash and each
`deviation` VIN to still diverge. A stale excuse fails the run and the gate
detail names the VIN, what it does now, and its registry kind — retire the entry
from `scripts/known_problems.json`, the same house rule as the coverage gate
failing on a stale allowance.

A connection error is *not* evidence in either direction: it is reported as
`infra-error` and fails the gate as **unverifiable** rather than quietly
re-certifying the whole list, so an oracle outage can never be mistaken for
confirmation. A VIN with no record at all is treated the same way — the probe
writes its report in one go at the end, so a probe that dies (or an oracle it
could not connect to) leaves no file, the gate is handed nothing, and all of
them come back unverifiable rather than silently passing.

Row counts can't show an in-place label edit (2026_07 silently renamed
`bodystyle` 7 from `...(SUV)...` to `...[SUV]...` — 71 rows before and after),
so `run` also freezes every vpic table with ≤ 512 rows into `vpic/lookups.json`:
the rename becomes a one-line PR diff, and the report names each changed value
against the committed freeze.

## The agent path

If `refresh` fails — schema drift, a proc change, bad data, a new oracle
defect — the `fix` job runs xAI's Grok Build CLI (the
`.github/actions/grok-agent` composite action) on the same runner with the full
toolchain (rust, uv, docker) and a strict contract: never hand-edit generated
files, parity is the spec, a genuine upstream defect gets a
`scripts/known_problems.json` entry plus its evidence section in
`docs/KNOWN_DEVIATIONS.md`, done only when
`refresh.py run` exits 0 and `make check` is green. It pushes to the same
`data/YYYY_MM` branch and opens/updates the PR (label `agent-fixed`, or a draft
with `needs-human` + diagnosis if it can't get there honestly).

## The review gate

`data-review.yaml` publishes a `review-verdict` check run on the PR head —
a **required status check** on master, so nothing merges without it. It gates
the two branch prefixes the automation merges by itself, `data/*` and
`deps/*`; every other PR passes in seconds for $0. Data PRs whose diff is pure
regeneration (`vpic/**` + the corpus) pass a deterministic allowlist, also
$0; any data PR carrying code or doc changes — i.e. agent-fixed months — must
be approved by an adversarial Grok reviewer (read-only, verdict-only) that
checks diff scope, that every decoder edit is justified by an upstream `vpic/`
hunk, gate integrity (a new `scripts/known_problems.json` entry must name the
defective upstream artifact, not merely an output diff), and injection
artifacts. `deps/*` PRs — the
nightly lockfile bump, which merges itself — get a second adversarial
reviewer with no allowlist shortcut, judging scope, gate integrity,
supply-chain sanity of the lock diff (sources stay on crates.io/PyPI, new
packages are plausible transitive deps, moves match the bump report), and
injection artifacts; see `docs/NIGHTLY.md`. Whenever a model reviews, the
check run is opened `in_progress` before the review starts, so watchers see a
pending required check for the whole review rather than a window where it
does not exist yet. Human-created PRs trigger it via
`pull_request_target` (the gate always runs master's copy of itself and never
executes PR code); pipeline-created PRs emit no events, so the refresh/fix
jobs dispatch it explicitly. Uncertainty fails closed with findings posted as
a PR comment.

## Setup

| what | why |
|---|---|
| secret `XAI_API_KEY` | enables the agent fix job and the model leg of the review gate (without it, failures land in the job summary for a human) |
| var `DATA_REFRESH_AUTOMERGE=true` | merge **data-only** PRs: the workflow waits for the required checks (CI + `review-verdict`) and squash-merges synchronously, then tags `data-YYYY_MM` on the merge commit |
| var `DATA_REFRESH_AUTOMERGE_SCHEMA=true` | extend merging to **schema-change** PRs too, including agent-fixed ones — the full-autonomy switch; leave off to keep a human on the merge button when decoder code changed |
| var `DATA_REFRESH_AUTORELEASE=true` | after merging, also push the next patch `v` tag and dispatch `release.yaml` on it to ship PyPI |

No GitHub App and no PAT anywhere: `GITHUB_TOKEN` *events* never trigger
workflows, but explicit `workflow_dispatch` calls are exempt — so the pipeline
kicks `ci.yaml` and `data-review.yaml` itself after creating a PR, and the
merge/tag/release chain runs synchronously in-job
(`.github/actions/merge-data-pr`). One side effect: the merge commit lands on
master without a redundant master-push CI run — the identical tree was just
fully checked on the branch. If you merge a data PR by hand, tag the month
yourself (`git tag data-YYYY_MM && git push origin data-YYYY_MM`).

Defaults with zero setup: PRs are opened and checks run; merging, releasing,
and agent fixes are off.

**One GitHub setting is load-bearing:** "Allow GitHub Actions to create and
approve pull requests" (Settings → Actions → General → Workflow permissions)
must be enabled at **both** the org and repo level, or `gh pr create` from any
workflow 403s regardless of job permissions — the July 2026 live run hit
exactly this (the agent escalated via issue, as designed).

**Selfcheck:** `gh workflow run data-refresh-selfcheck.yaml` proves the agent's
prerequisites in under a minute with zero API spend — `detect` runs on a bare
runner, the environment mounts a valid `XAI_API_KEY`, and `GITHUB_TOKEN`
can create a PR (via a `[skip ci]` canary that is closed and deleted
immediately). Run it after changing the settings above or rotating the key.

## Failure playbook

- **build-data sha256 mismatch on an old PR/branch** — NHTSA re-touched the
  pinned month's file. The daily `detect` also catches this (`reason=reissue`,
  via URL `Last-Modified` vs the manifest's commit time) and opens a refresh PR
  for the *same* month with the new hash.
- **corpus/sweep gate names an unknown VIN** — either the decoder no longer
  matches the new data (fix Rust) or the dump itself is defective (oracle
  crash, stale cache table — precedent in `docs/KNOWN_DEVIATIONS.md`). The
  agent triages. The gates accept a diverging VIN from exactly two sources:
  registration in `scripts/known_problems.json`, which needs a named defective
  upstream artifact and not just the diff; or adjudication into the
  machine-enumerated stale-cache class, which needs the diff to touch only
  elements 142/143/144/156/191, land on a cell `scripts/stale_cache_cells.json`
  lists, and point at a position that cell is stale at. A VIN the gate names is
  one *neither* source covered — never widen either to make it go away.
- **stale-cache gate fails** — read the detail. `INCONSISTENT` means the
  committed list disagrees with the summary printed beside it: it was
  hand-edited or its halves came from different scans, so regenerate it rather
  than patch it. `REJECTED` names either a jump past 500 newly stale cells —
  confirm `answerkey verify` is green and read `target/refresh/stale_cache.json`
  before accepting a month that big, because that is the shape a decoder charset
  regression takes — or a non-empty `vpic.wmiyearvalidchars_cacheexceptions`,
  which is upstream changing how the proc reads the cache and needs a human to
  re-derive the scan's assumptions against `vpic/procs/spvindecode_errorcode.sql`.
- **known-problems gate names a healed VIN** — upstream fixed the defect (or the
  dump stopped carrying it). Confirm against §-evidence in
  `docs/KNOWN_DEVIATIONS.md`, retire the entry from
  `scripts/known_problems.json`, and retire the section if its last VIN went.
  Never re-green it by widening the registry.
  If instead the detail says **UNVERIFIABLE**, the oracle was unreachable and
  nothing was proven either way — fix the oracle and re-run.
- **known-problems gate says a deviation "changed shape"** — the VIN still
  diverges, but not the way `HEAD`'s corpus froze it, so the evidence in
  `docs/KNOWN_DEVIATIONS.md` now describes something other than what happens.
  Re-investigate it as a fresh divergence: if the upstream defect moved, update
  the entry *and* its evidence section deliberately; if ultravin's side moved,
  that is a decoder regression wearing an old excuse. Never re-freeze it away —
  the re-freeze is what proposes the new shape, not what justifies it.
- **freeze skips oracle-crash VINs** — reported as a follow-up, not a failure:
  those are upstream defects (malformed `Pattern.keys` regexes) and a crash has
  no answer to snapshot. `freeze.py` catches the `psycopg.Error`, records the
  skip and carries on, wherever the VIN came from — including the `--vins` list,
  which in the refresh path is the committed corpus plus every registered
  deviation (that path no longer samples a fresh corpus at all). A crash the
  *sweep* hits is the one that bites: it lands in `oracle_errors` and **fails**
  the sweep gate unless the VIN is registered as `oracle-crash`. Neither path
  produces a mechanical traceback; either way the agent documents the VIN per
  `KNOWN_DEVIATIONS.md` before it can go green.
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
