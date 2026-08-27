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

Plain ``random`` fuzzing, no hypothesis: pytest-randomly seeds every test from the
run's ``--randomly-seed`` (printed in the pytest header) and every failure message
carries the exact failing input, so any failure reproduces from the report alone.
"""

from __future__ import annotations

import json
import random
from pathlib import Path

import pytest
import ultravin as uv

from tests.vin_samples import VINS

# The three field repros that raised PanicException through the wheel.
CRASHERS = ["AAé", "1HGCM8263Ł3A00435", "1HGCM82633A0043é2"]

# Multibyte chars of length 2/3/4 bytes, so an insertion lands boundaries at a
# spread of byte offsets. The one single-byte member is a plain ASCII space.
NON_ASCII = "éŁçñ —€日本語\U0001f600"

# Char positions biased toward the engine's byte slice/index boundaries: 3, 8, 9,
# 17 (build_var_keys) and 9/10 (check digit / model year), 1-based-ish.
BOUNDARY_POS = [0, 1, 2, 3, 4, 7, 8, 9, 10, 16, 17]

# Half of generated chars come from this seam-biased pool (VIN alphabet, the
# sanitize target ``&``, whitespace, the multibyte set); the rest are drawn
# uniformly from the whole codepoint space.
SEAM_CHARS = "ABCDEFGHJKLMNPRSTUVWXYZ0123456789& \t" + NON_ASCII


def _base_vins() -> list[str]:
    """A spread of real VINs to mutate, from the frozen parity corpus if present."""
    fallback = [v for v in VINS if not v.startswith("ZZZ")] + [v for v in VINS if v.startswith("ZZZ")]
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


def rand_text(*, surrogates: bool) -> str:
    """Up to 64 chars, half seam-biased, half uniform over all codepoints.
    With ``surrogates=True`` lone surrogates stay in — the widest input the wheel
    must survive; without them PyO3 always converts to a Rust ``&str``."""
    out = []
    for _ in range(random.randrange(65)):
        if random.randrange(2):
            out.append(random.choice(SEAM_CHARS))
            continue
        cp = random.randrange(0x110000)
        while not surrogates and 0xD800 <= cp <= 0xDFFF:
            cp = random.randrange(0x110000)
        out.append(chr(cp))
    return "".join(out)


def near_vin() -> str:
    """A real VIN with one non-ASCII char inserted at a boundary-biased position."""
    base = random.choice(BASE_VINS)
    ch = random.choice(NON_ASCII)
    if random.randrange(2):
        pos = random.choice([p for p in BOUNDARY_POS if p <= len(base)])
    else:
        pos = random.randrange(len(base) + 1)
    return base[:pos] + ch + base[pos:]


def wild_input() -> str:
    """WILD | ENCODABLE | near-VIN, equal odds."""
    k = random.randrange(3)
    if k == 0:
        return rand_text(surrogates=True)
    return rand_text(surrogates=False) if k == 1 else near_vin()


def encodable_input() -> str:
    """ENCODABLE | near-VIN, equal odds."""
    return rand_text(surrogates=False) if random.randrange(2) else near_vin()


def ampersandize(v: str) -> str:
    """Replace every non-ASCII char with ``&`` — the Python mirror of the Rust
    entry-seam sanitize (which maps then trims), so it holds for whitespace too."""
    return "".join(c if c.isascii() else "&" for c in v)


def test_decode_never_panics_and_is_deterministic() -> None:
    pinned = [*CRASHERS, "\ud800", ""]
    for v in pinned + [wild_input() for _ in range(2000)]:
        try:
            r = uv.decode(v)
        except (ValueError, TypeError):
            # The only allowed rejections: strings PyO3 cannot convert to a Rust
            # &str (e.g. a lone surrogate).
            continue
        # A PanicException is a BaseException, not Exception — catch it (and any
        # other escapee) only to attach the failing input, then re-raise.
        except BaseException as e:  # noqa: BLE001
            msg = f"decode({v!r}) raised {type(e).__name__}: {e}"
            raise AssertionError(msg) from e
        assert uv.decode(v) == r, f"decode({v!r}) is not deterministic"


def test_lone_surrogate_raises_valueerror() -> None:
    # PyO3 cannot UTF-8-encode a lone surrogate, so it rejects the argument before
    # any Rust runs — a clean ValueError (UnicodeEncodeError), not a panic.
    with pytest.raises(ValueError, match="surrogates not allowed"):
        uv.decode("\ud800")


def test_decode_json_matches_decode() -> None:
    for v in CRASHERS + [encodable_input() for _ in range(1000)]:
        assert json.loads(uv.decode_json(v)) == uv.decode(v), f"decode_json != decode for {v!r}"


def test_the_provenance_shape_survives_the_same_inputs() -> None:
    """`full=True` marshals a 15-key dict per element instead of one mapping —
    a second, wider path over the same sanitized string, so fuzz it too."""
    for v in CRASHERS + [encodable_input() for _ in range(500)]:
        try:
            r = uv.decode(v, full=True)
        except BaseException as e:  # noqa: BLE001
            msg = f"decode({v!r}, full=True) raised {type(e).__name__}: {e}"
            raise AssertionError(msg) from e
        assert json.loads(uv.decode_json(v, full=True)) == r, f"json != dict for {v!r}"
        # The two shapes are the same decode, so the headers must agree exactly.
        flat = uv.decode(v)
        assert {k: x for k, x in r.items() if k != "elements"} == {
            k: x for k, x in flat.items() if k != "attributes"
        }, f"headers differ for {v!r}"


def test_decode_batch_matches_singles() -> None:
    pinned = [
        CRASHERS,  # one bad VIN per batch must not take the batch down
        ["1HGCM82633A004352", "1HGCM8263é3A00435", "1HGCM82633A004352"],
    ]
    batches = pinned + [[encodable_input() for _ in range(random.randrange(9))] for _ in range(400)]
    for vs in batches:
        assert uv.decode_batch(vs) == [uv.decode(v) for v in vs], f"batch != singles for {vs!r}"


def test_non_ascii_equals_invalid_ascii() -> None:
    for v in CRASHERS + [encodable_input() for _ in range(1000)]:
        assert uv.decode(v) == uv.decode(ampersandize(v)), f"decode({v!r}) != decode of its &-form"
