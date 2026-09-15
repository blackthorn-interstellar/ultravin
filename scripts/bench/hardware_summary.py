"""Summarize captured macOS process counters without treating them as wall time."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import typer

ROOT = Path(__file__).resolve().parents[2]


def summarize(run: dict[str, Any]) -> dict[str, Any]:
    samples = run["samples"]
    totals = {
        key: sum(sample["task"][key] for sample in samples)
        for key in [
            "cputime_ns",
            "ptime_ns",
            "cpu_instructions",
            "cpu_cycles",
            "pcpu_instructions",
            "pcpu_cycles",
            "epswitches",
            "sfi_ns",
        ]
    }
    wall_ns = sum(sample["elapsed_ns"] for sample in samples)
    cpu = totals["cputime_ns"]
    system_ns = sum(sample["task"]["cputime_ns"] * (1 - sample["task"]["cputime_userland_ratio"]) for sample in samples)
    e_cycles = totals["cpu_cycles"] - totals["pcpu_cycles"]
    return {
        "label": run["label"],
        "workers": run["workers"],
        "batch": run["batch"],
        "whole_pass_vins_per_second": run["native_json"]["actual_rows_per_second"],
        "counter_seconds": wall_ns / 1e9,
        "cpu_equivalent_cores": cpu / wall_ns,
        "p_core_equivalent": totals["ptime_ns"] / wall_ns,
        "e_core_equivalent": (cpu - totals["ptime_ns"]) / wall_ns,
        "live_other_process_cpu_equivalent": sum(sample["live_other_cpu_ns"] for sample in samples) / wall_ns,
        "host_active_cores": sum(sample["host_active_cores"] * sample["elapsed_ns"] for sample in samples) / wall_ns,
        "p_instructions_per_cpu_second": totals["pcpu_instructions"] / (totals["ptime_ns"] / 1e9),
        "e_instructions_per_cpu_second": (totals["cpu_instructions"] - totals["pcpu_instructions"])
        / ((cpu - totals["ptime_ns"]) / 1e9),
        "system_cpu_percent": 100 * system_ns / cpu,
        "ipc": totals["cpu_instructions"] / totals["cpu_cycles"],
        "p_core_ipc": totals["pcpu_instructions"] / totals["pcpu_cycles"],
        "e_core_ipc": (totals["cpu_instructions"] - totals["pcpu_instructions"]) / e_cycles if e_cycles else None,
        "p_e_switches_per_second": totals["epswitches"] / (wall_ns / 1e9),
        "selective_forced_idle_seconds": totals["sfi_ns"] / 1e9,
        "thermal_states": sorted({sample["thermal_pressure"] for sample in samples}),
        "phase_seconds": run["native_json"]["phase_seconds"],
    }


def main(path: Path = ROOT / "scripts/bench/hardware_profile_2026_09_14.json") -> None:
    payload = json.loads(path.read_text())
    typer.echo(json.dumps([summarize(run) for run in payload["runs"]], indent=2))


if __name__ == "__main__":
    typer.run(main)
