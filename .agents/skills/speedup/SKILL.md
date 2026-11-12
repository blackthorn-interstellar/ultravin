---
name: speedup
description: Chase an ambitious, probably impossible ultravin speed target (default 100x) by working backward from the cycle budget it implies. Use when asked to make ultravin faster, or via /speedup [target].
---

# Speedup

Target: the argument (`100x`, `10x single-core`, `5M VIN/s sorted`). Default: 100x
on the headline benchmark.

The target is meant to be out of reach of tuning. That is the point. A 10% goal
invites shaving instructions; a 100x goal forces the question of which work need
not exist at all. Do not argue that the target is impossible and do not quietly
shrink it. Get as far toward it as the truth allows and report where the wall is.

## Method

1. **Turn the target into a budget.** Take cycles and instructions per VIN from
   the latest headline in `docs/BENCHMARKS.md` and divide by the target. Say what
   that budget buys in real terms (how many cache misses, branches, bytes
   touched). Every idea is judged against this number.
2. **Design backward.** Ignore the current code and sketch the decoder that would
   fit the budget. Then find the largest piece of that design reachable from
   here. Prefer deleting whole classes of work (moving it to build time, a
   different data layout, a different algorithm) over making existing work
   cheaper.
3. **Profile, then attack the biggest cost.** Measure where the cycles go today
   before touching anything. Guesses about hot spots in this codebase are
   usually wrong; it has been through many rounds already.
4. **Loop.** One change, paired measurement, keep or revert, commit each kept
   win. Keep going until every idea you have is either measured or ruled out in
   writing. Running out of easy ideas is the moment to go back to step 2, not
   to stop.

## Rules

- **Same answers.** A faster wrong decoder is worth nothing. `make check` and
  `make answerkey-verify` pass, and every output shape is unchanged.
- **No cheating the benchmark.** No whole-VIN result cache and no special-casing
  the corpus. A win must hold on unique shuffled VINs.
- **Read the graveyard first.** `PERFORMANCE_IMPROVEMENTS.md` lists experiments
  already rejected. Do not rerun one without a new reason it would work now.
- **Measure honestly.** Follow "Correctness and measurement rules" in that file:
  paired release builds run back to back, order alternated. This host is shared,
  so trust cycles and instructions per VIN over VIN/s. Check startup time and
  peak memory too, so the cost has not just moved.
- **Write it down.** Add a dated section to `PERFORMANCE_IMPROVEMENTS.md` with
  each kept change (bottleneck, mechanism, measurement) and each rejected
  experiment. If anything was kept, rerun the headline benchmark and update
  `docs/BENCHMARKS.md`, the README numbers, and `make chart`.

## Report

Before and after, the multiple achieved against the target, what the wall is,
and the single most promising idea left untested.
