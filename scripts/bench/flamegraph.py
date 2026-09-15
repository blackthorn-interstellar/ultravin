"""Build an offline interactive flame graph from macOS sample captures."""

from __future__ import annotations

import gzip
import hashlib
import json
import subprocess
from pathlib import Path
from typing import Any

import typer

from scripts.bench.sample_stacks import parse_sample

ROOT = Path(__file__).resolve().parents[2]


def build(capture: Path, demangler: Path) -> dict[str, Any]:
    provenance = json.loads(capture.read_text())
    profiles: list[dict[str, Any]] = []
    for entry in provenance["profiles"]:
        source = Path(entry["sample_path"])
        raw = source.read_bytes()
        if hashlib.sha256(raw).hexdigest() != entry["sample_sha256"]:
            raise ValueError("Sample checksum does not match capture metadata")
        samples = parse_sample(raw.decode())
        names = sorted({frame for sample in samples for frame in sample.frames})
        decoded = subprocess.run(
            [str(demangler.resolve())], input="\n".join(names) + "\n", text=True, capture_output=True, check=True
        ).stdout.splitlines()
        if len(decoded) != len(names):
            raise ValueError("Demangler must return exactly one line per symbol")
        mapping = dict(zip(names, decoded, strict=True))
        main_thread = samples[0].thread_id
        profiles.append(
            {
                "id": str(entry["workers"]),
                "label": f"{entry['workers']} workers",
                "meta": {
                    "capture": provenance["captured_at_utc"],
                    "workload": "10 million unique VINs · full managed results · batch 12,000 · warmed engine",
                    "sampling": "5 seconds, nominal 5 ms interval; thread observations, including waits",
                    "source_sha256": entry["sample_sha256"],
                },
                "stacks": [
                    {
                        "frames": [mapping[frame] for frame in sample.frames],
                        "weight": sample.samples,
                        "thread": "main" if sample.thread_id == main_thread else f"worker / thread {sample.thread_id}",
                        "wait": sample.occupancy == "known_wait",
                    }
                    for sample in samples
                ],
            }
        )
    symbols = sorted({frame for profile in profiles for stack in profile["stacks"] for frame in stack["frames"]})
    symbol_ids = {name: index for index, name in enumerate(symbols)}
    for profile in profiles:
        for stack in profile["stacks"]:
            stack["frames"] = [symbol_ids[frame] for frame in stack["frames"]]
    return {
        "symbols": symbols,
        "meta": {
            "title": "Ultravin · native function flame graph",
            "description": "Explore where eight and twelve workers were sampled. Click a frame to zoom; search a function to highlight it.",
        },
        "provenance": provenance,
        "profiles": profiles,
    }


def render(payload: dict[str, Any], output: Path) -> None:
    template = Path(__file__).with_name("flamegraph_viewer.html").read_text()
    encoded = json.dumps(payload, separators=(",", ":"), ensure_ascii=True).replace("<", "\\u003c")
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(template.replace("__PROFILE_JSON__", encoded))
    with gzip.GzipFile(str(output.with_suffix(".json.gz")), "wb", mtime=0) as stream:
        stream.write(encoded.encode())


def main(capture: Path, demangler: Path, output: Path = ROOT / "docs/figures/native-flamegraph.html") -> None:
    """CAPTURE is capture metadata JSON; DEMANGLER reads one symbol per line."""
    payload = build(capture, demangler)
    render(payload, output)
    typer.echo(f"Wrote {output}")


if __name__ == "__main__":
    typer.run(main)
