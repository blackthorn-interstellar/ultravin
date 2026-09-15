"""Render the measured native multicore bottleneck summary.

Run with:
UV_FROZEN=1 uv run --with matplotlib python -m scripts.bench.bottleneck_visual
"""

from __future__ import annotations

import hashlib
import json
import statistics
from pathlib import Path
from typing import Any

import typer

from scripts.bench.end_to_end import ROOT

DIRECT_PATH = ROOT / "scripts/bench/direct_placement_2026_09_14.json"
PIPELINE_PATH = ROOT / "scripts/bench/bounded_pipeline_2026_09_14.json"
SELECTION_PATHS = {
    8: ROOT / "scripts/bench/native_selection_8w_2026_09_14.json",
    12: ROOT / "scripts/bench/native_selection_12w_2026_09_14.json",
}
OUTPUT_DIR = ROOT / "docs/figures"
WORKERS = (8, 12)


def _load(path: Path) -> dict[str, Any]:
    data = json.loads(path.read_text())
    if data.get("status") != "complete":
        message = f"benchmark artifact is not complete: {path}"
        raise ValueError(message)
    return data


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _phase_summary(data: dict[str, Any], workers: int) -> dict[str, float]:
    samples = [sample for sample in data["samples"] if sample["workers"] == workers and sample["label"] == "after"]
    if len(samples) != 2:
        message = f"expected two after samples for {workers} workers"
        raise ValueError(message)
    decode_shares = []
    cleanup_shares = []
    for sample in samples:
        phase = sample["native_json"]["phase_seconds"]
        accounted = phase["decode_and_local_sort"] + phase["drop_results"]
        decode_shares.append(100 * phase["decode_and_local_sort"] / accounted)
        cleanup_shares.append(100 * phase["drop_results"] / accounted)
    return {
        "decode_result_percent": statistics.median(decode_shares),
        "cleanup_percent": statistics.median(cleanup_shares),
        "median_rows_per_second": data["summary"][str(workers)]["after"]["median_rows_per_second"],
        "placement_gain_percent": 100
        * (
            data["summary"][str(workers)]["after"]["median_rows_per_second"]
            / data["summary"][str(workers)]["before"]["median_rows_per_second"]
            - 1
        ),
    }


def _pipeline_summary(data: dict[str, Any], workers: int) -> dict[str, dict[str, Any]]:
    summary = data["summary"][str(workers)]
    return {
        mode: {
            "median_rows_per_second": values["median_rows_per_second"],
            "range_rows_per_second": values["range_rows_per_second"],
            "median_peak_rss_bytes": values["median_peak_rss_bytes"],
        }
        for mode, values in summary.items()
    }


def _fixed_batch_summary(path: Path, workers: int) -> dict[str, float]:
    data = _load(path)
    cells = [cell for cell in data["cells"] if cell["batch_size"] == 16_384]
    if len(cells) != 2:
        message = f"expected two B=16384 samples for {workers} workers"
        raise ValueError(message)
    return {
        "median_rows_per_second": statistics.median(cell["actual_rows_per_second"] for cell in cells),
        "median_process_cpu_microseconds_per_vin": statistics.median(
            1_000_000 * (cell["process_user_cpu_seconds"] + cell["process_system_cpu_seconds"]) / cell["rows"]
            for cell in cells
        ),
    }


def main(output: Path = OUTPUT_DIR) -> None:
    """Write PNG, SVG, and source-data JSON for the bottleneck figure."""
    import matplotlib as mpl  # noqa: PLC0415

    mpl.use("Agg")
    import matplotlib.pyplot as plt  # noqa: PLC0415

    direct = _load(DIRECT_PATH)
    pipeline = _load(PIPELINE_PATH)
    phases = {str(workers): _phase_summary(direct, workers) for workers in WORKERS}
    pipelines = {str(workers): _pipeline_summary(pipeline, workers) for workers in WORKERS}
    fixed_batch = {str(workers): _fixed_batch_summary(SELECTION_PATHS[workers], workers) for workers in WORKERS}
    source = {
        "kind": "derived visualization of saved benchmark artifacts",
        "date": "2026-09-14",
        "phase_scope": (
            "post-placement fixed B=12000; decode/result includes sorting, construction, "
            "restoration, and waits; cleanup is synchronous; both phases use the decoder pool"
        ),
        "pipeline_scope": (
            "pre-placement engine; n=2 whole-corpus trials; bounded pipeline and sequential "
            "controls; 24000 maximum live-row budget"
        ),
        "fixed_batch_scope": (
            "pre-placement B=16384; n=2 unique-prefix cells; process CPU time is not homogeneous core capacity"
        ),
        "sources": {
            str(DIRECT_PATH.relative_to(ROOT)): _sha256(DIRECT_PATH),
            str(PIPELINE_PATH.relative_to(ROOT)): _sha256(PIPELINE_PATH),
            **{str(path.relative_to(ROOT)): _sha256(path) for path in SELECTION_PATHS.values()},
        },
        "phases": phases,
        "pipeline": pipelines,
        "fixed_batch": fixed_batch,
        "unresolved": [
            "cache and memory-system contention",
            "allocator and full-result ownership pressure",
            "heterogeneous-core and operating-system scheduling",
        ],
    }
    output.mkdir(parents=True, exist_ok=True)
    (output / "native-bottleneck.json").write_text(json.dumps(source, indent=2) + "\n")

    plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 10, "svg.fonttype": "none"})
    figure, (phase_axis, cpu_axis, pipeline_axis) = plt.subplots(1, 3, figsize=(15, 7.2))
    figure.subplots_adjust(left=0.065, right=0.97, bottom=0.25, top=0.75, wspace=0.38)
    figure.suptitle(
        "Where native multicore scaling stalls",
        x=0.07,
        y=0.96,
        ha="left",
        fontsize=19,
        weight="bold",
    )
    figure.text(
        0.07,
        0.88,
        "Measured snapshots · 2026-09-14 · left: post-placement engine; middle/right: earlier-engine controls",
        fontsize=12,
        color="#475569",
    )

    positions = list(range(len(WORKERS)))
    decode_values = [phases[str(worker)]["decode_result_percent"] for worker in WORKERS]
    cleanup_values = [phases[str(worker)]["cleanup_percent"] for worker in WORKERS]
    phase_axis.barh(positions, decode_values, color="#2563eb")
    phase_axis.barh(
        positions,
        cleanup_values,
        left=decode_values,
        color="#f59e0b",
    )
    for position, decode, cleanup in zip(positions, decode_values, cleanup_values, strict=True):
        phase_axis.text(
            decode / 2,
            position,
            f"{decode:.1f}%\ndecode/result*",
            ha="center",
            va="center",
            color="white",
            weight="bold",
        )
        phase_axis.text(
            decode + cleanup / 2,
            position,
            f"{cleanup:.1f}%\ncleanup",
            ha="center",
            va="center",
            color="#111827",
            fontsize=9,
            weight="bold",
        )
    phase_axis.set_yticks(positions, [f"{worker} workers" for worker in WORKERS])
    phase_axis.invert_yaxis()
    phase_axis.set_xlim(0, 100)
    phase_axis.set_xlabel("Share of accounted managed-batch wall time")
    phase_axis.set_title(
        "Where batch time goes\nB=12,000 · after direct placement",
        loc="left",
        weight="bold",
    )
    phase_axis.grid(axis="x", color="#e2e8f0", linewidth=0.8)
    phase_axis.set_axisbelow(True)

    worker_labels = [f"{workers}w" for workers in WORKERS]
    fixed_rates = [fixed_batch[str(worker)]["median_rows_per_second"] / 1000 for worker in WORKERS]
    cpu_costs = [fixed_batch[str(worker)]["median_process_cpu_microseconds_per_vin"] for worker in WORKERS]
    cpu_axis.bar(worker_labels, fixed_rates, color="#60a5fa", width=0.58)
    cpu_axis.set_ylim(0, 720)
    cpu_axis.set_ylabel("Throughput · thousand VIN/s", color="#1d4ed8")
    cpu_axis.tick_params(axis="y", colors="#1d4ed8")
    cpu_axis.set_title("B=16,384: rate plateaus", loc="left", weight="bold")
    for position, rate in enumerate(fixed_rates):
        cpu_axis.text(position, rate + 17, f"{rate:.1f}k", ha="center", color="#1d4ed8", weight="bold")
    cpu_cost_axis = cpu_axis.twinx()
    cpu_cost_axis.plot(worker_labels, cpu_costs, color="#dc2626", marker="D", linewidth=2)
    cpu_cost_axis.set_ylim(0, 14)
    cpu_cost_axis.set_ylabel("Process CPU · µs/VIN", color="#b91c1c")
    cpu_cost_axis.tick_params(axis="y", colors="#b91c1c")
    for position, cost in enumerate(cpu_costs):
        cpu_cost_axis.text(position, cost - 0.85, f"{cost:.2f}", ha="center", color="#b91c1c", weight="bold")
    cpu_axis.text(
        0.5,
        0.08,
        "+0.25% rate\n+16.9% CPU/VIN",
        transform=cpu_axis.transAxes,
        ha="center",
        color="#334155",
        weight="bold",
    )
    cpu_axis.grid(axis="y", color="#e2e8f0", linewidth=0.8)
    cpu_axis.set_axisbelow(True)

    modes = ("sequential_budget", "pipeline")
    labels = ("Sequential\nequal budget", "Bounded\npipeline")
    x_positions = list(range(len(WORKERS)))
    offsets = (-0.1, 0.1)
    mode_colors = ("#64748b", "#0f766e")
    for mode, label, offset, color in zip(modes, labels, offsets, mode_colors, strict=True):
        medians = [pipelines[str(worker)][mode]["median_rows_per_second"] / 1000 for worker in WORKERS]
        lows = [
            (
                pipelines[str(worker)][mode]["median_rows_per_second"]
                - pipelines[str(worker)][mode]["range_rows_per_second"][0]
            )
            / 1000
            for worker in WORKERS
        ]
        highs = [
            (
                pipelines[str(worker)][mode]["range_rows_per_second"][1]
                - pipelines[str(worker)][mode]["median_rows_per_second"]
            )
            / 1000
            for worker in WORKERS
        ]
        pipeline_axis.errorbar(
            [position + offset for position in x_positions],
            medians,
            yerr=[lows, highs],
            fmt="o",
            markersize=8,
            capsize=5,
            linewidth=1.8,
            color=color,
            label=label.replace("\n", " "),
        )
    pipeline_axis.set_xticks(x_positions, worker_labels)
    pipeline_axis.set_ylabel("Throughput · thousand VIN/s")
    pipeline_axis.set_title("Equal-budget overlap is mixed", loc="left", weight="bold")
    pipeline_axis.legend(frameon=False, loc="lower left")
    pipeline_axis.grid(axis="y", color="#e2e8f0", linewidth=0.8)
    pipeline_axis.set_axisbelow(True)
    pipeline_axis.set_ylim(560, 667)
    pipeline_axis.set_xlabel("24k live-row bound: pipeline B=12k; sequential B=24k")
    pipeline_axis.text(0, 657, "+2.6%", ha="center", color="#0f766e", weight="bold")
    pipeline_axis.text(1, 657, "-6.0%", ha="center", color="#0f766e", weight="bold")
    for spine in (
        *phase_axis.spines.values(),
        *cpu_axis.spines.values(),
        *cpu_cost_axis.spines.values(),
        *pipeline_axis.spines.values(),
    ):
        spine.set_visible(False)

    figure.text(
        0.07,
        0.16,
        "* Decode/result includes sorting, construction, output extraction, and worker waits. Both phases use the worker pool.",
        fontsize=9.5,
        color="#475569",
    )
    figure.text(
        0.07,
        0.115,
        "Middle/right: pre-placement controls, n=2; whiskers = observed min-max. M2 Max: 8 performance + 4 efficiency cores.",
        fontsize=9.5,
        color="#475569",
    )
    figure.text(
        0.07,
        0.065,
        "Next: worker timelines + OS thread states + CPU/cache/memory counters to isolate the cause.",
        fontsize=10,
        color="#0f766e",
        weight="bold",
    )
    for suffix in ("png", "svg"):
        figure.savefig(output / f"native-bottleneck.{suffix}", dpi=180, facecolor="white")
    plt.close(figure)
    typer.echo(output / "native-bottleneck.png")


if __name__ == "__main__":
    typer.run(main)
