import json

import pytest

from scripts.bench.ny_corpus import build, clean, reuse_key

ROWS = ["2HGFB2F58CH578531", " 2HGFB2F58CH578125 ", "2HGFB2F58CH578531", "9999236", "1A9BB1528RA123456", ""]


def test_clean_keeps_first_17_character_occurrences():
    assert clean(ROWS) == ["2HGFB2F58CH578531", "2HGFB2F58CH578125", "1A9BB1528RA123456"]


def test_reuse_key_drops_check_digit_and_serial():
    assert reuse_key("2HGFB2F58CH578531") == reuse_key("2HGFB2F55CH000001") == "2HGFB2F5CH"
    assert reuse_key("1A9BB1528RA123456") == "1A9BB152RA123"


def test_build_writes_three_orders_and_manifest(tmp_path):
    metadata = build(fetch=lambda: ROWS, out_dir=tmp_path)
    published = (tmp_path / "ny-corpus.txt").read_text().split()
    assert published == clean(ROWS)
    assert (tmp_path / "ny-corpus-sorted.txt").read_text().split() == sorted(published)
    assert sorted((tmp_path / "ny-corpus-shuffled.txt").read_text().split()) == sorted(published)
    assert metadata["count"] == 3
    assert metadata["distinct_reuse_keys"] == 2
    assert json.loads((tmp_path / "ny-corpus.manifest.json").read_text()) == metadata


def test_build_rejects_an_empty_download(tmp_path):
    with pytest.raises(ValueError, match="no 17-character VINs"):
        build(fetch=lambda: ["123"], out_dir=tmp_path)
