from datetime import datetime, timedelta, timezone

from scripts.bump_deps import markdown_moves, moves, plan, versions

NOW = datetime(2026, 8, 7, tzinfo=timezone.utc)
CUTOFF = NOW - timedelta(days=7)
OLD_AGE = NOW - timedelta(days=90)
YOUNG_AGE = NOW - timedelta(days=2)

LOCK_OLD = """
[[package]]
name = "serde"
version = "1.0.100"

[[package]]
name = "libc"
version = "0.2.1"

[[package]]
name = "syn"
version = "1.0.0"

[[package]]
name = "syn"
version = "2.0.0"
"""

LOCK_NEW = """
[[package]]
name = "serde"
version = "1.0.200"

[[package]]
name = "libc"
version = "0.2.1"

[[package]]
name = "syn"
version = "1.0.0"

[[package]]
name = "syn"
version = "2.0.5"

[[package]]
name = "brandnew"
version = "0.1.0"
"""


def test_versions_collects_duplicate_names():
    v = versions(LOCK_OLD)
    assert v["syn"] == {"1.0.0", "2.0.0"}
    assert v["serde"] == {"1.0.100"}


def test_versions_skips_the_versionless_editable_root():
    v = versions('[[package]]\nname = "ultravin"\nsource = { editable = "." }\n')
    assert v == {}


def test_moves_splits_clean_from_unclean():
    clean, unclean = moves(versions(LOCK_OLD), versions(LOCK_NEW))
    assert ("serde", "1.0.100", "1.0.200") in clean
    assert ("syn", "2.0.0", "2.0.5") in clean  # one left, one arrived — still clean
    assert ("brandnew", {"0.1.0"}) in unclean
    assert all(name != "libc" for name, *_ in clean)  # unchanged pin is not a move


def test_plan_reverts_young_and_unknown_clean_moves():
    clean = [("serde", "1.0.100", "1.0.200"), ("syn", "2.0.0", "2.0.5")]
    ages: dict[tuple[str, str], datetime | None] = {("serde", "1.0.200"): YOUNG_AGE, ("syn", "2.0.5"): OLD_AGE}
    reverts, blockers = plan(clean, [], ages, CUTOFF)
    assert reverts == [("serde", "1.0.100", "1.0.200")]
    assert blockers == []

    reverts, blockers = plan(clean, [], {("syn", "2.0.5"): OLD_AGE}, CUTOFF)
    assert reverts == [("serde", "1.0.100", "1.0.200")]  # unknown age fails closed
    assert blockers == []


def test_plan_blocks_on_young_or_unknown_unclean_arrivals():
    unclean = [("brandnew", {"0.1.0"})]
    _, blockers = plan([], unclean, {("brandnew", "0.1.0"): YOUNG_AGE}, CUTOFF)
    assert blockers == [("brandnew", "0.1.0")]
    _, blockers = plan([], unclean, {}, CUTOFF)
    assert blockers == [("brandnew", "0.1.0")]
    _, blockers = plan([], unclean, {("brandnew", "0.1.0"): OLD_AGE}, CUTOFF)
    assert blockers == []


def test_markdown_moves_renders_table_and_empty_case():
    md = markdown_moves([("serde", "1.0.100", "1.0.200")], [("brandnew", {"0.1.0"})])
    assert "| serde | 1.0.100 | 1.0.200 |" in md
    assert "| brandnew | — | 0.1.0 |" in md
    assert markdown_moves([], []) == "nothing to bump\n"
