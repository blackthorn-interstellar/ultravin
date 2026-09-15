"""Measure phase spans and worker completion tails in sparse batch traces."""

from __future__ import annotations

import json
import statistics
from collections import defaultdict
from itertools import pairwise
from pathlib import Path
from typing import Any

import typer

from scripts.bench.hardware_profile import ROOT


def analyze(run: dict[str, Any]) -> dict[str, Any]:
    native = run["native_json"]
    workers = native["workers"]
    trace = native["stage_trace"]
    batches: dict[int, list[dict[str, Any]]] = defaultdict(list)
    for event in trace["events"]:
        batches[event["batch_id"]].append(event)
    results = []
    for batch_id, events in sorted(batches.items()):
        stages = {event["stage"]: event for event in events if not event["stage"].endswith("_worker")}
        batch: dict[str, Any] = {
            "batch_id": batch_id,
            "stage_ms": {name: (event["end_ns"] - event["start_ns"]) / 1e6 for name, event in stages.items()},
        }
        for phase in ["decode", "cleanup"]:
            outer = stages[f"{phase}_parallel"]
            duration = outer["end_ns"] - outer["start_ns"]
            spans = [event for event in events if event["stage"] == f"{phase}_worker"]
            if sum(event["rows"] for event in spans) != outer["rows"]:
                message = f"Worker row counts do not conserve {phase} batch {batch_id}"
                raise ValueError(message)
            worker_rows = []
            worker_wall = []
            firsts = []
            lasts = []
            for worker in range(workers):
                ws = sorted(
                    (event for event in spans if event["worker"] == worker), key=lambda event: event["start_ns"]
                )
                if any(left["end_ns"] > right["start_ns"] for left, right in pairwise(ws)):
                    message = "Overlapping spans on a worker cannot be counted as disjoint activity"
                    raise ValueError(message)
                if any(event["start_ns"] < outer["start_ns"] or event["end_ns"] > outer["end_ns"] for event in ws):
                    raise ValueError("Worker span falls outside parallel phase")
                worker_rows.append(sum(event["rows"] for event in ws))
                worker_wall.append(sum(event["end_ns"] - event["start_ns"] for event in ws))
                firsts.append(ws[0]["start_ns"] if ws else outer["end_ns"])
                lasts.append(ws[-1]["end_ns"] if ws else outer["end_ns"])
            leading = sum(start - outer["start_ns"] for start in firsts)
            tail = sum(outer["end_ns"] - end for end in lasts)
            work = sum(worker_wall)
            capacity = workers * duration
            batch[phase] = {
                "worker_rows": worker_rows,
                "worker_span_ms": [value / 1e6 for value in worker_wall],
                "span_occupancy_percent": 100 * work / capacity,
                "leading_gap_percent": 100 * leading / capacity,
                "completion_tail_gap_percent": 100 * tail / capacity,
                "internal_gap_percent": 100 * (capacity - work - leading - tail) / capacity,
                "median_worker_finish_to_phase_end_ms": (outer["end_ns"] - statistics.median(lasts)) / 1e6,
                "median_worker_finish_to_phase_end_percent": 100
                * (outer["end_ns"] - statistics.median(lasts))
                / duration,
                "rows_coefficient_of_variation": statistics.pstdev(worker_rows) / statistics.mean(worker_rows),
            }
        results.append(batch)
    summary: dict[str, Any] = {
        "label": run["label"],
        "workers": workers,
        "batch": native["batch_rows"],
        "trace_every": run["trace_every"],
        "vins_per_second": native["actual_rows_per_second"],
        "process_cpu_seconds": native["process_user_cpu_seconds"] + native["process_system_cpu_seconds"],
        "cpu_microseconds_per_vin": 1e6
        * (native["process_user_cpu_seconds"] + native["process_system_cpu_seconds"])
        / native["rows"],
        "average_busy_cores": native["average_busy_cores"],
        "sampled_batches": len(results),
        "batches": results,
    }
    if results:
        summary["mean_stage_ms"] = {
            stage: statistics.mean(batch["stage_ms"][stage] for batch in results) for stage in results[0]["stage_ms"]
        }
        summary["median_phase_metrics"] = {
            phase: {
                key: statistics.median(batch[phase][key] for batch in results)
                for key in results[0][phase]
                if not key.startswith("worker_")
            }
            for phase in ["decode", "cleanup"]
        }
        summary["time_weighted_phase_metrics"] = {
            phase: {
                key: sum(batch[phase][key] * batch["stage_ms"][f"{phase}_parallel"] for batch in results)
                / sum(batch["stage_ms"][f"{phase}_parallel"] for batch in results)
                for key in [
                    "span_occupancy_percent",
                    "leading_gap_percent",
                    "completion_tail_gap_percent",
                    "internal_gap_percent",
                ]
            }
            for phase in ["decode", "cleanup"]
        }
    return summary


def main(path: Path = ROOT / "scripts/bench/stage_profile_2026_09_14.json") -> None:
    data = json.loads(path.read_text())
    typer.echo(json.dumps([analyze(run) for run in data["runs"]], indent=2))


if __name__ == "__main__":
    typer.run(main)
