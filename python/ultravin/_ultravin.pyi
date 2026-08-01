"""Type stubs for the compiled `ultravin._ultravin` extension module."""

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
    passenger car, 7 = MPV). Returns fewer than ``n`` only when nothing matches.

    ``n`` may not exceed 10,000,000; a larger request raises ``ValueError``
    rather than attempting a multi-terabyte allocation.
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

    ``limit`` stops early (0 = all ~1.7M, which takes minutes to build).
    """

def seeded(*, limit: int = 0) -> list[str]:
    """Every decoding rule matched and every 2-way interaction covered, in one list.

    Each rule's own key seeds a VIN — the positions it pins stay pinned, so the
    rule is guaranteed to match — and the positions it leaves free are chosen to
    knock out outstanding class pairs. ``limit`` stops early (0 = all ~1.7M).
    """
