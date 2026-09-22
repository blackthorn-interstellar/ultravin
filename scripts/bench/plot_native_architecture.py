"""Plot native ordered-delivery controls and instrumented decode stages as SVG.

Run with:
UV_FROZEN=1 uv run --frozen --with matplotlib python -m scripts.bench.plot_native_architecture
"""

from __future__ import annotations

import json
from collections import defaultdict
from pathlib import Path
from typing import Annotated, Any, NoReturn

import typer

from scripts.bench.end_to_end import ROOT

DEFAULT_GLOB = "native_arch_*ceilings*.json"
DEFAULT_STAGE = ROOT / "scripts/bench/native_arch_stages_2026_09_15.json"
DEFAULT_OUTPUT = ROOT / "docs/figures/native-architecture-diagnostics.svg"
COLORS = {"ordered": "#2563eb", "local": "#e8792e"}


def _error(message: str) -> NoReturn:
    raise ValueError(message)


def _load(path: Path) -> dict[str, Any]:
    data = json.loads(path.read_text())
    if not isinstance(data, dict):
        _error(f"expected a JSON object: {path}")
    return data


def _throughput_samples(paths: list[Path]) -> dict[tuple[str, int], list[float]]:
    grouped: dict[tuple[str, int], list[float]] = defaultdict(list)
    candidate_shas: set[str] = set()
    for path in paths:
        evidence = _load(path)
        if evidence.get("status") != "complete":
            _error(f"ceiling evidence is not complete: {path}")
        candidate_sha = evidence.get("candidate", {}).get("binary_sha256")
        if not isinstance(candidate_sha, str) or not candidate_sha:
            _error(f"ceiling evidence has no candidate binary hash: {path}")
        candidate_shas.add(candidate_sha)
        for sample in evidence.get("samples", []):
            mode = sample.get("mode")
            workers = sample.get("workers")
            measurement = sample.get("measurement", {})
            if mode not in COLORS or workers not in (1, 8, 12):
                _error(f"unexpected control sample in {path}: {mode=}, {workers=}")
            if measurement.get("rows") != evidence.get("corpus", {}).get("rows"):
                _error(f"sample does not cover the recorded corpus: {path}")
            rate = measurement.get("rows_per_second")
            if not isinstance(rate, (int, float)) or rate <= 0:
                _error(f"invalid throughput sample in {path}")
            grouped[(mode, workers)].append(float(rate))
    if not grouped:
        _error("no completed ordered/local samples found")
    if len(candidate_shas) != 1:
        _error(f"control evidence uses different candidate binaries: {sorted(candidate_shas)}")
    missing = [(mode, workers) for mode in COLORS for workers in (1, 8, 12) if not grouped.get((mode, workers))]
    if missing:
        _error(f"missing ordered/local worker samples: {missing}")
    return grouped


def _stage_report(data: dict[str, Any]) -> dict[str, Any]:
    candidates = [
        data.get("stages", {}).get("measurement", {}).get("report"),
        data.get("measurement", {}).get("report"),
        data.get("report"),
    ]
    report = next((candidate for candidate in candidates if isinstance(candidate, dict)), None)
    if report is None:
        _error("stage JSON has no measurement report")
    if report.get("workers") != 1 or report.get("slots") != 1:
        _error("stage diagnostic must record one worker and one slot")
    if not report.get("rows"):
        _error("stage diagnostic has no sampled rows")
    if report.get("full_result_black_box") is not True:
        _error("stage diagnostic does not prove full-result black-box observation")
    return report


def _jitter(index: int, count: int) -> float:
    if count <= 1:
        return 0.0
    return -0.08 + 0.16 * index / (count - 1)


def render(
    evidence_paths: list[Path],
    stage_path: Path,
    output: Path,
    preview: Path | None = None,
) -> None:
    import matplotlib as mpl  # noqa: PLC0415

    mpl.use("Agg")
    import matplotlib.pyplot as plt  # noqa: PLC0415

    grouped = _throughput_samples(evidence_paths)
    stage_evidence = _load(stage_path)
    if stage_evidence.get("status") not in (None, "complete"):
        _error(f"stage evidence is not complete: {stage_path}")
    stage = _stage_report(stage_evidence)
    rows = float(stage["rows"])
    stage_values = [
        float(stage["internal_decode_ns"]) / rows / 1_000,
        float(stage["full_projection_ns"]) / rows / 1_000,
        float(stage["cleanup_ns"]) / rows / 1_000,
    ]

    plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 10, "svg.fonttype": "none"})
    figure, (throughput_axis, stage_axis) = plt.subplots(
        1,
        2,
        figsize=(13.5, 6.5),
        gridspec_kw={"width_ratios": (1.45, 1)},
    )
    figure.subplots_adjust(left=0.075, right=0.975, bottom=0.28, top=0.78, wspace=0.3)
    figure.suptitle(
        "Native decode architecture diagnostics",
        x=0.075,
        y=0.96,
        ha="left",
        fontsize=20,
        weight="bold",
    )
    figure.text(
        0.075,
        0.89,
        "Ordered production delivery vs. worker-local diagnostic control\n"
        "Each dot is one full-corpus run; short bars mark means.",
        color="#475569",
        fontsize=11,
        va="top",
    )

    workers = (1, 8, 12)
    offsets = {"ordered": -0.16, "local": 0.16}
    labels = {"ordered": "Ordered production", "local": "Worker-local diagnostic control"}
    for mode in ("ordered", "local"):
        means_x: list[float] = []
        means_y: list[float] = []
        for worker in workers:
            values = grouped.get((mode, worker), [])
            if not values:
                continue
            center = worker + offsets[mode]
            xs = [center + _jitter(index, len(values)) for index in range(len(values))]
            throughput_axis.scatter(
                xs,
                [value / 1_000_000 for value in values],
                s=42,
                color=COLORS[mode],
                alpha=0.78,
                edgecolor="white",
                linewidth=0.7,
                zorder=3,
                label=labels[mode] if not means_x else None,
            )
            mean = sum(values) / len(values) / 1_000_000
            throughput_axis.plot(
                [center - 0.11, center + 0.11],
                [mean, mean],
                color=COLORS[mode],
                linewidth=2.2,
            )
            means_x.append(center)
            means_y.append(mean)
        throughput_axis.plot(means_x, means_y, color=COLORS[mode], linewidth=1.2, alpha=0.55)
    throughput_axis.set_title("Full-result throughput", loc="left", weight="bold", pad=12)
    throughput_axis.set_xlabel("Native decoder workers")
    throughput_axis.set_ylabel("Million VIN/s · higher is better")
    throughput_axis.set_xticks(workers)
    throughput_axis.set_ylim(bottom=0)
    throughput_axis.grid(axis="y", color="#d8dee4", linewidth=0.8)
    throughput_axis.spines[["top", "right"]].set_visible(False)
    throughput_axis.legend(frameon=False, loc="upper left")

    stage_labels = ["Raw decode +\nselection", "Full projection", "Cleanup +\nreuse"]
    bars = stage_axis.bar(
        stage_labels,
        stage_values,
        color=["#64748b", "#8b5cf6", "#14b8a6"],
        width=0.62,
    )
    for bar, value in zip(bars, stage_values, strict=True):
        stage_axis.text(
            bar.get_x() + bar.get_width() / 2,
            value,
            f"{value:.2f}",
            ha="center",
            va="bottom",
            fontsize=9,
        )
    stage_axis.set_title("Instrumented stage timing", loc="left", weight="bold", pad=12)
    stage_axis.set_ylabel("Microseconds per sampled VIN")
    stage_axis.grid(axis="y", color="#d8dee4", linewidth=0.8)
    stage_axis.spines[["top", "right"]].set_visible(False)
    stage_axis.text(
        0,
        -0.25,
        "Single worker, one slot. Evenly spaced sampled batches.\nInstrumented wall times are diagnostic and non-additive.",
        transform=stage_axis.transAxes,
        color="#475569",
        fontsize=9,
        va="top",
    )
    figure.text(
        0.075,
        0.055,
        "Worker-local control changes result lifetime and backpressure. It estimates coordination headroom;\n"
        "it is not a production benchmark gain.",
        color="#475569",
        fontsize=9.5,
    )
    output.parent.mkdir(parents=True, exist_ok=True)
    figure.savefig(output, format="svg", facecolor="white")
    if preview is not None:
        preview.parent.mkdir(parents=True, exist_ok=True)
        figure.savefig(preview, format="png", dpi=160, facecolor="white")
    plt.close(figure)


def main(
    evidence: Annotated[
        list[Path] | None,
        typer.Argument(help=f"Completed ceiling evidence JSONs; default: scripts/bench/{DEFAULT_GLOB}"),
    ] = None,
    stage: Annotated[Path, typer.Option(help="Corrected single-worker stage JSON.")] = DEFAULT_STAGE,
    output: Annotated[Path, typer.Option(help="Output SVG path.")] = DEFAULT_OUTPUT,
    preview: Annotated[Path | None, typer.Option(help="Optional PNG preview path.")] = None,
) -> None:
    """Render native architecture diagnostics from completed evidence."""
    evidence = evidence or sorted((ROOT / "scripts/bench").glob(DEFAULT_GLOB))
    if not evidence:
        message = "no ceiling evidence files found"
        raise typer.BadParameter(message)
    render(evidence, stage, output, preview)
    typer.echo(output)


if __name__ == "__main__":
    typer.run(main)
