"""Summarize captured native stack observations without treating them as CPU time."""

from __future__ import annotations

import gzip
import hashlib
import json
import math
import re
import shutil
from collections import Counter
from pathlib import Path
from typing import Any

import typer

from scripts.bench.end_to_end import ROOT

DEFAULT_INPUT = ROOT / "docs/figures/native-flamegraph.json.gz"
DEFAULT_OUTPUT = ROOT / "scripts/bench/native_stack_insights_2026_09_15.json"

ALLOCATOR = re.compile(
    r"(?:^|::)_?mi_[A-Za-z0-9_]*|(?:^|::)(?:malloc|calloc|realloc|free)(?:$|::)|"
    r"alloc::(?:alloc|raw_vec)|madvise|malloc_zone",
    re.IGNORECASE,
)
MEMORY_PRIMITIVE = re.compile(r"memcpy|memmove|memcmp|bcopy|memset", re.IGNORECASE)
SORTING = re.compile(r"(?:^|::)(?:sort|quicksort|ipnsort|smallsort|insertion_sort)", re.IGNORECASE)
FORMATTING = re.compile(r"core::fmt|fmt::|write_str|String as core::fmt", re.IGNORECASE)

LEAF_CATEGORY_ORDER = (
    "explicit_wait",
    "decode_core",
    "full_result_construction",
    "decode_error_and_correction",
    "cleanup_drop",
    "allocator",
    "memory_primitive",
    "sorting",
    "database_lookup",
    "formatting_and_strings",
    "other_ultravin",
    "other_runtime_or_system",
)
WAIT_CATEGORY_ORDER = (
    "rayon_sleep_wake",
    "rayon_idle_or_yield",
    "allocator_ancestor",
    "other_wait",
)


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _outer_symbol(symbol: str) -> str:
    """Remove nested Rust generic arguments while retaining the called symbol."""

    def without_generics(value: str) -> str:
        output: list[str] = []
        depth = 0
        for character in value:
            if character == "<":
                depth += 1
            elif character == ">" and depth:
                depth -= 1
            elif depth == 0:
                output.append(character)
        return "".join(output)

    if not symbol.startswith("<"):
        return without_generics(symbol)
    depth = 0
    for index, character in enumerate(symbol):
        if character == "<":
            depth += 1
        elif character == ">":
            depth -= 1
            if depth == 0:
                receiver = without_generics(symbol[1:index])
                suffix = without_generics(symbol[index + 1 :])
                return f"<{receiver}>{suffix}"
    return without_generics(symbol)


def _leaf_category(leaf: str, *, wait: bool) -> str:
    if wait:
        return "explicit_wait"
    outer = _outer_symbol(leaf)
    if outer.startswith("ultravin::decode::decode_core"):
        return "decode_core"
    if outer.startswith("<ultravin::RawResult>::full"):
        return "full_result_construction"
    if outer.startswith(("ultravin::errors::", "ultravin::append_correction")):
        return "decode_error_and_correction"
    if outer.startswith("core::ptr::drop_glue") or ("batch_results::BatchResults" in outer and "drop" in outer):
        return "cleanup_drop"
    if ALLOCATOR.search(outer):
        return "allocator"
    if MEMORY_PRIMITIVE.search(outer):
        return "memory_primitive"
    if SORTING.search(outer):
        return "sorting"
    if "ultravin::db::Db" in outer:
        return "database_lookup"
    if FORMATTING.search(outer):
        return "formatting_and_strings"
    if "ultravin::" in outer:
        return "other_ultravin"
    return "other_runtime_or_system"


def _has(frames: list[str], predicate: Any) -> bool:
    return any(predicate(frame) for frame in frames)


def _wait_category(leaf: str, outer_frames: list[str]) -> str:
    if _has(outer_frames, lambda frame: bool(ALLOCATOR.search(frame))):
        return "allocator_ancestor"
    if leaf == "__psynch_mutexwait" and _has(
        outer_frames, lambda frame: frame.startswith("<rayon_core::sleep::Sleep>")
    ):
        return "rayon_sleep_wake"
    if _has(
        outer_frames,
        lambda frame: frame.startswith(
            (
                "<rayon_core::registry::WorkerThread>::wait_until",
                "<rayon_core::sleep::Sleep>::sleep",
            )
        ),
    ):
        return "rayon_idle_or_yield"
    return "other_wait"


def _summarize_profile(profile: dict[str, Any], symbols: list[str]) -> dict[str, Any]:
    leaf_counts: Counter[str] = Counter()
    inclusive: Counter[str] = Counter()
    thread_total: Counter[str] = Counter()
    thread_wait: Counter[str] = Counter()
    wait_categories: Counter[str] = Counter()
    wait_leaf_symbols: Counter[str] = Counter()
    wait_ancestry: Counter[str] = Counter()
    main_weight = 0
    worker_weight = 0
    for stack in profile["stacks"]:
        weight = stack["weight"]
        if not isinstance(weight, int) or weight <= 0:
            msg = f"invalid stack weight in profile {profile['id']}: {weight!r}"
            raise ValueError(msg)
        frames = [symbols[index] for index in stack["frames"]]
        outer_frames = [_outer_symbol(frame) for frame in frames]
        if not frames:
            msg = f"empty stack in profile {profile['id']}"
            raise ValueError(msg)
        thread = stack["thread"]
        if thread == "main":
            main_weight += weight
            continue
        worker_weight += weight
        thread_total[thread] += weight
        if stack["wait"]:
            thread_wait[thread] += weight
            wait_categories[_wait_category(outer_frames[-1], outer_frames)] += weight
            wait_leaf_symbols[outer_frames[-1]] += weight
            ancestry_rules = {
                "rayon_worker_wait_until": lambda frame: frame.startswith(
                    "<rayon_core::registry::WorkerThread>::wait_until"
                ),
                "rayon_sleep_method": lambda frame: frame.startswith("<rayon_core::sleep::Sleep>::sleep"),
                "rayon_wake_method": lambda frame: frame.startswith(
                    ("<rayon_core::sleep::Sleep>::wake_any", "<rayon_core::sleep::Sleep>::wake_specific")
                ),
                "allocator": lambda frame: bool(ALLOCATOR.search(frame)),
            }
            for name, predicate in ancestry_rules.items():
                if _has(outer_frames, predicate):
                    wait_ancestry[name] += weight
        leaf_counts[_leaf_category(frames[-1], wait=stack["wait"])] += weight
        predicates = {
            "decode_core_path": lambda frame: frame.startswith("ultravin::decode::decode_core"),
            "full_result_path": lambda frame: frame.startswith("<ultravin::RawResult>::full"),
            "cleanup_path": lambda frame: (
                ("batch_results::BatchResults" in frame and "drop" in frame) or frame.startswith("core::ptr::drop_glue")
            ),
            "allocator_path": lambda frame: bool(ALLOCATOR.search(frame)),
        }
        if stack["wait"]:
            inclusive["explicit_wait"] += weight
        for name, predicate in predicates.items():
            if _has(outer_frames, predicate):
                inclusive[name] += weight

    if sum(leaf_counts.values()) != worker_weight:
        raise AssertionError("leaf categories are not exhaustive and disjoint")
    wait_values = [thread_wait[thread] for thread in sorted(thread_total)]
    total_values = [thread_total[thread] for thread in sorted(thread_total)]
    non_wait_weight = worker_weight - leaf_counts["explicit_wait"]
    allocator_non_wait_percent = 100 * leaf_counts["allocator"] / non_wait_weight
    return {
        "id": profile["id"],
        "label": profile["label"],
        "meta": profile.get("meta", {}),
        "scope": "worker threads only; main-thread observations reported separately",
        "worker_observation_weight": worker_weight,
        "main_observation_weight_excluded": main_weight,
        "leaf_categories": {
            category: {
                "weight": leaf_counts[category],
                "percent": 100 * leaf_counts[category] / worker_weight,
            }
            for category in LEAF_CATEGORY_ORDER
        },
        "allocator_leaf_among_non_wait": {
            "allocator_weight": leaf_counts["allocator"],
            "non_wait_weight": non_wait_weight,
            "percent": allocator_non_wait_percent,
        },
        "explicit_wait_breakdown": {
            "disjoint_categories": {
                category: {"weight": wait_categories[category]} for category in WAIT_CATEGORY_ORDER
            },
            "leaf_symbols": dict(sorted(wait_leaf_symbols.items())),
            "inclusive_ancestry": {
                name: wait_ancestry[name]
                for name in (
                    "rayon_worker_wait_until",
                    "rayon_sleep_method",
                    "rayon_wake_method",
                    "allocator",
                )
            },
        },
        "inclusive_paths": {
            name: {"weight": weight, "percent": 100 * weight / worker_weight}
            for name, weight in sorted(inclusive.items())
        },
        "per_thread_observations": {
            "threads": len(thread_total),
            "total_weight_min": min(total_values),
            "total_weight_max": max(total_values),
            "explicit_wait_weight_min": min(wait_values),
            "explicit_wait_weight_max": max(wait_values),
            "explicit_wait_weight_by_thread": dict(sorted(thread_wait.items())),
        },
    }


def _tool_feasibility() -> dict[str, dict[str, Any]]:
    notes = {
        "xctrace": "Executable present does not prove that required Xcode templates, PMU counters, or permissions are available.",
        "sample": "Function-level stack sampler; it does not expose hardware performance counters.",
        "spindump": "Stack and wait-state diagnostic; it does not provide per-function PMU counter attribution.",
        "powermetrics": "System-level metrics generally require elevated privileges and do not provide this flamegraph's function attribution.",
        "instruments": "Legacy Instruments command, when present; xctrace is the current command-line interface.",
        "perf": "Linux perf is not expected on macOS.",
    }
    return {name: {"path": shutil.which(name), "note": note} for name, note in notes.items()}


def main(input_path: Path = DEFAULT_INPUT, output: Path = DEFAULT_OUTPUT) -> None:
    """Write exact weighted stack counts and cautious diagnostic interpretation."""
    with gzip.open(input_path, "rt") as stream:
        data = json.load(stream)
    symbols = data["symbols"]
    profiles = [_summarize_profile(profile, symbols) for profile in data["profiles"]]
    by_id = {profile["id"]: profile for profile in profiles}
    allocation_change = (
        by_id["12"]["inclusive_paths"]["allocator_path"]["percent"]
        - by_id["8"]["inclusive_paths"]["allocator_path"]["percent"]
    )
    wait_change = (
        by_id["12"]["inclusive_paths"]["explicit_wait"]["percent"]
        - by_id["8"]["inclusive_paths"]["explicit_wait"]["percent"]
    )
    allocator_non_wait_change = (
        by_id["12"]["allocator_leaf_among_non_wait"]["percent"] - by_id["8"]["allocator_leaf_among_non_wait"]["percent"]
    )
    result = {
        "schema_version": 1,
        "source": {
            "path": str(input_path.relative_to(ROOT)),
            "sha256": _sha256(input_path),
            "meta": data.get("meta", {}),
            "provenance": data.get("provenance", {}),
        },
        "unit": "weighted sampled thread observations; not CPU time or elapsed time",
        "leaf_category_precedence": list(LEAF_CATEGORY_ORDER),
        "profiles": profiles,
        "assessment": {
            "allocator_evidence": (
                "Allocator frames are directly observed as leaves and on inclusive paths, "
                "so allocation/free activity is real. Stack observations alone do not show "
                "that allocation is the dominant limiter or quantify its elapsed cost."
            ),
            "wait_and_straggler_evidence": (
                "Explicit worker waiting is more prevalent at 12 workers. Its narrow "
                "per-thread range does not expose one obvious persistently lagging worker; "
                "the capture instead shows broadly distributed waiting. This is consistent "
                "with coordination slack or changing stragglers, but cannot distinguish "
                "scheduler, heterogeneous-core, allocator, cache, or memory-system causes."
            ),
            "percentage_point_change_12_minus_8": {
                "inclusive_allocator_path": allocation_change,
                "allocator_leaf_among_non_wait": allocator_non_wait_change,
                "explicit_wait": wait_change,
            },
        },
        "hardware_counter_tool_feasibility": _tool_feasibility(),
    }
    if not all(math.isfinite(value) for value in (allocation_change, allocator_non_wait_change, wait_change)):
        raise ValueError("non-finite comparison")
    output.write_text(json.dumps(result, indent=2) + "\n")
    typer.echo(output)


if __name__ == "__main__":
    typer.run(main)
