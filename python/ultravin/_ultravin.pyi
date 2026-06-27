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
