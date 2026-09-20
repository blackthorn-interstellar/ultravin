# Converge

State for the `/converge` loop. `VISION.md` is the fixed reference. Only the human raises a ceiling.

## Ceilings

Measured over tracked files at 3614bce (2026-09-20), each set at count + 5%.

| What | Count at creation | Ceiling |
|---|---|---|
| Non-test lines | 49814 | 52304 |
| Test lines | 6978 | 7326 |

Non-test:

    git ls-files -z -- '*.rs' '*.py' '*.sh' '*.toml' '*.yaml' '*.yml' Makefile | grep -zvE '^tests/|/tests/' | xargs -0 cat | wc -l

Test:

    git ls-files -z -- 'tests/*.py' 'crates/*/tests/*' | xargs -0 cat | wc -l

## Done

- 2026-09-20 bug: `ultravin decode-batch` on a non-UTF-8 file dumped a traceback (exit 1); now a one-line error, exit 2.

## Rejected

## Consecutive empty iterations

0

## Open questions

- Local `master` is 1 commit ahead of and 6 behind `origin/master` (nightly dependency bumps, the 2026_09 vPIC data update, a CI coverage-gate change), and the working tree holds another agent's uncommitted native-architecture work (`crates/ultravin/src/lib.rs`, `native_stream.rs`, `crates/ultravin/Cargo.toml`, `uv.lock`, `scripts/bench/native_trials.py`). The loop does not pull, merge, or push. Options: (A) human merges origin/master once that work is committed — recommended, keeps the loop working on current data; (B) leave it, and loop commits pile up on a stale base with a larger merge later.

## Leads

Scout findings not yet through the skeptic. Re-verify before acting.

- delete: rejected `batch-slab` / Storage V2 experiment (~1,430 lines: `experimental_batch.rs`, `examples/storage_probe.rs`, `examples/support/allocation_counter.rs`, `scripts/bench/batch_storage*.py` + JSON). Its own doc (`docs/REUSABLE_SLOTS_AND_STORAGE_V2_2026_09_14.md:52`) says it regresses. Blocked while another agent has uncommitted edits in `lib.rs` and `crates/ultravin/Cargo.toml`.
- delete: six `scripts/bench/*_2026_09_14.json` result files (2,756 lines) referenced by nothing: `batch_storage_allocations`, `batch_storage_screen`, `large_native_12cores`, `native_managed_fit_..._rejected_unweighted_per_worker`, `worker_scaling_cache`, `worker_scaling_single_corrections`.
- delete: `crates/ultravin/examples/allocation_probe.rs` (161 lines), no run command anywhere.
- delete: `reusable_slots_probe.rs` + `slot_coordination_probe.rs` (2,025 lines) feed only unlinked docs that end "no production default changed".
- delete: unused `pub` items in `arrow_io.rs`: `ArrowBatchRebatcher::buffered_rows`, `ArrowDecoder::with_columns`.
- docs: README.md:100 `full=True` example — `elements[0]` is `Suggested VIN`, not Make, and Make's source is never `'Manu. Name'`.
- docs: docs/NIGHTLY.md:47 says the exposed secret is the "Anthropic key"; the only secret in `nightly.yaml` is `XAI_API_KEY`.
- docs: docs/CORPUS.md:222 `coverage sweep` does not exist; the command is `coverage emit sweep`.
- docs: README.md:69 says note fields are "always `list[str]`", but the Arrow/Parquet path keeps only the first note (`ids.rs:317`, deliberate).
- docs: docs/DATA_REFRESH.md:99 says 63 crash VINs; `scripts/known_problems.json` has 66.
