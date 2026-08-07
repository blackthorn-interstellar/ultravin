from scripts.parity.campaign import is_infra_error


def test_connection_failures_are_infra_not_signal():
    assert is_infra_error({"error": "OperationalError('the connection is closed')"})
    assert is_infra_error({"error": "consuming input failed: server closed the connection unexpectedly"})


def test_real_divergences_and_oracle_errors_are_signal():
    assert not is_infra_error(
        {"error": "InvalidRegularExpression('invalid regular expression: invalid character range')"}
    )
    assert not is_infra_error({"fingerprint": {"field_diffs": [[28, "value", "", "28450"]]}})
    assert not is_infra_error({"error": None})
