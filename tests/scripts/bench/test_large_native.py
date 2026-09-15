import hashlib
import json

import pytest

from scripts.bench.large_native import minimum_unique_seconds, validate_corpus


def test_validate_corpus_checks_unique_valid_vins_and_manifest(tmp_path):
    corpus = tmp_path / "corpus.txt"
    corpus.write_text("1HGCM82633A004352\n1M8GDM9AXKP042788\n")
    digest = hashlib.sha256(corpus.read_bytes()).hexdigest()
    manifest = tmp_path / "manifest.json"
    manifest.write_text(json.dumps({"rows": 2, "distinct_rows": 2, "sha256": digest}))

    facts = validate_corpus(corpus, manifest)

    assert facts["rows"] == 2
    assert facts["distinct_rows"] == 2
    assert facts["sha256"] == digest


def test_validate_corpus_rejects_duplicate(tmp_path):
    corpus = tmp_path / "corpus.txt"
    corpus.write_text("1HGCM82633A004352\n1HGCM82633A004352\n")

    with pytest.raises(ValueError, match="duplicate VIN"):
        validate_corpus(corpus)


def test_validate_corpus_rejects_invalid_check_digit(tmp_path):
    corpus = tmp_path / "corpus.txt"
    corpus.write_text("1HGCM82643A004352\n")

    with pytest.raises(ValueError, match="invalid check digit"):
        validate_corpus(corpus)


def test_minimum_unique_seconds_uses_fastest_sample():
    samples = [{"rows_per_second": 400_000}, {"rows_per_second": 500_000}]

    assert minimum_unique_seconds(5_000_000, samples) == 10.0


@pytest.mark.parametrize("rate", [0, -1, float("nan"), float("inf")])
def test_minimum_unique_seconds_rejects_non_positive_or_non_finite_rates(rate):
    with pytest.raises(ValueError, match="throughput must be positive"):
        minimum_unique_seconds(5_000_000, [{"rows_per_second": rate}])
