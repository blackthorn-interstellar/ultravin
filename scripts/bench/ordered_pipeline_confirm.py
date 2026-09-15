"""Confirm the best ordered cell against a nearby control."""

import json
from pathlib import Path

from scripts.bench.end_to_end import _sha256
from scripts.bench.ordered_pipeline import SNAPSHOT, sample


def main() -> None:
    output = Path(__file__).with_name("ordered_pipeline_confirm_2026_09_14.json")
    result = {
        "immutable_binary": str(SNAPSHOT),
        "immutable_binary_sha256": _sha256(SNAPSHOT),
        "samples": [],
    }
    for mode, batch in (("shared", 12000), ("ordered", 200)):
        value = sample(SNAPSHOT, mode, 12, batch)
        result["samples"].append(value)
        output.write_text(json.dumps(result, indent=2) + "\n")
        print(f"finished {mode} B{batch}: {value['rows_per_second']:,.0f} VIN/s")


if __name__ == "__main__":
    main()
