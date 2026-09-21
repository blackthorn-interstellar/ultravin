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

## Consecutive empty iterations

0

## Open questions

- Local `master` is 1 commit ahead of and 6 behind `origin/master` (nightly dependency bumps, the 2026_09 vPIC data update, a CI coverage-gate change), and the working tree holds another agent's uncommitted native-architecture work (`crates/ultravin/src/lib.rs`, `native_stream.rs`, `crates/ultravin/Cargo.toml`, `uv.lock`, `scripts/bench/native_trials.py`). The loop does not pull, merge, or push. Options: (A) human merges origin/master once that work is committed — recommended, keeps the loop working on current data; (B) leave it, and loop commits pile up on a stale base with a larger merge later.
- The Arrow/Parquet path (`decode_stream`, `decode-parquet`) silently keeps only the first note of each multi-valued free-text field (the names in `ultravin.MULTI_VALUED`, e.g. "Other Trailer Info"), while `decode()` returns all of them as `list[str]`. About 5% of corpus VINs (391 of 7,162) lose notes this way. It is deliberate (`crates/ultravin/src/ids.rs:321` "the first note wins", pinned by `tests/test_parquet.py`), but nothing user-facing says so, and the vision asks for full-field spVinDecode parity. Options: (A) add one README sentence under "Columns and layout" saying columnar output keeps the first note and `decode()` returns all — recommended, cheap and honest, no schema change; (B) emit those fields as `List<Utf8>` columns — faithful, but changes the output schema for existing users; (C) leave it undocumented.
- The loop cannot run `make coverage` (the decode-path region gate that CI runs on every PR) because `cargo-llvm-cov` and the `llvm-tools-preview` rustup component are not installed on this machine. Any Rust change to the gated files (`decode.rs`, `errors.rs`, `matcher.rs`, `year.rs`, `checkdigit.rs`, `wmi.rs`, `conversion.rs`, `resolve.rs`) therefore cannot be verified before commit, so the loop skips such changes (one accepted 5-line simplification in `checkdigit.rs` was shelved for this). Options: (A) human runs `cargo install cargo-llvm-cov && rustup component add llvm-tools-preview` — recommended, makes the gate checkable locally; (B) leave it, and the loop keeps avoiding decode-path Rust edits.
- Nothing guards the vision line "Decode individual VINs in under one millisecond": `crates/ultravin/benches/decode.rs` asserts nothing, no workflow runs a bench, and no test has a timing assertion, so a 10x single-VIN regression would merge green. Today it holds with room (warm median 118 us, p99 226 us, cold first decode 16 ms, measured through Python on the debug dev build). Options: (A) add a nightly CI step that runs a release-build single-VIN timing and fails above a generous budget such as 1 ms median — recommended, catches real regressions without flaking `make check`; (B) add a wall-clock assertion to the unit tests — simple but flaky on loaded machines; (C) leave it to the manual benchmark docs.

- Five `pub` items in the `ultravin` crate have no caller outside their own unit tests: `parquet_io::open_chunks_auto` (the `open_chunks_auto_at` sibling is what the Python bindings use), `Db::vspecschemas_for_make`, `adaptive::BatchFeedback::calibration_needed`, `adaptive::DEFAULT_MEMORY_BYTES`, and `parquet_io::decode_parquet_to_file` (whose 15 unit tests are the only src→dst parquet round-trip coverage). The crate is published, so removing any of them is a compatibility call the loop will not make on its own. Options: (A) human says which are fair to remove before 1.0 — recommended for the first four (dead surface is maintenance with no user), keep `decode_parquet_to_file` for its tests; (B) keep all five as public API.

## Leads

Scout findings not yet through the skeptic. Re-verify before acting.

- bug (do first): `ultravin decode-parquet --columns "²"` (or `①`, any char where `str.isdigit()` is true but `int()` refuses) dumps `ValueError: invalid literal for int()` with a traceback, exit 1. `python/ultravin/cli.py:116` classifies tokens with `tok.lstrip("-").isdigit()` outside the `try`; `int()` accepts exactly the `isdecimal()` set (Unicode Nd), so `isdigit()` → `isdecimal()` makes the token a column name and the existing handler reports "unknown column", exit 2. Write the failing CLI test first.
- bug (boundary inputs, same shape as the rejected `columns=[2**31]`; probably reject): `--year 2147483648` (`decode`, `decode-batch`) → `OverflowError` traceback exit 1 while `--year 99999` is a clean error-12 decode; `decode_stream` with an int64 year column value ≥ 2³¹ silently nulls both the passthrough and the hint (`arrow_io.rs:373` safe cast) where `decode(year=2**31)` raises; `--columns 2147483648` → TypeError "got int"; `--batch-size 2**63` → pyo3 enum TypeError instead of the unreachable "too large" ValueError at `ultravin-py/src/lib.rs:1021`; `--batch-memory-mb`/`--sample-rows` ≥ 2⁶⁴ → OverflowError. Note spVinDecode's `@modelyear` is SQL `int`, so the i32 bound itself is oracle-faithful.
- bug/docs (library only): `generate(-1)`, `generate(1, seed=-1)`, `seeded(limit=-1)`, `pairwise(limit=-1)`, `decode_stream(sample_rows=-1 | batch_memory_mb=-1)` raise `OverflowError: can't convert negative int to unsigned` where the stub (`_ultravin.pyi:212,320`) documents `ValueError` for caller mistakes. Check whether the stub promises ValueError for negatives specifically before calling it a contradiction.
- clean (do not re-probe): all six decode entry points agree on 3,562 VINs × 13 year hints, plain and full; empty/blank/CRLF stdin, zero-row parquet, dst-inside-src, duplicate columns, second use of a stream, generate filters and determinism all behave.

- delete: rejected `batch-slab` / Storage V2 experiment (~1,430 lines: `experimental_batch.rs`, `examples/storage_probe.rs`, `examples/support/allocation_counter.rs`, `scripts/bench/batch_storage*.py` + JSON). Its own doc (`docs/REUSABLE_SLOTS_AND_STORAGE_V2_2026_09_14.md:52`) says it regresses. Blocked while another agent has uncommitted edits in `lib.rs` and `crates/ultravin/Cargo.toml`.
- docs (skeptic accepted 2026-09-21; do next): `docs/SCANNER-NOTES.md:120,129-130` says the Makefile installs uv with `wget` and that earlier notes calling it `curl` were wrong; `Makefile:136` is `curl -LsSf ... | sh` (wget was replaced in d87a990). The correction runs backwards.
- docs (skeptic accepted 2026-09-21; do next): `docs/DATA_REFRESH.md:205` says the daily `detect` catches a re-issued dump via `Last-Modified`; `scripts/refresh.py:236-249` compares `Content-Length` to the manifest's `dump_bytes` and its docstring says why mtime is deliberately not used. `DATA_REFRESH.md:250-251` already states it correctly; keep its same-size caveat.
- docs: `docs/NIGHTLY.md:87` "13 of its 15 jobs" — `security.yaml` has 13 jobs, so gating all but scorecard + snyk is 11 of 13.
- docs (low value, counts drift): `docs/CORPUS.md:143,155,162,164` allowance counts (39/138/26/16) vs `scripts/coverage_allowances.json` today (38/136/29/11); `docs/RELEASE.md:27` "~82MB" for an 83.4 MB artifact the same doc calls 83MB at :39; `docs/SCANNER-NOTES.md` cites `db.rs` unsafe-site line numbers, `.gitignore:120`, `BENCHMARKS.md:202`, `data-review.yaml:14` that have all moved.
- delete (public API — needs the human): dead `pub` items with zero non-test callers: `parquet_io.rs:395 open_chunks_auto` (only `_at` sibling is used), `db.rs:745 Db::vspecschemas_for_make` (one test), `adaptive.rs:99 BatchFeedback::calibration_needed` and `adaptive.rs:18 DEFAULT_MEMORY_BYTES` (tests only), `parquet_io.rs:613 decode_parquet_to_file` (15 tests, the only src→dst round-trip coverage). Same shape as the rejected `buffered_rows` / `with_columns`; see open questions.
