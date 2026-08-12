from datetime import datetime, timedelta, timezone

import pytest

# bump_deps is nightly-CI tooling and runs on a current python there; the
# library floor (3.10) predates tomllib, so skip rather than fail collection.
pytest.importorskip("tomllib")

from scripts.bump_deps import deps, markdown_moves, moves, only_metadata_changed, plan, versions  # noqa: E402

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
    reverts, pinned, blockers = plan(clean, [], ages, CUTOFF, {})
    assert reverts == [("serde", "1.0.100", "1.0.200")]
    assert (pinned, blockers) == ({}, [])

    reverts, _, blockers = plan(clean, [], {("syn", "2.0.5"): OLD_AGE}, CUTOFF, {})
    assert reverts == [("serde", "1.0.100", "1.0.200")]  # unknown age fails closed
    assert blockers == []


def test_plan_blocks_on_young_or_unknown_unclean_arrivals():
    unclean = [("brandnew", {"0.1.0"})]
    *_, blockers = plan([], unclean, {("brandnew", "0.1.0"): YOUNG_AGE}, CUTOFF, {})
    assert blockers == [("brandnew", "0.1.0")]
    *_, blockers = plan([], unclean, {}, CUTOFF, {})
    assert blockers == [("brandnew", "0.1.0")]
    *_, blockers = plan([], unclean, {("brandnew", "0.1.0"): OLD_AGE}, CUTOFF, {})
    assert blockers == []


def test_plan_still_abandons_when_no_mover_can_withdraw_the_young_crate():
    """The fallback: nothing in the tree bumped into `brandnew`, so nothing can drop it."""
    clean, unclean = moves(versions(LOCK_OLD), versions(LOCK_NEW))
    ages: dict[tuple[str, str], datetime | None] = {
        ("serde", "1.0.200"): OLD_AGE,
        ("syn", "2.0.5"): OLD_AGE,
        ("brandnew", "0.1.0"): YOUNG_AGE,
    }
    reverts, pinned, blockers = plan(clean, unclean, ages, CUTOFF, deps(LOCK_NEW))
    assert blockers == [("brandnew", "0.1.0")]
    assert (reverts, pinned) == ([], {})


# The shape that stalled the lane: an eligible parent (regex) bumps and pins a
# young transitive (regex-automata) that therefore cannot be reverted alone,
# while two unrelated eligible crates wait behind it.
DEPS_LOCK_OLD = """
[[package]]
name = "serde"
version = "1.0.100"

[[package]]
name = "clap"
version = "4.0.0"

[[package]]
name = "regex"
version = "1.12.4"
dependencies = [
 "regex-automata",
]

[[package]]
name = "regex-automata"
version = "0.4.14"
dependencies = [
 "regex-syntax 0.8.7",
]

[[package]]
name = "regex-syntax"
version = "0.8.7"
"""

DEPS_LOCK_NEW = """
[[package]]
name = "serde"
version = "1.0.200"

[[package]]
name = "clap"
version = "4.1.0"

[[package]]
name = "regex"
version = "1.13.1"
dependencies = [
 "regex-automata",
]

[[package]]
name = "regex-automata"
version = "0.4.18"
dependencies = [
 "regex-syntax 0.8.9",
]

[[package]]
name = "regex-syntax"
version = "0.8.9"
"""

DEPS_AGES: dict[tuple[str, str], datetime | None] = {
    ("serde", "1.0.200"): OLD_AGE,
    ("clap", "4.1.0"): OLD_AGE,
    ("regex", "1.13.1"): OLD_AGE,
    ("regex-automata", "0.4.18"): OLD_AGE,
    ("regex-syntax", "0.8.9"): OLD_AGE,
}


def test_deps_reads_name_level_edges():
    graph = deps(DEPS_LOCK_NEW)
    assert graph["regex"] == {"regex-automata"}
    assert graph["regex-automata"] == {"regex-syntax"}  # the "name version" entry loses its version
    assert graph["serde"] == set()


def test_deps_unions_the_edges_of_every_version_of_a_name():
    lock = """
[[package]]
name = "syn"
version = "1.0.0"
dependencies = ["quote"]

[[package]]
name = "syn"
version = "2.0.0"
dependencies = ["proc-macro2"]
"""
    assert deps(lock)["syn"] == {"quote", "proc-macro2"}


def test_plan_reverts_the_parent_that_pins_a_young_transitive():
    clean, unclean = moves(versions(DEPS_LOCK_OLD), versions(DEPS_LOCK_NEW))
    ages = DEPS_AGES | {("regex-automata", "0.4.18"): YOUNG_AGE}
    reverts, pinned, blockers = plan(clean, unclean, ages, CUTOFF, deps(DEPS_LOCK_NEW))
    assert blockers == []  # no abandon: the parent is revertible
    assert pinned == {"regex": ["regex-automata 0.4.18"]}
    assert reverts == [("regex", "1.12.4", "1.13.1"), ("regex-automata", "0.4.14", "0.4.18")]
    # the eligible crates — including a sibling that merely moved — still ship
    assert {"serde", "clap", "regex-syntax"}.isdisjoint({n for n, *_ in reverts})


def test_plan_walks_dependents_transitively():
    clean, unclean = moves(versions(DEPS_LOCK_OLD), versions(DEPS_LOCK_NEW))
    ages = DEPS_AGES | {("regex-syntax", "0.8.9"): YOUNG_AGE}
    reverts, pinned, blockers = plan(clean, unclean, ages, CUTOFF, deps(DEPS_LOCK_NEW))
    assert blockers == []
    assert pinned == {"regex": ["regex-syntax 0.8.9"], "regex-automata": ["regex-syntax 0.8.9"]}
    assert [n for n, *_ in reverts] == ["regex", "regex-automata", "regex-syntax"]  # dependents first


def test_plan_withdraws_a_young_new_crate_by_reverting_the_parent_that_pulled_it():
    """A young *unclean* arrival is not a blocker when a mover above it can drop it."""
    new = DEPS_LOCK_NEW.replace(' "regex-automata",\n', ' "regex-automata",\n "memchr",\n')
    new += '\n[[package]]\nname = "memchr"\nversion = "2.7.6"\n'
    clean, unclean = moves(versions(DEPS_LOCK_OLD), versions(new))
    assert unclean == [("memchr", {"2.7.6"})]
    ages = DEPS_AGES | {("memchr", "2.7.6"): YOUNG_AGE}
    reverts, pinned, blockers = plan(clean, unclean, ages, CUTOFF, deps(new))
    assert blockers == []
    assert pinned == {"regex": ["memchr 2.7.6"]}
    assert reverts == [("regex", "1.12.4", "1.13.1")]


UV_LOCK_HEAD = """
version = 1
revision = 3
requires-python = ">=3.10"

[[package]]
name = "ruff"
version = "0.16.1"
"""

# What `uv lock --upgrade --exclude-newer` writes on a night nothing resolved
# differently: the same pins, plus the cutoff it was handed.
UV_LOCK_STANZA_ONLY = UV_LOCK_HEAD.replace(
    'requires-python = ">=3.10"\n',
    'requires-python = ">=3.10"\n\n[options]\nexclude-newer = "2026-08-01T09:19:41Z"\n',
)


def test_only_metadata_changed_spots_the_exclude_newer_stanza():
    assert only_metadata_changed(UV_LOCK_HEAD, UV_LOCK_STANZA_ONLY)


def test_only_metadata_changed_lets_a_real_pin_move_through():
    bumped = UV_LOCK_STANZA_ONLY.replace('version = "0.16.1"', 'version = "0.16.2"')
    assert not only_metadata_changed(UV_LOCK_HEAD, bumped)


def test_only_metadata_changed_ignores_an_untouched_lockfile():
    assert not only_metadata_changed(UV_LOCK_HEAD, UV_LOCK_HEAD)


def test_only_metadata_changed_spots_a_dropped_package():
    dropped = UV_LOCK_HEAD.replace('name = "ruff"\nversion = "0.16.1"\n', "")
    assert not only_metadata_changed(UV_LOCK_HEAD, dropped)


def test_markdown_moves_renders_table_and_empty_case():
    md = markdown_moves([("serde", "1.0.100", "1.0.200")], [("brandnew", {"0.1.0"})])
    assert "| serde | 1.0.100 | 1.0.200 |" in md
    assert "| brandnew | — | 0.1.0 |" in md
    assert markdown_moves([], []) == "nothing to bump\n"
