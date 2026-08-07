# Nightly Autonomous Maintenance

`.github/workflows/nightly.yaml` (08:41 UTC daily, or `gh workflow run
nightly.yaml`) runs two independent lanes. Both follow the data-refresh
pattern: a deterministic mechanical path first, a Claude agent only when there
is real work, delivery as a PR through the same CI + `review-verdict` checks,
and explicit dispatch of those checks (workflow-created PRs emit no events).

```
deps    lockfile bump (7-day cooldown) ──▶ make check ──▶ PR ─┐
                                              │ broken        │
                                              ▼               ▼
                                        agent fixes ──▶ PR ──▶ deps-checks: watch;
                                                               red → agent works the
                                                               checks green; then
                                                               MERGE (end to end)

fixes   covfuzz probe tops up backlog ──▶ agent drains ONE cluster ──▶ PR,
                                          then the same agent kicks + watches its
                                          checks and fixes failures (human merges)
```

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
and deliver the PR even when stuck (`[needs-human]` draft). The agent has
**rule judgment**: a new lint/type rule arriving with a tool bump is a
proposal, not law — it conforms where the rule genuinely improves the
codebase and config-disables it (justified in the PR body) where it is churn.
The codebase is not a hostage to whatever ruff rolls out. What it may never
do is weaken an existing check or edit the automation itself (`.github/**`,
`Makefile` — such diffs are refused at merge).

Whoever delivered the PR, the `deps-checks` job then owns its checks: it
waits for the dispatched runs, and if any fail, an agent is checked out on
the branch to work them green — read the failing log, fix, push, re-kick,
wait (one blocking ~20-min call per CI cycle), repeat — until green or an
honest give-up (diagnosis commented on the PR, job fails, human takes over).

Once green, the deps PR **merges itself** — mechanical or agent-fixed alike,
end to end with no human in the path. The single refusal: a diff touching
`.github/**` or the `Makefile` fails the merge step for a human, because the
machinery must not be able to edit its own judges.

## The fixes lane

`tests/parity_backlog.jsonl` is the work queue: VINs where ultravin and the
Postgres vPIC oracle disagreed, logged by the parity campaign
(`scripts/parity/campaign.py`). It was seeded from the local campaign's finds
(the systematic engine ran to completion: 5.46M VINs, 48 model years) and is
topped up every night in CI by a 15-minute covfuzz probe chunk against the
pinned-dump oracle (coverage state resumes via actions/cache; a decoder change
reopens coverage, so the probe never permanently retires).

Each night the agent gets a loaded oracle and a built extension, and resolves
**one root-cause cluster**: reproduce first (stale entries are deleted — that
alone is a valid PR), then either fix the decoder or document a genuine
oracle/dump defect in `docs/KNOWN_DEVIATIONS.md` + `KNOWN_DEVIATION_VINS`.
Fixed clusters leave a representative VIN in `tests/brutal_repros.json`, which
the monthly refresh freezes into the parity corpus as a permanent regression.
The finish line is machine-checked: `scripts.parity.sweep --cases` over the
resolved VINs must show zero undocumented divergence, and `make check` green.

After delivering, the same agent session kicks CI + the review gate itself,
watches them in the foreground, and fixes any failures on the branch until
everything is green (the workflow re-kicks as a fallback if the session died
before kicking). Parity-fix PRs are **always merged by a human** — there is
no automerge switch for decoder changes. One open `parity-backlog` PR blocks
the next night's run (no stacking); merge or close it to resume draining.

On demand: `gh workflow run nightly.yaml -f lane=fixes -f vins="VIN1 VIN2"`
runs an RCA on exactly those VINs, skipping the probe and the backlog pick.

## Quiet nights

No lockfile movement and an empty backlog cost $0 in API spend: the agent
steps are gated behind real work. Without `ANTHROPIC_API_KEY` (data-refresh
environment) the fixes lane skips with a summary note, and a broken deps bump
fails the run for a human.

## Failure playbook

- **deps red, agent delivered a `[needs-human]` draft** — the bump genuinely
  broke something the agent couldn't fix honestly; the draft has the
  diagnosis. Fix, or close it and let tomorrow retry.
- **Cargo bumps repeatedly abandoned** — a young release is pinned by its
  parent (named in the report). Expected; it ages out within 7 days.
- **fixes lane keeps skipping** — an open `parity-backlog` PR is waiting for
  your merge.
- **covfuzz probe finds nothing for weeks** — coverage is saturated for the
  current decoder. Fine. It reopens the next time decode paths change.
