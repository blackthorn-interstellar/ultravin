from __future__ import annotations

import json

from scripts.bench.make_chart import API_OUT, API_RESULTS, OUT, RESULTS, render, render_api


def test_committed_chart_matches_committed_results() -> None:
    assert render(json.loads(RESULTS.read_text())) == OUT.read_text()


def test_committed_api_chart_matches_committed_results() -> None:
    assert render_api(json.loads(API_RESULTS.read_text())) == API_OUT.read_text()
