"""The dataset door: `decode_parquet` with vPIC variable names as well as ids.

The extension keys projections on `element_id` — the only key NHTSA does not
rename between data releases. Names are a convenience on top of that, resolved
here so the compiled boundary never sees anything but ints.
"""

from pathlib import Path
from typing import Any

import ultravin as uv
from ultravin import _ultravin


def _ids_for(codes: list[str]) -> list[int]:
    """Map vPIC variable names to their element ids, refusing unknown ones."""
    elements = uv.ELEMENTS
    ids = []
    for code in codes:
        entry = elements.get(code)
        if entry is None:
            msg = f"unknown vPIC variable {code!r}; see ultravin.ELEMENTS"
            raise ValueError(msg)
        ids.append(entry["element_id"])
    return ids


def decode_parquet(
    src: str | Path,
    dst: str | Path | None = None,
    *,
    vin: str | None = None,
    year: str | None = None,
    ids: list[int] | None = None,
    codes: list[str] | None = None,
    batch_size: int = 65_536,
    sample_rows: int = 100,
) -> int | dict[str, list[Any]]:
    """Decode a parquet file (or directory of them), projecting the named elements.

    `src` is a parquet file or a directory of `*.parquet` read in sorted order.
    The VIN column and the optional caller-year column are found by name, then by
    sniffing the first `sample_rows`; pass `vin=`/`year=` to name them outright.

    The projection is `ids` (vPIC `element_id`s) or `codes` (variable names,
    resolved against `ultravin.ELEMENTS`), never both — omit both for every
    publicly decodable element. Output columns are the passthrough VIN (and year),
    `decoded_model_year`, then one typed column per projected element.

    With `dst`, the rows are written there as parquet and the row count is
    returned; no row ever becomes a Python object. Without `dst`, the decoded
    columns come back as one `{name: [values]}` dict — O(source) memory, so for
    small inputs only; use `ParquetBatchIter` to stream instead.
    """
    if ids is not None and codes is not None:
        msg = "pass ids or codes, not both"
        raise ValueError(msg)
    if codes is not None:
        ids = _ids_for(codes)
    return _ultravin.decode_parquet(
        src,
        dst,
        vin=vin,
        year=year,
        ids=ids,
        batch_size=batch_size,
        sample_rows=sample_rows,
    )
