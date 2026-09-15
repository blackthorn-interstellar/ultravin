"""A decode job must retain its data identity and clock across every output path."""

import json
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq
import pytest
import ultravin as uv

VIN = "1HGCM82633A004352"
PAST = datetime(1970, 1, 1, tzinfo=timezone.utc)
NOW = datetime(2026, 1, 1, tzinfo=timezone.utc)


def test_provenance_identifies_loaded_release() -> None:
    manifest = json.loads((Path(__file__).parents[1] / "crates/ultravin/data/manifest.json").read_text())
    assert uv.provenance() == {
        "data_month": manifest["month"],
        "artifact_blake3": manifest["artifact_blake3"],
        "decoder_version": uv.__version__,
    }


@pytest.mark.parametrize("full", [False, True])
@pytest.mark.parametrize("count", [1, 3])
@pytest.mark.parametrize("now", [PAST, NOW])
def test_every_decode_shape_uses_the_same_frozen_clock(full: bool, count: int, now: datetime) -> None:
    vins = [VIN] * count
    years: list[int | None] = [1995] * count
    one = uv.decode(VIN, year=1995, full=full, now=now)
    assert json.loads(uv.decode_json(VIN, year=1995, full=full, now=now)) == one
    assert uv.decode_batch(vins, years=years, full=full, now=now) == [one] * count
    assert json.loads(uv.decode_batch_json(vins, years=years, full=full, now=now)) == [one] * count
    text = uv._decode_batch_jsonl(vins, years=years, full=full, now=now)
    assert text.endswith("\n")
    assert [json.loads(line) for line in text.splitlines()] == [one] * count


def test_decode_clock_has_no_history_and_normalizes_timezones() -> None:
    first = uv.decode(VIN, now=PAST)
    assert uv.decode(VIN, now=NOW) != first
    assert uv.decode(VIN, now=PAST) == first
    assert uv.decode(VIN, now=NOW.replace(tzinfo=None)) == uv.decode(VIN, now=NOW)
    assert uv.decode(VIN, now=NOW.astimezone(timezone(timedelta(hours=-8)))) == uv.decode(VIN, now=NOW)


@pytest.mark.parametrize("method", ["decode", "decode_json", "decode_batch", "decode_batch_json", "decode_stream"])
def test_decode_clock_rejects_non_datetimes(method: str) -> None:
    source = pa.table({"vin": [VIN]}) if method == "decode_stream" else [VIN] if "batch" in method else VIN
    with pytest.raises(TypeError, match="datetime"):
        getattr(uv, method)(source, now="2026-01-01")


@pytest.mark.parametrize("full", [False, True])
def test_empty_batches_preserve_output_contract(full: bool) -> None:
    assert uv.decode_batch([], now=PAST, full=full) == []
    assert uv.decode_batch_json([], now=PAST, full=full) == "[]"
    assert uv._decode_batch_jsonl([], now=PAST, full=full) == ""
    with pytest.raises(ValueError, match="years has 1 entries but vins has 0"):
        uv.decode_batch([], years=[2000], now=PAST, full=full)


def test_arrow_stream_uses_frozen_clock_for_lazily_produced_batches(tmp_path: Path) -> None:
    source = pa.table({"vin": [VIN]})

    def batches():
        yield source.to_batches()[0]
        uv.decode(VIN, now=NOW)  # A different clock used between pulls must not affect this job.
        yield source.to_batches()[0]

    reader = pa.RecordBatchReader.from_batches(source.schema, batches())
    dst = tmp_path / "decoded.parquet"
    stream = uv.decode_stream(reader, columns=["Make"], now=PAST)
    assert stream.to_parquet(dst) == 2
    table = pq.read_table(dst)
    expected = uv.decode(VIN, now=PAST)
    assert table["decoded_model_year"].to_pylist() == [expected["model_year"]] * 2
    assert table["Make"].to_pylist() == [expected["attributes"].get("Make") or None] * 2
    identity = uv.provenance()
    assert table.schema.metadata == {
        b"ultravin.data_month": identity["data_month"].encode(),
        b"ultravin.artifact_blake3": identity["artifact_blake3"].encode(),
        b"ultravin.decoder_version": identity["decoder_version"].encode(),
        b"ultravin.now_micros": b"0",
    }
    assert table.schema.field("Make").metadata == {b"element_id": b"26", b"variable": b"Make"}


def test_parquet_directory_reuses_clock_across_files_and_chunks(tmp_path: Path) -> None:
    parts = tmp_path / "parts"
    parts.mkdir()
    for name in ["a", "b"]:
        pq.write_table(pa.table({"vin": [VIN, VIN]}), parts / f"{name}.parquet")
    reader = pa.RecordBatchReader.from_stream(uv.decode_stream(parts, columns=["Make"], batch_size=1, now=PAST))
    batches = list(reader)
    assert len(batches) == 4
    expected = uv.decode(VIN, now=PAST)
    for batch in batches:
        assert batch["decoded_model_year"].to_pylist() == [expected["model_year"]]
        assert batch["Make"].to_pylist() == [expected["attributes"].get("Make") or None]
        assert batch.schema.metadata[b"ultravin.now_micros"] == b"0"
