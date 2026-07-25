"""The library's VIN generation: deterministic, offline, and actually decodable."""

from __future__ import annotations

import re

import pytest
import ultravin

VIN_CHARS = re.compile(r"^[A-HJ-NPR-Z0-9]{17}$")


def test_generate_is_deterministic_per_seed() -> None:
    assert ultravin.generate(20, seed=42) == ultravin.generate(20, seed=42)
    assert ultravin.generate(20, seed=42) != ultravin.generate(20, seed=43)


def test_generated_vins_are_well_formed() -> None:
    for vin in ultravin.generate(50, seed=1):
        assert VIN_CHARS.match(vin), vin


def test_generated_vins_decode_to_real_vehicles() -> None:
    # The point of generating from the data rather than at random: these decode
    # to attributes, not to "manufacturer not registered".
    for result in ultravin.decode_batch(ultravin.generate(25, seed=2)):
        assert 7 not in result["error_codes"], result["vin"]
        assert result["elements"]


def test_generate_filters_by_wmi() -> None:
    vins = ultravin.generate(10, seed=3, wmi="1HG")
    assert vins
    assert all(v.startswith("1HG") for v in vins)


def test_generate_filters_by_make() -> None:
    vins = ultravin.generate(10, seed=4, make="HONDA")
    assert vins
    makes = {
        e["value"].upper()
        for r in ultravin.decode_batch(vins)
        for e in r["elements"]
        if e["element_id"] == 26 and e["value"]
    }
    assert makes == {"HONDA"}


def test_generate_filters_by_year() -> None:
    vins = ultravin.generate(10, seed=5, year=2020)
    assert vins
    assert {r["model_year"] for r in ultravin.decode_batch(vins)} == {2020}


def test_generate_returns_nothing_for_an_impossible_filter() -> None:
    assert ultravin.generate(10, seed=6, wmi="ZZZZZZ") == []


def test_sweep_dimensions_are_independent() -> None:
    wmis = ultravin.sweep(["wmi"])
    exceptions = ultravin.sweep(["exception"])
    assert len(wmis) > 10_000  # one per WMI in the data
    assert len(exceptions) > 1_000
    assert len(ultravin.sweep(["wmi", "exception"])) == len(wmis) + len(exceptions)


def test_sweep_rejects_an_unknown_dimension() -> None:
    with pytest.raises(ValueError, match="unknown dimension: nope"):
        ultravin.sweep(["nope"])


def test_exception_sweep_returns_the_data_verbatim() -> None:
    # VinException rows *are* VINs; the sweep must not rebuild them. Emitting the
    # data as-is means repeats and one lowercase row come through unchanged —
    # both are what NHTSA ships, and both are worth having in a test corpus.
    vins = ultravin.sweep(["exception"])
    assert all(len(v) == 17 for v in vins)
    assert sum(1 for v in vins if not VIN_CHARS.match(v)) == 1
    assert any(v != v.upper() for v in vins)


def test_cover_is_small_and_decodes() -> None:
    cover = ultravin.cover_vins()
    assert 50 < len(cover) < 1_000, len(cover)
    assert len(set(cover)) == len(cover)
    # Deliberate error cases are in there, so not every entry is 17 characters;
    # every one of them still decodes without raising.
    for result in ultravin.decode_batch(cover):
        assert "error_codes" in result


def test_cover_spans_every_source_rung() -> None:
    sources = {e["source"].split(":")[0] for r in ultravin.decode_batch(ultravin.cover_vins()) for e in r["elements"]}
    for rung in ("Pattern", "Default", "VehType", "ModelYear", "Vehicle Specs", "Manu. Name"):
        assert rung in sources, rung


def test_cover_spans_the_reachable_error_codes() -> None:
    codes = {c for r in ultravin.decode_batch(ultravin.cover_vins()) for c in r["error_codes"]}
    # 12 needs a caller-supplied model year, which the decode API cannot accept.
    assert {0, 1, 5, 6, 7, 8, 11, 14, 400} <= codes, sorted(codes)
