"""Render the README charts from committed results.

`assets/benchmark.svg` is the headline: ultravin against the other engines, from
scripts/bench/results.json. `assets/api-benchmark.svg` compares ultravin's own
output paths, from scripts/bench/api_compare_2026_11_14.json.

Horizontal bars on a linear x-axis: the engines span ~5 orders of magnitude, so
every SQL procedure collapses to a sliver next to ultravin — which is the honest
visual of how far ahead the in-process engine is. One ultravin row carries every
core count: its bars overlap, the fewest cores darkest and in front, with a
legend for the shades. Each bar's rate is printed on it, or beside the row where
the bars are too close together to hold them.

results.json carries both the figures (`engines`: {engine: vins_per_second}) and
the run's `provenance`, which the chart renders instead of restating, so the
numbers and their caption cannot drift apart.

Usage: make chart  (or: python -m scripts.bench.make_chart)
"""

from __future__ import annotations

import json
import math
from pathlib import Path

BENCH = Path(__file__).parent
RESULTS = BENCH / "results.json"
API_RESULTS = BENCH / "api_compare_2026_11_14.json"
ASSETS = Path(__file__).resolve().parents[2] / "assets"
OUT = ASSETS / "benchmark.svg"
API_OUT = ASSETS / "api-benchmark.svg"

CORES = (1, 4, 8)

# (label, results-key or {cores: results-key}, highlighted?) top -> bottom.
ROWS: list[tuple[str, str | dict[int, str], bool]] = [
    ("ultravin — sorted", {n: f"ultravin-sorted-{n}" for n in CORES}, True),
    ("ultravin — random", {n: f"ultravin-random-{n}" for n in CORES}, True),
    ("corgi v3", "corgi-v3", False),
    ("corgi v2", "corgi-v2", False),
    ("NHTSA MSSQL", "mssql", False),
    ("NHTSA Postgres", "postgres", False),
    ("NHTSA vPIC API (rate limit)", "nhtsa-api", False),
]

# (label, api_compare cell) top -> bottom, fastest at eight cores first.
API_CORPUS = "ny-sorted-2m"
API_ROWS = [
    ("Rust: flat JSON", "rust-json-flat"),
    ("decode-compact (two Parquet files)", "cli-decode-compact"),
    ("Python: decode_batch_json", "py-json-flat"),
    ("Rust: full JSON", "rust-json-full"),
    ("decode-batch --jsonl", "cli-decode-batch"),
    ("Rust: flat results", "rust-batch-flat-read"),
    ("Rust: stream, full results", "rust-native-read"),
    ("Rust: Arrow columns", "rust-arrow"),
    ("Python: decode_stream (Arrow)", "py-stream-arrow"),
    ("Rust: full results", "rust-batch-full-read"),
    ("Python: decode_batch dicts", "py-dicts-flat"),
    ("decode-parquet (one wide file)", "cli-decode-parquet"),
    ("Python: decode_batch full dicts", "py-dicts-full"),
]

Bars = list[tuple[int, float]]  # (cores, VIN/s), fewest cores first; other engines have one bar

X1_PAD = 470  # plot width (px)
ROW_H, TOP = 30, 16
BAR_H = 18
STACK_H = 26  # an overlapping row is taller: it carries a label per core count
CHAR_W = 6.1  # rough width of one 11px character, to place labels and the legend


def human(n: float) -> str:
    if n >= 1_000:
        return f"{n:,.0f}"
    if n >= 100:
        return f"{n:.0f}"
    return f"{n:.1f}".rstrip("0").rstrip(".")


def short(n: float) -> str:
    if n >= 1_000_000:
        return f"{n / 1_000_000:.1f}M"
    return f"{n / 1000:.0f}k"


def legend(counts: list[int], centre: float, y: float) -> list[str]:
    """One swatch per core count, in the bars' own shades, centred under the axis title."""
    # Whole pixels: a float sum lands on rounding ties that Python 3.12 breaks differently.
    widths = [round(16 + len(cores(n)) * CHAR_W) for n in counts]
    lx = centre - (sum(widths) + 14 * (len(counts) - 1)) / 2
    out = []
    for level, (n, width) in enumerate(zip(counts, widths, strict=True)):
        out.append(f'<rect class="s{level}" x="{lx:.1f}" y="{y - 9.5:.1f}" width="11" height="11" rx="2"/>')
        out.append(f'<text class="axis" x="{lx + 16:.1f}" y="{y:.1f}">{cores(n)}</text>')
        lx += width + 14
    return out


def cores(n: int) -> str:
    return "1 core" if n == 1 else f"{n} cores"


def tick_label(value: int) -> str:
    if value == 0:
        return "0"
    if value >= 1_000_000:
        return f"{value / 1_000_000:g}M"
    return f"{value // 1000}k"


def axis(peak: float) -> tuple[int, int]:
    """Smallest round gridline step that keeps the axis to at most 13 labels."""
    steps = (50_000, 100_000, 250_000, 500_000, 1_000_000, 2_500_000, 5_000_000)
    step = next((s for s in steps if peak / s <= 12), 1_000_000)
    return step, max(step, math.ceil(peak / step) * step)


def render(data: dict) -> str:
    """Return the headline SVG for one parsed results.json."""
    prov, engines = data["provenance"], data["engines"]
    rows: list[tuple[str, Bars, bool]] = []
    for lbl, key, hi in ROWS:
        keys = key if isinstance(key, dict) else {0: key}
        bars = sorted((n, engines[k]) for n, k in keys.items() if k in engines)
        if bars:
            rows.append((lbl, bars, hi))
    desc = (
        f"Ultravin: {prov['date']}, {prov['machine']}; sorted is {prov['sorted_corpus']}, "
        f"random is {prov['random_corpus']}. The overlapping bars in a row are the worker counts in the legend. "
        "Other engines retain historical comparison figures."
    )
    return draw(rows, desc, x0=220)


def render_api(data: dict) -> str:
    """Return the API comparison SVG for one api_compare JSON."""
    rates = {(r["cell"], r["workers"]): r["vins_per_second"] for r in data["records"] if r["corpus"] == API_CORPUS}
    rows = [(lbl, [(n, rates[cell, n]) for n in CORES], False) for lbl, cell in API_ROWS]
    desc = (
        f"Ultravin output paths on {API_CORPUS} at commit {data['commit']}: VINs per second with the "
        "output consumed, at the worker counts in the legend."
    )
    return draw(rows, desc, x0=280)


def draw(rows: list[tuple[str, Bars, bool]], desc: str, *, x0: int) -> str:
    x1 = x0 + X1_PAD
    step, axis_max = axis(max(value for _, bars, _ in rows for _, value in bars))
    ticks = [(value, tick_label(value)) for value in range(0, axis_max + 1, step)]
    counts = sorted({n for _, bars, _ in rows if len(bars) > 1 for n, _ in bars})

    def x(value: float) -> float:
        return x0 + min(1.0, value / axis_max) * (x1 - x0)

    def inside(bars: Bars) -> bool:
        """Whether every bar's rate fits on the part of it the bars in front leave showing."""
        ends = [float(x0)] + [x(value) for _, value in bars]
        return all(end - start >= len(short(v)) * CHAR_W + 10 for start, end, (_, v) in zip(ends, ends[1:], bars))

    # Rates go on the bars only if they fit on every row; otherwise beside every row.
    on_bars = all(inside(bars) for _, bars, _ in rows if len(bars) > 1)
    beside = 0 if on_bars else max(sum(len(short(v)) + 3 for _, v in bars) for _, bars, _ in rows) * (CHAR_W + 0.6)
    width = x1 + max(70, int(beside) + 10)
    height = TOP + len(rows) * ROW_H + (64 if counts else 42)

    s: list[str] = []
    s.append(
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" '
        "font-family=\"-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, "
        'Helvetica, Arial, sans-serif">'
    )
    s.append(f"<desc>{desc}</desc>")
    s.append(
        "<style>"
        ".background{fill:#ffffff}.label{fill:#57606a;font-size:13px}.value{fill:#57606a;font-size:13px}"
        ".strong{fill:#1f2328;font-weight:700}.axis{fill:#8c8c98;font-size:11px}"
        ".grid{stroke:#d8dee4;stroke-width:1}.bar{fill:#c3aef5}.bar-hi{fill:#7c4dff}"
        ".s0{fill:#4a1fd6}.s1{fill:#7c4dff}.s2{fill:#b9a4ff}.s3{fill:#ddd3ff}"
        ".on{fill:#ffffff;font-size:11px;font-weight:600}.on2,.on3{fill:#2a1466}"
        ".off{fill:#1f2328;font-size:12px;font-weight:600}"
        "@media(prefers-color-scheme:dark){"
        ".background{fill:#0d1117}.label,.value{fill:#9198a1}.strong,.off{fill:#f0f6fc}.axis{fill:#7d8590}"
        ".grid{stroke:#30363d}.bar{fill:#6b5bb0}.bar-hi{fill:#a786ff}"
        ".s0{fill:#5b33e0}.s1{fill:#8a63ff}.s2{fill:#b9a4ff}.s3{fill:#ddd3ff}}"
        "</style>"
    )
    s.append(f'<rect class="background" width="{width}" height="{height}" rx="8"/>')

    plot_bottom = TOP + len(rows) * ROW_H
    for val, _ in ticks:
        gx = x(val)
        s.append(f'<line class="grid" x1="{gx:.1f}" y1="{TOP - 2}" x2="{gx:.1f}" y2="{plot_bottom}"/>')

    for i, (lbl, bars, hi) in enumerate(rows):
        cy = TOP + i * ROW_H
        bar_h = STACK_H if len(bars) > 1 else BAR_H
        by = cy + (ROW_H - bar_h) / 2
        text_y = cy + ROW_H / 2 + 4.5
        peak = max(value for _, value in bars)
        bw = x(peak) - x0
        lcls = "label strong" if hi else "label"
        s.append(f'<text class="{lcls}" x="{x0 - 10}" y="{text_y:.1f}" text-anchor="end">{lbl}</text>')
        if len(bars) == 1:
            cls, vcls = ("bar-hi", "value strong") if hi else ("bar", "value")
            s.append(f'<rect class="{cls}" x="{x0}" y="{by:.1f}" width="{max(bw, 2):.1f}" height="{bar_h}" rx="2"/>')
            s.append(f'<text class="{vcls}" x="{x0 + bw + 6:.1f}" y="{text_y:.1f}">{human(peak)}</text>')
            continue
        # Widest first, so a shorter bar sits in front of a longer one.
        for level in sorted(range(len(bars)), key=lambda k: -bars[k][1]):
            end = x(bars[level][1])
            s.append(f'<rect class="s{level}" x="{x0}" y="{by:.1f}" width="{end - x0:.1f}" height="{bar_h}" rx="2"/>')
            if on_bars:
                s.append(
                    f'<text class="on on{level}" x="{end - 5:.1f}" y="{text_y - 1:.1f}" '
                    f'text-anchor="end">{short(bars[level][1])}</text>'
                )
        if not on_bars:
            rates = " · ".join(short(value) for _, value in bars)
            s.append(f'<text class="off" x="{x0 + bw + 6:.1f}" y="{text_y - 1:.1f}">{rates}</text>')

    s.append(f'<line class="grid" x1="{x0}" y1="{plot_bottom}" x2="{x1}" y2="{plot_bottom}"/>')
    for val, lab in ticks:
        s.append(f'<text class="axis" x="{x(val):.1f}" y="{plot_bottom + 16}" text-anchor="middle">{lab}</text>')
    s.append(
        f'<text class="axis" x="{(x0 + x1) / 2:.1f}" y="{plot_bottom + 32}" '
        'text-anchor="middle">VINs decoded per second — higher is better</text>'
    )
    s.extend(legend(counts, (x0 + x1) / 2, plot_bottom + 52) if counts else [])
    s.append("</svg>")
    return "\n".join(s) + "\n"


def main() -> int:
    OUT.write_text(render(json.loads(RESULTS.read_text())))
    API_OUT.write_text(render_api(json.loads(API_RESULTS.read_text())))
    print(f"wrote {OUT} and {API_OUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
