"""Render the throughput chart (assets/benchmark.svg) from results.

Horizontal bars on a linear x-axis: the engines span ~3 orders of magnitude, so
every SQL procedure collapses to a sliver next to ultravin — which is the honest
visual of how far ahead the in-process engine is. ultravin rows are highlighted.

Reads scripts/bench/results.json, which carries both the figures
(`engines`: {engine: vins_per_second}) and the run's `provenance` — commit,
date, machine, corpus. The chart renders that provenance instead of restating
it, so the numbers and their caption cannot drift apart.

Usage: make chart  (or: python -m scripts.bench.make_chart)
"""

from __future__ import annotations

import json
import math
from pathlib import Path

RESULTS = Path(__file__).parent / "results.json"
OUT = Path(__file__).resolve().parents[2] / "assets" / "benchmark.svg"

# (label, results-key, highlighted?) top -> bottom: ultravin by core count, then the rest.
ROWS = [
    ("ultravin — 12 cores, sorted", "ultravin-sorted-12", True),
    ("ultravin — 12 cores, random", "ultravin-auto-12", True),
    ("ultravin — 4 cores, sorted", "ultravin-sorted-4", True),
    ("ultravin — 4 cores, random", "ultravin-batch", True),
    ("ultravin — 1 core, sorted", "ultravin-sorted-1", True),
    ("ultravin — 1 core, random", "ultravin", True),
    ("corgi v3", "corgi-v3", False),
    ("corgi v2", "corgi-v2", False),
    ("NHTSA MSSQL", "mssql", False),
    ("NHTSA Postgres", "postgres", False),
    ("NHTSA vPIC API (rate limit)", "nhtsa-api", False),
]

X0, X1 = 220, 690  # plot area (px); X0 leaves room for the longest label
WIDTH = X1 + 70  # leave room for the value label beside a bar near AXIS_MAX
ROW_H, TOP = 30, 16
BAR_H = 18


def human(n: float) -> str:
    if n >= 1_000:
        return f"{n:,.0f}"
    if n >= 100:
        return f"{n:.0f}"
    return f"{n:.1f}".rstrip("0").rstrip(".")


def tick_label(value: int) -> str:
    if value == 0:
        return "0"
    if value >= 1_000_000:
        return f"{value / 1_000_000:g}M"
    return f"{value // 1000}k"


def axis(peak: float) -> tuple[int, int]:
    """Smallest round gridline step that keeps the axis to at most 13 labels."""
    steps = (50_000, 100_000, 250_000, 500_000, 1_000_000)
    step = next((s for s in steps if peak / s <= 12), 1_000_000)
    return step, max(step, math.ceil(peak / step) * step)


def x(value: float, axis_max: int) -> float:
    frac = min(1.0, value / axis_max)
    return X0 + frac * (X1 - X0)


def render(data: dict) -> str:
    """Return the SVG text for one parsed results.json."""
    prov, engines = data["provenance"], data["engines"]
    rows = [(lbl, engines[key], hi) for lbl, key, hi in ROWS if key in engines]
    step, axis_max = axis(max(value for _, value, _ in rows))
    ticks = [(value, tick_label(value)) for value in range(0, axis_max + 1, step)]
    height = TOP + len(rows) * ROW_H + 42

    s: list[str] = []
    s.append(
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {WIDTH} {height}" '
        "font-family=\"-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, "
        'Helvetica, Arial, sans-serif">'
    )
    s.append(
        f"<desc>Ultravin: {prov['date']}, {prov['machine']}, {prov['corpus']}; "
        "Ultravin rows decode the corpus in random or sorted order, as marked, "
        "at the worker count shown. "
        "Other engines retain historical comparison figures.</desc>"
    )
    s.append(
        "<style>"
        ".background{fill:#ffffff}.label{fill:#57606a;font-size:13px}.value{fill:#57606a;font-size:13px}"
        ".strong{fill:#1f2328;font-weight:700}.axis{fill:#8c8c98;font-size:11px}"
        ".grid{stroke:#d8dee4;stroke-width:1}.bar{fill:#c3aef5}.bar-hi{fill:#7c4dff}"
        "@media(prefers-color-scheme:dark){"
        ".background{fill:#0d1117}.label,.value{fill:#9198a1}.strong{fill:#f0f6fc}.axis{fill:#7d8590}"
        ".grid{stroke:#30363d}.bar{fill:#6b5bb0}.bar-hi{fill:#a786ff}}"
        "</style>"
    )

    s.append(f'<rect class="background" width="{WIDTH}" height="{height}" rx="8"/>')

    plot_bottom = TOP + len(rows) * ROW_H
    # gridlines
    for val, _ in ticks:
        gx = x(val, axis_max)
        s.append(f'<line class="grid" x1="{gx:.1f}" y1="{TOP - 2}" x2="{gx:.1f}" y2="{plot_bottom}"/>')

    for i, (lbl, val, hi) in enumerate(rows):
        cy = TOP + i * ROW_H
        by = cy + (ROW_H - BAR_H) / 2
        text_y = by + BAR_H - 5
        bw = x(val, axis_max) - X0
        cls = "bar-hi" if hi else "bar"
        lcls = "label strong" if hi else "label"
        vcls = "value strong" if hi else "value"
        s.append(f'<text class="{lcls}" x="{X0 - 10}" y="{text_y:.1f}" text-anchor="end">{lbl}</text>')
        s.append(f'<rect class="{cls}" x="{X0}" y="{by:.1f}" width="{max(bw, 2):.1f}" height="{BAR_H}" rx="2"/>')
        s.append(f'<text class="{vcls}" x="{X0 + bw + 6:.1f}" y="{text_y:.1f}">{human(val)}</text>')

    # axis
    s.append(f'<line class="grid" x1="{X0}" y1="{plot_bottom}" x2="{X1}" y2="{plot_bottom}"/>')
    for val, lab in ticks:
        gx = x(val, axis_max)
        s.append(f'<text class="axis" x="{gx:.1f}" y="{plot_bottom + 16}" text-anchor="middle">{lab}</text>')
    s.append(
        f'<text class="axis" x="{(X0 + X1) / 2:.1f}" y="{plot_bottom + 32}" '
        'text-anchor="middle">VINs decoded per second — higher is better</text>'
    )
    s.append("</svg>")
    return "\n".join(s) + "\n"


def main() -> int:
    OUT.write_text(render(json.loads(RESULTS.read_text())))
    print(f"wrote {OUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
