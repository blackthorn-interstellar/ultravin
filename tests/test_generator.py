"""The parity generator's VIN materialization (pure; no oracle needed).

`scripts/parity/generator.py` is a Python fork of `generate.rs` for the
oracle-backed tooling, so the halves that are easy to get wrong and impossible to
notice are pinned here: I/O/Q are not VIN characters (a bracket class must remap
around them, and a VIN that still holds one is dropped), and a `#` in a Formula
Pattern's Keys is a digit slot, not a literal to copy into the VIN.
"""

from __future__ import annotations

import re

from scripts.parity.generator import _parse_keys, build_vin, check_digit

VIN_CHARS = re.compile(r"^[A-HJ-NPR-Z0-9]{17}$")


def test_bracket_ranges_expand_and_take_their_lowest_member():
    assert _parse_keys("[C-F]M82[67]") == ["C", "M", "8", "2", "6"]
    assert _parse_keys("[DC]") == ["C"]
    vin = build_vin("1HG", "[C-F]M826", 2020)
    assert vin is not None
    assert vin[3:8] == "CM826"


def test_bracket_class_never_yields_ioq():
    # [I-Z] must remap to the lowest legal member, not hand back the illegal 'I'.
    assert _parse_keys("[I-Z]") == ["J"]
    assert _parse_keys("[N-P]") == ["N"]
    assert _parse_keys("[O-P]") == ["P"]
    vin = build_vin("1HG", "[I-Z]M826", 2020)
    assert vin is not None
    assert vin[3:8] == "JM826"
    assert VIN_CHARS.match(vin)


def test_ioq_candidates_are_dropped():
    # A class admitting only I/O/Q, a key literal, or the WMI itself: no legal VIN.
    assert build_vin("1HG", "****[OQ]", 2020) is None
    assert build_vin("1HG", "IM826", 2020) is None
    assert build_vin("1OG", "*****", 2020) is None
    assert build_vin("1HG", "*****|*IAAAAAA", 2020) is None


def test_hash_is_a_digit_slot_not_a_literal():
    assert _parse_keys("A#B") == ["A", "1", "B"]
    vin = build_vin("1HG", "A#B*C", 2020)
    assert vin is not None
    assert "#" not in vin
    assert vin[3:8] == "A1BAC"
    assert VIN_CHARS.match(vin)


def test_reversed_range_leaves_the_fill():
    # A reversed range accepts nothing, exactly as the regex engine sees it.
    assert _parse_keys("[F-C]") == [None]
    vin = build_vin("1HG", "[F-C]M826", 2020)
    assert vin is not None
    assert vin[3:5] == "AM"


def test_wildcards_leave_the_fill():
    assert _parse_keys("*_*") == [None, None, None]
    vin = build_vin("1HG", "*****", 2020)
    assert vin is not None
    assert vin[3:8] == "AAAAA"
    assert vin[11:] == "111111"


def test_unterminated_class_is_a_literal_bracket():
    # No exception: the '[' has no closing ']', so it is just a character.
    assert _parse_keys("[67") == ["[", "6", "7"]


def test_generated_vins_are_well_formed():
    specs = [
        "*****",
        "CM826",
        "A#B*C",
        "[C-F]M82[67]",
        "[I-Z]M826",
        "**[A-D]*_|*****111",
        "[F-C]###*",
    ]
    for keys in specs:
        for year in (2010, 2020, 2039):
            vin = build_vin("1HG", keys, year)
            assert vin is not None, keys
            assert VIN_CHARS.match(vin), (keys, vin)
            assert check_digit(list(vin)) == vin[8], (keys, vin)


def test_check_digit_refuses_a_letter_in_the_numeric_only_serial():
    # A key can pin a character that is legal in a VIN but not *at its position*.
    # fVINCheckDigit2 answers '?' for those rather than transliterating them, so
    # there is no digit to stamp and position 9 keeps its '0' placeholder. Without
    # the position rule these came out as '5' and '1' — a check digit the oracle
    # does not agree with, and not what the Rust generator emits.
    assert build_vin("1HG", "*****|*****A", 2020) == "1HGAAAAA0LA111A11"
    assert build_vin("1HG", "CM826|*****A11", 2020) == "1HGCM8260LA111A11"
    assert check_digit(list("1HGAAAAA5LA111A11")) == "?"


def test_check_digit_refuses_a_non_model_year_char_at_position_10():
    # 'Z' is a fine VIN character and not a model-year one; position 10 takes the
    # patternMY class, so the whole function refuses.
    assert check_digit(list("1HGAAAAA0ZA111111")) == "?"
    vin = build_vin("1HG", "*****|Z*******", 2020)
    assert vin is not None
    assert vin[8:11] == "0ZA"


def test_check_digit_computes_when_every_position_is_legal():
    assert check_digit(list("1HGCM82633A004352")) == "3"


def test_six_char_wmis_fill_the_second_block():
    vin = build_vin("1G9ABC", "*****", 2020)
    assert vin is not None
    assert vin.startswith("1G9")
    assert vin[11:14] == "ABC"
