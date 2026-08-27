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

from scripts.parity import answerkey, normalize, stale_cache


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
    """Rows an oracle would have to have produced to disagree about the *vehicle*.

    Deliberately not an error element: this is the clean-decode shape, the one
    the policy boundary says a machine may not excuse on its own.
    """
    rows = _canonical(vin)
    i = next(i for i, r in enumerate(rows) if r["element_id"] not in normalize.ERROR_ELEMENTS)
    return [*rows[:i], {**rows[i], "value": "SOMETHING ELSE"}, *rows[i + 1 :]]


def _diverging_error_field(vin: str) -> list[dict]:
    """The same, but confined to the error/correction elements the cache feeds.

    A changed value rather than an added row: `diff_rows` compares the two
    GroupName-rank *sequences*, so a row present on one side only makes
    `order_ok` false and no diff of that shape is ever read as narrow.
    """
    rows = _canonical(vin)
    i = next(i for i, r in enumerate(rows) if r["element_id"] in normalize.ERROR_ELEMENTS)
    return [*rows[:i], {**rows[i], "value": "SOMETHING ELSE"}, *rows[i + 1 :]]


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
    assert answerkey._ask(VIN) == (VIN, answerkey.ultravin_hashes([VIN])[0], None)


def test_a_divergence_is_described_for_the_second_pass(monkeypatch) -> None:
    # The entry stays the oracle's hash for now; the third value carries what the
    # classifier needs — our answer, and whether the diff stays inside the error
    # elements, which is the one thing only this pass has both sides for.
    rows = _diverging(VIN)
    _answers(monkeypatch, lambda _vin: rows)
    vin, digest, divergence = answerkey._ask(VIN)
    assert (vin, digest) == (VIN, answerkey.hash_rows(rows))
    assert divergence is not None
    assert divergence.ours == answerkey.ultravin_hashes([VIN])[0]


def test_the_error_field_scope_is_decided_where_both_sides_exist(monkeypatch) -> None:
    # A difference on an error element is inside the class's blast radius; one on
    # a vehicle element is a clean-decode deviation and needs a human.
    def scope(rows_for) -> bool:
        _answers(monkeypatch, rows_for)
        divergence = answerkey._ask(VIN)[2]
        assert divergence is not None, "the fixture did not diverge"
        return divergence.error_fields_only

    assert scope(_diverging_error_field)
    assert not scope(_diverging)


def test_a_registered_vin_is_still_described(monkeypatch) -> None:
    # It used to be dropped here. The registry is what decides a *clean-decode*
    # divergence, and only the classifier knows whether this is one — so the
    # judgement belongs there, with the evidence.
    _answers(monkeypatch, _diverging)
    vin = min(answerkey.KNOWN_DEVIATIONS)
    assert answerkey._ask(vin)[2] is not None


def test_an_oracle_crash_is_recorded_not_raised(monkeypatch) -> None:
    def boom(_conn, _vin):
        msg = "server closed the connection"
        raise RuntimeError(msg)

    monkeypatch.setattr(answerkey.oracle, "decode", boom)
    assert answerkey._ask(VIN) == (VIN, answerkey.UNANSWERED + "RuntimeError", None)


class _NullConn:
    """Enough connection for `classify` to hand onwards."""

    def rollback(self) -> None:
        return None

    def close(self) -> None:
        return None


def _classifier(
    monkeypatch,
    freshened: dict[str, list[dict]],
    drift: list[str] | None = None,
    repin: str = "not-this-class",
) -> None:
    monkeypatch.setattr(answerkey.oracle, "connect", lambda **_: _NullConn())
    monkeypatch.setattr(answerkey.oracle, "decode", lambda _conn, vin: _as_oracle(_canonical(vin)))
    monkeypatch.setattr(answerkey.stale_cache, "stale_cells_of", lambda *_a, **_k: ({("MLH", 2019): []}, drift or []))
    monkeypatch.setattr(answerkey.stale_cache, "repin_verdict", lambda *_a, **_k: repin)
    monkeypatch.setattr(
        answerkey.stale_cache,
        "counterfactual_rows",
        lambda _conn, vins, _stale, **_k: iter([(v, freshened[v]) for v in vins]),
    )


def _diverged(vin: str, *, error_fields_only: bool) -> answerkey.Divergence:
    return answerkey.Divergence(answerkey.ultravin_hashes([vin])[0], error_fields_only)


def test_an_error_field_divergence_the_cache_explains_is_machine_excused(monkeypatch) -> None:
    a = ultravin.generate(1, seed=41)[0]
    _classifier(monkeypatch, {a: _canonical(a)})
    assert answerkey.classify({a: _diverged(a, error_fields_only=True)}) == {a: answerkey.MACHINE_EXCUSED}


def test_nothing_the_freshened_cache_fails_to_reproduce_is_excused(monkeypatch) -> None:
    # The counterfactual comes first and outranks every other consideration: an
    # error-field diff the cache does not account for is still a hard mismatch.
    a = ultravin.generate(1, seed=42)[0]
    _classifier(monkeypatch, {a: _diverging(a)})
    verdicts = answerkey.classify({a: _diverged(a, error_fields_only=True)})
    assert verdicts == {a: answerkey.NOT_CACHE_CAUSED}
    assert answerkey.NOT_CACHE_CAUSED not in answerkey.EXCUSED


def test_a_year_flip_that_collapses_on_the_oracles_year_is_machine_excused(monkeypatch) -> None:
    a = ultravin.generate(1, seed=43)[0]
    _classifier(monkeypatch, {a: _canonical(a)}, repin=stale_cache.COLLAPSED)
    assert answerkey.classify({a: _diverged(a, error_fields_only=False)}) == {a: answerkey.REPIN_EXCUSED}


def test_a_clean_decode_divergence_needs_a_human_even_when_cache_caused(monkeypatch) -> None:
    # The policy boundary. The cache really did cause it, and it is still not the
    # machine's to excuse, because it moved the vehicle rather than the error text.
    a = ultravin.generate(1, seed=44)[0]
    _classifier(monkeypatch, {a: _canonical(a)}, repin=stale_cache.NOT_THIS_CLASS)
    assert answerkey.classify({a: _diverged(a, error_fields_only=False)}) == {a: answerkey.NEEDS_REGISTRATION}
    assert answerkey.NEEDS_REGISTRATION not in answerkey.EXCUSED


def test_the_same_divergence_is_excused_once_a_human_registers_it(monkeypatch) -> None:
    # Read live out of scripts/known_problems.json, never hardcoded: registering
    # the VIN is the whole difference between the previous test and this one.
    a = ultravin.generate(1, seed=45)[0]
    monkeypatch.setattr(answerkey, "KNOWN_DEVIATIONS", frozenset({a}))
    _classifier(monkeypatch, {a: _canonical(a)}, repin=stale_cache.NOT_THIS_CLASS)
    assert answerkey.classify({a: _diverged(a, error_fields_only=False)}) == {a: answerkey.REGISTERED}
    assert answerkey.REGISTERED in answerkey.EXCUSED


def test_a_registered_vin_the_cache_does_not_explain_is_not_pinned(monkeypatch) -> None:
    # Registration says the oracle is wrong here, not that ultravin's answer is
    # frozen. Without the counterfactual there is nothing to pin it to, so the
    # entry stays the oracle's hash and `verify` goes on skipping the VIN.
    a = ultravin.generate(1, seed=46)[0]
    monkeypatch.setattr(answerkey, "KNOWN_DEVIATIONS", frozenset({a}))
    _classifier(monkeypatch, {a: _diverging(a)})
    assert answerkey.classify({a: _diverged(a, error_fields_only=False)}) == {a: answerkey.NOT_CACHE_CAUSED}


def test_a_cell_list_that_drifts_from_the_dump_stops_the_build(monkeypatch) -> None:
    # Excusing on the strength of a stale cell nobody recorded would make the
    # committed list — which every other gate reasons about — a fiction.
    a = ultravin.generate(1, seed=47)[0]
    _classifier(monkeypatch, {a: _canonical(a)}, drift=["1 cell(s) not listed as stale: [('MLH', 2019)]"])
    with pytest.raises(typer.Exit) as exc:
        answerkey.classify({a: _diverged(a, error_fields_only=True)})
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
