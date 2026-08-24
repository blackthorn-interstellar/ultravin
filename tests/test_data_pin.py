"""The crate's copy of the data pin must match the repo's.

build.rs verifies a downloaded vpic.rkyv against ``artifact_blake3`` in
``crates/ultravin/data/manifest.json`` (the packaged crate cannot reach
``vpic/``). vpic-import writes both files; this catches a hand edit to one.
"""

from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def test_crate_data_pin_matches_repo_manifest():
    repo = (ROOT / "vpic" / "manifest.json").read_bytes()
    crate = (ROOT / "crates" / "ultravin" / "data" / "manifest.json").read_bytes()
    assert crate == repo, "crates/ultravin/data/manifest.json drifted from vpic/manifest.json; `make data` writes both"
