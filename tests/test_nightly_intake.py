"""The covfuzz intake, exactly as `.github/workflows/nightly.yaml` ships it.

The intake is a `python -` heredoc inside the workflow, and it is the one place
that decides whether a fresh oracle-vs-ultravin divergence becomes a backlog
entry an agent spends a night on or is dropped as a documented class. Dropping a
record it could not judge is the failure that matters, and no review of the
classifier modules can rule it out — the decision lives in the YAML. So the
shipped text is extracted and run here, against synthetic records, with the
oracle and the decoder stubbed out.

Extraction is by heredoc delimiter, and `test_the_workflow_holds_exactly_one_intake`
fails the moment nightly.yaml grows a second one, rather than letting these tests
quietly exercise the wrong block.
"""

from __future__ import annotations

import json
import textwrap
from pathlib import Path
from typing import Any

import pytest
import ultravin

from scripts.parity import oracle, stale_cache
from scripts.refresh import KNOWN_DEVIATION_VINS

WORKFLOW = Path(__file__).resolve().parents[1] / ".github" / "workflows" / "nightly.yaml"
_OPEN = "<<'PYEOF'\n"
_CLOSE = "\n          PYEOF\n"


def intake_source() -> str:
    """The intake heredoc's Python, dedented out of the workflow."""
    text = WORKFLOW.read_text()
    start = text.index(_OPEN) + len(_OPEN)
    return textwrap.dedent(text[start : text.index(_CLOSE, start)])


def test_the_workflow_holds_exactly_one_intake() -> None:
    assert WORKFLOW.read_text().count(_OPEN) == 1


class _Conn:
    """Stands in for an oracle connection. Nothing here may reach Postgres."""

    def __init__(self) -> None:
        self.closed = False

    def close(self) -> None:
        self.closed = True


def _never_called(*_args: Any, **_kwargs: Any) -> Any:
    msg = "the counterfactual asked the oracle about a record it should not have"
    raise AssertionError(msg)


def _verdicts(verdicts: dict[str, str], drift: list[str] | None = None) -> Any:
    """A `counterfactual_verdicts` that answers with exactly this."""
    return lambda *_a, **_k: (verdicts, drift or [])


def _excuse_everything_asked(_scan: Any, _conn: Any, vins: list[str], *_a: Any, **_k: Any) -> Any:
    """A `counterfactual_verdicts` that excuses exactly the VINs it is handed."""
    return dict.fromkeys(vins, stale_cache.CACHE_CAUSED), []


@pytest.fixture(autouse=True)
def _stub_the_world(monkeypatch: pytest.MonkeyPatch) -> None:
    """No Postgres, no decoder, and no excuse unless the test asks for one."""
    monkeypatch.setattr(ultravin, "decode", lambda _vin, **_k: {"model_year": 2019})
    monkeypatch.setattr(oracle, "connect", lambda *_a, **_k: _Conn())
    monkeypatch.setattr(stale_cache, "is_expected_divergence", lambda *_a, **_k: False)
    monkeypatch.setattr(stale_cache, "cell_for", lambda *_a, **_k: ("MLH", 2019))
    monkeypatch.setattr(stale_cache, "counterfactual_verdicts", _never_called)


def _record(vin: str, **extra: Any) -> dict[str, Any]:
    """A covfuzz failure record: the error-count flip shape, in campaign's shape."""
    return {
        "vin": vin,
        "engine": "covfuzz",
        "fingerprint": {
            "field_diffs": [[143, "value", "1,3,14", "1,5,14"]],
            "missing": [],
            "extra": [],
            "order_ok": True,
        },
        **extra,
    }


def _run(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    records: list[dict[str, Any]],
    backlog: list[dict[str, Any]] | None = None,
) -> list[dict[str, Any]]:
    """Run the shipped intake over `records`; return the backlog it leaves behind."""
    (tmp_path / "tests").mkdir(exist_ok=True)
    (tmp_path / "campaign").mkdir(exist_ok=True)
    lines = "".join(json.dumps(r) + "\n" for r in backlog or [])
    (tmp_path / "tests" / "parity_backlog.jsonl").write_text(lines)
    (tmp_path / "campaign" / "fails-covfuzz.jsonl").write_text("".join(json.dumps(r) + "\n" for r in records))
    monkeypatch.chdir(tmp_path)
    exec(compile(intake_source(), str(WORKFLOW), "exec"), {"__name__": "__main__"})  # noqa: S102
    filed = (tmp_path / "tests" / "parity_backlog.jsonl").read_text().splitlines()
    return [json.loads(line) for line in filed if line.strip()]


VIN = "MLH00000000000001"
OTHER = "JH200000000000002"


def test_a_contained_divergence_is_dropped_without_asking_the_oracle(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture
) -> None:
    """The cheap path is unchanged and still first: what the cell list already
    contains never reaches the experiment. (`counterfactual_verdicts` raises if
    it is called at all.)"""
    monkeypatch.setattr(stale_cache, "is_expected_divergence", lambda *_a, **_k: True)
    assert _run(tmp_path, monkeypatch, [_record(VIN)]) == []
    out = capsys.readouterr().out
    assert "0 new backlog entries" in out
    assert "1 known stale-cache divergences dropped across 1 cell(s)" in out


def test_a_counterfactual_pass_drops_the_record_under_its_own_counter(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture
) -> None:
    """Containment cannot recognise an error-count flip; the freshened oracle can."""
    monkeypatch.setattr(stale_cache, "counterfactual_verdicts", _verdicts({VIN: stale_cache.CACHE_CAUSED}))
    assert _run(tmp_path, monkeypatch, [_record(VIN)]) == []
    out = capsys.readouterr().out
    assert "1 stale_counterfactual" in out
    assert "0 new backlog entries" in out


def test_a_year_flip_that_collapses_on_the_oracles_year_is_dropped(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture
) -> None:
    monkeypatch.setattr(stale_cache, "counterfactual_verdicts", _verdicts({VIN: stale_cache.CACHE_CAUSED_ON_REPIN}))
    assert _run(tmp_path, monkeypatch, [_record(VIN)]) == []
    assert "1 year-flip repin" in capsys.readouterr().out


def test_a_counterfactual_that_does_not_reproduce_is_filed(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """The experiment is the excuse. Nothing it fails to reproduce is dropped."""
    monkeypatch.setattr(stale_cache, "counterfactual_verdicts", _verdicts({VIN: stale_cache.NOT_CACHE_CAUSED}))
    assert [r["vin"] for r in _run(tmp_path, monkeypatch, [_record(VIN)])] == [VIN]


def test_a_cache_caused_divergence_out_of_scope_is_still_filed(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """Cache-caused is not the same as machine-excusable: a divergence that moved
    the vehicle needs a human and a `scripts/known_problems.json` entry."""
    monkeypatch.setattr(stale_cache, "counterfactual_verdicts", _verdicts({VIN: stale_cache.OUT_OF_SCOPE}))
    assert [r["vin"] for r in _run(tmp_path, monkeypatch, [_record(VIN)])] == [VIN]


def test_drift_disables_the_excuse_for_the_whole_run(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture
) -> None:
    """The cell list describes a dump this is not, so no excuse resting on it is
    sound tonight: every record is filed, the drift is named, and the lane lives."""
    drift = ["1 cell(s) not listed as stale: [('MLH', 2019)]"]
    monkeypatch.setattr(stale_cache, "counterfactual_verdicts", _verdicts({}, drift))
    filed = _run(tmp_path, monkeypatch, [_record(VIN), _record(OTHER)])
    assert [r["vin"] for r in filed] == [VIN, OTHER]
    out = capsys.readouterr().out
    assert "::warning::" in out
    assert "counterfactual excusing disabled for this run" in out
    assert drift[0] in out


def test_an_exception_in_the_counterfactual_files_every_record(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture
) -> None:
    """A dead oracle, a psycopg error, a bug in the classifier — whatever it was,
    the records it was asked about are work, not noise."""

    def _boom(*_a: Any, **_k: Any) -> Any:
        raise RuntimeError("the oracle went away")

    monkeypatch.setattr(stale_cache, "counterfactual_verdicts", _boom)
    filed = _run(tmp_path, monkeypatch, [_record(VIN), _record(OTHER)])
    assert [r["vin"] for r in filed] == [VIN, OTHER]
    assert "::warning::the counterfactual classifier failed, filing 2 record(s)" in capsys.readouterr().out


def test_a_record_the_first_pass_cannot_judge_is_filed_with_a_warning(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture
) -> None:
    """A decode that raises never reaches the experiment: unjudged means filed."""

    def _boom(*_a: Any, **_k: Any) -> Any:
        raise ValueError("not a VIN")

    monkeypatch.setattr(ultravin, "decode", _boom)
    assert [r["vin"] for r in _run(tmp_path, monkeypatch, [_record(VIN)])] == [VIN]
    out = capsys.readouterr().out
    assert f"::warning::unclassifiable '{VIN}'" in out
    assert "1 filed unclassified" in out


def test_records_past_the_vin_cap_are_filed_unexamined(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture
) -> None:
    """The experiment costs live oracle time, so a pathological night is bounded —
    and it is bounded in the safe direction, by filing rather than by dropping."""
    monkeypatch.setattr(stale_cache, "counterfactual_verdicts", _excuse_everything_asked)
    records = [_record(f"MLH{i:014d}") for i in range(501)]  # one WMI, 501 VINs
    assert len(_run(tmp_path, monkeypatch, records)) == 1
    assert "::warning::1 record(s) past the 500-VIN / 100-WMI counterfactual caps" in capsys.readouterr().out


def test_records_past_the_wmi_cap_are_filed_unexamined(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture
) -> None:
    """The freshening scan is per WMI per year against stock procs — the half that
    actually costs the lane its time — so the WMIs are capped as well."""
    monkeypatch.setattr(stale_cache, "counterfactual_verdicts", _excuse_everything_asked)
    records = [_record(f"{i:03d}{i:014d}") for i in range(101)]  # 101 distinct WMIs
    assert len(_run(tmp_path, monkeypatch, records)) == 1
    assert "::warning::1 record(s) past the 500-VIN / 100-WMI counterfactual caps" in capsys.readouterr().out


def test_a_crash_record_never_enters_the_experiment(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture
) -> None:
    """A novel oracle crash carries no diff for a freshened cache to reproduce, so
    it is filed and the oracle is never asked about it — the stubbed
    `counterfactual_verdicts` raises if the run so much as calls it."""
    crash = {"vin": VIN, "engine": "covfuzz", "error": "SomeNovelOracleError: it fell over"}
    assert [r["vin"] for r in _run(tmp_path, monkeypatch, [crash])] == [VIN]
    assert "counterfactual: 0 candidate(s), 0 examined" in capsys.readouterr().out


def test_a_registered_known_deviation_is_dropped_not_refiled(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture
) -> None:
    """A VIN a human already argued in scripts/known_problems.json cost an agent a
    night every time the fuzzer rediscovered it. The registry is read live."""
    assert _run(tmp_path, monkeypatch, [_record(min(KNOWN_DEVIATION_VINS))]) == []
    assert "1 registered known-deviation VINs dropped" in capsys.readouterr().out


def test_the_summary_separates_an_examined_night_from_a_quiet_one(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture
) -> None:
    """Every filed verdict is counted, so "the experiment ran and excused nothing"
    cannot be mistaken for "the experiment never ran"."""
    monkeypatch.setattr(
        stale_cache,
        "counterfactual_verdicts",
        _verdicts({VIN: stale_cache.OUT_OF_SCOPE, OTHER: stale_cache.SHIPPED_AGREES}),
    )
    assert len(_run(tmp_path, monkeypatch, [_record(VIN), _record(OTHER)])) == 2
    out = capsys.readouterr().out
    assert "counterfactual: 2 candidate(s), 2 examined" in out
    assert "1 cache-caused but out of scope" in out
    assert "1 the byte-faithful oracle already agrees with" in out


def test_a_vin_already_in_the_backlog_is_never_filed_twice(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """Dedupe still runs ahead of the experiment, and a VIN seen twice in one
    night is one record — the deferred pass must not reopen either question."""
    monkeypatch.setattr(stale_cache, "counterfactual_verdicts", _verdicts({OTHER: stale_cache.NOT_CACHE_CAUSED}))
    filed = _run(tmp_path, monkeypatch, [_record(VIN), _record(OTHER), _record(OTHER)], backlog=[_record(VIN)])
    assert [r["vin"] for r in filed] == [VIN, OTHER]
