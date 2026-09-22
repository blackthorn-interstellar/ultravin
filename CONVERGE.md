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
- 2026-09-20 docs: NIGHTLY.md named a nonexistent Anthropic key as the agents' only reachable secret; it is `XAI_API_KEY` plus a read-only `GITHUB_TOKEN`.
- 2026-09-20 docs: CORPUS.md named a nonexistent `coverage sweep` command and listed "every error code" as a sweep dimension; now `coverage emit sweep` with the six real dimensions.
- 2026-09-20 bug: `ultravin decode-parquet` dumped a traceback (exit 1) for a missing/non-parquet source or unwritable destination; now a one-line error, exit 2.
- 2026-09-20 simplify: `Db::build` duplicated `Db::build_trusted`'s 21-field initializer; it now validates and calls it (-26 lines).
- 2026-09-20 performance: matcher archive-keys test recompiled 56k regexes, 5.8k distinct; checking each string once takes it 15.0s -> 1.6s and warm `make check` ~57s -> ~41s. Break test still fails it.
- 2026-09-20 simplify: `generate` in the Python bindings re-inlined `decode_clock`; it now calls it (-6 lines).
- 2026-09-20 bug: the importer accepted a dump that ends inside a `COPY` block (exit 0, partial artifact + fresh manifests); it now fails naming the table, before the artifact and manifests are written.
- 2026-09-20 delete: `test_decode_batch_jsonl_rejects_a_zero_batch_size` was character-for-character the `[0]` case of `test_batch_size_rejects_invalid_values`; break test confirms the parametrized case still fails.
- 2026-09-21 delete: three tests that guarded nothing (-17 test lines; skeptic accepted all three). `test_regex_crash.py::test_the_real_sample_error_matches` — every break (predicate always false, extra marker, misspelled marker) also fails the banked-repro test and two others. `test_answerkey.py::test_a_hash_is_stable_for_the_same_vin` — an unstable hash fails 11 other tests. `test_caller_year.py::test_divergent_year_runs_its_own_pass_and_can_win` — `test_cli.py::test_decode_passes_the_caller_year_through` asserts equality with `uv.decode(VIN, year=1995)` plus the same literals, and `decode.rs::caller_year_pass_can_win_best_of` pins the Rust core.
- 2026-09-21 delete: `SETTINGS` in `scripts/bench/adaptive.py` had one reference, its own definition (the CLI carries the same default string); skeptic accepted.
- 2026-09-21 bug: `ultravin decode-parquet --columns "²"` dumped a traceback (exit 1) because `cli.py` classified tokens with `isdigit()`, which accepts superscript/circled digits `int()` refuses; now `isdecimal()`, so the token reaches the unknown-column usage error (exit 2). Failing test written first, parametrized onto the existing unknown-column test.
- 2026-09-21 docs: SCANNER-NOTES.md quoted the uv bootstrap as `wget` at `Makefile:117` with a note calling earlier `curl` descriptions wrong; it is `curl -LsSf` at `Makefile:136` (wget went in d87a990). DATA_REFRESH.md:205 said `detect` finds a re-issued dump by `Last-Modified`; it compares `Content-Length` to the manifest's `dump_bytes`, as the doc's own :250 and `refresh.py`'s docstring say. Skeptic accepted both.
- 2026-09-21 delete: `test_answerkey.py::test_element_144_position_assignment_is_not_collation` (-10 test lines) pinned `(4:5)(5:4) != (4:4)(5:5)` with the same literals as `test_normalize.py::test_charset_to_position_assignment_still_diverges`; two breaks (whole-string sort, charset erased) fail both, none fails it alone. Skeptic accepted.
- 2026-09-21 delete: five more tests whose every break another test catches (-24 test lines; skeptic accepted all): `test_large_native.py::test_minimum_unique_seconds_uses_fastest_sample` (three arithmetic breaks each also fail `test_multicore.py`'s `duration_gate` pin), `test_stale_cache.py::test_vin_wmi_is_the_first_three_characters` (three slice breaks each fail 18 others), `test_flat.py::test_the_default_shape_is_header_plus_attributes` (an extra flat-dict key fails 16 others incl. both survivors), `test_caller_year.py::test_out_of_window_year_still_flags_error_12` (identical pin in `decode.rs`; a bindings break dropping `year` fails 5 others), `test_json_api.py::test_decode_batch_json_empty` (byte-exact twin in `test_provenance_clock.py`).
- 2026-09-21 simplify: `adaptive.rs` `BatchTuner::observe` inlined the body of its own private `learn_width`; it now calls it, and `BatchFeedback::decoded` drops the `output_bytes > 0 && rows > 0` guard the callee re-checks (-8 lines). Skeptic accepted; breaking the shared method fails `memory_estimate_reacts_immediately_to_a_sharp_increase`.
- 2026-09-21 simplify: `parquet_io.rs` hand-rolled `arrow_io::names` (join column names for an ambiguity error) twice; `names` is now `pub(crate)` and both sites call it (-7 lines, byte-identical messages; the three ambiguity-message unit tests still pass). Skeptic accepted.

## Rejected

- delete `reusable_slots_probe.rs` / `slot_coordination_probe.rs` and their runners: hashed, documented experiment records, and reusable slots shipped (772ee9d).
- delete `examples/allocation_probe.rs`: its command is the recorded recipe behind two allocation result files.
- delete six unreferenced `scripts/bench/*_2026_09_14.json` results: inert experiment records, nothing measurable gained, fails the reversal test.
- drop `check_digit_kernel`'s redundant `pos3` parameter (-5 lines; skeptic accepted): `checkdigit.rs` is under the `make coverage` region gate, which cannot run locally (no `cargo-llvm-cov`); not worth an unverifiable CI risk.
- add a test asserting single-VIN decode < 1 ms: the capability exists (warm median 118 us, p99 226 us through Python on the dev build); a guard is test coverage, and a wall-clock assertion in the unit suite would flake on a shared machine. See open questions.
- make `answerkey verify` fail on an empty key directory (today: "every answer matches", exit 0): skeptic — CI's fetch checks status + checksum and publication rejects empty keys, so the path is already closed; `test_an_unpinned_registered_vin_is_still_skipped` expects success with zero comparable entries.
- `_BatchTuner.observe` stub marks params keyword-only while PyO3 accepts them positionally: private class, stub errs strict, nothing breaks.
- validate the local `data/vpic.rkyv` in `crates/ultravin/build.rs` like the `ULTRAVIN_DATA` route: trusting the in-checkout artifact is the documented design, and `crates/ultravin/tests/decode.rs::artifact_blake3_matches_manifest` already fails `make check` on a corrupt one.
- derive `Default` on `VpicData` to drop ~68 lines of empty-`Vec` literals: skeptic — `VpicData` is public and `default()` would be an archive that fails `validate_arena` (arena needs sentinel vectors); exhaustive literal in `build.rs` forces a decision when a table is added. Fails the reversal test.
- delete unused `pub` `ArrowBatchRebatcher::buffered_rows` / `ArrowDecoder::with_columns`: public crate API, removal is a compatibility call.
- docs DATA_REFRESH.md:99 "the 63 crash VINs" (now 66): historical rationale, true when written, and any count there drifts with each data refresh.
- bug: `columns=[2**31]` gives `TypeError ... got int` instead of `ValueError: unknown element_id`: skeptic — contrived boundary input, element ids are in the hundreds, speculative hardening.
- drop the `--no-default-features` clippy row (`Makefile:33`, `release.yaml:87`): redundant for today's code but 0.1s warm, and the row guards future feature-gated code — fails the reversal test.
- share `arrow_io::tests`' seven helpers (`loaded`, `batch`, `utf8`, `i32s`, `out_names`, `col_utf8`, `col_i32`) with `parquet_io::tests` via `pub(crate)` (-46 lines, compiled and green under both feature sets): skeptic — fails the reversal test; restoring self-contained sibling test modules is equally defensible, so it trades duplication for coupling.
- hoist `ids.rs`'s duplicated 3-arm `IdsDType → ColumnValues::with_capacity` match into a helper, and the bindings' repeated full/flat `decode_json_at` fork into `one_json`: each removes two 5-line blocks but adds a 7-line fn — net −1/0 lines for a new name. Not measurably less.
- merge `arrow_io::vin_by_name`/`year_by_name` (only the candidate list and the ambiguity message differ, and the VIN message carries an " all match by name" tail the year one lacks) or hoist `pairwise`/`seeded`'s shared 12-line schema prologue in `generate.rs` into a 4-tuple-returning helper: each needs a new fn or closure whose own lines cancel most of the saving, and keeping the messages byte-identical means threading them through. Merely different.
- docs CORPUS.md:55 "the overlap is only 32,476" (sweep fills open positions with `1`, pairwise with `0`, so on 2026_08 it is 0): the figure sits in the doc's dated measurement table (584,019 / 1,736,895 / ~2.3M), every cell of which moves with each data month — count drift, same as the rejected crash-VIN count.
- `scripts/refresh.py` `UV_RUN`/`UV_PY` constants for the eight `["uv", "run", "--frozen", "--", "python", "-m", ...]` prefixes: ruff still wraps the three long argv lists vertically, so it saves ~13 lines by adding splats and two module constants; explicit argv is an equally defensible undo. Fails the reversal test.
- delete `test_answerkey.py::test_element_144_collation_reorder_still_collides`, `::test_element_144_still_compares_its_contents`, `::test_other_elements_keep_their_order` as twins of `test_normalize.py`: skeptic — the answerkey literals are digit charsets `(6:_123456789)` where the normalize ones are letters, so a regex narrowed to `[A-Z_]` or a lost `9` fails only them; and `== rows` is the only check that the normalizer returns non-144 rows unchanged (a copied row with an added key would pass the diff-based twin).

## Consecutive empty iterations

2

## Open questions

- Local `master` is 1 commit ahead of and 6 behind `origin/master` (nightly dependency bumps, the 2026_09 vPIC data update, a CI coverage-gate change), and the working tree holds another agent's uncommitted native-architecture work (`crates/ultravin/src/lib.rs`, `native_stream.rs`, `crates/ultravin/Cargo.toml`, `uv.lock`, `scripts/bench/native_trials.py`). The loop does not pull, merge, or push. Options: (A) human merges origin/master once that work is committed — recommended, keeps the loop working on current data; (B) leave it, and loop commits pile up on a stale base with a larger merge later.
- The Arrow/Parquet path (`decode_stream`, `decode-parquet`) silently keeps only the first note of each multi-valued free-text field (the names in `ultravin.MULTI_VALUED`, e.g. "Other Trailer Info"), while `decode()` returns all of them as `list[str]`. About 5% of corpus VINs (391 of 7,162) lose notes this way. It is deliberate (`crates/ultravin/src/ids.rs:321` "the first note wins", pinned by `tests/test_parquet.py`), but nothing user-facing says so, and the vision asks for full-field spVinDecode parity. Options: (A) add one README sentence under "Columns and layout" saying columnar output keeps the first note and `decode()` returns all — recommended, cheap and honest, no schema change; (B) emit those fields as `List<Utf8>` columns — faithful, but changes the output schema for existing users; (C) leave it undocumented.
- The loop cannot run `make coverage` (the decode-path region gate that CI runs on every PR) because `cargo-llvm-cov` and the `llvm-tools-preview` rustup component are not installed on this machine. Any Rust change to the gated files (`decode.rs`, `errors.rs`, `matcher.rs`, `year.rs`, `checkdigit.rs`, `wmi.rs`, `conversion.rs`, `resolve.rs`) therefore cannot be verified before commit, so the loop skips such changes (one accepted 5-line simplification in `checkdigit.rs` was shelved for this). Options: (A) human runs `cargo install cargo-llvm-cov && rustup component add llvm-tools-preview` — recommended, makes the gate checkable locally; (B) leave it, and the loop keeps avoiding decode-path Rust edits.
- Nothing guards the vision line "Decode individual VINs in under one millisecond": `crates/ultravin/benches/decode.rs` asserts nothing, no workflow runs a bench, and no test has a timing assertion, so a 10x single-VIN regression would merge green. Today it holds with room (warm median 118 us, p99 226 us, cold first decode 16 ms, measured through Python on the debug dev build). Options: (A) add a nightly CI step that runs a release-build single-VIN timing and fails above a generous budget such as 1 ms median — recommended, catches real regressions without flaking `make check`; (B) add a wall-clock assertion to the unit tests — simple but flaky on loaded machines; (C) leave it to the manual benchmark docs.

- Five `pub` items in the `ultravin` crate have no caller outside their own unit tests: `parquet_io::open_chunks_auto` (the `open_chunks_auto_at` sibling is what the Python bindings use), `Db::vspecschemas_for_make`, `adaptive::BatchFeedback::calibration_needed`, `adaptive::DEFAULT_MEMORY_BYTES`, and `parquet_io::decode_parquet_to_file` (whose 15 unit tests are the only src→dst parquet round-trip coverage). The crate is published, so removing any of them is a compatibility call the loop will not make on its own. Options: (A) human says which are fair to remove before 1.0 — recommended for the first four (dead surface is maintenance with no user), keep `decode_parquet_to_file` for its tests; (B) keep all five as public API.

## Leads

Scout findings not yet through the skeptic. Re-verify before acting.

- bug (boundary inputs, same shape as the rejected `columns=[2**31]`; probably reject): `--year 2147483648` (`decode`, `decode-batch`) → `OverflowError` traceback exit 1 while `--year 99999` is a clean error-12 decode; `decode_stream` with an int64 year column value ≥ 2³¹ silently nulls both the passthrough and the hint (`arrow_io.rs:373` safe cast) where `decode(year=2**31)` raises; `--columns 2147483648` → TypeError "got int"; `--batch-size 2**63` → pyo3 enum TypeError instead of the unreachable "too large" ValueError at `ultravin-py/src/lib.rs:1021`; `--batch-memory-mb`/`--sample-rows` ≥ 2⁶⁴ → OverflowError. Note spVinDecode's `@modelyear` is SQL `int`, so the i32 bound itself is oracle-faithful.
- checked, not a contradiction: `generate(-1)`, `seeded(limit=-1)`, `decode_stream(sample_rows=-1)` etc. raise `OverflowError` from the unsigned conversion; the stub promises `ValueError` only for `n > 10,000,000` and for unknown column / element id / ambiguous autodetect, never for negatives. Changing the exception type would be speculative hardening.
- delete test (coverage-gated file, skip unless the human installs `cargo-llvm-cov`): `checkdigit.rs::short_vin_returns_none` — the mutation test's inline reference already hard-codes `len != 17 → None` for 0/16/18-char inputs.
- clean (do not re-run, 2026-09-21): every fenced example with shown output in README.md, crates/ultravin/README.md, docs/CORPUS.md, docs/BATCH_PREDICTOR.md, docs/KNOWN_DEVIATIONS.md and the `_ultravin.pyi` docstrings was executed and matches (keys, values, exception types, exit codes; only "~" counts moved with 2026_08). `ruff --select ARG,B007,ERA001,PLW0602,PLW0603,F841,F811,PIE790,PIE794,PIE810,SIM102,SIM108,SIM110,SIM118,RET504,RET505` finds only test fakes' unused params and two `nosemgrep` comments. The shipped package uses no 3.11+ syntax, so `requires-python = ">=3.10"` is honest and CI tests 3.10–3.15. No `#[allow(dead_code)]` outside `build.rs`'s `#[path] mod tables`.
- clean (do not re-probe): all six decode entry points agree on 3,562 VINs × 13 year hints, plain and full; empty/blank/CRLF stdin, zero-row parquet, dst-inside-src, duplicate columns, second use of a stream, generate filters and determinism all behave.

- delete: rejected `batch-slab` / Storage V2 experiment (~1,430 lines: `experimental_batch.rs`, `examples/storage_probe.rs`, `examples/support/allocation_counter.rs`, `scripts/bench/batch_storage*.py` + JSON). Its own doc (`docs/REUSABLE_SLOTS_AND_STORAGE_V2_2026_09_14.md:52`) says it regresses. Blocked while another agent has uncommitted edits in `lib.rs` and `crates/ultravin/Cargo.toml`.
- docs: `docs/NIGHTLY.md:87` "13 of its 15 jobs" — `security.yaml` has 13 jobs, so gating all but scorecard + snyk is 11 of 13.
- docs (low value, counts drift): `docs/CORPUS.md:143,155,162,164` allowance counts (39/138/26/16) vs `scripts/coverage_allowances.json` today (38/136/29/11); `docs/RELEASE.md:27` "~82MB" for an 83.4 MB artifact the same doc calls 83MB at :39; `docs/SCANNER-NOTES.md` cites `db.rs` unsafe-site line numbers, `.gitignore:120`, `BENCHMARKS.md:202`, `data-review.yaml:14` that have all moved.
- delete (public API — needs the human): dead `pub` items with zero non-test callers: `parquet_io.rs:395 open_chunks_auto` (only `_at` sibling is used), `db.rs:745 Db::vspecschemas_for_make` (one test), `adaptive.rs:99 BatchFeedback::calibration_needed` and `adaptive.rs:18 DEFAULT_MEMORY_BYTES` (tests only), `parquet_io.rs:613 decode_parquet_to_file` (15 tests, the only src→dst round-trip coverage). Same shape as the rejected `buffered_rows` / `with_columns`; see open questions.
