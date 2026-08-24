"""The library's VIN generation: deterministic, offline, and actually decodable."""

from __future__ import annotations

import re
from collections import Counter
from datetime import date, datetime, timedelta, timezone

import pytest
import ultravin

VIN_CHARS = re.compile(r"^[A-HJ-NPR-Z0-9]{17}$")


def test_generate_is_deterministic_per_seed() -> None:
    assert ultravin.generate(20, seed=42) == ultravin.generate(20, seed=42)
    assert ultravin.generate(20, seed=42) != ultravin.generate(20, seed=43)


@pytest.mark.parametrize("seed", range(1, 21))
def test_generated_vins_are_well_formed(seed: int) -> None:
    # seed=7 previously produced I/O/Q VINs (e.g. 1H9AOAAA0WA588111) that failed
    # this regex and decoded to error 400; such candidates are now skipped.
    vins = ultravin.generate(500, seed=seed)
    assert len(vins) == 500
    for vin in vins:
        assert VIN_CHARS.match(vin), vin
    # The decoder recomputes the position-9 check digit; a genuine one — not the
    # '0' fallback the old I/O/Q path emitted — validates.
    for result in ultravin.decode_batch(vins):
        assert result["check_digit_valid"], result["vin"]


def test_generated_vins_decode_to_real_vehicles() -> None:
    # The point of generating from the data rather than at random: these decode
    # to attributes, not to "manufacturer not registered".
    for result in ultravin.decode_batch(ultravin.generate(25, seed=2)):
        assert 7 not in result["error_codes"], result["vin"]
        assert result["elements"]


def test_generate_will_not_draw_from_an_unpublished_wmi() -> None:
    # The decoder resolves a WMI only once its public-availability date has passed
    # and reports the miss as error 7, "manufacturer not registered"; generation
    # used to draw from every WMI row regardless. Find the unpublished rows the way
    # the decoder sees them -- one VIN per raw WMI row, kept when it decodes to
    # error 7 -- and require that asking for one by name yields nothing. Only ~1 in
    # 13k rows is unpublished, which is why sampling generate() cannot test this:
    # a few thousand random draws miss it most of the time.
    per_row = ultravin.sweep(["wmi"])
    unpublished = sorted({v[:3] for v, r in zip(per_row, ultravin.decode_batch(per_row)) if 7 in r["error_codes"]})
    if not unpublished:
        pytest.skip("every WMI in this data month is published")
    for wmi in unpublished:
        assert ultravin.generate(50, seed=9, wmi=wmi) == [], wmi


def test_no_generated_vin_comes_from_an_unregistered_wmi() -> None:
    # The broad net behind the exact test above: the 25 VINs checked earlier cannot
    # see a defect at the one-in-thousands rate this one samples at.
    results = ultravin.decode_batch(ultravin.generate(4_000, seed=9))
    assert not [r["vin"] for r in results if 7 in r["error_codes"]]


def test_generated_model_years_span_the_schema_band() -> None:
    # A generated corpus is a sample of the data, so the model year has to move:
    # taking each schema's newest allowed year collapsed ~83% of VINs onto
    # current_year + 2 and left every older year in the band untested. Sampling
    # the band spreads them (~48 distinct years, none above 9%); the thresholds
    # here are loose enough that only a re-collapse trips them.
    years = [r["model_year"] for r in ultravin.decode_batch(ultravin.generate(4_000, seed=10))]
    counts = Counter(years)
    top, n = counts.most_common(1)[0]
    assert n < len(years) * 0.5, f"{top} took {n}/{len(years)}"


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
    assert len(vins) == 10
    assert {r["model_year"] for r in ultravin.decode_batch(vins)} == {2020}


def test_generate_gives_up_on_a_year_no_vin_can_decode_to() -> None:
    # Position 10 cannot express a year past current+2: `fVinModelYear2` pulls
    # anything above that back 30 years, so a 2039 request decodes to 2009 and no
    # candidate can ever satisfy the filter. Schemas covering 2039 exist (open-ended
    # `yearto`), so the WMI/schema filters cannot rule it out first — this is the
    # starvation path, and the contract is an empty list, not a wrong VIN.
    assert ultravin.generate(10, seed=5, year=2039) == []


def test_generate_returns_nothing_for_an_impossible_filter() -> None:
    assert ultravin.generate(10, seed=6, wmi="ZZZZZZ") == []


def test_generate_rejects_an_absurd_count() -> None:
    # An unchecked `n` this size drives a multi-terabyte pre-allocation that aborts
    # the process (uncatchable) instead of raising; the boundary rejects it first.
    with pytest.raises(ValueError, match="too large"):
        ultravin.generate(10**18)


def test_generation_is_reproducible_across_runs() -> None:
    # The corpora are ordered by BTreeSet, not HashSet (whose iteration order is
    # randomized per process), so they are byte-identical run to run — an answer
    # key built on one machine has to verify on another.
    assert ultravin.pairwise(limit=300) == ultravin.pairwise(limit=300)
    assert ultravin.seeded(limit=300) == ultravin.seeded(limit=300)


def test_seeded_emits_each_vin_once() -> None:
    # Two schemas can generate the same filler-heavy VIN string. The answer-key
    # corpus (ultravin.seeded) must emit each unique VIN once, or the fail-closed
    # compare gate rejects the redundant rows. Dedup keeps first-occurrence order.
    vins = ultravin.seeded(limit=20_000)
    assert len(vins) == len(set(vins))


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


def test_pairwise_vins_are_valid_and_match_patterns() -> None:
    # The failure mode this guards: filling the numeric-only serial positions with
    # letters. That yields error 400 on nearly every VIN — a corpus of malformed
    # input dressed up as coverage, and it is invisible unless you look.
    sample = ultravin.pairwise(limit=5000)
    results = ultravin.decode_batch(sample)
    assert not [r for r in results if 400 in r["error_codes"]]
    matched = sum(1 for r in results if any(e.get("pattern_id") for e in r["elements"]))
    assert matched > len(sample) * 0.9, f"only {matched}/{len(sample)} matched a pattern"


def test_limit_is_an_exact_cap() -> None:
    # `limit` is a hard ceiling, not a per-schema-batch stopping point: the last
    # schema's covering array must not push the result past the requested count.
    assert len(ultravin.pairwise(limit=300)) <= 300
    assert len(ultravin.seeded(limit=300)) <= 300


def test_pairwise_pins_the_model_year_inside_the_schema_band() -> None:
    # Varying position 10 would move the VIN out of its schema's year band, which
    # tests year resolution rather than the pattern interaction pairwise is for.
    years = {r["model_year"] for r in ultravin.decode_batch(ultravin.pairwise(limit=2000))}
    assert years, "no model years resolved at all"
    assert all(y is None or 1980 <= y <= 2040 for y in years)


def test_now_freezes_the_clock_a_seed_is_drawn_against() -> None:
    # Without `now` the caller cannot pin the clock, so a fixture that pins only
    # the seed silently changes the day the model year rolls over.
    frozen = datetime(2026, 6, 1, 12, 0, 0)  # noqa: DTZ001 -- naive on purpose: the binding reads it as UTC
    assert ultravin.generate(200, seed=42, now=frozen) == ultravin.generate(200, seed=42, now=frozen)


def test_a_frozen_clock_bounds_the_model_years_that_can_be_drawn() -> None:
    # The clock caps the year sampled inside a schema's band at current + 2, so
    # two clocks a decade apart draw from different bands and cannot agree.
    old = ultravin.generate(300, seed=3, now=datetime(2015, 6, 1))  # noqa: DTZ001 -- read as UTC
    new = ultravin.generate(300, seed=3, now=datetime(2026, 6, 1))  # noqa: DTZ001 -- read as UTC
    assert old != new

    def years(vins: list[str]) -> list[int]:
        return sorted(y for r in ultravin.decode_batch(vins) if (y := r["model_year"]) is not None)

    old_years, new_years = years(old), years(new)
    assert old_years
    assert new_years
    # Not `all(y <= 2017)`: a schema band that *starts* after the cap has exactly
    # one expressible year and defers to it, so a handful legitimately sit above.
    # The bulk still moves, which is the property the frozen clock buys.
    assert old_years[len(old_years) // 2] <= 2017 < new_years[len(new_years) // 2]


def test_naive_now_is_read_as_utc_not_local_time() -> None:
    # A fixture pinned to a naive literal has to mean the same instant wherever it
    # replays; reading it as local time would make the corpus machine-dependent.
    naive = datetime(2026, 6, 1, 12, 0, 0)  # noqa: DTZ001 -- the naive spelling is the subject of this test
    aware = datetime(2026, 6, 1, 12, 0, 0, tzinfo=timezone.utc)
    assert ultravin.generate(200, seed=5, now=naive) == ultravin.generate(200, seed=5, now=aware)


def test_a_now_that_is_not_a_datetime_is_refused() -> None:
    # `date` has no time of day and a bare year is not a clock reading; taking
    # either would silently pick a meaning the caller never asked for.
    # Both arguments are deliberately the wrong type: the stub already rejects
    # them statically, and this pins that the runtime refuses them too.
    with pytest.raises(TypeError, match="datetime"):
        ultravin.generate(5, now=date(2026, 6, 1))  # ty: ignore[invalid-argument-type]
    with pytest.raises(TypeError, match="datetime"):
        ultravin.generate(5, now=2026)  # ty: ignore[invalid-argument-type]


def test_an_aware_now_is_converted_rather_than_truncated() -> None:
    # The same instant spelled in two zones is one clock reading, not two.
    utc = datetime(2026, 6, 1, 12, 0, 0, tzinfo=timezone.utc)
    plus_two = utc.astimezone(timezone(timedelta(hours=2)))
    assert plus_two.hour != utc.hour  # genuinely a different wall clock
    assert ultravin.generate(200, seed=6, now=utc) == ultravin.generate(200, seed=6, now=plus_two)


def test_omitting_now_still_reads_the_system_clock() -> None:
    # The parameter is additive: the old call has to keep working unchanged.
    assert len(ultravin.generate(50, seed=11)) == 50
    assert ultravin.generate(50, seed=11) == ultravin.generate(50, seed=11)


def test_generate_may_repeat_a_vin() -> None:
    # Documented, not a defect: patterns that pin nothing leave the whole VIN to
    # the fill, so draws collide. `generate` promises n VINs, not n distinct ones.
    vins = ultravin.generate(5_000, seed=1)
    assert len(vins) == 5_000
    assert len(set(vins)) < len(vins), "the duplicate behaviour the docs describe is gone"
