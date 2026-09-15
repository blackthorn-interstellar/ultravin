# Three native decoder optimization trials

Three independent Sol subagents implemented the proposed experiments. Root
reviewed each change, coordinated tests and sequential benchmarks, and selected
**per-VIN pass-context reuse** for production. The conversion and deferred-output
experiments did not demonstrate a useful throughput improvement.

## Independent comparisons

Each row below is a separate candidate–baseline–baseline–candidate comparison.
Numbers are arithmetic means of two runs per binary. The baseline is the frozen
V7 binary from the [previous million-VIN result](NATIVE_MILLION_2026_09_15.md),
rerun alongside each candidate; these gains are additional to that earlier work.

| Experiment | Baseline VIN/s | Candidate VIN/s | Throughput change | Instructions/VIN change | Decision |
|---|---:|---:|---:|---:|---|
| Reuse per-VIN pass context | 1,164,144 | 1,182,396 | +1.57% | −1.62% | Keep; confirmed in main checkout |
| Keep conversion source indices | 1,152,465 | 1,158,721 | +0.54% | +0.01% | Do not ship: gain smaller than observed run variation |
| Defer losing-pass corrections | 1,146,350 | 1,141,875 | −0.39% | +0.08% | Do not ship: no measured throughput gain |

Context reuse reduced process CPU time per VIN by 3.15% in its independent
comparison. Conversion indices reduced it by 0.60%; deferred corrections by
0.56%. These small CPU-time changes alone do not establish a throughput gain.

All runs use twelve workers, automatic batch selection, the same 20 million
unique VINs, a fixed clock, full result construction, ordered delivery, and
worker-owned cleanup. Every process runs a complete warm pass before its timed
complete pass, which exceeds ten seconds. Automatic calibration and worker
startup are included. All selected 100 VINs per batch and five slots per worker.

Only one benchmark process ran at a time, with no concurrent builds, tests, or
profiling. Unrelated host activity was not suspended. Higher absolute rates than
the earlier 1.038-million result also appeared in the unchanged baseline, so that
entire increase cannot be attributed to these changes.

Raw output, per-run counters, source hashes, and immutable binary locations:

- [Context reuse](../scripts/bench/native_trial_context_2026_09_15.json)
- [Conversion indices](../scripts/bench/native_trial_conversions_2026_09_15.json)
- [Deferred corrections](../scripts/bench/native_trial_deferred_2026_09_15.json)

## Final confirmation

After applying context reuse to the main checkout, the full repository checks
passed and the release binary was rebuilt and archived for a second sequential
comparison. Every archived source hash matches the retained implementation.

| Run | VIN/s |
|---|---:|
| Candidate 1 | 1,188,369 |
| Baseline 1 | 1,154,625 |
| Baseline 2 | 1,147,445 |
| Candidate 2 | 1,190,934 |

The final candidate averaged **1,189,651 VIN/s**, versus **1,151,035 VIN/s** for
the baseline: **+3.35%**. Process CPU time per VIN fell 2.62% and instructions per
VIN fell 1.66%. [Final confirmation evidence](../scripts/bench/native_trial_final_2026_09_15.json).

Pooling both context-reuse comparisons gives four observations per version:
**1,186,024 versus 1,157,589 VIN/s (+2.46%)**, **2.88% less CPU time per VIN**,
and **1.64% fewer instructions per VIN**. This includes both comparisons instead
of selecting only the faster one. Mean peak RSS was effectively unchanged:
2,092,875,776 versus 2,090,819,584 bytes (+0.10%).

## Validation

- `UV_FROZEN=1 make checku` passed: formatting, lint, type checking, all Rust
  feature configurations, 243 Rust library tests, 14 Rust integration tests,
  and 951 Python tests. One Rust diagnostic test remains intentionally ignored.
- Conversion indices passed 242 Rust library tests; deferred output passed 243.
  The additional custom-database scoring fixture also passed its targeted test.
- Both the context trial and final main-checkout release matched **162,403
  full-result fingerprints** against the unchanged baseline, including values,
  errors, ordering, and provenance. Inputs include caller years, malformed and
  Unicode strings, and deterministic answer-key samples. The fingerprint
  example uses its own fixed clock, independently of the timing corpus clock.

[Validation commands, counts, and hashes](../scripts/bench/native_trial_validation_2026_09_15.json).

## Production change and review

One context per sanitized VIN resolves the unrestricted first WMI row and the
first row public at the captured clock. Candidate-year passes reuse those rows,
the WMI car/light-truck classification, and the VIN-exception lookup. No data is
cached across VINs, databases, or clocks.

Review caught and fixed a low-volume VIN distinction: position-three `9` forces
the effective check-digit car/light-truck flag to false, while year resolution
still uses the WMI classification. The original WMI string remains available to
vehicle-spec joins; selecting one public row must not replace all-row joins.
Root also corrected compilation and release-only warning issues before timing.

The conversion trial avoids cloning provenance for losing conversions while
preserving stable priority ordering, existing destinations, and the rule that
new conversions cannot become sources during the same pass. The deferred-output
trial computes error state for every pass, scores virtual correction-row
presence using live database weights, and materializes corrections only for the
winner. Its custom-database tests include positive, negative, zero, and NULL
weights, duplicate/empty rows, missing lookups, and two blank lookup names whose
separator makes the joined message nonempty.

## Reproduce the trials

The [runner](../scripts/bench/native_trials.py) archives source and the freshly
built binary before starting each comparison. It verifies the fixed corpus and
baseline binary hashes and refuses to overwrite evidence.

Each patch applies independently to base commit
`911ea46c1101dcabd7846a64b4216f3e8e9d1477`:

- [Pass context](../scripts/bench/native_trials_2026_09_15/pass_context.patch)
- [Conversion indices](../scripts/bench/native_trials_2026_09_15/conversion_indices.patch)
- [Deferred output, including strengthened custom-database test](../scripts/bench/native_trials_2026_09_15/deferred_output.patch)

Build a trial worktree with `CARGO_TARGET_DIR` pointing to this checkout's
`target` and `ULTRAVIN_DATA` pointing to its `crates/ultravin/data/vpic.rkyv`:

```sh
cargo build --release -p ultravin --example throughput
```

Then run from this checkout, after all builds and tests have stopped:

```sh
UV_FROZEN=1 uv run -- python -m scripts.bench.native_trials \
  --source-root /path/to/trial-worktree \
  --output scripts/bench/native_trial_local.json
```

The supplied source root must be the source used to build that binary. The
deferred-output benchmark predates its additional test-only fixture change;
its archived production implementation is unchanged by that test.
