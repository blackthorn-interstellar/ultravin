"""The caller-supplied model year (vPIC's `modelyear`) must thread through every
decode entry point and change results exactly as the oracle does.

Expected values are oracle-verified against `vpic.spvindecode(vin, false, year)`
on the pinned 2026_07 dump; the chosen years keep their in/out-of-window status
forever (the window is [1980, current_year + 2] and only grows).
"""

from __future__ import annotations

import json

import pytest
import ultravin as uv

VIN = "1HGCM82633A004352"  # decodes to model year 2003 with no hint


def test_matching_year_changes_nothing() -> None:
    assert uv.decode(VIN, year=2003) == uv.decode(VIN)


def test_divergent_year_runs_its_own_pass_and_can_win() -> None:
    r = uv.decode(VIN, year=1995, flat=True)
    assert r["model_year"] == 1995
    assert r["error_codes"] == [3, 12, 14]


def test_out_of_window_year_still_flags_error_12() -> None:
    r = uv.decode(VIN, year=1979, flat=True)
    assert r["model_year"] == 2003
    assert r["error_codes"] == [0, 12]


def test_json_paths_match_dict_paths() -> None:
    assert json.loads(uv.decode_json(VIN, year=1995)) == uv.decode(VIN, year=1995)
    assert json.loads(uv.decode_json(VIN, year=1995, flat=True)) == uv.decode(VIN, year=1995, flat=True)


def test_batch_years_thread_per_vin() -> None:
    batch = uv.decode_batch([VIN, VIN], years=[None, 1995])
    assert batch[0] == uv.decode(VIN)
    assert batch[1] == uv.decode(VIN, year=1995)
    json_batch = json.loads(uv.decode_batch_json([VIN, VIN], years=[None, 1995]))
    assert json_batch == batch


def test_batch_years_length_mismatch_raises() -> None:
    with pytest.raises(ValueError, match=r"one entry .* per VIN"):
        uv.decode_batch([VIN], years=[2003, 1995])
    with pytest.raises(ValueError, match=r"one entry .* per VIN"):
        uv.decode_batch_json([VIN, VIN], years=[2003])
