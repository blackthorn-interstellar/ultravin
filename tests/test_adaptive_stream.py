"""Adaptive dataset batches preserve the fixed-size stream contract."""

from __future__ import annotations

from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import pyarrow as pa
import pyarrow.parquet as pq
import pytest
import ultravin as uv

from tests.vin_samples import VINS

NOW = datetime(2026, 9, 1, tzinfo=timezone.utc)
PROJECTION = [26, 9, 13, 143, 114]


def _rows(count: int) -> tuple[list[str | None], list[int | None]]:
    vins: list[str | None] = [VINS[index % len(VINS)] for index in range(count)]
    years: list[int | None] = [[None, 1995, 2003, 1979][index % 4] for index in range(count)]
    if count > 3:
        vins[3] = None
    return vins, years


def _table(source: Any, **kwargs: Any) -> pa.Table:
    return pa.table(uv.decode_stream(source, now=NOW, **kwargs))


def _write(path: Path, count: int, *, row_group_size: int = 5) -> Path:
    vins, years = _rows(count)
    pq.write_table(pa.table({"vin": vins, "year": years}), path, row_group_size=row_group_size)
    return path


def test_auto_parquet_matches_fixed_batches_with_nulls_years_and_partial_tail(tmp_path: Path) -> None:
    src = _write(tmp_path / "input.parquet", 17)
    expected = _table(src, columns=PROJECTION, batch_size=5)
    actual = _table(src, columns=PROJECTION, batch_size="auto", batch_memory_mb=1)
    assert actual.equals(expected)


def test_auto_preserves_sorted_multifile_order(tmp_path: Path) -> None:
    src = tmp_path / "parts"
    src.mkdir()
    first_vins, first_years = _rows(7)
    second_vins, second_years = _rows(9)
    pq.write_table(pa.table({"vin": second_vins, "year": second_years}), src / "b.parquet")
    pq.write_table(pa.table({"vin": first_vins, "year": first_years}), src / "a.parquet")

    expected = _table(src, columns=PROJECTION, batch_size=3)
    actual = _table(src, columns=PROJECTION, batch_size="auto", batch_memory_mb=1)
    assert actual.equals(expected)
    assert actual.column("vin").to_pylist() == first_vins + second_vins


def test_auto_handles_empty_parquet(tmp_path: Path) -> None:
    src = tmp_path / "empty.parquet"
    pq.write_table(pa.table({"vin": pa.array([], type=pa.string())}), src)
    expected = _table(src, columns=PROJECTION, batch_size=8)
    actual = _table(src, columns=PROJECTION, batch_size="auto", batch_memory_mb=1)
    assert actual.equals(expected)
    assert actual.num_rows == 0


def test_auto_wide_projection_matches_fixed_output(tmp_path: Path) -> None:
    src = _write(tmp_path / "wide.parquet", 11, row_group_size=4)
    expected = _table(src, batch_size=4)
    actual = _table(src, batch_size="auto", batch_memory_mb=1)
    assert actual.equals(expected)
    assert actual.num_columns > 100


def test_auto_splits_one_large_arrow_batch_and_keeps_the_fixed_clock() -> None:
    vins, years = _rows(20_000)
    source = pa.table({"vin": vins, "year": years})
    fixed = _table(source, columns=[26], batch_size=8_192)
    adaptive_reader = pa.RecordBatchReader.from_stream(
        uv.decode_stream(source, columns=[26], batch_size="auto", batch_memory_mb=1, now=NOW)
    )
    adaptive_batches = list(adaptive_reader)
    adaptive = pa.Table.from_batches(adaptive_batches)
    assert len(adaptive_batches) > 1
    assert max(batch.num_rows for batch in adaptive_batches) < 20_000
    assert adaptive.equals(fixed)
    assert adaptive.schema.metadata[b"ultravin.now_micros"] == str(int(NOW.timestamp() * 1_000_000)).encode()


def test_auto_rebatches_many_tiny_arrow_batches_without_reordering() -> None:
    vins, years = _rows(20_003)
    source = pa.table({"vin": vins, "year": years})
    producer = pa.RecordBatchReader.from_batches(source.schema, source.to_batches(max_chunksize=7))
    adaptive_reader = pa.RecordBatchReader.from_stream(
        uv.decode_stream(producer, columns=[26], batch_size="auto", batch_memory_mb=1, now=NOW)
    )
    adaptive_batches = list(adaptive_reader)
    assert max(batch.num_rows for batch in adaptive_batches) > 7
    assert pa.Table.from_batches(adaptive_batches).equals(_table(source, columns=[26], batch_size=8_192))


def test_auto_arrow_ignores_wide_unused_columns_before_rebatching() -> None:
    vins, years = _rows(20_003)
    payload = ["x" * 256] * len(vins)
    source = pa.table({"vin": vins, "year": years} | {f"unused_{index}": payload for index in range(16)})
    producer = pa.RecordBatchReader.from_batches(source.schema, source.to_batches(max_chunksize=31))
    actual = pa.table(uv.decode_stream(producer, columns=[26], batch_size="auto", batch_memory_mb=1, now=NOW))
    expected = _table(pa.table({"vin": vins, "year": years}), columns=[26], batch_size=8_192)
    assert actual.equals(expected)


def test_explicit_arrow_batch_size_keeps_producer_chunks() -> None:
    vins, years = _rows(13)
    source = pa.table({"vin": vins, "year": years})
    producer = pa.RecordBatchReader.from_batches(source.schema, source.to_batches(max_chunksize=4))
    decoded = pa.RecordBatchReader.from_stream(
        uv.decode_stream(producer, columns=PROJECTION, batch_size=2, batch_memory_mb=1, now=NOW)
    )
    batches = list(decoded)
    assert [batch.num_rows for batch in batches] == [4, 4, 4, 1]
    assert pa.Table.from_batches(batches).equals(_table(source, columns=PROJECTION, batch_size=2))


@pytest.mark.parametrize("batch_size", [0, -1])
def test_invalid_numeric_batch_size_is_rejected(tmp_path: Path, batch_size: int) -> None:
    src = _write(tmp_path / "input.parquet", 2)
    with pytest.raises((ValueError, OverflowError), match=r"batch|range"):
        uv.decode_stream(src, batch_size=batch_size)


def test_unknown_batch_size_mode_is_rejected(tmp_path: Path) -> None:
    src = _write(tmp_path / "input.parquet", 2)
    with pytest.raises((TypeError, ValueError), match=r"auto|integer|batch"):
        uv.decode_stream(src, batch_size="dynamic")  # ty: ignore[invalid-argument-type]


def test_boolean_batch_size_is_rejected(tmp_path: Path) -> None:
    src = _write(tmp_path / "input.parquet", 2)
    with pytest.raises(ValueError, match=r"auto|positive integer"):
        uv.decode_stream(src, batch_size=True)


def test_nonpositive_batch_memory_budget_is_rejected(tmp_path: Path) -> None:
    src = _write(tmp_path / "input.parquet", 2)
    with pytest.raises(ValueError, match=r"batch_memory_mb|memory"):
        uv.decode_stream(src, batch_size="auto", batch_memory_mb=0)
