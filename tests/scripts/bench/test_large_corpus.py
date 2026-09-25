from __future__ import annotations

import json
from collections.abc import Iterator
from datetime import datetime
from pathlib import Path

import pytest

from scripts.bench.large_corpus import (
    FROZEN_NOW,
    INVALID_RATE,
    build,
    check_digit,
    check_digit_failures,
    collect_unique_vins,
)


def vins() -> Iterator[str]:
    for number in range(100):
        yield f"1HGCM8263{number:08d}"


def test_collection_deduplicates_and_advances_seed() -> None:
    source = vins()
    calls: list[tuple[int, int, datetime]] = []

    def generate(n: int, *, seed: int, now: datetime, invalid_rate: float) -> list[str]:
        assert invalid_rate == INVALID_RATE
        calls.append((n, seed, now))
        if len(calls) == 1:
            vin = next(source)
            return [vin, vin]
        return [next(source) for _ in range(n)]

    result, chunks, candidates = collect_unique_vins(
        4, seed=9, now=FROZEN_NOW, chunk_size=2, max_chunks=4, generate=generate
    )

    assert len(result) == len(set(result)) == 4
    assert [call[1] for call in calls] == [9, 10, 11]
    assert all(call[2] == FROZEN_NOW for call in calls)
    assert chunks == 3
    assert candidates == 5


def test_short_generator_cannot_silently_underfill() -> None:
    with pytest.raises(RuntimeError, match="only 0 unique VINs after 3 chunks"):
        collect_unique_vins(
            2,
            seed=1,
            now=FROZEN_NOW,
            chunk_size=2,
            max_chunks=3,
            generate=lambda n, *, seed, now, invalid_rate: [],
        )


@pytest.mark.parametrize("vin", ["SHORT", "1HGCM8263IA004352", "1HGCM8263LA00435!"])
def test_invalid_generator_output_fails(vin: str) -> None:
    with pytest.raises(ValueError, match="generator returned invalid VIN"):
        collect_unique_vins(
            1,
            seed=1,
            now=FROZEN_NOW,
            chunk_size=1,
            max_chunks=1,
            generate=lambda n, *, seed, now, invalid_rate: [vin],
        )


def test_build_writes_exact_corpus_and_hashed_manifest(tmp_path: Path) -> None:
    generated = iter(["1HGCM82633A004352", "1M8GDM9AXKP042788"])
    out = tmp_path / "corpus.txt"
    manifest = tmp_path / "manifest.json"

    metadata = build(
        count=2,
        seed=7,
        chunk_size=2,
        max_chunks=1,
        out=out,
        manifest=manifest,
        generate=lambda n, *, seed, now, invalid_rate: [next(generated) for _ in range(n)],
    )

    assert out.read_text().splitlines() == ["1HGCM82633A004352", "1M8GDM9AXKP042788"]
    assert json.loads(manifest.read_text()) == metadata
    assert metadata["count"] == 2
    assert metadata["distinct_rows"] == 2
    assert len(metadata["corpus"]["sha256"]) == 64
    assert metadata["diversity"] == {
        "distinct_descriptor_year_keys": 2,
        "distinct_wmis": 2,
        "distinct_year_characters": 2,
    }
    assert metadata["verification"] == {"unique": True, "vin_alphabet": True}
    assert metadata["check_digit_failures"] == 0


def test_check_digit_accepts_known_digits_and_counts_wrong_ones() -> None:
    assert check_digit("1HGCM82633A004352") == "3"
    assert check_digit("1M8GDM9AXKP042788") == "X"
    assert check_digit_failures(["1HGCM82633A004352", "1HGCM82643A004352"]) == 1


def test_build_rejects_colliding_output_paths_before_generation(tmp_path: Path) -> None:
    path = tmp_path / "same"
    called = False

    def generate(n: int, *, seed: int, now: datetime, invalid_rate: float) -> list[str]:
        nonlocal called
        called = True
        return []

    with pytest.raises(ValueError, match="paths must differ"):
        build(count=1, out=path, manifest=path, generate=generate)
    assert not called
