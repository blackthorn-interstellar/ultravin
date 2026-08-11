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


def test_element_144_hashes_as_a_set_not_a_sorted_string() -> None:
    # Element 144's byte order is the dump host's collation, not NHTSA's rules
    # (docs/KNOWN_DEVIATIONS.md #3). The key must not freeze one host's order, or
    # re-pointing the oracle silently invalidates it.
    rows = [{"element_id": 144, "value": "_0129AZ", "attribute_id": "_0129AZ"}]
    shuffled = [{"element_id": 144, "value": "0129AZ_", "attribute_id": "0129AZ_"}]
    assert normalize.collation_agnostic(rows) == normalize.collation_agnostic(shuffled)


def test_element_144_still_compares_its_contents() -> None:
    a = normalize.collation_agnostic([{"element_id": 144, "value": "_0129AZ"}])
    b = normalize.collation_agnostic([{"element_id": 144, "value": "_0129A"}])
    assert a != b


def test_other_elements_keep_their_order() -> None:
    rows = [{"element_id": 143, "value": "BA"}]
    assert normalize.collation_agnostic(rows) == rows


def _write_key(path: Path, pairs: list[tuple[str, str]]) -> Path:
    with path.open("w") as fh:
        fh.write(json.dumps({"month": "m", "artifact_blake3": "art"}) + "\n")
        for vin, digest in pairs:
            fh.write(json.dumps([vin, digest]) + "\n")
    return path


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
