"""`flat=True` is a projection of `elements`, nothing more.

Its whole contract: collapsing `elements` to `variable -> value` in Python must
reproduce it exactly. If the two ever disagree, the fast shape is silently
dropping or merging data — the failure mode the list-valued note fields exist to
prevent. Assert the equivalence rather than the output, so the test keeps working
across vPIC data refreshes.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest
import ultravin as uv

VINS = [
    "1HGCM82633A004352",
    "SAL00000000000000",
    "ZZZCM82633A004352",
    "5UXWX7C5XBA123456",
    "1FTFW1ET5DFC10312",
    "JH4KA8260MC000000",
]

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
def test_flat_equals_collapsed_elements(vin: str) -> None:
    full = uv.decode(vin)
    flat = uv.decode(vin, flat=True)
    assert flat["attributes"] == collapse(full)
    assert {k: v for k, v in flat.items() if k != "attributes"} == {k: v for k, v in full.items() if k != "elements"}


def test_flat_matches_over_corpus() -> None:
    """Lock the equivalence over the full benchmark corpus, not just samples."""
    corpus = Path(__file__).parent.parent / "scripts" / "bench" / "corpus.txt"
    if not corpus.exists():
        return
    vins = [ln.strip() for ln in corpus.read_text().splitlines() if len(ln.strip()) == 17]
    for flat, full in zip(uv.decode_batch(vins, flat=True), uv.decode_batch(vins)):
        assert flat["attributes"] == collapse(full)


def test_multi_valued_are_always_lists() -> None:
    """A note field is a list even when it holds one value — or none at all."""
    seen_single = False
    for vin in VINS:
        for name, value in uv.decode(vin, flat=True)["attributes"].items():
            if name in uv.MULTI_VALUED:
                assert isinstance(value, list), (vin, name)
                seen_single |= len(value) == 1
            else:
                assert isinstance(value, str), (vin, name)
    assert seen_single, "expected at least one single-element note field"


def test_flat_preserves_element_order() -> None:
    full = uv.decode("1HGCM82633A004352")
    flat = uv.decode("1HGCM82633A004352", flat=True)
    expected = list(dict.fromkeys(e["variable"] for e in full["elements"]))
    assert list(flat["attributes"]) == expected


def test_flat_json_matches_flat_dict() -> None:
    for vin in VINS:
        assert json.loads(uv.decode_json(vin, flat=True)) == uv.decode(vin, flat=True)
    assert json.loads(uv.decode_batch_json(VINS, flat=True)) == uv.decode_batch(VINS, flat=True)


def test_flat_keys_are_header_plus_attributes() -> None:
    assert set(uv.decode(VINS[0], flat=True)) == HEADER_KEYS | {"attributes"}


def test_multi_valued_is_a_subset_of_the_elements_table() -> None:
    """A name that can never be a key would be a lie — the count is data-driven,
    so assert the relationship rather than a number (a vPIC refresh can change
    which exempt elements are publicly decodable)."""
    assert uv.MULTI_VALUED
    assert set(uv.ELEMENTS) >= uv.MULTI_VALUED
    assert "Note" in uv.MULTI_VALUED


def test_elements_table_describes_decoded_variables() -> None:
    """Every variable a decode emits is described by the static table."""
    for element in uv.decode(VINS[0])["elements"]:
        entry = uv.ELEMENTS[element["variable"]]
        assert entry["element_id"] == element["element_id"]
        assert entry["data_type"] == element["data_type"]
        assert entry["group_name"] == element["group_name"]


def test_unknown_module_attribute_raises() -> None:
    with pytest.raises(AttributeError, match="no attribute 'nope'"):
        uv.nope  # noqa: B018
