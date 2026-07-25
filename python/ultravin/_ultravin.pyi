"""Type stubs for the compiled `ultravin._ultravin` extension module."""

from typing import Any

__version__: str

def decode(vin: str, *, flat: bool = False) -> dict[str, Any]:
    """Decode a VIN.

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

def decode_batch(vins: list[str], *, flat: bool = False) -> list[dict[str, Any]]: ...
def decode_json(vin: str, *, flat: bool = False) -> str:
    """Decode a VIN to a JSON object string (same shape as :func:`decode`).

    Serialized in Rust; ``json.loads(decode_json(vin)) == decode(vin)``.
    """

def decode_batch_json(vins: list[str], *, flat: bool = False) -> str:
    """Decode many VINs to a single JSON array string, serialized in Rust.

    The high-throughput batch path: ``json.loads(decode_batch_json(vins)) ==
    decode_batch(vins)``, but the result is built without per-element Python
    dicts. Best when the consumer wants JSON bytes (files, DB, streams) rather
    than Python objects.
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
