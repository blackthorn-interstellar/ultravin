"""The dataset door is a projection of `decode`, nothing more.

`decode_stream` never builds a Python row, so nothing about it can be checked by
reading its output alone — the contract is that row *i* of the output equals
`decode(vin, year=y)` for row *i* of the input, with each projected element cast
to the type its vPIC `data_type` declares and an empty value written as null.
Assert that equivalence rather than the values, so the tests survive a vPIC data
refresh. The other half of the contract is a cost, not a value: peak memory is
O(`batch_size`), not O(rows) and not O(input columns), which the last two tests
measure in a subprocess.

pyarrow appears here to write fixtures, to read results back, and as the Arrow
producer on the input side — ultravin itself carries no pyarrow dependency, and a
test that used its writer to produce the expected values would be checking arrow
against arrow.
"""

from __future__ import annotations

import re
import subprocess
import sys
import threading
from collections.abc import Sequence
from datetime import date
from pathlib import Path
from typing import Any, Literal

import pyarrow as pa
import pyarrow.parquet as pq
import pytest
import ultravin as uv
from typer.testing import CliRunner, Result
from ultravin.cli import app

from tests.vin_samples import VINS

# Projected elements, pinned by `element_id` — the key NHTSA does not rename
# between releases — with the vPIC `data_type` each one exercises.
MAKE = 26  # lookup  -> str
CYLINDERS = 9  # int     -> int64
DISPLACEMENT_L = 13  # decimal -> float64
ERROR_CODE = 143  # lookup  -> str; where a contradicted caller year shows up as 12
NOTE = 114  # string  -> str, and repeat-exempt: first occurrence wins
PROJECTED = [MAKE, CYLINDERS, DISPLACEMENT_L, ERROR_CODE, NOTE]

BY_ID = {entry["element_id"]: entry for entry in uv.ELEMENTS.values()}

HONDA = "1HGCM82633A004352"  # decodes to model year 2003 with no hint
FORD = "1FTFW1ET5DFC10312"

# Mixed corpus: the shared samples (a clean hit, a single-WMI fallback, an
# unknown WMI, …) plus the row shapes only a dataset can produce — a null cell,
# an empty string, junk, a truncated VIN — and the caller years that matter.
# 1995 contradicts the VIN-derived 2003 and 1979 falls outside vPIC's window;
# both add error code 12 — the case a dropped-on-the-floor `year` column would
# still look right without.
CORPUS: list[tuple[str | None, int | None]] = [
    *((vin, None) for vin in VINS),
    (HONDA, 2003),
    (HONDA, 2013),
    (HONDA, 1995),
    (HONDA, 1979),
    (None, 2013),
    ("", None),
    ("NOTAVIN", None),
    (HONDA[:-1], None),
]


def text(values: Sequence[str | None]) -> pa.Array:
    return pa.array(values, pa.string())


def ints(values: Sequence[int | None]) -> pa.Array:
    return pa.array(values, pa.int32())


def write(path: Path, **columns: pa.Array) -> Path:
    """One parquet fixture, columns in the order given."""
    pq.write_table(pa.table(dict(columns)), path)
    return path


def columns(source: Any, **kwargs: Any) -> dict[str, list[Any]]:
    """A whole decode stream drained into one `{column: [values]}` dict.

    The C stream capsule is the transport; pyarrow is only here to turn it back
    into something a test can assert on.
    """
    return pa.table(uv.decode_stream(source, **kwargs)).to_pydict()


def to_file(source: Any, dst: Path, **kwargs: Any) -> int:
    """`decode_stream(...).to_parquet(dst)`: the rows written."""
    return uv.decode_stream(source, **kwargs).to_parquet(dst)


def cast(element_id: int, value: str | list[str]) -> Any:
    """One decoded value as its column holds it: typed, and empty means null."""
    if isinstance(value, list):
        # A repeat-exempt note field. A column holds one value per row, so the
        # kernel keeps the first — the notes are provenance, not rival answers.
        value = value[0] if value else ""
    if not value:
        return None
    data_type = BY_ID[element_id]["data_type"]
    if data_type == "int":
        return int(value)
    if data_type == "decimal":
        return float(value)
    return value


def reference(corpus: list[tuple[str | None, int | None]], ids: list[int]) -> dict[str, list[Any]]:
    """The decoded columns a caller would build by hand, one `decode` per row.

    A null VIN cell decodes as the empty VIN — the row still has to exist.
    """
    out: dict[str, list[Any]] = {"decoded_model_year": []}
    for element_id in ids:
        out[BY_ID[element_id]["variable"]] = []
    for vin, year in corpus:
        r = uv.decode(vin or "", year=year)
        out["decoded_model_year"].append(r["model_year"])
        for element_id in ids:
            name = BY_ID[element_id]["variable"]
            out[name].append(cast(element_id, r["attributes"].get(name, "")))
    return out


def corpus_file(path: Path, corpus: list[tuple[str | None, int | None]] = CORPUS) -> Path:
    return write(path, vin=text([v for v, _ in corpus]), year=ints([y for _, y in corpus]))


def test_every_row_equals_its_own_decode(tmp_path: Path) -> None:
    src = corpus_file(tmp_path / "in.parquet")
    passthrough = {"vin": [v for v, _ in CORPUS], "year": [y for _, y in CORPUS]}
    decoded = reference(CORPUS, PROJECTED)
    # A corpus of all hits or all misses would pass this test while proving
    # nothing, so check the mix before comparing against it.
    assert {"HONDA", None} <= set(decoded["Make"])
    assert any(v is not None for v in decoded["Displacement (L)"])
    assert columns(src, columns=PROJECTED) == passthrough | decoded


def test_a_contradicted_caller_year_flags_error_12(tmp_path: Path) -> None:
    """The caller year reaches the decode, wins, and takes error 12 with it."""
    corpus: list[tuple[str | None, int | None]] = [(HONDA, None), (HONDA, 1995)]
    out = columns(corpus_file(tmp_path / "in.parquet", corpus), columns=[ERROR_CODE])
    assert out["decoded_model_year"] == [2003, 1995]
    assert out["Error Code"] == ["0", "3,12,14"]


def test_names_and_ids_pick_the_same_columns(tmp_path: Path) -> None:
    src = corpus_file(tmp_path / "in.parquet")
    names = [BY_ID[element_id]["variable"] for element_id in PROJECTED]
    assert columns(src, columns=names) == columns(src, columns=PROJECTED)


def test_a_projection_can_mix_names_and_ids(tmp_path: Path) -> None:
    src = corpus_file(tmp_path / "in.parquet")
    mixed = columns(src, columns=["Make", CYLINDERS, "Displacement (L)"])
    # [vin, year, decoded_model_year, ..projected] — the projection is the tail.
    assert list(mixed)[3:] == ["Make", "Engine Number of Cylinders", "Displacement (L)"]
    assert mixed == columns(src, columns=[MAKE, CYLINDERS, DISPLACEMENT_L])


def test_naming_one_element_twice_over_is_refused(tmp_path: Path) -> None:
    """A name and the id it maps to are the same column, not two."""
    src = corpus_file(tmp_path / "in.parquet")
    with pytest.raises(ValueError, match="requested more than once"):
        uv.decode_stream(src, columns=["Make", MAKE])


def test_the_default_projection_is_every_public_element(tmp_path: Path) -> None:
    src = corpus_file(tmp_path / "in.parquet")
    narrow = columns(src, columns=PROJECTED)
    wide = columns(src)
    assert set(wide) > set(narrow)
    assert len(wide) > 100, f"suspiciously narrow default projection: {len(wide)} columns"
    for name, values in narrow.items():
        assert wide[name] == values, name


def test_written_columns_carry_the_vpic_data_type(tmp_path: Path) -> None:
    src = corpus_file(tmp_path / "in.parquet")
    dst = tmp_path / "out.parquet"
    assert to_file(src, dst, columns=PROJECTED) == len(CORPUS)

    table = pq.read_table(dst)
    assert table.schema.names == ["vin", "year", "decoded_model_year", *(BY_ID[i]["variable"] for i in PROJECTED)]
    assert dict(zip(table.schema.names, (str(t) for t in table.schema.types))) == {
        "vin": "string",  # passed through as written
        "year": "int32",  # the caller-year column, whatever width it arrived in
        "decoded_model_year": "int32",
        BY_ID[MAKE]["variable"]: "string",
        BY_ID[CYLINDERS]["variable"]: "int64",
        BY_ID[DISPLACEMENT_L]["variable"]: "double",
        BY_ID[ERROR_CODE]["variable"]: "string",
        BY_ID[NOTE]["variable"]: "string",
    }
    # The file and the streamed shape are the same decode, so one is the
    # reference for what landed on disk.
    assert table.to_pydict() == columns(src, columns=PROJECTED)


@pytest.mark.parametrize("column_names", ["variable", "id"])
def test_every_projected_column_carries_both_metadata_keys(
    tmp_path: Path, column_names: Literal["variable", "id"]
) -> None:
    """Whichever key the label spells out, the other is the one a consumer would
    otherwise have to re-derive — so both ride along, and both survive parquet."""
    src = corpus_file(tmp_path / "in.parquet")
    dst = tmp_path / "out.parquet"
    to_file(src, dst, columns=PROJECTED, column_names=column_names)
    streamed = pa.schema(uv.decode_stream(src, columns=PROJECTED, column_names=column_names))

    for schema in (streamed, pq.read_schema(dst)):
        # Passthrough columns are not elements, so they carry nothing.
        for name in ("vin", "year", "decoded_model_year"):
            assert not schema.field(name).metadata
        for element_id in PROJECTED:
            variable = BY_ID[element_id]["variable"]
            label = variable if column_names == "variable" else f"attr_{element_id}"
            assert schema.field(label).metadata == {
                b"element_id": str(element_id).encode(),
                b"variable": variable.encode(),
            }


def test_id_naming_labels_the_projection_by_element_id(tmp_path: Path) -> None:
    """vPIC variable names move between monthly releases; element ids do not, so
    a long-lived table pins to `attr_<id>` and stops drifting."""
    src = corpus_file(tmp_path / "in.parquet")
    by_id = columns(src, columns=PROJECTED, column_names="id")
    # Passthrough columns keep their own names in both modes.
    assert list(by_id)[:3] == ["vin", "year", "decoded_model_year"]
    assert list(by_id)[3:] == [f"attr_{element_id}" for element_id in PROJECTED]

    # Same decode, different labels: only the naming changed.
    by_name = columns(src, columns=PROJECTED)
    assert list(by_id.values()) == list(by_name.values())


def test_an_unknown_column_names_mode_is_refused(tmp_path: Path) -> None:
    src = corpus_file(tmp_path / "in.parquet")
    with pytest.raises(ValueError, match=r'column_names must be "variable" or "id"'):
        uv.decode_stream(src, columns=[MAKE], column_names="attr")  # ty: ignore[invalid-argument-type]


def test_an_attr_label_collides_the_way_a_variable_name_does() -> None:
    """`attr_26` is a real label under id naming, so a source column of that name
    is shadowed exactly as a source column called `Make` is under variable naming."""
    table = pa.table({"attr_26": text([HONDA])})
    with pytest.raises(ValueError, match="collides with a passthrough column"):
        uv.decode_stream(table, vin_column="attr_26", columns=[MAKE], column_names="id")
    # Under variable naming the labels no longer clash, so the same input is fine.
    assert columns(table, vin_column="attr_26", columns=[MAKE])["Make"] == ["HONDA"]

    # The mirror image: a source column named `Make` collides under variable naming.
    with pytest.raises(ValueError, match="collides with a passthrough column"):
        uv.decode_stream(pa.table({"Make": text([HONDA])}), vin_column="Make", columns=[MAKE])


@pytest.mark.parametrize(
    ("vin_column", "year_column"),
    [
        ("vin", "year"),
        ("VIN", "Model_Year"),  # both matched by name, case-insensitively
        ("chassis_no", "built"),  # neither name is known: both are sniffed
    ],
)
def test_the_vin_and_year_columns_are_autodetected(tmp_path: Path, vin_column: str, year_column: str) -> None:
    src = write(tmp_path / "in.parquet", **{vin_column: text([HONDA, None]), year_column: ints([1995, None])})
    out = columns(src, columns=[MAKE])
    assert list(out) == [vin_column, year_column, "decoded_model_year", "Make"]
    assert out[vin_column] == [HONDA, None]
    assert out["decoded_model_year"] == [1995, None]


def test_sample_rows_zero_still_sniffs_a_row(tmp_path: Path) -> None:
    """Sniffing zero rows would report every column as unrecognizable rather than
    as unsampled, so it clamps to one the way `batch_size` does."""
    src = write(tmp_path / "in.parquet", chassis_no=text([HONDA, FORD]))
    assert columns(src, columns=[MAKE], sample_rows=0)["Make"] == ["HONDA", "FORD"]


@pytest.mark.parametrize(
    ("dtype", "value", "name"),
    [
        (pa.date32(), date(2013, 6, 1), "Date32"),
        (pa.bool_(), True, "Boolean"),
        (pa.string(), "2013", "Utf8"),
    ],
)
def test_a_caller_year_column_that_is_not_a_number_is_refused(
    tmp_path: Path, dtype: pa.DataType, value: object, name: str
) -> None:
    """A Date32 casts to days-since-epoch and a Boolean to 0/1; either way every
    value lands outside vPIC's year window, which discards the hint *and* stamps
    error 12 on every row — a whole-dataset corruption that looks like a result."""
    src = tmp_path / "in.parquet"
    pq.write_table(pa.table({"vin": text([HONDA]), "year": pa.array([value], dtype)}), src)
    with pytest.raises(ValueError, match=rf'caller-year column "year" holds {name}, not a number'):
        columns(src, year_column="year", columns=[MAKE])


def test_a_float_caller_year_column_decodes_and_nan_means_no_hint(tmp_path: Path) -> None:
    """float64 is what pandas gives an integer column holding a missing value, so
    it is how a real year column most often arrives — it must not be refused.

    The cast is safe rather than lossy: 2013.0 is the hint, NaN is no hint.
    """
    src = tmp_path / "in.parquet"
    years = pa.array([2013.0, float("nan")], pa.float64())
    pq.write_table(pa.table({"vin": text([HONDA, HONDA]), "year": years}), src)

    out = columns(src, year_column="year", columns=[MAKE])
    # Row 0 takes the hint; row 1 falls back to the VIN's own 2003.
    assert out["decoded_model_year"] == [2013, 2003]
    # The passthrough is emitted as Int32, with NaN carried through as null.
    assert out["year"] == [2013, None]
    assert out["Make"] == ["HONDA", "HONDA"]


def test_a_sniffed_column_still_decodes_its_own_rows(tmp_path: Path) -> None:
    """The batch the sniffers read is the first batch — it must not be eaten."""
    src = write(tmp_path / "in.parquet", chassis_no=text([HONDA] * 5))
    assert columns(src, columns=[MAKE], sample_rows=2)["Make"] == ["HONDA"] * 5


def test_ambiguous_columns_are_refused(tmp_path: Path) -> None:
    two_vins = write(tmp_path / "vins.parquet", vin=text([HONDA]), VIN=text([HONDA]))
    with pytest.raises(ValueError, match="ambiguous VIN column"):
        columns(two_vins, columns=[MAKE])

    two_years = write(tmp_path / "years.parquet", vin=text([HONDA]), built=ints([2003]), sold=ints([2013]))
    with pytest.raises(ValueError, match="ambiguous caller-year column"):
        columns(two_years, columns=[MAKE])


def test_a_source_without_a_vin_column_is_refused(tmp_path: Path) -> None:
    src = write(tmp_path / "in.parquet", notes=text(["hello", "there"]))
    with pytest.raises(ValueError, match="could not autodetect a VIN-like column"):
        columns(src, columns=[MAKE])


def test_naming_the_columns_skips_autodetect(tmp_path: Path) -> None:
    """Two VIN-shaped and two year-shaped columns: autodetect refuses both, so
    naming them is the only way in — and a name that isn't there is a mistake."""
    src = write(
        tmp_path / "in.parquet",
        primary=text([HONDA]),
        secondary=text([FORD]),
        built=ints([2003]),
        sold=ints([1995]),
    )
    out = columns(src, vin_column="secondary", year_column="sold", columns=[MAKE])
    assert out["Make"] == ["FORD"]
    assert out["decoded_model_year"] == [1995]

    with pytest.raises(ValueError, match='no column named "nope"'):
        columns(src, vin_column="nope", columns=[MAKE])


def test_a_directory_reads_as_one_stream_in_sorted_order(tmp_path: Path) -> None:
    parts = tmp_path / "parts"
    parts.mkdir()
    # Written out of order, and with a non-parquet file to be ignored.
    write(parts / "b.parquet", vin=text([FORD]))
    write(parts / "a.parquet", vin=text([HONDA, "ZZZCM82633A004352"]))
    (parts / "README.txt").write_text("ignored")

    assert columns(parts, columns=[MAKE])["Make"] == ["HONDA", None, "FORD"]


def test_a_directory_whose_files_disagree_is_refused_the_same_way_everywhere(tmp_path: Path) -> None:
    """`b` has a year column `a` lacks, so the two resolve different output shapes.

    Zipped positionally that wrote `b`'s passthrough year into
    `decoded_model_year`. It only surfaces on the second file, mid-stream, so it
    also pins that a caller mistake stays a `ValueError` after the round trip
    through the Arrow C interface — where the error type is arrow's, not ours.
    """
    parts = tmp_path / "parts"
    parts.mkdir()
    write(parts / "a.parquet", vin=text([HONDA]))
    write(parts / "b.parquet", vin=text([HONDA]), year=ints([1979]))

    refused = "b.parquet: passes through"
    with pytest.raises(ValueError, match=refused):
        columns(parts, columns=[MAKE])
    with pytest.raises(ValueError, match=refused):
        to_file(parts, tmp_path / "out.parquet", columns=[MAKE])


def test_writing_over_the_source_is_refused(tmp_path: Path) -> None:
    """The writer truncates `dst` while the reader is still on it."""
    src = corpus_file(tmp_path / "in.parquet")
    with pytest.raises(ValueError, match="is the source being decoded"):
        to_file(src, src, columns=[MAKE])
    # Refused before the writer opened, so the input survived.
    assert len(pq.read_table(src)) == len(CORPUS)

    parts = tmp_path / "parts"
    parts.mkdir()
    write(parts / "a.parquet", vin=text([HONDA]))
    with pytest.raises(ValueError, match="inside the source directory"):
        to_file(parts, parts / "out.parquet", columns=[MAKE])


def test_a_refused_destination_leaves_the_stream_usable(tmp_path: Path) -> None:
    """The clobber check runs before the source is consumed, so the caller can
    retry with a destination that works."""
    src = corpus_file(tmp_path / "in.parquet")
    stream = uv.decode_stream(src, columns=[MAKE])
    with pytest.raises(ValueError, match="is the source being decoded"):
        stream.to_parquet(src)
    assert stream.to_parquet(tmp_path / "out.parquet") == len(CORPUS)


def test_a_dictionary_encoded_vin_column_is_autodetected(tmp_path: Path) -> None:
    """What pandas writes for a categorical column — text under an index."""
    src = tmp_path / "in.parquet"
    pq.write_table(pa.table({"chassis_no": text([HONDA, HONDA]).dictionary_encode()}), src)
    assert pq.read_schema(src).field("chassis_no").type == pa.dictionary(pa.int32(), pa.string())

    assert columns(src, columns=[MAKE]) == {
        "chassis_no": [HONDA, HONDA],
        "decoded_model_year": [2003, 2003],
        "Make": ["HONDA", "HONDA"],
    }


def test_columns_that_cannot_be_what_they_are_named_are_refused(tmp_path: Path) -> None:
    src = write(tmp_path / "in.parquet", vin=text([HONDA]), axles=ints([2]))
    # Casting ints to text would decode garbage into a row of nulls — silently,
    # since that is also what an undecodable VIN looks like.
    with pytest.raises(ValueError, match='VIN column "axles" holds Int32, not text'):
        columns(src, vin_column="axles", columns=[MAKE])
    with pytest.raises(ValueError, match="both the VIN and the caller-year"):
        columns(src, vin_column="vin", year_column="vin", columns=[MAKE])


def test_an_empty_projection_still_writes_the_passthrough_columns(tmp_path: Path) -> None:
    src = corpus_file(tmp_path / "in.parquet")
    dst = tmp_path / "out.parquet"
    expected = {
        "vin": [v for v, _ in CORPUS],
        "year": [y for _, y in CORPUS],
        "decoded_model_year": reference(CORPUS, [])["decoded_model_year"],
    }
    assert columns(src, columns=[]) == expected
    assert to_file(src, dst, columns=[]) == len(CORPUS)
    assert pq.read_table(dst).to_pydict() == expected


def test_the_stream_chunks_rather_than_materializing_the_source(tmp_path: Path) -> None:
    src = corpus_file(tmp_path / "in.parquet")
    reader = pa.RecordBatchReader.from_stream(uv.decode_stream(src, columns=PROJECTED, batch_size=4))
    sizes = [batch.num_rows for batch in reader]
    assert len(sizes) > 1, "the corpus should not fit in a single chunk"
    assert max(sizes) <= 4
    assert sum(sizes) == len(CORPUS)


def test_a_missing_source_is_an_oserror(tmp_path: Path) -> None:
    with pytest.raises(OSError, match="not found"):
        uv.decode_stream(tmp_path / "nope.parquet", columns=[MAKE])


def test_a_bad_projection_is_a_value_error(tmp_path: Path) -> None:
    src = corpus_file(tmp_path / "in.parquet")
    with pytest.raises(ValueError, match="unknown element_id 999999"):
        uv.decode_stream(src, columns=[999_999])
    with pytest.raises(ValueError, match="requested more than once"):
        uv.decode_stream(src, columns=[MAKE, MAKE])
    with pytest.raises(ValueError, match='unknown column "Nope"'):
        uv.decode_stream(src, columns=["Nope"])
    with pytest.raises(TypeError, match=r"element ids \(int\) or variable names"):
        uv.decode_stream(src, columns=[1.5])  # ty: ignore[invalid-argument-type]
    # `bool` is an `int` subclass, so `True` would otherwise project element 1.
    # The type checker accepts it for the same reason, which is why the guard has
    # to be at runtime — note the absence of an ignore directive here.
    with pytest.raises(TypeError, match=r"element ids \(int\) or variable names"):
        uv.decode_stream(src, columns=[True])


# ── Arrow sources ─────────────────────────────────────────────────────────────


def test_a_pyarrow_table_decodes_without_touching_the_disk() -> None:
    table = pa.table({"vin": text([HONDA, FORD, None]), "year": ints([None, None, 2013])})
    out = columns(table, columns=[MAKE])
    assert out == {
        "vin": [HONDA, FORD, None],
        "year": [None, None, 2013],
        "decoded_model_year": [2003, 2013, 2013],
        "Make": ["HONDA", "FORD", None],
    }


def test_a_bare_array_is_read_as_the_vin_column() -> None:
    """A single unnamed array carries no field name, and the only thing this
    decoder could be handed is VINs."""
    out = columns(pa.array([HONDA, FORD]), columns=[MAKE])
    assert list(out) == ["vin", "decoded_model_year", "Make"]
    assert out["Make"] == ["HONDA", "FORD"]


def test_a_record_batch_reader_streams_through() -> None:
    table = pa.table({"vin": text([HONDA] * 9)})
    reader = pa.RecordBatchReader.from_batches(table.schema, table.to_batches(max_chunksize=2))
    assert columns(reader, columns=[MAKE])["Make"] == ["HONDA"] * 9


@pytest.mark.parametrize(
    "dtype",
    [pa.string(), pa.large_string(), pa.string_view(), pa.dictionary(pa.int32(), pa.string())],
)
def test_every_text_encoding_of_a_vin_column_decodes(dtype: pa.DataType) -> None:
    """polars hands over LargeUtf8, arrow-rs 5x hands over Utf8View, pandas
    categoricals arrive as a dictionary — all are text and all must decode."""
    table = pa.table({"vin": text([HONDA, FORD]).cast(dtype)})
    assert columns(table, columns=[MAKE])["Make"] == ["HONDA", "FORD"]


def test_an_arrow_source_without_a_vin_column_is_refused() -> None:
    with pytest.raises(ValueError, match="could not autodetect a VIN-like column"):
        uv.decode_stream(pa.table({"notes": text(["hello"])}), columns=[MAKE])
    # Unlike parquet, there is nothing to sniff here — but naming it works.
    assert columns(pa.table({"notes": text([HONDA])}), vin_column="notes", columns=[MAKE])["Make"] == ["HONDA"]


def test_an_arrow_source_writes_the_same_parquet_a_path_source_does(tmp_path: Path) -> None:
    """`to_parquet` is source-agnostic: the writer sits behind the same reader
    interface either way, so the two doors must produce identical files."""
    src = corpus_file(tmp_path / "in.parquet")
    from_path = tmp_path / "from_path.parquet"
    from_arrow = tmp_path / "from_arrow.parquet"

    assert to_file(src, from_path, columns=PROJECTED) == len(CORPUS)
    assert to_file(pq.read_table(src), from_arrow, columns=PROJECTED) == len(CORPUS)

    assert pq.read_table(from_arrow).to_pydict() == pq.read_table(from_path).to_pydict()
    assert pq.read_schema(from_arrow) == pq.read_schema(from_path)


def test_a_python_backed_arrow_producer_survives_the_released_gil(tmp_path: Path) -> None:
    """`to_parquet` releases the GIL and then pulls the input stream.

    When that input is a pyarrow reader wrapping a *Python* generator, each pull
    re-enters the interpreter from a thread that has just given the GIL up. It has
    to re-acquire it rather than deadlock, so drive a real generator through the
    whole path — a regression here hangs instead of failing, hence the row count
    assert on the far side.
    """
    schema = pa.schema([("vin", pa.string())])
    chunks = 5
    rows_per_chunk = 3

    def batches() -> object:
        for _ in range(chunks):
            yield pa.record_batch([text([HONDA] * rows_per_chunk)], schema=schema)

    reader = pa.RecordBatchReader.from_batches(schema, batches())
    dst = tmp_path / "out.parquet"
    assert uv.decode_stream(reader, columns=[MAKE]).to_parquet(dst) == chunks * rows_per_chunk
    assert pq.read_table(dst).column("Make").to_pylist() == ["HONDA"] * (chunks * rows_per_chunk)


def test_an_arrow_source_and_a_parquet_source_agree(tmp_path: Path) -> None:
    src = corpus_file(tmp_path / "in.parquet")
    assert columns(pq.read_table(src), columns=PROJECTED) == columns(src, columns=PROJECTED)


# ── Stream semantics ──────────────────────────────────────────────────────────


def test_a_stream_is_single_use(tmp_path: Path) -> None:
    """Both exits consume the source; a second use would hand back a silently
    truncated result, so it raises instead."""
    src = corpus_file(tmp_path / "in.parquet")
    consumed = "already been consumed"

    stream = uv.decode_stream(src, columns=[MAKE])
    pa.table(stream)
    with pytest.raises(RuntimeError, match=consumed):
        pa.table(stream)

    stream = uv.decode_stream(src, columns=[MAKE])
    assert stream.to_parquet(tmp_path / "out.parquet") == len(CORPUS)
    with pytest.raises(RuntimeError, match=consumed):
        stream.to_parquet(tmp_path / "out2.parquet")

    stream = uv.decode_stream(src, columns=[MAKE])
    pa.table(stream)
    with pytest.raises(RuntimeError, match=consumed):
        stream.to_parquet(tmp_path / "out3.parquet")


def test_the_schema_is_known_before_a_row_is_decoded(tmp_path: Path) -> None:
    """`__arrow_c_schema__` does not consume the stream — a caller can look at
    the output shape and still decode it."""
    src = corpus_file(tmp_path / "in.parquet")
    stream = uv.decode_stream(src, columns=[MAKE])
    assert pa.schema(stream).names == ["vin", "year", "decoded_model_year", "Make"]
    assert pa.schema(stream).names == ["vin", "year", "decoded_model_year", "Make"]
    assert pa.table(stream).num_rows == len(CORPUS)


def test_only_one_of_two_racing_consumers_gets_the_stream(tmp_path: Path) -> None:
    """The reader is handed over under a lock, so two threads cannot both take
    it and decode overlapping halves of the source."""
    src = corpus_file(tmp_path / "in.parquet")
    stream = uv.decode_stream(src, columns=[MAKE])
    start = threading.Barrier(2)
    won: list[int] = []
    lost: list[str] = []

    def consume() -> None:
        start.wait()
        try:
            won.append(pa.table(stream).num_rows)  # list.append is atomic
        except RuntimeError as exc:
            lost.append(str(exc))

    threads = [threading.Thread(target=consume) for _ in range(2)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    assert won == [len(CORPUS)]
    assert len(lost) == 1
    assert "already been consumed" in lost[0]


def test_a_zero_row_source_still_carries_the_output_schema(tmp_path: Path) -> None:
    """A caller writing parquet needs the columns even when there are no rows,
    so the schema is settled at open time rather than from the first batch."""
    empty = pa.table({"vin": pa.array([], pa.string())})
    assert pa.table(uv.decode_stream(empty, columns=[MAKE])).num_rows == 0

    src = write(tmp_path / "in.parquet", vin=text([]))
    dst = tmp_path / "out.parquet"
    assert to_file(src, dst, columns=[MAKE]) == 0
    assert pq.read_schema(dst).names == ["vin", "decoded_model_year", "Make"]


def test_a_struct_array_is_read_as_its_named_columns() -> None:
    """Unlike a bare array, a struct already carries field names — including a
    caller-year column, which has to reach the decode."""
    struct = pa.StructArray.from_arrays([pa.array([HONDA]), pa.array([2013], pa.int32())], names=["vin", "year"])
    assert columns(struct, columns=[MAKE]) == {
        "vin": [HONDA],
        "year": [2013],
        "decoded_model_year": [2013],
        "Make": ["HONDA"],
    }


def test_an_abandoned_capsule_releases_its_stream(tmp_path: Path) -> None:
    """The exported stream owns the decoder; dropping the capsule undrained has
    to run its release callback, or every abandoned stream leaks one reader."""
    src = corpus_file(tmp_path / "in.parquet")
    for _ in range(200):
        capsule = uv.decode_stream(src, columns=[MAKE]).__arrow_c_stream__()
        del capsule
    # Half-drained is the other release path: the consumer stops mid-stream.
    for _ in range(200):
        reader = pa.RecordBatchReader.from_stream(uv.decode_stream(src, columns=[MAKE], batch_size=2))
        next(iter(reader))
        del reader


def test_a_list_of_vins_points_at_decode_batch() -> None:
    with pytest.raises(TypeError, match="not a list of VINs"):
        uv.decode_stream([HONDA, FORD])  # ty: ignore[invalid-argument-type]
    # The source is diagnosed before the projection, so a caller who got both
    # wrong is told about the argument that has no chance of working.
    with pytest.raises(TypeError, match="not a list of VINs"):
        uv.decode_stream([HONDA], columns=[999_999])  # ty: ignore[invalid-argument-type]


def test_a_source_that_is_neither_a_path_nor_arrow_is_refused() -> None:
    with pytest.raises(TypeError, match="Arrow C data interface"):
        uv.decode_stream(42)  # ty: ignore[invalid-argument-type]


def test_to_pandas_needs_pyarrow(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """pyarrow is imported lazily and is not an ultravin dependency, so the
    failure has to name the fix rather than surfacing as a bare ImportError."""
    src = corpus_file(tmp_path / "in.parquet")
    # `None` in sys.modules is how CPython spells "this import is blocked".
    monkeypatch.setitem(sys.modules, "pyarrow", None)
    with pytest.raises(ImportError, match=r"to_pandas\(\) requires pyarrow"):
        uv.decode_stream(src, columns=[MAKE]).to_pandas()


def test_to_pandas_returns_the_decoded_frame(tmp_path: Path) -> None:
    pytest.importorskip("pandas", reason="pandas is not an ultravin dependency, dev or otherwise")
    src = corpus_file(tmp_path / "in.parquet")
    frame = uv.decode_stream(src, columns=[MAKE]).to_pandas()
    assert list(frame.columns) == ["vin", "year", "decoded_model_year", "Make"]
    assert len(frame) == len(CORPUS)
    assert frame["Make"][0] == uv.decode(CORPUS[0][0] or "")["attributes"]["Make"]


# ── CLI ───────────────────────────────────────────────────────────────────────


STYLING = re.compile(r"\x1b\[[0-9;]*m")


def cli(*args: str) -> Result:
    """`decode-parquet` under a console wide enough not to wrap an error message.

    typer renders parameter errors through rich, which wraps them in a panel
    sized to the console. `COLUMNS` is the one knob still live at call time —
    the rest of typer's console setup is snapshotted at import.
    """
    return CliRunner(env={"COLUMNS": "200"}).invoke(app, ["decode-parquet", *args])


def plain(text: str) -> str:
    """stderr as a human reads it, with rich's styling taken back out.

    Seeing `GITHUB_ACTIONS` in the environment, typer force-enables color, and
    the highlighter then opens a style run per flag fragment — `--batch-size`
    arrives as `-`, `-batch`, `-size` with escape codes between them, which is
    why a substring assert that passes locally failed on CI. The decision is
    made when `typer.rich_utils` is imported, so no environment the runner sets
    can undo it; the message is the contract, the styling is not.
    """
    return STYLING.sub("", text)


def test_cli_writes_parquet_and_reports_the_rows_on_stderr(tmp_path: Path) -> None:
    src = corpus_file(tmp_path / "in.parquet")
    dst = tmp_path / "out.parquet"
    result = cli(str(src), str(dst), "--columns", "Make,Model")

    assert result.exit_code == 0, result.output
    # Nothing on stdout: the summary is progress, not data a pipe should swallow.
    assert result.stdout == ""
    assert plain(result.stderr) == f"wrote {len(CORPUS)} rows to {dst}\n"
    assert pq.read_table(dst).column("Make")[0].as_py() == "HONDA"


def test_cli_columns_take_ids_and_names_together(tmp_path: Path) -> None:
    """A token that parses as an integer is an element id; anything else is a
    variable name — so one flag covers both spellings."""
    src = corpus_file(tmp_path / "in.parquet")
    dst = tmp_path / "out.parquet"
    assert cli(str(src), str(dst), "--columns", f"Make,{CYLINDERS}").exit_code == 0
    assert pq.read_table(dst).schema.names[-2:] == ["Make", "Engine Number of Cylinders"]


def test_cli_rejects_an_unknown_column(tmp_path: Path) -> None:
    src = corpus_file(tmp_path / "in.parquet")
    result = cli(str(src), str(tmp_path / "out.parquet"), "--columns", "Nope")

    assert result.exit_code == 2
    assert 'unknown column "Nope"' in plain(result.stderr)


def test_cli_column_names_id_labels_by_element_id(tmp_path: Path) -> None:
    src = corpus_file(tmp_path / "in.parquet")
    dst = tmp_path / "out.parquet"
    assert cli(str(src), str(dst), "--columns", f"Make,{CYLINDERS}", "--column-names", "id").exit_code == 0
    schema = pq.read_schema(dst)
    assert schema.names[-2:] == [f"attr_{MAKE}", f"attr_{CYLINDERS}"]
    assert schema.field(f"attr_{MAKE}").metadata[b"variable"] == b"Make"


def test_cli_rejects_an_unknown_column_names_mode(tmp_path: Path) -> None:
    src = corpus_file(tmp_path / "in.parquet")
    result = cli(str(src), str(tmp_path / "out.parquet"), "--column-names", "attr")
    assert result.exit_code == 2
    assert "--column-names" in plain(result.stderr)


@pytest.mark.parametrize("flag", ["--batch-size", "--sample-rows"])
def test_cli_rejects_a_row_count_below_one(tmp_path: Path, flag: str) -> None:
    """Both are row counts: 0 or negative reached Rust as a huge usize."""
    src = corpus_file(tmp_path / "in.parquet")
    result = cli(str(src), str(tmp_path / "out.parquet"), flag, "0")

    assert result.exit_code == 2
    assert f"Invalid value for '{flag}'" in plain(result.stderr)


# ── Cost, not value ───────────────────────────────────────────────────────────


def decode_peak(src: Path, dst: Path, **kwargs: Any) -> tuple[int, int]:
    """Decode `src` in a subprocess; return the rows written and its peak RSS.

    A subprocess so the measurement is that decode's peak and not the suite's —
    and on Linux `ru_maxrss` is not that measurement. `fork` hands the child
    the parent's page tables, so its RSS starts at the parent's, and `execve`
    folds that pre-exec high-water mark into the new process's `ru_maxrss`
    (`setmax_mm_hiwater_rss` in `exec_mmap`). The child therefore reports
    whatever pytest had peaked at across the whole session, which is how this
    read 849-865MB on CI while the decode itself stayed near 150MB. `VmHWM` is
    the post-exec mm's own mark and carries none of it. macOS spawns without
    the inheritance, so `ru_maxrss` — already bytes there — is the child's
    alone. Either way the child prints bytes.
    """
    peak_bytes = (
        "resource.getrusage(resource.RUSAGE_SELF).ru_maxrss"
        if sys.platform == "darwin"
        else "1024 * int(re.search(r'VmHWM:\\s+(\\d+)', open('/proc/self/status').read()).group(1))"
    )
    opts = "".join(f", {name}={value!r}" for name, value in kwargs.items())
    code = (
        "import re, resource, ultravin\n"
        f"rows = ultravin.decode_stream({str(src)!r}{opts}).to_parquet({str(dst)!r})\n"
        f"print(rows, {peak_bytes})\n"
    )
    proc = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, check=False)
    assert proc.returncode == 0, proc.stderr
    written, measured = proc.stdout.split()
    return int(written), int(measured)


def test_peak_memory_is_bounded_by_the_batch_size(tmp_path: Path) -> None:
    """Decoding must never hold the source in memory.

    100_000 rows under the default (every public element) projection, which
    materializes ~8.5KB per row. Both ends were measured on this fixture: the
    same decode taking the file as one chunk peaks at ~845MB, and streaming it in
    4_096-row chunks peaks at 115-150MB (macOS/arm64 to Linux/x86_64), so the cap
    sits between them with room on either side. More rows would be a wider trap,
    but these tests run against an unoptimized build where 100_000 rows already
    costs ~15s.
    """
    rows = 100_000
    batch_size = 4_096
    cap = 400 * 1024 * 1024

    src = tmp_path / "big.parquet"
    tiled = (VINS * (rows // len(VINS) + 1))[:rows]
    pq.write_table(pa.table({"vin": text(tiled)}), src, row_group_size=batch_size)

    written, peak = decode_peak(src, tmp_path / "out.parquet", batch_size=batch_size)
    assert written == rows
    assert peak < cap, f"peak RSS {peak / 1e6:.0f}MB decoding {rows} rows"


def test_peak_memory_is_bounded_by_the_columns_the_decode_reads(tmp_path: Path) -> None:
    """A wide source costs what its VIN column costs, not what its width does.

    Only the VIN (and any caller-year) column is ever read, so the reader is
    projected down to it and the other 400 never get materialized. Measured on
    this fixture: decoding every column peaks at ~470MB — each 8_192-row chunk
    holds 400 x 8_192 strings — against ~80MB projected, which is what the same
    16_384 rows cost as a two-column file. The cap sits between.
    """
    rows = 16_384
    batch_size = 8_192
    cap = 250 * 1024 * 1024

    src = tmp_path / "wide.parquet"
    tiled = (VINS * (rows // len(VINS) + 1))[:rows]
    # One filler array referenced 400 times: pyarrow shares the buffers, so the
    # width this measures costs nothing to build here.
    filler = text(["x" * 96] * rows)
    cols = {f"pad{i}": filler for i in range(400)}
    cols["vin"] = text(tiled)
    pq.write_table(pa.table(cols), src, row_group_size=batch_size)

    written, peak = decode_peak(src, tmp_path / "out.parquet", columns=[MAKE], batch_size=batch_size)
    assert written == rows
    assert peak < cap, f"peak RSS {peak / 1e6:.0f}MB decoding {rows} rows of 401 columns"
