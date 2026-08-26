"""Type stubs for the compiled `ultravin._ultravin` extension module."""

from collections.abc import Sequence
from datetime import datetime
from pathlib import Path
from typing import Any, Literal, Protocol

__version__: str

class ArrowStreamSource(Protocol):
    """Anything exposing the Arrow C stream interface (pyarrow, polars, duckdb)."""

    def __arrow_c_stream__(self, requested_schema: object | None = None) -> object: ...

class ArrowArraySource(Protocol):
    """Anything exposing a single Arrow array over the C data interface."""

    def __arrow_c_array__(self, requested_schema: object | None = None) -> tuple[object, object]: ...

def decode(vin: str, *, year: int | None = None, full: bool = False) -> dict[str, Any]:
    """Decode a VIN.

    ``year`` is the optional caller-supplied model year (vPIC's ``modelyear``
    parameter). When it lands in ``[1980, current_year + 2]`` and differs from
    the year the VIN itself implies, it gets its own decode pass that competes
    in the best-pass scoring — so a plausible hint can win and set
    ``model_year``. In or out of that window, a year that contradicts the
    decoded model year adds error code 12, exactly as the vPIC procedure does.

    Returns a dict with keys: ``vin``, ``wmi``, ``descriptor``, ``model_year``
    (int | None), ``error_codes`` (list[int]), ``check_digit_valid`` (bool),
    ``corrected_vin`` (str), and ``attributes`` — one ``variable -> value``
    mapping. Values are ``str``, except the names in ``ultravin.MULTI_VALUED``,
    which are always ``list[str]``.

    With ``full=True``, ``attributes`` is replaced by ``elements``: a list of
    per-element dicts carrying the value *and* its provenance — ``group_name``,
    ``variable``, ``value``, ``element_id``, ``attribute_id``, ``code``,
    ``data_type``, ``decode``, ``source``, ``pattern_id``, ``vin_schema_id``,
    ``keys``, ``created_on``, ``wmi_id``, ``to_be_qced``. That costs ~615 dict
    entries per VIN against the default's ~41, so it is materially slower to
    marshal — reach for it when you need to know *where* a value came from.
    """

def decode_batch(
    vins: list[str],
    *,
    years: list[int | None] | None = None,
    full: bool = False,
) -> list[dict[str, Any]]:
    """Decode many VINs; ``years`` optionally supplies one caller model year per
    VIN (``None`` entries allowed), mirroring the vPIC batch API's per-line
    ``VIN,year`` format. Raises ``ValueError`` if the lengths differ.
    """

def decode_json(vin: str, *, year: int | None = None, full: bool = False) -> str:
    """Decode a VIN to a JSON object string (same shape as :func:`decode`).

    Serialized in Rust; ``json.loads(decode_json(vin)) == decode(vin)``.
    """

def decode_batch_json(
    vins: list[str],
    *,
    years: list[int | None] | None = None,
    full: bool = False,
) -> str:
    """Decode many VINs to a single JSON array string, serialized in Rust.

    The high-throughput batch path: ``json.loads(decode_batch_json(vins)) ==
    decode_batch(vins)``, but the result is built without per-element Python
    dicts. Best when the consumer wants JSON bytes (files, DB, streams) rather
    than Python objects. ``years`` as in :func:`decode_batch`.
    """

def multi_valued() -> list[str]:
    """Variable names whose ``attributes`` value is always a list.

    Exposed as the ``ultravin.MULTI_VALUED`` frozenset.
    """

def elements() -> list[dict[str, Any]]:
    """The static element table, one dict per publicly decodable element.

    Keys: ``variable``, ``element_id``, ``group_name``, ``code``, ``data_type``,
    ``decode``. Exposed as the ``ultravin.ELEMENTS`` mapping, keyed by variable.
    """

def generate(
    n: int,
    *,
    seed: int = 0,
    wmi: str | None = None,
    make: str | None = None,
    year: int | None = None,
    min_year: int | None = None,
    max_year: int | None = None,
    vehicle_type: int | None = None,
    now: datetime | None = None,
) -> list[str]:
    """Generate ``n`` valid VINs, deterministic for a given ``seed``.

    Each VIN is built from a real WMI, a schema that WMI uses, and one of that
    schema's patterns, with a correct check digit — so it decodes to real vehicle
    attributes rather than to an unknown-manufacturer error. No database and no
    network: everything comes from the embedded artifact.

    Filters are conjunctive. ``vehicle_type`` is a VehicleType row id (2 =
    passenger car, 7 = MPV). ``year`` and ``make`` are what the VIN *decodes
    to*: position 10 alone cannot pin the year down (its character is a 30-year
    cycle, so ``L`` is both 2020 and 1990), and a WMI linked to a make can still
    build VINs whose patterns resolve to a sibling make (Honda's WMIs also carry
    Acura) — so candidates are decoded and kept only if they resolve to what was
    asked. ``min_year``/``max_year`` bound the decoded model year to an inclusive
    range instead of pinning it; they conjoin with ``year``, so an empty
    intersection matches nothing. Returns fewer than ``n`` when nothing
    matches — including a ``wmi``
    that is in the data but not published yet, which the decoder refuses to
    resolve and this refuses to emit.

    Same seed, same VINs — within one data month and one clock reading. The
    clock reaches the result three ways: it drops WMIs whose public-availability
    date has not passed (so does the decoder, as "manufacturer not registered"),
    it bounds the model year sampled inside a schema's band, and under ``year``
    it decides which years a VIN can resolve to at all (a year past the current
    one + 2 is pulled back 30, so no VIN can decode to it).

    ``now`` freezes that clock, so a fixture keeps returning the same VINs across
    a year rollover instead of drifting the day the calendar turns::

        from datetime import datetime
        ultravin.generate(100, seed=42, now=datetime(2026, 6, 1))

    A naive ``now`` is read as UTC, not local time, so the same literal means the
    same instant on every machine; an aware one is read in whatever zone it
    carries, so both spellings of an instant agree. Anything that is not a
    ``datetime`` raises ``TypeError``. A ``now`` before the Unix epoch leaves
    every WMI's publication date in the future, so nothing is drawable and the
    result is empty rather than an error. Omit it to read the system clock.

    Every position a pattern leaves open is randomized — the serial digits, the
    unpinned VDS/plant positions, and the free choices inside a key — so two
    draws of the same (WMI, schema, pattern) still yield different strings
    essentially always. **VINs may still repeat** (the draws are independent,
    nothing dedups), but a collision needs identical random fills on top of an
    identical draw, so repeats are vanishingly rare rather than the norm.
    Deduplicating would mean silently returning fewer than ``n``, so the odd
    repeat is left to the caller; :func:`seeded` is the deterministic
    deduplicated corpus builder.

    ``n`` may not exceed 10,000,000; a larger request raises ``ValueError``
    rather than attempting a multi-terabyte allocation. A filter that is
    satisfiable in principle but effectively never — ``year=2039`` today — gives
    up after a long run of misses and returns what it found.
    """

def sweep(dimensions: list[str] | None = None) -> list[str]:
    """One VIN per row of every requested data dimension — the brute-force list.

    ``dimensions`` names any of ``wmi``, ``pattern``, ``engine``, ``vspec``,
    ``exception``, ``default``; omit for all six. Large: the ``pattern``
    dimension alone is ~545k VINs, and all six are ~584k.
    """

def cover_vins() -> list[str]:
    """The smallest VIN set exercising every decode behaviour this data reaches.

    Computed when the artifact was built, so it costs nothing here. A few hundred
    VINs that between them touch every resolution rung, error code, conversion
    and tiebreak the data supports — a ready-made corpus for testing a decoder.
    """

def pairwise(*, limit: int = 0) -> list[str]:
    """Every pair of descriptor character-classes each schema can distinguish.

    The full output space cannot be enumerated: elements driven by disjoint
    descriptor positions vary independently, so their values multiply. This is
    the strongest coverage that is finite — strength-2 covering arrays over the
    per-position equivalence classes, which buy the interactions the decoder's
    own logic turns on (dedup, tiebreaks) at roughly 3x the row sweep.

    ``limit`` caps the result at that many VINs (0 = all ~1.7M, which takes
    minutes to build).
    """

def seeded(*, limit: int = 0) -> list[str]:
    """Every decoding rule matched and every 2-way interaction covered, in one list.

    Each rule's own key seeds a VIN — the positions it pins stay pinned, so the
    rule is guaranteed to match — and the positions it leaves free are chosen to
    knock out outstanding class pairs. ``limit`` caps the result at that many
    VINs (0 = all ~1.7M).

    Deduplicated: filler-heavy rows from different schemas collide on the same
    17 characters, and only the first occurrence is kept. This is the corpus
    builder to reach for when :func:`generate`'s repeats are a problem.
    """

def decode_stream(
    source: str | Path | ArrowStreamSource | ArrowArraySource,
    *,
    vin_column: str | None = None,
    year_column: str | None = None,
    columns: Sequence[int | str] | None = None,
    column_names: Literal["variable", "id"] = "variable",
    batch_size: int = 65_536,
    sample_rows: int = 100,
) -> DecodeStream:
    """Decode a dataset into a stream of Arrow batches.

    ``source`` is a parquet file, a directory of ``*.parquet`` read in sorted
    order, or any object exposing the Arrow C data interface — a pyarrow
    ``Table``/``RecordBatch``/``RecordBatchReader``, a polars ``DataFrame``, a
    duckdb result. A bare unnamed array is read as the VIN column. A ``list`` of
    VINs raises ``TypeError``: that is :func:`decode_batch`'s job.

    The VIN column and the optional caller-year column are resolved by name and,
    for a parquet source, then by sniffing the first ``sample_rows`` values;
    ``vin_column=``/``year_column=`` name them outright and skip that. The VIN
    column may be ``Utf8``, ``LargeUtf8``, ``Utf8View`` or a dictionary of any of
    them — it is normalized on the way in.

    ``columns`` picks the projection, mixing vPIC ``element_id``s (``int``) and
    variable names (``str``) freely; omit it for every publicly decodable
    element. Output columns are the passthrough VIN (and caller year),
    ``decoded_model_year``, then one ``Utf8``/``Int64``/``Float64`` column per
    projected element following vPIC's own ``data_type``, with an empty value
    written as null.

    ``column_names`` labels those projected columns: ``"variable"`` (the default)
    uses the vPIC variable name, ``"id"`` uses ``attr_<element_id>`` — which does
    not move when NHTSA renames a variable between monthly data releases, so it is
    what a long-lived table should be pinned to. Passthrough columns keep their own
    names either way. Anything else raises ``ValueError``.

    Whichever mode is used, every projected field carries **both** keys as Arrow
    field metadata — ``{"element_id": "26", "variable": "Make"}`` — and they
    survive a parquet round-trip, so the label you did not pick is still readable
    off the schema.

    For a parquet source, rows stream through in ``batch_size``-row chunks with
    the GIL released, so peak memory is one chunk however large the source is. For
    an Arrow source the producer decides the input chunking and ``batch_size``
    only sets the parquet row-group size of :meth:`DecodeStream.to_parquet`.

    Raises ``ValueError`` for a caller mistake (unknown column or element id,
    ambiguous autodetect) and ``OSError`` for an unreadable file.
    """

class DecodeStream:
    """A one-shot stream of decoded Arrow batches.

    Every exit consumes the source, so a second use raises ``RuntimeError``
    rather than handing back a silently truncated result. Build another with
    :func:`decode_stream` to re-read.
    """

    def __arrow_c_stream__(self, requested_schema: object | None = None) -> object:
        """The Arrow C stream capsule — what ``pa.table(stream)``,
        ``pl.DataFrame(stream)`` and duckdb consume. ``requested_schema`` is
        accepted and ignored: the output schema is fixed by the projection.
        """

    def __arrow_c_schema__(self) -> object:
        """The output schema capsule, known before a row is decoded."""

    def to_parquet(self, dst: str | Path) -> int:
        """Decode straight to a parquet file, returning the rows written.

        Snappy-compressed, one row group per chunk. The whole job stays in Rust
        with the GIL released — no row is ever a Python object. Refuses a ``dst``
        that is (or is inside) the parquet source being read.
        """

    def to_pandas(self) -> Any:
        """The decode as a pandas ``DataFrame``, via pyarrow.

        pyarrow is imported lazily and is not an ultravin dependency; without it
        this raises ``ImportError``.
        """
