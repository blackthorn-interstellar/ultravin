"""Render the throughput chart (assets/benchmark.svg) from results.

Horizontal bars on a linear x-axis: the engines span ~3 orders of magnitude, so
every SQL procedure collapses to a sliver next to ultravin — which is the honest
visual of how far ahead the in-process engine is. Reads results from
scripts/bench/results.json: {engine: vins_per_second}. ultravin rows are
highlighted.

Usage: python -m scripts.bench.make_chart
"""

from __future__ import annotations

import json
from pathlib import Path

RESULTS = Path(__file__).parent / "results.json"
OUT = Path(__file__).resolve().parents[2] / "assets" / "benchmark.svg"

# (label, results-key, highlighted?) top -> bottom, fastest first.
ROWS = [
    ("ultravin — batched (10 cores)", "ultravin-batch", True),
    ("ultravin — 1 core", "ultravin", True),
    ("corgi v3", "corgi-v3", False),
    ("corgi v2", "corgi-v2", False),
    ("NHTSA MSSQL", "mssql", False),
    ("NHTSA Postgres", "postgres", False),
    ("NHTSA vPIC API (rate limit)", "nhtsa-api", False),
]

# Linear axis: 0 .. 120,000 VIN/s.
AXIS_MAX = 120_000
TICKS = [
    (0, "0"),
    (20_000, "20k"),
    (40_000, "40k"),
    (60_000, "60k"),
    (80_000, "80k"),
    (100_000, "100k"),
    (120_000, "120k"),
]

X0, X1 = 220, 690  # plot area (px); X0 leaves room for the longest label
WIDTH = X1 + 50
ROW_H, TOP = 30, 16
BAR_H = 18


def human(n: float) -> str:
    if n >= 1_000:
        return f"{n:,.0f}"
    if n >= 100:
        return f"{n:.0f}"
    return f"{n:.1f}".rstrip("0").rstrip(".")


def x(value: float) -> float:
    frac = min(1.0, value / AXIS_MAX)
    return X0 + frac * (X1 - X0)


def main() -> int:
    data = json.loads(RESULTS.read_text())
    rows = [(lbl, data[key], hi) for lbl, key, hi in ROWS if key in data]
    height = TOP + len(rows) * ROW_H + 42

    s: list[str] = []
    s.append(
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {WIDTH} {height}" '
        "font-family=\"-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, "
        'Helvetica, Arial, sans-serif">'
    )
    s.append(
        "<style>"
        ".label{fill:#57606a;font-size:13px}.value{fill:#57606a;font-size:13px}"
        ".strong{fill:#1f2328;font-weight:700}.axis{fill:#8c8c98;font-size:11px}"
        ".grid{stroke:#d8dee4;stroke-width:1}.bar{fill:#c3aef5}.bar-hi{fill:#7c4dff}"
        "@media(prefers-color-scheme:dark){"
        ".label,.value{fill:#9198a1}.strong{fill:#f0f6fc}.axis{fill:#7d8590}"
        ".grid{stroke:#30363d}.bar{fill:#6b5bb0}.bar-hi{fill:#a786ff}}"
        "</style>"
    )

    plot_bottom = TOP + len(rows) * ROW_H
    # gridlines
    for val, _ in TICKS:
        gx = x(val)
        s.append(f'<line class="grid" x1="{gx:.1f}" y1="{TOP - 2}" x2="{gx:.1f}" y2="{plot_bottom}"/>')

    for i, (lbl, val, hi) in enumerate(rows):
        cy = TOP + i * ROW_H
        by = cy + (ROW_H - BAR_H) / 2
        text_y = by + BAR_H - 5
        bw = x(val) - X0
        cls = "bar-hi" if hi else "bar"
        lcls = "label strong" if hi else "label"
        vcls = "value strong" if hi else "value"
        s.append(f'<text class="{lcls}" x="{X0 - 10}" y="{text_y:.1f}" text-anchor="end">{lbl}</text>')
        s.append(f'<rect class="{cls}" x="{X0}" y="{by:.1f}" width="{max(bw, 2):.1f}" height="{BAR_H}" rx="2"/>')
        s.append(f'<text class="{vcls}" x="{X0 + bw + 6:.1f}" y="{text_y:.1f}">{human(val)}</text>')

    # axis
    s.append(f'<line class="grid" x1="{X0}" y1="{plot_bottom}" x2="{X1}" y2="{plot_bottom}"/>')
    for val, lab in TICKS:
        gx = x(val)
        s.append(f'<text class="axis" x="{gx:.1f}" y="{plot_bottom + 16}" text-anchor="middle">{lab}</text>')
    s.append(
        f'<text class="axis" x="{(X0 + X1) / 2:.1f}" y="{plot_bottom + 32}" '
        'text-anchor="middle">VINs decoded per second — higher is better</text>'
    )
    s.append("</svg>")
    OUT.write_text("\n".join(s) + "\n")
    print(f"wrote {OUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
