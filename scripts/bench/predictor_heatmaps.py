"""Export heatmaps from the shipped native predictor, without duplicating its formula.

Run with: uv run --frozen --with matplotlib python -m scripts.bench.predictor_heatmaps
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import typer
import ultravin as uv

from scripts.bench.end_to_end import ROOT, _sha256

WORKERS = (1, 2, 4, 8, 12, 16, 24, 32, 48, 64)
SPEEDS = (10_000, 20_000, 40_000, 80_000, 160_000, 320_000, 640_000)
FORMATS = ("parquet", "jsonl", "native")


def main(output: Path = ROOT / "docs/figures") -> None:
    """Draw optimal batch-size and output-buffer working-memory surfaces."""
    import matplotlib as mpl  # noqa: PLC0415

    mpl.use("Agg")
    import matplotlib.pyplot as plt  # noqa: PLC0415
    from matplotlib.colors import LogNorm, Normalize  # noqa: PLC0415

    output.mkdir(parents=True, exist_ok=True)
    predictions: dict[str, list[list[dict[str, Any]]]] = {
        format_name: [
            [
                uv.predict_batch_size(workers=workers, single_core_rows_per_second=speed, output=format_name)
                for workers in WORKERS
            ]
            for speed in SPEEDS
        ]
        for format_name in FORMATS
    }
    data = {
        "kind": "model estimates, not a hardware benchmark grid",
        "workers": WORKERS,
        "single_core_native_rows_per_second": SPEEDS,
        "defaults": {
            "parquet": {"working_mib": 64, "bytes_per_row": 1800},
            "jsonl": {"working_mib": 8, "bytes_per_row": 4500},
            "native": {"working_mib": 512, "bytes_per_row": 17_408},
        },
        "predictor_source_sha256": _sha256(ROOT / "crates/ultravin/src/predictor.rs"),
        "fit_source_sha256": _sha256(ROOT / "scripts/bench/predictor_model_fit.json"),
        "native_fit_source_sha256": _sha256(ROOT / "scripts/bench/slot_budget_sweep_2026_09_15.json"),
        "native_followup_source_sha256": _sha256(ROOT / "scripts/bench/slot_budget_followup_2026_09_15.json"),
        "native_calibration_source_sha256": _sha256(ROOT / "scripts/bench/native_worker_calibration_2026_09_15.json"),
        "native_batch_size_unit": "rows per worker batch",
        "native_memory_unit": "all worker slots",
        "predictions": predictions,
    }
    (output / "batch-predictor-grid.json").write_text(json.dumps(data, indent=2) + "\n")
    plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 10, "svg.fonttype": "none"})
    charts = (
        (
            "batch_size",
            1,
            "Predicted batch size",
            "Rows per batch",
            "viridis",
            "batch-size-heatmap",
        ),
        (
            "estimated_working_bytes",
            1 / 1024**2,
            "Estimated output-batch working memory at the predicted batch size",
            "Working output storage · MiB",
            "magma",
            "batch-memory-heatmap",
        ),
    )
    for field, multiplier, title, color_label, cmap, filename in charts:
        values = [
            prediction[field] * multiplier for matrix in predictions.values() for row in matrix for prediction in row
        ]
        norm = LogNorm(min(values), max(values)) if field == "batch_size" else Normalize(min(values), max(values))
        figure, axes = plt.subplots(1, len(FORMATS), figsize=(24, 7.3), sharey=True)
        figure.subplots_adjust(left=0.075, right=0.88, bottom=0.22, top=0.82, wspace=0.09)
        figure.suptitle(title, x=0.075, y=0.965, ha="left", fontsize=21, weight="bold")
        subtitle = (
            "Native: rows per worker batch; Parquet and JSONL: rows per output batch"
            if field == "batch_size"
            else "Native: all worker slots; other formats: two buffers. Excludes database, input, allocator, and process storage"
        )
        figure.text(0.075, 0.905, subtitle, fontsize=12, color="#475569")
        for axis, format_name in zip(axes, FORMATS, strict=True):
            matrix = [[prediction[field] * multiplier for prediction in row] for row in predictions[format_name]]
            artist = axis.imshow(matrix, origin="lower", aspect="auto", cmap=cmap, norm=norm)
            budget = data["defaults"][format_name]["working_mib"]
            label = {"parquet": "Parquet / Arrow", "jsonl": "JSONL", "native": "Full native Rust"}[format_name]
            axis.set_title(
                f"{label} · {budget} MiB working-buffer target",
                pad=12,
                weight="bold",
            )
            axis.set_xticks(range(len(WORKERS)), WORKERS)
            axis.set_yticks(range(len(SPEEDS)), [f"{speed / 1000:g}k" for speed in SPEEDS])
            axis.set_xlabel("Decoder workers", labelpad=12)
            axis.tick_params(length=0, pad=7)
            axis.axvline(4.5, color="white", linewidth=1.5, linestyle="--", alpha=0.8)
            for y, row in enumerate(matrix):
                for x, value in enumerate(row):
                    normalized = float(norm(value))
                    color = "#111827" if normalized > 0.65 else "white"
                    axis.text(x, y, f"{value:,.0f}", ha="center", va="center", fontsize=9, color=color)
            for spine in axis.spines.values():
                spine.set_visible(False)
        axes[0].set_ylabel("Single-core native throughput · VIN/s", labelpad=14)
        bar_axis = figure.add_axes((0.9, 0.22, 0.017, 0.6))
        figure.colorbar(artist, cax=bar_axis, label=color_label)
        figure.text(
            0.075,
            0.115,
            "Model estimates from the shipped Rust predictor. Measurements: M2 Max, 1-12 workers; dashed line marks the measured limit.",
            fontsize=10,
            color="#475569",
        )
        figure.text(
            0.075,
            0.075,
            "CPU-speed variation is modeled, not measured on separate machines. Reference output widths: columnar 1,800 B; JSONL 4,500 B; native 17,408 B.",
            fontsize=10,
            color="#475569",
        )
        for suffix in ("png", "svg"):
            figure.savefig(output / f"{filename}.{suffix}", dpi=180, facecolor="white")
        plt.close(figure)
    typer.echo(output)


if __name__ == "__main__":
    typer.run(main)
