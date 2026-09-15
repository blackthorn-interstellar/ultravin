from __future__ import annotations

import json

from scripts.bench.make_chart import OUT, RESULTS, render


def test_committed_chart_matches_committed_results() -> None:
    assert render(json.loads(RESULTS.read_text())) == OUT.read_text()
