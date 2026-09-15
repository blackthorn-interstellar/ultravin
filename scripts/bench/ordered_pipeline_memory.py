"""Run separate owned-byte diagnostics on the immutable benchmark snapshot."""

import json
from pathlib import Path

from scripts.bench.end_to_end import _sha256
from scripts.bench.ordered_pipeline import SNAPSHOT, sample

OUTPUT = Path(__file__).with_name("ordered_pipeline_memory_2026_09_14.json")


def main() -> None:
    result = {"immutable_binary": str(SNAPSHOT), "immutable_binary_sha256": _sha256(SNAPSHOT), "samples": []}
    for mode, batch in (("shared", 12000), ("ordered", 200)):
        result["samples"].append(sample(SNAPSHOT, mode, 12, batch, True))
        OUTPUT.write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    main()
