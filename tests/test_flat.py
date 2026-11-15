"""The default `attributes` shape is a projection of `full=True`'s `elements`.

Its whole contract: collapsing `elements` to `variable -> value` in Python must
reproduce it exactly. If the two ever disagree, the default shape is silently
dropping or merging data — the failure mode the list-valued note fields exist to
prevent. Assert the equivalence rather than the output, so the test keeps working
across vPIC data refreshes.
"""

from __future__ import annotations

from copy import deepcopy

import pytest
import ultravin as uv

from tests.vin_samples import VINS

HEADER_KEYS = {
    "vin",
    "wmi",
    "descriptor",
    "model_year",
    "error_codes",
    "check_digit_valid",
    "corrected_vin",
}


def collapse(result: dict) -> dict:
    """The reference implementation: what a caller would write by hand."""
    attrs: dict = {}
    for element in result["elements"]:
        name = element["variable"]
        if name in uv.MULTI_VALUED:
            attrs.setdefault(name, []).append(element["value"])
        elif name not in attrs:
            attrs[name] = element["value"]
    return attrs


@pytest.mark.parametrize("vin", VINS)
def test_attributes_equal_collapsed_elements(vin: str) -> None:
    full = uv.decode(vin, full=True)
    flat = uv.decode(vin)
    assert flat["attributes"] == collapse(full)
    assert {k: v for k, v in flat.items() if k != "attributes"} == {k: v for k, v in full.items() if k != "elements"}


def test_attributes_match_over_corpus(bench_corpus: list[str]) -> None:
    """Lock the equivalence over the full benchmark corpus, not just samples."""
    for flat, full in zip(uv.decode_batch(bench_corpus), uv.decode_batch(bench_corpus, full=True)):
        assert flat["attributes"] == collapse(full)


def test_multi_valued_are_always_lists() -> None:
    """A note field is a list even when it holds one value — or none at all."""
    seen_single = False
    for vin in VINS:
        for name, value in uv.decode(vin)["attributes"].items():
            if name in uv.MULTI_VALUED:
                assert isinstance(value, list), (vin, name)
                seen_single |= len(value) == 1
            else:
                assert isinstance(value, str), (vin, name)
    assert seen_single, "expected at least one single-element note field"


def test_attributes_preserve_element_order() -> None:
    full = uv.decode("1HGCM82633A004352", full=True)
    flat = uv.decode("1HGCM82633A004352")
    expected = list(dict.fromkeys(e["variable"] for e in full["elements"]))
    assert list(flat["attributes"]) == expected


def test_full_swaps_attributes_for_elements() -> None:
    """The two shapes are exclusive: `full=True` buys provenance by dropping the
    mapping, so a caller cannot accidentally read a stale one."""
    assert set(uv.decode(VINS[0], full=True)) == HEADER_KEYS | {"elements"}
    assert set(uv.decode_batch(VINS, full=True)[0]) == HEADER_KEYS | {"elements"}
    assert set(uv.decode_batch(VINS)[0]) == HEADER_KEYS | {"attributes"}


def test_multi_valued_is_a_subset_of_the_elements_table() -> None:
    """A name that can never be a key would be a lie — the count is data-driven,
    so assert the relationship rather than a number (a vPIC refresh can change
    which exempt elements are publicly decodable)."""
    assert uv.MULTI_VALUED
    assert set(uv.ELEMENTS) >= uv.MULTI_VALUED
    assert "Note" in uv.MULTI_VALUED


def test_elements_table_describes_decoded_variables() -> None:
    """Every variable a decode emits is described by the static table."""
    for element in uv.decode(VINS[0], full=True)["elements"]:
        entry = uv.ELEMENTS[element["variable"]]
        assert entry["element_id"] == element["element_id"]
        assert entry["data_type"] == element["data_type"]
        assert entry["group_name"] == element["group_name"]


def test_unknown_module_attribute_raises() -> None:
    with pytest.raises(AttributeError, match="no attribute 'nope'"):
        uv.nope  # noqa: B018


def test_mutating_full_element_dictionaries_does_not_change_later_decodes() -> None:
    vin = "1HGCM82633A004352"
    expected = deepcopy(uv.decode(vin, full=True))
    changed = uv.decode(vin, full=True)
    changed["elements"][0]["variable"] = "changed metadata"
    changed["elements"][0]["value"] = "changed value"
    changed["elements"][0]["new field"] = "must not leak"
    changed["elements"][1].clear()
    assert uv.decode(vin, full=True) == expected
    assert uv.decode_batch([vin], full=True) == [expected]


#: Clean VINs, each with notes, so their flat dicts hold lists.
CLEAN = ["1C4JJXP68PW604973", "1C6SRFJT2NN220929", "1FDUF5HY2DEB64815", "1GBJC34U72F217933"]


def _repeating() -> tuple[list[str], list[int | None]]:
    """Enough rows to stream: clean VINs again and again (rows sharing a
    result), with wrong check digits, messy and invalid input between them,
    and a caller year on some rows."""
    vins = []
    for i in range(9_000):
        vin = CLEAN[i % len(CLEAN)]
        if i % 7 == 0:
            vin = vin[:8] + "X" + vin[9:]
        elif i % 11 == 0:
            vin = f"  {vin.lower()}\t"
        elif i % 13 == 0:
            vin = "INVALID"
        vins.append(vin)
    years = [None if i % 3 else 2000 + i % 25 for i in range(len(vins))]
    return vins, years


def _containers(value: object) -> list[dict | list]:
    """Every dict and list reachable from `value`, itself included."""
    if isinstance(value, dict):
        return [value, *(c for v in value.values() for c in _containers(v))]
    if isinstance(value, list):
        return [value, *(c for v in value for c in _containers(v))]
    return []


@pytest.mark.parametrize("full", [False, True])
def test_batch_dicts_of_repeated_results_are_equal_and_independent(full: bool) -> None:
    vins, years = _repeating()
    singles = {(v, y): uv.decode(v, year=y, full=full) for v, y in set(zip(vins, years))}
    results = uv.decode_batch(vins, years=years, full=full)
    assert results == [singles[v, y] for v, y in zip(vins, years)]
    assert [list(r) for r in results] == [list(singles[v, y]) for v, y in zip(vins, years)]
    if not full:
        assert any(isinstance(value, list) for value in results[1]["attributes"].values())
    # No dict or list is shared between two results, so changing one
    # result cannot change another.
    containers = [id(c) for r in results for c in _containers(r)]
    assert len(containers) == len(set(containers))
    for result in results[::2]:
        for container in _containers(result):
            container.clear()
    assert results[1::2] == [singles[v, y] for v, y in list(zip(vins, years))[1::2]]
