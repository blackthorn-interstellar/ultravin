"""Type stubs for the compiled `ultravin._ultravin` extension module."""

from pathlib import Path
from typing import Any

__version__: str

def decode(vin: str, *, year: int | None = None, flat: bool = False) -> dict[str, Any]:
    """Decode a VIN.

    ``year`` is the optional caller-supplied model year (vPIC's ``modelyear``
    parameter). When it lands in ``[1980, current_year + 2]`` and differs from
    the year the VIN itself implies, it gets its own decode pass that competes
    in the best-pass scoring — so a plausible hint can win and set
    ``model_year``. In or out of that window, a year that contradicts the
    decoded model year adds error code 12, exactly as the vPIC procedure does.

    Returns a dict with keys: ``vin``, ``wmi``, ``descriptor``, ``model_year``
    (int | None), ``error_codes`` (list[int]), ``check_digit_valid`` (bool),
    ``corrected_vin`` (str), and ``elements`` — a list of per-element dicts, each
    with: ``group_name``, ``variable``, ``value``, ``element_id``,
    ``attribute_id``, ``code``, ``data_type``, ``decode``, ``source``,
    ``pattern_id``, ``vin_schema_id``, ``keys``, ``created_on``, ``wmi_id``,
    ``to_be_qced``.

    With ``flat=True``, ``elements`` is replaced by ``attributes``: one
    ``variable -> value`` dict. Values are ``str``, except the names in
    ``ultravin.MULTI_VALUED``, which are always ``list[str]``. The other 13
    per-element columns (provenance: ``source``, ``attribute_id``, ``pattern_id``,
    …) are dropped — use the default shape if you need them. Costs ~41 dict
    entries per VIN instead of ~615, so it is materially faster to marshal.
    """

def decode_batch(
    vins: list[str],
    *,
    years: list[int | None] | None = None,
    flat: bool = False,
) -> list[dict[str, Any]]:
    """Decode many VINs; ``years`` optionally supplies one caller model year per
    VIN (``None`` entries allowed), mirroring the vPIC batch API's per-line
    ``VIN,year`` format. Raises ``ValueError`` if the lengths differ.
    """

def decode_json(vin: str, *, year: int | None = None, flat: bool = False) -> str:
    """Decode a VIN to a JSON object string (same shape as :func:`decode`).

    Serialized in Rust; ``json.loads(decode_json(vin)) == decode(vin)``.
    """

def decode_batch_json(
    vins: list[str],
    *,
    years: list[int | None] | None = None,
    flat: bool = False,
) -> str:
    """Decode many VINs to a single JSON array string, serialized in Rust.

    The high-throughput batch path: ``json.loads(decode_batch_json(vins)) ==
    decode_batch(vins)``, but the result is built without per-element Python
    dicts. Best when the consumer wants JSON bytes (files, DB, streams) rather
    than Python objects. ``years`` as in :func:`decode_batch`.
    """

def multi_valued() -> list[str]:
    """Variable names whose ``flat=True`` value is always a list.

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
    vehicle_type: int | None = None,
) -> list[str]:
    """Generate ``n`` valid VINs, deterministic for a given ``seed``.

    Each VIN is built from a real WMI, a schema that WMI uses, and one of that
    schema's patterns, with a correct check digit — so it decodes to real vehicle
    attributes rather than to an unknown-manufacturer error. No database and no
    network: everything comes from the embedded artifact.

    Filters are conjunctive. ``vehicle_type`` is a VehicleType row id (2 =
    passenger car, 7 = MPV). ``year`` is the year the VIN *decodes to*, which
    position 10 alone cannot pin down (its character is a 30-year cycle, so ``L``
    is both 2020 and 1990); candidates are decoded and kept only if they resolve
    to it. Returns fewer than ``n`` when nothing matches — including a ``wmi``
    that is in the data but not published yet, which the decoder refuses to
    resolve and this refuses to emit.

    Same seed, same VINs — within one data month and one clock reading. The
    clock is read once per call and reaches the result three ways: it drops WMIs
    whose public-availability date has not passed (so does the decoder, as
    "manufacturer not registered"), it bounds the model year sampled inside a
    schema's band, and under ``year`` it decides which years a VIN can resolve
    to at all (a year past the current one + 2 is pulled back 30, so no VIN can
    decode to it). A fixture that must outlive the year should pin the VINs it
    got, not the call that made them.

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
    """

def decode_parquet(
    src: str | Path,
    dst: str | Path | None = None,
    *,
    vin: str | None = None,
    year: str | None = None,
    ids: list[int] | None = None,
    batch_size: int = 65_536,
    sample_rows: int = 100,
) -> int | dict[str, list[Any]]:
    """Decode a parquet file (or directory of them), projecting the named elements.

    ``ids`` are vPIC ``element_id``s; omit it for every publicly decodable
    element. The library wrapper (:func:`ultravin.decode_parquet`) adds a
    ``codes=`` alternative that takes variable names.

    The VIN column and the optional caller-year column are resolved from the
    footer schema by name, then by sniffing the first ``sample_rows`` values;
    ``vin=``/``year=`` name them outright and skip that. Output columns are the
    passthrough VIN (and year), ``decoded_model_year``, then one ``Utf8``/
    ``Int64``/``Float64`` column per projected element.

    With ``dst`` the rows are written there as parquet and the row count is
    returned — the decode never leaves Rust and peak memory is O(``batch_size``).
    Without ``dst`` the columns come back as one ``{name: [values]}`` dict, which
    holds the whole source in memory; use :class:`ParquetBatchIter` to stream.

    Raises ``ValueError`` for a caller mistake (unknown column or element id,
    ambiguous autodetect) and ``OSError`` for an unreadable file.
    """

class ParquetBatchIter:
    """Chunk-at-a-time :func:`decode_parquet`, yielding ``{name: [values]}`` dicts.

    Each chunk is at most ``batch_size`` rows, decoded with the GIL released, so
    a source far larger than memory streams through at O(chunk) cost. Arguments
    are :func:`decode_parquet`'s, minus ``dst``.
    """

    def __init__(
        self,
        src: str | Path,
        *,
        vin: str | None = None,
        year: str | None = None,
        ids: list[int] | None = None,
        batch_size: int = 65_536,
        sample_rows: int = 100,
    ) -> None: ...
    def __iter__(self) -> ParquetBatchIter: ...
    def __next__(self) -> dict[str, list[Any]]: ...
