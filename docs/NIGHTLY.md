# Nightly Autonomous Maintenance

`.github/workflows/nightly.yaml` (08:41 UTC daily, or `gh workflow run
nightly.yaml`) runs two independent lanes. Both follow the data-refresh
pattern: a deterministic mechanical path first, a Grok agent only when there
is real work, delivery as a PR through the same CI + `review-verdict` checks,
and explicit dispatch of those checks (workflow-created PRs emit no events).

```
deps    lockfile bump (7-day cooldown) ──▶ make check ──▶ deps-publish opens the PR
                                              │ broken
                                              ▼
                                        deps-fix (agent, banks a patch)
                                              ──▶ deps-fix-publish opens the PR

        whoever delivered it, deps-watch (no token) then waits for its checks
        (ci + data-review + the Security scan):
          green           ──▶ deps-merge: trusted-diff guard, MERGE (end to end)
          review rejected ──▶ deps-merge: label needs-human, stop
          Security red    ──▶ deps-merge: label needs-human, stop
          red             ──▶ deps-remedy (agent, ONE attempt, banks a patch)
                              ──▶ deps-remedy-publish pushes + re-kicks
                              ──▶ deps-merge waits again: green MERGES,
                                  still red → label needs-human, end red

fixes   covfuzz probe tops up backlog ──▶ fixes (agent) drains ONE cluster and
                                          banks a patch ──▶ fixes-publish opens
                                          the PR and kicks its checks (human merges)
```

## Token discipline

No job both runs untrusted code and holds a write token, and step-scoping is
not treated as a boundary: same-VM code can read another process's env. Every
job that builds just-bumped dependencies, reads their CI logs, loads the NHTSA
dump, or hosts an agent is **keyless** — `contents: read` at most, checked out
with `persist-credentials: false`, no `GITHUB_TOKEN` write scope anywhere on
the VM. Such a job delivers by **committing locally** and banking `git diff
--binary` as an artifact.

Each has a matching `*-publish` job that runs no repository code and resolves
no local actions (`git` and `gh` only), holds the write token, and replays the
patch. Before pushing, every publish job refuses a staged path under
`.github/` or `Makefile` (`--no-renames`, so a rename cannot hide a deletion):
the machinery is not self-editable, and that refusal is what lets a later
privileged job check the branch out safely. The agents' transcripts are still
uploaded as 30-day artifacts — with the write token gone, the only secret in
reach is the Anthropic key.

## The deps lane

`uv lock --upgrade` and `cargo update`, both under a **7-day publication
cooldown**: an unattended nightly bump must never be the first installer of a
freshly published (possibly freshly compromised) release. uv enforces this
natively (`--exclude-newer`); cargo has no equivalent, so
`scripts/bump_deps.py cooldown` post-processes the lockfile against crates.io
publish dates and reverts young arrivals (`--precise` back to HEAD's pin).
Anything it cannot revert safely — a crate new to the tree, a young transitive
dep whose kept parent requires it — abandons the whole Cargo.lock bump for the
night, by name, in the report. Fail closed; tomorrow retries.

A green bump becomes a lockfile-only PR on the rolling `deps/nightly` branch
(body = the bump report). A broken bump banks the evidence (bumped lockfiles,
`check.log`, report) and hands it to the agent, whose contract is: fix the
code for the new versions (or pin a genuinely broken package back, justified),
and commit even when stuck — `deps-fix-publish` opens the PR either way, with
the agent's own explanation in the body, and an empty session fails the run
loudly instead of ending green and silent. The agent has
**rule judgment**: a new lint/type rule arriving with a tool bump is a
proposal, not law — it conforms where the rule genuinely improves the
codebase and config-disables it (justified in the PR body) where it is churn.
The codebase is not a hostage to whatever ruff rolls out. What it may never
do is weaken an existing check or edit the automation itself (`.github/**`,
`Makefile` — such diffs are refused at publish, and again at merge).

The PR is also **content-reviewed**: `data-review.yaml` gates `deps/*` the
same way it gates `data/*`, with an adversarial read-only Grok reviewer and
no allowlist shortcut, publishing the required `review-verdict` check. It
approves only a diff that is lockfiles (plus fallout fixes the PR body
justifies), leaves every existing test/lint/type/parity gate intact, and
whose lock diff is supply-chain sane: sources stay on crates.io and PyPI, new
packages are plausible transitive dependencies, version moves match the bump
report. Nothing in the diff or report may read like an instruction to the
automation. Uncertainty is a rejection.

It is also **scanned**: `security.yaml`'s 15 jobs run on bot PRs and gate the
deps merge. GitHub files their `pull_request` run as `action_required`, and an
unapproved run executes nothing and contributes no check runs — so it would be
invisible rather than red. Each publish job therefore polls (12 × 10s) until a
Security run on the head SHA is actually moving, and `deps-merge` requires one
to exist and have completed before it judges. A remediated SHA gets an explicit
`gh workflow run security.yaml --ref deps/nightly`: only the publish jobs that
*open* the PR get a scan for free, and a GITHUB_TOKEN push fires nothing.

Whoever delivered the PR, `deps-watch` then waits for the dispatched runs and
classifies the outcome — it holds no token and checks nothing out, so a red
branch cannot reach anything privileged from there. If checks failed
mechanically, `deps-remedy` gets **one** keyless attempt on the branch: read
the failing log (`gh run view --log-failed`), fix, verify with a local `make
check`, commit. `deps-remedy-publish` replays that patch onto `deps/nightly`,
pushes, and re-kicks the checks; `deps-merge` waits for them once more and
merges on green. That second wait is **pinned to the SHA the publish job
pushed** — it waits until the re-dispatched `ci`, `data-review` **and
`security`** runs exist and have completed on that commit before judging its
check runs, because `gh pr checks` resolves the PR head lazily and would
otherwise read the pre-remediation SHA's already-concluded reds. A SHA that
never gets a Security run never merges — the wait simply times out. Every
timeout in it counts as red: it may block a merge on doubt, never wave one
through. Still red after
that one attempt — the agent never sees the re-run — labels the PR
`needs-human` and ends the run red; the agent's diagnosis is in its commit
message and the full session in the `nightly-deps-remedy-transcript` artifact.
The old fix→push→watch→repeat loop is gone with the agent's token: one attempt
per night, local `make check` as its finish line.

That agent handles **mechanical** failures only. If the failing check is
`review-verdict`, no agent runs at all: `deps-merge` labels the PR
`needs-human` and comments why. The reviewer's verdict is a human's call,
never something for the pipeline to work around.

The same holds for the scanners. When `ci` and `data-review` are green on the
head SHA and only `security.yaml` is red, `deps-watch` classifies the night as
needs-human and `deps-merge` labels it, naming the failed scanner jobs. A
Security run that is red *alongside* a mechanical failure changes no routing —
remediation proceeds, and the re-wait re-judges the scan on the remediated SHA.

Whenever the lane delivered a PR and then ended without merging it — for any
reason, including the ones that skip `deps-merge` entirely (the agent banked
nothing, the branch moved under it, the automation guard refused its patch,
the first wait timed out) — `deps-needs-human` labels the PR `needs-human` and
comments with a link to the failing run. **An open `deps/nightly` PR without
that label always means the night is still in progress or ended merged.**

Once CI is green **and** the review approved **and** the Security scan passed,
`deps-merge` **merges the PR itself** — mechanical or agent-fixed alike, end
to end with no human in the path. That job runs no repository code and reads
no CI logs, which is what lets it hold the write token. The single refusal
beyond the two verdicts: a diff touching `.github/**` or the `Makefile` fails
the merge step for a human, because the machinery must not be able to edit its
own judges.

## The fixes lane

`tests/parity_backlog.jsonl` is the work queue: VINs where ultravin and the
Postgres vPIC oracle disagreed, logged by the parity campaign
(`scripts/parity/campaign.py`). It was seeded from the local campaign's finds
(the systematic engine ran to completion: 5.46M VINs, 48 model years) and is
topped up every night in CI by a 15-minute covfuzz probe chunk against its
own **fast-procs** copy of the pinned-dump oracle — isolated so probe load
can never kill the byte-faithful oracle the agent verifies against (the first
live probe's temp-table churn filled the stock oracle's tmpfs and PANICked
it). Dead-oracle connection errors abort the probe and are filtered out of
the queue, never enqueued. Coverage state resumes via actions/cache; a
decoder change reopens coverage, so the probe never permanently retires.

Each night the agent gets a loaded oracle and a built extension, and resolves
**one root-cause cluster**: reproduce first (stale entries are deleted — that
alone is a valid delivery), then either fix the decoder or document a genuine
oracle/dump defect in `scripts/known_problems.json` + its `docs/KNOWN_DEVIATIONS.md` section.
Fixed clusters leave a representative VIN in `tests/brutal_repros.json`, which
the monthly refresh freezes into the parity corpus as a permanent regression.
The finish line is machine-checked and **local**: `scripts.parity.sweep
--cases` over the resolved VINs must show zero undocumented divergence, and
`make check` green — the agent never sees this branch's CI run.

It commits that work locally, writing its PR title and body to
`target/fixes/` (plus a `needs-human` marker if it could not finish honestly).
`fixes-publish` recreates the branch from the banked patch, opens or updates
the PR — draft and labeled `needs-human` when the marker is there — and
dispatches CI and the review gate. Parity-fix PRs are **always merged by a
human** — there is no automerge switch for decoder changes — so their checks
land under the eyes of whoever merges them. One open `parity-backlog` PR
blocks the next night's run (no stacking); merge or close it to resume
draining.

On demand: `gh workflow run nightly.yaml -f lane=fixes -f vins="VIN1 VIN2"`
runs an RCA on exactly those VINs, skipping the probe and the backlog pick.

## Quiet nights

No lockfile movement and an empty backlog cost $0 in API spend: the agent
steps are gated behind real work. Without `XAI_API_KEY` (data-refresh
environment) the fixes lane skips with a summary note, and a broken deps bump
fails the run for a human.

## Failure playbook

- **deps red, PR opened with a diagnosis instead of a fix** — the bump
  genuinely broke something the agent couldn't fix honestly; its explanation
  is the PR body. Fix, or close it and let tomorrow retry.
- **deps PR labeled `needs-human`, checks red** — the remediation agent had
  its one attempt and the re-kicked checks are still red. Its diagnosis is the
  last commit message on `deps/nightly`; the session is the
  `nightly-deps-remedy-transcript` artifact on the failing run.
- **deps PR labeled `needs-human` with a "ended without merging" comment** —
  the lane broke before anything could judge the checks (agent banked nothing,
  branch moved under the publish job, automation guard refused the patch, wait
  timed out). The linked run's failing job says which.
- **`fixes` PR is a `[needs-human]` draft** — the parity agent could not close
  the cluster honestly; `target/fixes/pr-body.md` (now the PR body) has the
  diagnosis.
- **deps PR labeled `needs-human`, Security red** — a scanner found something
  in the bumped tree and `ci`/`data-review` are green. The comment names the
  failing jobs. No agent touched it: scanner verdicts are human-owned. Triage,
  push a fix and re-run (`gh workflow run security.yaml --ref deps/nightly`),
  or close the PR and let tomorrow retry.
- **deps PR labeled `needs-human`, `review-verdict` red** — the adversarial
  review gate rejected the bump's content; its findings comment says why. The
  pipeline deliberately stops rather than remediate a verdict. Address the
  finding and re-run the gate (`gh workflow run data-review.yaml -f pr=<n>`),
  or close the PR and let tomorrow retry.
- **Cargo bumps repeatedly abandoned** — a young release is pinned by its
  parent (named in the report). Expected; it ages out within 7 days.
- **fixes lane keeps skipping** — an open `parity-backlog` PR is waiting for
  your merge.
- **covfuzz probe finds nothing for weeks** — coverage is saturated for the
  current decoder. Fine. It reopens the next time decode paths change.
