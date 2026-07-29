"""Fuzz properties: ``decode`` must never panic, and a non-ASCII char must behave
exactly like the invalid ASCII ``&`` at the same character position.

Root cause these guard against: the engine indexes the VIN *by byte* — the
``&vin[3..8]`` / ``&vin[9..17]`` slices in ``build_var_keys``, the check-digit and
error-code scans — on the assumption that one byte is one character. That holds
only for ASCII; a multibyte char (``é``, ``Ł``, …) puts a UTF-8 char boundary
mid-index and raised ``pyo3_runtime.PanicException`` straight through the wheel
(and one bad VIN took the whole ``decode_batch`` down through rayon). ``decode``
now sanitizes once at the entry seam (every non-ASCII char -> ``&``), so these
properties must hold forever.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest
import ultravin as uv
from hypothesis import example, given, settings, strategies as st

# The three field repros that raised PanicException through the wheel.
CRASHERS = ["AAé", "1HGCM8263Ł3A00435", "1HGCM82633A0043é2"]

# Full Unicode, lone surrogates included (default ``characters()`` yields them):
# the widest input the wheel must survive without panicking.
WILD = st.text(st.characters(), max_size=64)

# UTF-8-encodable text only (no lone surrogates), so PyO3 always converts it to a
# Rust ``&str`` and the cross-API / ampersand equivalences never trip over a
# string PyO3 rejects at the boundary before any decode runs.
ENCODABLE = st.text(st.characters(codec="utf-8"), max_size=64)

# Multibyte chars of length 2/3/4 bytes, so an insertion lands boundaries at a
# spread of byte offsets. Includes a non-ASCII *whitespace* char (U+00A0).
NON_ASCII = "éŁçñ —€日本語\U0001f600"

# Char positions biased toward the engine's byte slice/index boundaries: 3, 8, 9,
# 17 (build_var_keys) and 9/10 (check digit / model year), 1-based-ish.
BOUNDARY_POS = [0, 1, 2, 3, 4, 7, 8, 9, 10, 16, 17]


def _base_vins() -> list[str]:
    """A spread of real VINs to mutate, from the frozen parity corpus if present."""
    fallback = [
        "1HGCM82633A004352",
        "SAL00000000000000",
        "5UXWX7C5XBA123456",
        "1FTFW1ET5DFC10312",
        "JH4KA8260MC000000",
        "ZZZCM82633A004352",
    ]
    corpus = Path(__file__).parent / "parity_corpus.json"
    if not corpus.exists():
        return fallback
    try:
        entries = json.loads(corpus.read_text())["entries"]
    except (OSError, ValueError, KeyError):
        return fallback
    vins = [e["vin"] for e in entries if isinstance(e.get("vin"), str)]
    return vins[::13][:64] or fallback


BASE_VINS = _base_vins()


@st.composite
def near_vin(draw: st.DrawFn) -> str:
    """A real VIN with one non-ASCII char inserted at a boundary-biased position."""
    base = draw(st.sampled_from(BASE_VINS))
    ch = draw(st.sampled_from(list(NON_ASCII)))
    boundaries = st.sampled_from([p for p in BOUNDARY_POS if p <= len(base)])
    pos = draw(boundaries | st.integers(min_value=0, max_value=len(base)))
    return base[:pos] + ch + base[pos:]


def ampersandize(v: str) -> str:
    """Replace every non-ASCII char with ``&`` — the Python mirror of the Rust
    entry-seam sanitize (which maps then trims), so it holds for whitespace too."""
    return "".join(c if c.isascii() else "&" for c in v)


@settings(max_examples=400, deadline=None)
@given(st.one_of(WILD, ENCODABLE, near_vin()))
@example("AAé")
@example("1HGCM8263Ł3A00435")
@example("1HGCM82633A0043é2")
@example("\ud800")  # lone surrogate: PyO3 rejects -> clean ValueError, never a panic
@example("")
def test_decode_never_panics_and_is_deterministic(v: str) -> None:
    try:
        r = uv.decode(v)
    except (ValueError, TypeError):
        # The only allowed rejections: strings PyO3 cannot convert to a Rust &str
        # (e.g. a lone surrogate). A PanicException is neither, so it would escape
        # this except and fail the test — which is the whole point.
        return
    assert uv.decode(v) == r


def test_lone_surrogate_raises_valueerror() -> None:
    # PyO3 cannot UTF-8-encode a lone surrogate, so it rejects the argument before
    # any Rust runs — a clean ValueError (UnicodeEncodeError), not a panic.
    with pytest.raises(ValueError, match="surrogates not allowed"):
        uv.decode("\ud800")


@settings(max_examples=250, deadline=None)
@given(st.one_of(ENCODABLE, near_vin()))
@example("AAé")
@example("1HGCM8263Ł3A00435")
@example("1HGCM82633A0043é2")
def test_decode_json_matches_decode(v: str) -> None:
    assert json.loads(uv.decode_json(v)) == uv.decode(v)


@settings(max_examples=200, deadline=None)
@given(st.lists(st.one_of(ENCODABLE, near_vin()), max_size=8))
@example(CRASHERS)  # one bad VIN per batch must not take the batch down
@example(["1HGCM82633A004352", "1HGCM8263é3A00435", "1HGCM82633A004352"])
def test_decode_batch_matches_singles(vs: list[str]) -> None:
    assert uv.decode_batch(vs) == [uv.decode(v) for v in vs]


@settings(max_examples=250, deadline=None)
@given(st.one_of(ENCODABLE, near_vin()))
@example("AAé")
@example("1HGCM8263Ł3A00435")
@example("1HGCM82633A0043é2")
def test_non_ascii_equals_invalid_ascii(v: str) -> None:
    assert uv.decode(v) == uv.decode(ampersandize(v))
