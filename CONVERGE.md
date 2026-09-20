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
- 2026-09-20 docs: README `full=True` example showed the wrong element and a source Make never has; now selects Make and shows real output.

## Rejected

- delete `reusable_slots_probe.rs` / `slot_coordination_probe.rs` and their runners: hashed, documented experiment records, and reusable slots shipped (772ee9d).
- delete `examples/allocation_probe.rs`: its command is the recorded recipe behind two allocation result files.
- delete six unreferenced `scripts/bench/*_2026_09_14.json` results: inert experiment records, nothing measurable gained, fails the reversal test.
- delete unused `pub` `ArrowBatchRebatcher::buffered_rows` / `ArrowDecoder::with_columns`: public crate API, removal is a compatibility call.

## Consecutive empty iterations

0

## Open questions

- Local `master` is 1 commit ahead of and 6 behind `origin/master` (nightly dependency bumps, the 2026_09 vPIC data update, a CI coverage-gate change), and the working tree holds another agent's uncommitted native-architecture work (`crates/ultravin/src/lib.rs`, `native_stream.rs`, `crates/ultravin/Cargo.toml`, `uv.lock`, `scripts/bench/native_trials.py`). The loop does not pull, merge, or push. Options: (A) human merges origin/master once that work is committed — recommended, keeps the loop working on current data; (B) leave it, and loop commits pile up on a stale base with a larger merge later.

## Leads

Scout findings not yet through the skeptic. Re-verify before acting.

- delete: rejected `batch-slab` / Storage V2 experiment (~1,430 lines: `experimental_batch.rs`, `examples/storage_probe.rs`, `examples/support/allocation_counter.rs`, `scripts/bench/batch_storage*.py` + JSON). Its own doc (`docs/REUSABLE_SLOTS_AND_STORAGE_V2_2026_09_14.md:52`) says it regresses. Blocked while another agent has uncommitted edits in `lib.rs` and `crates/ultravin/Cargo.toml`.
- docs: docs/NIGHTLY.md:47 says the exposed secret is the "Anthropic key"; the only secret in `nightly.yaml` is `XAI_API_KEY`.
- docs: docs/CORPUS.md:222 `coverage sweep` does not exist; the command is `coverage emit sweep`.
- docs: README.md:69 says note fields are "always `list[str]`", but the Arrow/Parquet path keeps only the first note (`ids.rs:317`, deliberate).
- docs: docs/DATA_REFRESH.md:99 says 63 crash VINs; `scripts/known_problems.json` has 66.
