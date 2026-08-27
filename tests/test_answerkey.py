"""The frozen-answer-key machinery (no oracle needed for these)."""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest
import typer
import ultravin

from scripts.parity import answerkey, normalize


def test_a_hash_is_stable_for_the_same_vin() -> None:
    vin = "1HGCM82633A004352"
    assert answerkey.ultravin_hashes([vin]) == answerkey.ultravin_hashes([vin])


def test_different_vins_hash_differently() -> None:
    a, b = ultravin.generate(2, seed=5)
    assert answerkey.ultravin_hashes([a]) != answerkey.ultravin_hashes([b])


def test_batched_hashing_matches_one_at_a_time() -> None:
    vins = ultravin.generate(12, seed=6)
    assert answerkey.ultravin_hashes(vins) == [answerkey.ultravin_hashes([v])[0] for v in vins]


def test_the_corpus_comes_from_the_data_not_from_our_decoder() -> None:
    # The whole point of the key: its VINs are chosen by enumerating NHTSA's
    # rules, never by asking ultravin what it finds interesting. If this ever
    # starts returning the behavioural cover, the key becomes circular.
    corpus = answerkey.corpus(limit=500, shard=0, shards=1)
    assert corpus, "empty corpus"
    assert set(corpus) != set(ultravin.cover_vins())
    assert len(corpus) > len(ultravin.cover_vins())


def test_the_corpus_is_reproducible() -> None:
    # A key built on one machine has to verify on another, so the corpus must be
    # byte-identical run to run — no hash-order dependence anywhere in it.
    assert answerkey.corpus(limit=3000, shard=0, shards=1) == answerkey.corpus(limit=3000, shard=0, shards=1)


def test_sharding_partitions_the_corpus_exactly() -> None:
    whole = answerkey.corpus(limit=2000, shard=0, shards=1)
    parts = [answerkey.corpus(limit=2000, shard=i, shards=4) for i in range(4)]
    assert sum(len(p) for p in parts) == len(whole)
    rejoined = [v for i in range(len(whole)) for v in [parts[i % 4][i // 4]]]
    assert rejoined == whole


def test_the_sample_does_not_depend_on_process_hash() -> None:
    # The equivalence gate freezes the same sample twice, in two separate jobs,
    # and `compare` fails unless both cover the identical VIN set. Python's
    # built-in hash() of a str is randomized per process by PYTHONHASHSEED, so a
    # sampler built on it would pick different VINs on each side. Prove the
    # selection is byte-identical under two different hash seeds — i.e. it does
    # not use hash() — and that a live in-process call agrees with it.
    vins = [f"{i:017d}" for i in range(3000)]
    code = (
        "import json, sys\n"
        "from scripts.parity.answerkey import sample_selected\n"
        "print(json.dumps([v for v in json.load(sys.stdin) if sample_selected(v, 200)]))\n"
    )

    def selected_under(seed: str) -> list[str]:
        proc = subprocess.run(
            [sys.executable, "-c", code],
            input=json.dumps(vins),
            capture_output=True,
            text=True,
            cwd=str(answerkey.REPO),
            env={**os.environ, "PYTHONHASHSEED": seed, "PYTHONPATH": str(answerkey.REPO)},
            check=True,
        )
        return json.loads(proc.stdout)

    under_zero = selected_under("0")
    assert under_zero, "the sample selected nothing"
    assert under_zero == selected_under("12345")
    assert under_zero == [v for v in vins if answerkey.sample_selected(v, 200)]


def test_the_sample_keeps_roughly_one_in_n() -> None:
    vins = ultravin.generate(40_000, seed=11)
    kept = sum(answerkey.sample_selected(v, 200) for v in vins)
    ratio = kept / len(vins)
    # 1/200 = 0.5%. A wide band around it absorbs binomial noise (~5 sigma over
    # 40k draws) so CI never flakes, while a broken sampler — selecting all,
    # none, or on the wrong stride — still lands well outside it.
    assert 0.003 < ratio < 0.007, f"kept {kept}/{len(vins)} = {ratio:.4%}"


def test_the_sample_is_the_same_set_every_time() -> None:
    # Mirrors the compare() invariant: the stock and fast keys must cover the
    # identical VIN set, so the sample has to be reproducible and self-consistent.
    a = answerkey.corpus(limit=6000, shard=0, shards=1, sample_mod=200)
    b = answerkey.corpus(limit=6000, shard=0, shards=1, sample_mod=200)
    assert a == b
    assert a, "the sample selected nothing"
    assert all(answerkey.sample_selected(v, 200) for v in a)
    assert set(a) < set(answerkey.corpus(limit=6000, shard=0, shards=1))


def test_sample_mod_takes_precedence_over_shard() -> None:
    # Documented behavior: when sampling, the shard slice is ignored, so both the
    # stock and fast builds select the same VINs whatever shard defaults are in
    # play. If this ever regressed, the two equivalence keys would diverge.
    unsharded = answerkey.corpus(limit=6000, shard=0, shards=1, sample_mod=200)
    sharded = answerkey.corpus(limit=6000, shard=3, shards=16, sample_mod=200)
    assert unsharded == sharded


def test_a_mismatch_is_reported(tmp_path: Path) -> None:
    vins = ultravin.generate(5, seed=7)
    good = answerkey.ultravin_hashes(vins)
    key = tmp_path / "key.jsonl"
    with key.open("w") as fh:
        fh.write(json.dumps({"month": "x", "artifact_blake3": "y", "count": len(vins)}) + "\n")
        for i, (vin, h) in enumerate(zip(vins, good, strict=True)):
            fh.write(json.dumps([vin, "0" * 16 if i == 2 else h]) + "\n")
    _, entries = answerkey.read_key(key)
    assert answerkey._check_chunk(entries) == [vins[2]]


def test_oracle_failures_are_recorded_not_compared() -> None:
    # A VIN the oracle crashes on has no answer to match; the key records that
    # fact rather than pretending the decoders agreed.
    assert answerkey.KNOWN_DEVIATIONS
    assert all(len(v) >= 8 for v in answerkey.KNOWN_DEVIATIONS)


def test_verify_refuses_a_key_from_another_month(tmp_path: Path) -> None:
    key = tmp_path / "key.jsonl"
    key.write_text(json.dumps({"month": "1999_01", "artifact_blake3": "z", "count": 0}) + "\n")
    with pytest.raises(typer.Exit) as exc:
        answerkey.verify(key=str(key), strict_artifact=True, workers=1)
    assert exc.value.exit_code == 2


def test_element_144_collation_reorder_still_collides() -> None:
    # Element 144 renders each error position as a `(position:charset)` group. The
    # order *within* a charset is the dump host's collation, not NHTSA's rules
    # (docs/KNOWN_DEVIATIONS.md #3): SQL Server sorts `_` before the digits, a
    # codepoint producer sorts it after. Those must normalize equal, or
    # re-pointing the oracle silently invalidates the key.
    rows = [{"element_id": 144, "value": "(6:_123456789)", "attribute_id": "(6:_123456789)"}]
    reordered = [{"element_id": 144, "value": "(6:123456789_)", "attribute_id": "(6:123456789_)"}]
    assert normalize.collation_agnostic(rows) == normalize.collation_agnostic(reordered)


def test_element_144_position_assignment_is_not_collation() -> None:
    # But which charset belongs to which position is data, not collation. Sorting
    # the whole string used to erase it, so `(4:5)(5:4)` and `(4:4)(5:5)` hashed
    # identically and a wrong element-144 output could pass the key. They must
    # normalize (and so hash) differently.
    a = [{"element_id": 144, "value": "(4:5)(5:4)", "attribute_id": "(4:5)(5:4)"}]
    b = [{"element_id": 144, "value": "(4:4)(5:5)", "attribute_id": "(4:4)(5:5)"}]
    assert normalize.collation_agnostic(a) != normalize.collation_agnostic(b)


def test_element_144_still_compares_its_contents() -> None:
    # Different charset *contents* (not just order) must still register as different.
    a = normalize.collation_agnostic([{"element_id": 144, "value": "(6:_123456789)"}])
    b = normalize.collation_agnostic([{"element_id": 144, "value": "(6:_12345678)"}])
    assert a != b


def test_element_144_bare_charset_still_neutralized() -> None:
    # A field with no parenthesized group (a bare charset) is sorted as one set.
    # Element 144 always parenthesizes today, but the neutralization must not
    # depend on that shape.
    a = normalize.collation_agnostic([{"element_id": 144, "value": "_0129AZ"}])
    b = normalize.collation_agnostic([{"element_id": 144, "value": "0129AZ_"}])
    assert a == b


def test_other_elements_keep_their_order() -> None:
    rows = [{"element_id": 143, "value": "BA"}]
    assert normalize.collation_agnostic(rows) == rows


# ------------------------------------------------- the build-time deviation excuse


def _canonical(vin: str) -> list[dict]:
    """ultravin's own canonical rows for a VIN — a stand-in for the oracle's."""
    return normalize.ultravin_rows(ultravin.decode(vin, full=True))


def _diverging(vin: str) -> list[dict]:
    """Rows an oracle would have to have produced for the two to disagree."""
    rows = _canonical(vin)
    return [{**rows[0], "value": "SOMETHING ELSE"}, *rows[1:]]


VIN = "1HGCM82633A004352"

# The canonical->raw column map, so a fixture can hand `_ask` rows in the shape
# `spvindecode` really returns rather than the shape parity has normalized them to.
_ORACLE_COLUMN = {
    "group_name": "groupname",
    "variable": "variable",
    "value": "value",
    "pattern_id": "itempatternid",
    "vin_schema_id": "itemvinschemaid",
    "keys": "itemkeys",
    "element_id": "itemelementid",
    "attribute_id": "itemattributeid",
    "created_on": "itemcreatedon",
    "wmi_id": "itemwmiid",
    "code": "code",
    "data_type": "datatype",
    "decode": "decode",
    "source": "itemsource",
    "to_be_qced": "itemtobeqced",
}


def _as_oracle(rows: list[dict]) -> list[dict]:
    return [{_ORACLE_COLUMN[k]: v for k, v in row.items()} for row in rows]


def _answers(monkeypatch, rows_for) -> None:
    monkeypatch.setattr(answerkey.oracle, "decode", lambda _conn, vin: _as_oracle(rows_for(vin)))


def test_agreement_leaves_nothing_for_the_second_pass(monkeypatch) -> None:
    _answers(monkeypatch, _canonical)
    assert answerkey._ask(VIN) == (VIN, answerkey.ultravin_hashes([VIN])[0], "")


def test_a_divergence_is_handed_to_the_second_pass_with_our_answer(monkeypatch) -> None:
    # The entry stays the oracle's hash for now; the third value is what the
    # freshened-cache pass will compare against, and the only thing that can
    # turn this entry into a `~`.
    rows = _diverging(VIN)
    _answers(monkeypatch, lambda _vin: rows)
    vin, digest, ours = answerkey._ask(VIN)
    assert (vin, digest) == (VIN, answerkey.hash_rows(rows))
    assert ours == answerkey.ultravin_hashes([VIN])[0]


def test_a_per_vin_known_problem_is_never_handed_over(monkeypatch) -> None:
    # scripts/known_problems.json registers those VINs one by one and `verify`
    # skips them; the class excuse must not quietly re-file them as its own.
    _answers(monkeypatch, _diverging)
    vin = min(answerkey.KNOWN_DEVIATIONS)
    _, digest, ours = answerkey._ask(vin)
    assert ours == ""
    assert digest == answerkey.hash_rows(_diverging(vin))


def test_an_oracle_crash_is_recorded_not_raised(monkeypatch) -> None:
    def boom(_conn, _vin):
        msg = "server closed the connection"
        raise RuntimeError(msg)

    monkeypatch.setattr(answerkey.oracle, "decode", boom)
    assert answerkey._ask(VIN) == (VIN, answerkey.UNANSWERED + "RuntimeError", "")


class _NullConn:
    """Enough connection for `excused_by_a_freshened_cache` to hand onwards."""

    def rollback(self) -> None:
        return None

    def close(self) -> None:
        return None


def _freshened(monkeypatch, rows_by_vin: dict[str, list[dict]], drift: list[str] | None = None) -> None:
    monkeypatch.setattr(answerkey.oracle, "connect", lambda **_: _NullConn())
    monkeypatch.setattr(answerkey.stale_cache, "stale_cells_of", lambda *_a, **_k: ({("MLH", 2019): []}, drift or []))
    monkeypatch.setattr(
        answerkey.stale_cache,
        "counterfactual_rows",
        lambda _conn, vins, _stale, **_k: iter([(v, rows_by_vin[v]) for v in vins]),
    )


def test_only_a_byte_for_byte_match_earns_the_excuse(monkeypatch) -> None:
    # The whole classifier: the freshened oracle either reproduces ultravin
    # exactly or it does not explain the divergence. No partial credit.
    a, b = sorted(ultravin.generate(2, seed=31))
    unresolved = dict(zip([a, b], answerkey.ultravin_hashes([a, b]), strict=True))
    _freshened(monkeypatch, {a: _canonical(a), b: _diverging(b)})
    assert answerkey.excused_by_a_freshened_cache(unresolved) == {a}


def test_a_cell_list_that_drifts_from_the_dump_stops_the_build(monkeypatch) -> None:
    # Excusing on the strength of a stale cell nobody recorded would make the
    # committed list — which every other gate reasons about — a fiction.
    a = ultravin.generate(1, seed=32)[0]
    unresolved = {a: answerkey.ultravin_hashes([a])[0]}
    _freshened(monkeypatch, {a: _canonical(a)}, drift=["1 cell(s) not listed as stale: [('MLH', 2019)]"])
    with pytest.raises(typer.Exit) as exc:
        answerkey.excused_by_a_freshened_cache(unresolved)
    assert exc.value.exit_code == 2


def _write_key(path: Path, pairs: list[tuple[str, str]], **header) -> Path:
    with path.open("w") as fh:
        fh.write(json.dumps({"month": "m", "artifact_blake3": "art", **header}) + "\n")
        for vin, digest in pairs:
            fh.write(json.dumps([vin, digest]) + "\n")
    return path


def _real_key(path: Path, pairs: list[tuple[str, str]]) -> Path:
    """A key `verify` will accept as built against the data this tree pins."""
    manifest = json.loads(answerkey.MANIFEST.read_text())
    return _write_key(path, pairs, month=manifest["month"], artifact_blake3=manifest["artifact_blake3"])


def test_a_key_that_mixes_builds_is_refused(tmp_path: Path) -> None:
    key = tmp_path / "key.jsonl"
    with key.open("w") as fh:
        fh.write(json.dumps({"month": "m", "artifact_blake3": "art"}) + "\n")
        fh.write(json.dumps({"month": "m", "artifact_blake3": "OTHER"}) + "\n")
    with pytest.raises(ValueError, match="mixes shards from different builds"):
        answerkey.read_key(key)


def test_verify_counts_a_pinned_deviation_and_still_passes(tmp_path: Path, capsys) -> None:
    vins = ultravin.generate(4, seed=21)
    hashes = answerkey.ultravin_hashes(vins)
    pairs = [(v, (answerkey.DEVIATION + h) if i == 1 else h) for i, (v, h) in enumerate(zip(vins, hashes, strict=True))]
    key = _real_key(tmp_path / "key.jsonl", pairs)
    answerkey.verify(key=str(key), strict_artifact=True, workers=1)  # no raise == green
    out = capsys.readouterr().out
    assert "1 pinned as documented deviations" in out
    assert "every answer matches" in out


def test_a_pinned_deviation_that_moved_still_fails(tmp_path: Path, capsys) -> None:
    # `~` is a regression pin, not a skip: the VIN's decode is still checked, and
    # a change to ultravin's own answer for it is still a failure.
    vins = ultravin.generate(3, seed=22)
    hashes = answerkey.ultravin_hashes(vins)
    pairs = [(vins[0], hashes[0]), (vins[1], answerkey.DEVIATION + "0" * 16), (vins[2], hashes[2])]
    key = _real_key(tmp_path / "key.jsonl", pairs)
    with pytest.raises(typer.Exit) as exc:
        answerkey.verify(key=str(key), strict_artifact=True, workers=1)
    assert exc.value.exit_code == 1
    err = capsys.readouterr().err
    assert "1 MISMATCH against a pinned documented deviation" in err
    assert vins[1] in err
    assert "the oracle's frozen answer" not in err


def test_verify_reports_the_two_kinds_of_failure_apart(tmp_path: Path, capsys) -> None:
    vins = ultravin.generate(3, seed=23)
    hashes = answerkey.ultravin_hashes(vins)
    pairs = [(vins[0], "0" * 16), (vins[1], answerkey.DEVIATION + "0" * 16), (vins[2], hashes[2])]
    key = _real_key(tmp_path / "key.jsonl", pairs)
    with pytest.raises(typer.Exit) as exc:
        answerkey.verify(key=str(key), strict_artifact=True, workers=1)
    assert exc.value.exit_code == 1
    err = capsys.readouterr().err
    assert "1 MISMATCH against the oracle's frozen answer" in err
    assert "1 MISMATCH against a pinned documented deviation" in err


def test_compare_agrees_on_identical_keys(tmp_path: Path, capsys) -> None:
    a = _write_key(tmp_path / "a.jsonl", [("V1", "h1"), ("V2", "h2")])
    b = _write_key(tmp_path / "b.jsonl", [("V1", "h1"), ("V2", "h2")])
    answerkey.compare(a=str(a), b=str(b))  # no raise == agreement
    assert "agree on every VIN" in capsys.readouterr().out


def test_compare_flags_a_hash_mismatch(tmp_path: Path) -> None:
    a = _write_key(tmp_path / "a.jsonl", [("V1", "h1"), ("V2", "h2")])
    b = _write_key(tmp_path / "b.jsonl", [("V1", "h1"), ("V2", "CHANGED")])
    with pytest.raises(typer.Exit) as exc:
        answerkey.compare(a=str(a), b=str(b))
    assert exc.value.exit_code == 1


def test_compare_rejects_duplicate_vins(tmp_path: Path, capsys) -> None:
    # dict() would silently drop the duplicate; a malformed key must fail, not pass.
    a = _write_key(tmp_path / "a.jsonl", [("V1", "h1"), ("V1", "h1"), ("V2", "h2")])
    b = _write_key(tmp_path / "b.jsonl", [("V1", "h1"), ("V2", "h2")])
    with pytest.raises(typer.Exit) as exc:
        answerkey.compare(a=str(a), b=str(b))
    assert exc.value.exit_code == 2
    err = capsys.readouterr().err
    assert "duplicate" in err
    assert "V1" in err


def test_compare_fails_when_one_side_drops_rows(tmp_path: Path, capsys) -> None:
    # The exact regression: a rewrite that drops a VIN used to pass on the overlap.
    a = _write_key(tmp_path / "a.jsonl", [("V1", "h1"), ("V2", "h2"), ("V3", "h3")])
    b = _write_key(tmp_path / "b.jsonl", [("V1", "h1"), ("V2", "h2")])
    with pytest.raises(typer.Exit) as exc:
        answerkey.compare(a=str(a), b=str(b))
    assert exc.value.exit_code == 1
    assert "V3" in capsys.readouterr().err


def test_compare_rejects_keys_with_no_overlap(tmp_path: Path) -> None:
    a = _write_key(tmp_path / "a.jsonl", [("V1", "h1")])
    b = _write_key(tmp_path / "b.jsonl", [("V2", "h2")])
    with pytest.raises(typer.Exit) as exc:
        answerkey.compare(a=str(a), b=str(b))
    assert exc.value.exit_code == 2
