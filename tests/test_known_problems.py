"""The known-problems registry: schema, consistency with its consumers, and evidence.

`scripts/known_problems.json` is the single source of truth for the vPIC defects
ultravin deliberately does not reproduce. Everything that used to be a hand-kept
list is derived from it, so the failure mode this file guards is *drift*: an
entry with a missing field, a set that stopped matching the registry, or half a
deviation — a registry entry with no evidence section, or an evidence section no
entry points at. Under docs/ACCEPTANCE.md an entry with no root-caused upstream
defect behind it is not admissible, and the evidence section is where that root
cause is argued, so a missing half is a policy failure, not a formatting one.
"""

from __future__ import annotations

import json
import re
from pathlib import Path

import pytest

from scripts import refresh
from scripts.parity import answerkey, stale_cache

DOC = Path(refresh.ROOT) / "docs" / "KNOWN_DEVIATIONS.md"
FIELDS = ("vin", "kind", "class", "scope", "cause", "first_observed", "evidence", "doc")
# The registry holds generated pattern VINs alongside real ones, so the wildcard
# bytes `#` and `?` are legal here; I/O/Q never are.
VIN_RE = re.compile(r"^[A-HJ-NPR-Z0-9#?]{17}$")
MONTH_RE = re.compile(r"^\d{4}_(0[1-9]|1[0-2])$")
SLUG_RE = re.compile(r"^[a-z0-9]+(-[a-z0-9]+)*$")
# A real VIN quoted in the prose, e.g. in a section heading.
DOC_VIN_RE = re.compile(r"\b[A-HJ-NPR-Z0-9]{17}\b")


@pytest.fixture(scope="module")
def entries() -> list[dict[str, str]]:
    return refresh.load_known_problems()


def _anchors(text: str) -> set[str]:
    return set(re.findall(r'<a id="([^"]+)"></a>', text))


def test_every_entry_has_every_field(entries: list[dict[str, str]]) -> None:
    for e in entries:
        assert tuple(e) == FIELDS, f"{e.get('vin')}: fields {tuple(e)}"
        for field in FIELDS:
            assert e[field].strip(), f"{e['vin']}: empty {field}"


def test_vins_are_vin_shaped_and_unique(entries: list[dict[str, str]]) -> None:
    vins = [e["vin"] for e in entries]
    assert len(vins) == len(set(vins)), "a VIN is registered twice"
    for vin in vins:
        assert VIN_RE.match(vin), f"{vin} is not 17 VIN-shaped characters"


def test_enumerated_fields_stay_inside_their_enum(entries: list[dict[str, str]]) -> None:
    for e in entries:
        assert e["kind"] in refresh.PROBLEM_KINDS, f"{e['vin']}: kind {e['kind']}"
        assert e["scope"] in refresh.PROBLEM_SCOPES, f"{e['vin']}: scope {e['scope']}"
        assert SLUG_RE.match(e["class"]), f"{e['vin']}: class {e['class']} is not a slug"
        assert MONTH_RE.match(e["first_observed"]), f"{e['vin']}: first_observed {e['first_observed']}"


def test_evidence_is_not_merely_an_output_diff(entries: list[dict[str, str]]) -> None:
    """docs/ACCEPTANCE.md: the evidence names the defective upstream artifact.

    A one-liner cannot be judged mechanically, but "they disagree" is the
    observation being explained, never the explanation — so a pointer short
    enough to be only that is refused on sight."""
    for e in entries:
        assert len(e["evidence"]) >= 40, f"{e['vin']}: evidence too thin to name an upstream artifact"
        assert len(e["cause"]) >= 40, f"{e['vin']}: cause too thin to be a root cause"


def test_refresh_sets_are_exactly_the_registry_split_by_kind(entries: list[dict[str, str]]) -> None:
    """The two frozensets are derived, so this pins that nothing hand-edits them back."""
    assert frozenset(e["vin"] for e in entries if e["kind"] == "oracle-crash") == refresh.ORACLE_CRASH_VINS
    assert frozenset(e["vin"] for e in entries if e["kind"] == "deviation") == refresh.KNOWN_DEVIATION_VINS
    assert not refresh.ORACLE_CRASH_VINS & refresh.KNOWN_DEVIATION_VINS


def test_the_stale_cache_class_is_enumerated_not_sampled(entries: list[dict[str, str]]) -> None:
    """Its error-field members live in scripts/stale_cache_cells.json, not here.

    Registering that class one VIN at a time was always a sample of an unbounded
    one — every VIN reaching a stale cell diverges the same way — and the nightly
    fuzzer kept re-finding members of it. The scan enumerates the cells instead
    (scripts/parity/stale_cache.py adjudicates against them). What the cell list
    may never excuse is a divergence wider than the error fields the cache feeds,
    so `clean-decode` members stay registered individually and are the only ones
    left here. A new `error-fields` entry in this class means someone re-sampled
    a class that is already enumerated."""
    stale = [e for e in entries if e["class"] == "stale-wmiyearvalidchars-cache"]
    assert stale, "the class vanished from the registry — its evidence section still claims it"
    assert {e["scope"] for e in stale} == {"clean-decode"}, (
        f"error-field members belong in {stale_cache.CELLS.name}: {[e['vin'] for e in stale if e['scope'] != 'clean-decode']}"
    )


def test_answerkey_excludes_every_registered_vin(entries: list[dict[str, str]]) -> None:
    """A crash has no answer to compare and a deviation's answer is the defect;
    the key records both kinds rather than freezing them."""
    assert {e["vin"] for e in entries} == answerkey.KNOWN_DEVIATIONS


def test_every_entry_points_at_an_evidence_section(entries: list[dict[str, str]]) -> None:
    doc = DOC.read_text()
    anchors = _anchors(doc)
    for e in entries:
        name, _, anchor = e["doc"].partition("#")
        assert name == "KNOWN_DEVIATIONS.md", f"{e['vin']}: doc points at {name}"
        assert anchor in anchors, f'{e["vin"]}: no <a id="{anchor}"> in docs/KNOWN_DEVIATIONS.md'


def test_every_evidence_section_is_claimed_by_an_entry(entries: list[dict[str, str]]) -> None:
    """The converse: a section whose last VIN was retired must be retired too,
    or the next reader takes a dead excuse for a live one."""
    claimed = {e["doc"].partition("#")[2] for e in entries}
    assert _anchors(DOC.read_text()) == claimed


def test_vins_named_in_an_anchored_section_are_registered(entries: list[dict[str, str]]) -> None:
    """A VIN argued in an anchored section but missing from the registry is an
    excuse no gate enforces. Unanchored sections (the unbounded element-144
    collation class) quote VINs as examples and are deliberately out of scope."""
    registered = {e["vin"] for e in entries}
    # An anchored section runs from its anchor to the next anchor or heading.
    section_re = re.compile(r'<a id="[^"]+"></a>\s*\n## .*?(?=\n<a id="|\n## |\Z)', re.DOTALL)
    sections = section_re.findall(DOC.read_text())
    assert sections, "no anchored sections found — the anchor convention changed"
    for section in sections:
        for vin in DOC_VIN_RE.findall(section):
            assert vin in registered, f"{vin} is documented but not in scripts/known_problems.json"


def test_registry_is_the_file_refresh_reads(tmp_path: Path) -> None:
    """The loader takes a path so the schema can be exercised on a fixture, but
    the default must stay the committed registry."""
    assert refresh.KNOWN_PROBLEMS.name == "known_problems.json"
    fixture = tmp_path / "known_problems.json"
    fixture.write_text(json.dumps({"entries": [{"vin": "X" * 17, "kind": "deviation"}]}))
    assert refresh.load_known_problems(fixture) == [{"vin": "X" * 17, "kind": "deviation"}]
