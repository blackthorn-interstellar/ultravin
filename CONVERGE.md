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

## Rejected

- delete `reusable_slots_probe.rs` / `slot_coordination_probe.rs` and their runners: hashed, documented experiment records, and reusable slots shipped (772ee9d).
- delete `examples/allocation_probe.rs`: its command is the recorded recipe behind two allocation result files.
- delete six unreferenced `scripts/bench/*_2026_09_14.json` results: inert experiment records, nothing measurable gained, fails the reversal test.
- delete unused `pub` `ArrowBatchRebatcher::buffered_rows` / `ArrowDecoder::with_columns`: public crate API, removal is a compatibility call.
- docs DATA_REFRESH.md:99 "the 63 crash VINs" (now 66): historical rationale, true when written, and any count there drifts with each data refresh.
- bug: `columns=[2**31]` gives `TypeError ... got int` instead of `ValueError: unknown element_id`: skeptic — contrived boundary input, element ids are in the hundreds, speculative hardening.

## Consecutive empty iterations

0

## Open questions

- Local `master` is 1 commit ahead of and 6 behind `origin/master` (nightly dependency bumps, the 2026_09 vPIC data update, a CI coverage-gate change), and the working tree holds another agent's uncommitted native-architecture work (`crates/ultravin/src/lib.rs`, `native_stream.rs`, `crates/ultravin/Cargo.toml`, `uv.lock`, `scripts/bench/native_trials.py`). The loop does not pull, merge, or push. Options: (A) human merges origin/master once that work is committed — recommended, keeps the loop working on current data; (B) leave it, and loop commits pile up on a stale base with a larger merge later.
- The Arrow/Parquet path (`decode_stream`, `decode-parquet`) silently keeps only the first note of each multi-valued free-text field (the names in `ultravin.MULTI_VALUED`, e.g. "Other Trailer Info"), while `decode()` returns all of them as `list[str]`. About 5% of corpus VINs (391 of 7,162) lose notes this way. It is deliberate (`crates/ultravin/src/ids.rs:321` "the first note wins", pinned by `tests/test_parquet.py`), but nothing user-facing says so, and the vision asks for full-field spVinDecode parity. Options: (A) add one README sentence under "Columns and layout" saying columnar output keeps the first note and `decode()` returns all — recommended, cheap and honest, no schema change; (B) emit those fields as `List<Utf8>` columns — faithful, but changes the output schema for existing users; (C) leave it undocumented.

## Leads

Scout findings not yet through the skeptic. Re-verify before acting.

- delete: rejected `batch-slab` / Storage V2 experiment (~1,430 lines: `experimental_batch.rs`, `examples/storage_probe.rs`, `examples/support/allocation_counter.rs`, `scripts/bench/batch_storage*.py` + JSON). Its own doc (`docs/REUSABLE_SLOTS_AND_STORAGE_V2_2026_09_14.md:52`) says it regresses. Blocked while another agent has uncommitted edits in `lib.rs` and `crates/ultravin/Cargo.toml`.
- missing capability: vision line "Decode individual VINs in under one millisecond" has no guard — `benches/decode.rs` asserts nothing, no workflow runs a bench, no timing assertion in any test. A 100x single-VIN regression merges green.
- performance: `matcher::tests::byte_sets_agree_with_regex_over_archive_keys` (`crates/ultravin/src/matcher.rs:659-697`) takes 15.6s of `make check` compiling 56,491 regexes of which 5,802 are distinct; skipping already-seen strings (+3 lines) measured 15.07s -> 1.58s, same assertions.
- simplify/performance: `Makefile:33` clippy `--no-default-features` compiles nothing the default-features and `--no-default-features --features arrow` runs do not already cover (`download-data` is only referenced in `build.rs`; no `not(feature = "arrow")` anywhere); 16s cold. Twin at `.github/workflows/release.yaml:87`.
- simplify: `generate` in `crates/ultravin-py/src/lib.rs:521-530` re-inlines `decode_clock` (lib.rs:469-477) (-9 lines).
- simplify: `check_digit_kernel`'s `pos3` parameter (`crates/ultravin/src/checkdigit.rs:109`) is always `b[2]` of the same slice (-3 lines, -1 param).
