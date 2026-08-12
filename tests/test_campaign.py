import json

from scripts.parity.campaign import _edge_key, _load_cov, _save_cov, is_infra_error


def test_connection_failures_are_infra_not_signal():
    assert is_infra_error({"error": "OperationalError('the connection is closed')"})
    assert is_infra_error({"error": "consuming input failed: server closed the connection unexpectedly"})


def test_real_divergences_and_oracle_errors_are_signal():
    assert not is_infra_error(
        {"error": "InvalidRegularExpression('invalid regular expression: invalid character range')"}
    )
    assert not is_infra_error({"fingerprint": {"field_diffs": [[28, "value", "", "28450"]]}})
    assert not is_infra_error({"error": None})


def test_edge_keys_are_stable_across_processes():
    # Frozen digests: the covfuzz seen-set is persisted and compared across runs, so a
    # per-process randomized hash() would make every restart look like brand-new coverage.
    assert _edge_key(("e", 5, "Conversion 12")) == 2869946280067115319
    assert _edge_key(("p", 1, 2)) == 9742770387217044688
    assert _edge_key(("e", 5, "Conversion 12")) != _edge_key(("e", 5, "Conversion 13"))


def test_coverage_checkpoint_round_trips_through_json(tmp_path):
    covf = tmp_path / "coverage.json"
    assert _load_cov(covf) == {"seen": set(), "corpus": []}
    cov = {"seen": {_edge_key(("p", 1, 2)), _edge_key(("c", ()))}, "corpus": ["1FTFW1ET5DFC10312"]}
    _save_cov(covf, cov)
    assert _load_cov(covf) == cov
    assert json.loads(covf.read_text())["seen"] == sorted(cov["seen"])
