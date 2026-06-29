"""Type stubs for the compiled `ultravin._ultravin` extension module."""

__version__: str

def decode(vin: str) -> dict[str, object]:
    """Decode a VIN.

    Returns a dict with keys: ``vin``, ``wmi``, ``descriptor``, ``model_year``
    (int | None), ``error_codes`` (list[int]), ``check_digit_valid`` (bool),
    ``corrected_vin`` (str), and ``elements`` — a list of per-element dicts, each
    with: ``group_name``, ``variable``, ``value``, ``element_id``,
    ``attribute_id``, ``code``, ``data_type``, ``decode``, ``source``,
    ``pattern_id``, ``vin_schema_id``, ``keys``, ``created_on``, ``wmi_id``,
    ``to_be_qced``.
    """

def decode_batch(vins: list[str]) -> list[dict[str, object]]: ...
def decode_json(vin: str) -> str:
    """Decode a VIN to a JSON object string (same shape as :func:`decode`).

    Serialized in Rust; ``json.loads(decode_json(vin)) == decode(vin)``.
    """

def decode_batch_json(vins: list[str]) -> str:
    """Decode many VINs to a single JSON array string, serialized in Rust.

    The high-throughput batch path: ``json.loads(decode_batch_json(vins)) ==
    decode_batch(vins)``, but the result is built without per-element Python
    dicts. Best when the consumer wants JSON bytes (files, DB, streams) rather
    than Python objects.
    """
