"""The dataset door is a projection of `decode`, nothing more.

`decode_parquet` never builds a Python row, so nothing about it can be checked by
reading its output alone — the contract is that row *i* of the output equals
`decode(vin, year=y)` for row *i* of the input, with each projected element cast
to the type its vPIC `data_type` declares and an empty value written as null.
Assert that equivalence rather than the values, so the tests survive a vPIC data
refresh. The other half of the contract is a cost, not a value: peak memory is
O(`batch_size`), not O(rows) and not O(input columns), which the last two tests
measure in a subprocess.

pyarrow appears here only to write fixtures and read results back — ultravin
itself carries no pyarrow dependency, and a test that used its writer to produce
the expected values would be checking arrow against arrow.
"""

from __future__ import annotations

import re
import subprocess
import sys
from collections.abc import Sequence
from pathlib import Path
from typing import Any

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


def columns(src: Path, **kwargs: Any) -> dict[str, list[Any]]:
    """`decode_parquet` with no `dst`: the whole source as one column dict."""
    out = uv.decode_parquet(src, **kwargs)
    assert isinstance(out, dict)
    return out


def to_file(src: Path, dst: Path, **kwargs: Any) -> int:
    """`decode_parquet` writing parquet: the rows written."""
    rows = uv.decode_parquet(src, dst, **kwargs)
    assert isinstance(rows, int)
    return rows


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
        r = uv.decode(vin or "", year=year, flat=True)
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
    assert columns(src, ids=PROJECTED) == passthrough | decoded


def test_a_contradicted_caller_year_flags_error_12(tmp_path: Path) -> None:
    """The caller year reaches the decode, wins, and takes error 12 with it."""
    corpus: list[tuple[str | None, int | None]] = [(HONDA, None), (HONDA, 1995)]
    out = columns(corpus_file(tmp_path / "in.parquet", corpus), ids=[ERROR_CODE])
    assert out["decoded_model_year"] == [2003, 1995]
    assert out["Error Code"] == ["0", "3,12,14"]


def test_codes_are_ids_by_another_name(tmp_path: Path) -> None:
    src = corpus_file(tmp_path / "in.parquet")
    names = [BY_ID[element_id]["variable"] for element_id in PROJECTED]
    assert columns(src, codes=names) == columns(src, ids=PROJECTED)


def test_the_default_projection_is_every_public_element(tmp_path: Path) -> None:
    src = corpus_file(tmp_path / "in.parquet")
    narrow = columns(src, ids=PROJECTED)
    wide = columns(src)
    assert set(wide) > set(narrow)
    assert len(wide) > 100, f"suspiciously narrow default projection: {len(wide)} columns"
    for name, values in narrow.items():
        assert wide[name] == values, name


def test_written_columns_carry_the_vpic_data_type(tmp_path: Path) -> None:
    src = corpus_file(tmp_path / "in.parquet")
    dst = tmp_path / "out.parquet"
    assert to_file(src, dst, ids=PROJECTED) == len(CORPUS)

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
    # The file and the in-memory shape are the same decode, so the dict form is
    # the reference for what landed on disk.
    assert table.to_pydict() == columns(src, ids=PROJECTED)


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
    out = columns(src, ids=[MAKE])
    assert list(out) == [vin_column, year_column, "decoded_model_year", "Make"]
    assert out[vin_column] == [HONDA, None]
    assert out["decoded_model_year"] == [1995, None]


def test_a_sniffed_column_still_decodes_its_own_rows(tmp_path: Path) -> None:
    """The batch the sniffers read is the first batch — it must not be eaten."""
    src = write(tmp_path / "in.parquet", chassis_no=text([HONDA] * 5))
    assert columns(src, ids=[MAKE], sample_rows=2)["Make"] == ["HONDA"] * 5


def test_ambiguous_columns_are_refused(tmp_path: Path) -> None:
    two_vins = write(tmp_path / "vins.parquet", vin=text([HONDA]), VIN=text([HONDA]))
    with pytest.raises(ValueError, match="ambiguous VIN column"):
        columns(two_vins, ids=[MAKE])

    two_years = write(tmp_path / "years.parquet", vin=text([HONDA]), built=ints([2003]), sold=ints([2013]))
    with pytest.raises(ValueError, match="ambiguous caller-year column"):
        columns(two_years, ids=[MAKE])


def test_a_source_without_a_vin_column_is_refused(tmp_path: Path) -> None:
    src = write(tmp_path / "in.parquet", notes=text(["hello", "there"]))
    with pytest.raises(ValueError, match="could not autodetect a VIN-like column"):
        columns(src, ids=[MAKE])


def test_naming_the_columns_skips_autodetect(tmp_path: Path) -> None:
    """Two VIN-shaped and two year-shaped columns: autodetect refuses both, so
    naming them is the only way in — and a name that isn't there is a mistake."""
    src = write(
        tmp_path / "in.parquet",
        primary=text([HONDA]),
        secondary=text(["1FTFW1ET5DFC10312"]),
        built=ints([2003]),
        sold=ints([1995]),
    )
    out = columns(src, vin="secondary", year="sold", ids=[MAKE])
    assert out["Make"] == ["FORD"]
    assert out["decoded_model_year"] == [1995]

    with pytest.raises(ValueError, match='no column named "nope"'):
        columns(src, vin="nope", ids=[MAKE])


def test_a_directory_reads_as_one_stream_in_sorted_order(tmp_path: Path) -> None:
    parts = tmp_path / "parts"
    parts.mkdir()
    # Written out of order, and with a non-parquet file to be ignored.
    write(parts / "b.parquet", vin=text(["1FTFW1ET5DFC10312"]))
    write(parts / "a.parquet", vin=text([HONDA, "ZZZCM82633A004352"]))
    (parts / "README.txt").write_text("ignored")

    assert columns(parts, ids=[MAKE])["Make"] == ["HONDA", None, "FORD"]


def test_a_directory_whose_files_disagree_is_refused_the_same_way_everywhere(tmp_path: Path) -> None:
    """`b` has a year column `a` lacks, so the two resolve different output shapes.

    Zipped positionally that wrote `b`'s passthrough year into
    `decoded_model_year`; all three output modes now refuse it identically.
    """
    parts = tmp_path / "parts"
    parts.mkdir()
    write(parts / "a.parquet", vin=text([HONDA]))
    write(parts / "b.parquet", vin=text([HONDA]), year=ints([1979]))

    refused = "b.parquet: passes through"
    with pytest.raises(ValueError, match=refused):
        columns(parts, ids=[MAKE])
    with pytest.raises(ValueError, match=refused):
        to_file(parts, tmp_path / "out.parquet", ids=[MAKE])
    with pytest.raises(ValueError, match=refused):
        list(uv.ParquetBatchIter(parts, ids=[MAKE]))


def test_writing_over_the_source_is_refused(tmp_path: Path) -> None:
    """The writer truncates `dst` while the reader is still on it."""
    src = corpus_file(tmp_path / "in.parquet")
    with pytest.raises(ValueError, match="is the source being decoded"):
        to_file(src, src, ids=[MAKE])
    # Refused before the writer opened, so the input survived.
    assert len(pq.read_table(src)) == len(CORPUS)

    parts = tmp_path / "parts"
    parts.mkdir()
    write(parts / "a.parquet", vin=text([HONDA]))
    with pytest.raises(ValueError, match="inside the source directory"):
        to_file(parts, parts / "out.parquet", ids=[MAKE])


def test_a_dictionary_encoded_vin_column_is_autodetected(tmp_path: Path) -> None:
    """What pandas writes for a categorical column — text under an index."""
    src = tmp_path / "in.parquet"
    pq.write_table(pa.table({"chassis_no": text([HONDA, HONDA]).dictionary_encode()}), src)
    assert pq.read_schema(src).field("chassis_no").type == pa.dictionary(pa.int32(), pa.string())

    assert columns(src, ids=[MAKE]) == {
        "chassis_no": [HONDA, HONDA],
        "decoded_model_year": [2003, 2003],
        "Make": ["HONDA", "HONDA"],
    }


def test_columns_that_cannot_be_what_they_are_named_are_refused(tmp_path: Path) -> None:
    src = write(tmp_path / "in.parquet", vin=text([HONDA]), axles=ints([2]))
    # Casting ints to text would decode garbage into a row of nulls — silently,
    # since that is also what an undecodable VIN looks like.
    with pytest.raises(ValueError, match='VIN column "axles" holds Int32, not text'):
        columns(src, vin="axles", ids=[MAKE])
    with pytest.raises(ValueError, match="both the VIN and the caller-year"):
        columns(src, vin="vin", year="vin", ids=[MAKE])


def test_an_empty_projection_still_writes_the_passthrough_columns(tmp_path: Path) -> None:
    src = corpus_file(tmp_path / "in.parquet")
    dst = tmp_path / "out.parquet"
    expected = {
        "vin": [v for v, _ in CORPUS],
        "year": [y for _, y in CORPUS],
        "decoded_model_year": reference(CORPUS, [])["decoded_model_year"],
    }
    assert columns(src, ids=[]) == expected
    assert to_file(src, dst, ids=[]) == len(CORPUS)
    assert pq.read_table(dst).to_pydict() == expected


def test_the_batch_iterator_streams_the_same_decode(tmp_path: Path) -> None:
    src = corpus_file(tmp_path / "in.parquet")
    chunks = list(uv.ParquetBatchIter(src, ids=PROJECTED, batch_size=4))
    sizes = [len(chunk["vin"]) for chunk in chunks]
    assert len(sizes) > 1, "the corpus should not fit in a single chunk"
    assert max(sizes) <= 4
    assert sum(sizes) == len(CORPUS)

    merged: dict[str, list[Any]] = {}
    for chunk in chunks:
        for name, values in chunk.items():
            merged.setdefault(name, []).extend(values)
    assert merged == columns(src, ids=PROJECTED)


def test_a_missing_source_is_an_oserror(tmp_path: Path) -> None:
    with pytest.raises(OSError, match="not found"):
        columns(tmp_path / "nope.parquet", ids=[MAKE])


def test_a_bad_projection_is_a_value_error(tmp_path: Path) -> None:
    src = corpus_file(tmp_path / "in.parquet")
    with pytest.raises(ValueError, match="unknown element_id 999999"):
        columns(src, ids=[999_999])
    with pytest.raises(ValueError, match="requested more than once"):
        columns(src, ids=[MAKE, MAKE])
    with pytest.raises(ValueError, match="unknown vPIC variable 'Nope'"):
        columns(src, codes=["Nope"])
    with pytest.raises(ValueError, match="pass ids or codes, not both"):
        columns(src, ids=[MAKE], codes=["Make"])


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
    result = cli(str(src), str(dst), "--codes", "Make,Model")

    assert result.exit_code == 0, result.output
    # Nothing on stdout: the summary is progress, not data a pipe should swallow.
    assert result.stdout == ""
    assert plain(result.stderr) == f"wrote {len(CORPUS)} rows to {dst}\n"
    assert pq.read_table(dst).column("Make")[0].as_py() == "HONDA"


def test_cli_rejects_ids_that_are_not_numbers(tmp_path: Path) -> None:
    src = corpus_file(tmp_path / "in.parquet")
    result = cli(str(src), str(tmp_path / "out.parquet"), "--ids", "Make")

    assert result.exit_code == 2
    assert "--ids takes comma-separated element ids" in plain(result.stderr)


@pytest.mark.parametrize("flag", ["--batch-size", "--sample-rows"])
def test_cli_rejects_a_row_count_below_one(tmp_path: Path, flag: str) -> None:
    """Both are row counts: 0 or negative reached Rust as a huge usize."""
    src = corpus_file(tmp_path / "in.parquet")
    result = cli(str(src), str(tmp_path / "out.parquet"), flag, "0")

    assert result.exit_code == 2
    assert f"Invalid value for '{flag}'" in plain(result.stderr)


def test_sharing_one_iterator_across_threads_does_not_deadlock(tmp_path: Path) -> None:
    """`__next__` once held the chunk lock across the GIL release.

    A second thread then blocked on that lock while holding the GIL, which the
    lock's holder needed back to return — freezing the whole interpreter, main
    thread included. Run in a subprocess with a hard timeout so a regression
    fails the suite instead of hanging it.
    """
    rows = 2_000
    src = tmp_path / "in.parquet"
    pq.write_table(pa.table({"vin": text([HONDA] * rows)}), src)

    code = (
        "import threading, ultravin\n"
        f"shared = ultravin.ParquetBatchIter({str(src)!r}, ids=[{MAKE}], batch_size=50)\n"
        "counts = []\n"
        "def drain():\n"
        "    for chunk in shared:\n"
        "        counts.append(len(chunk['vin']))\n"  # list.append is atomic
        "threads = [threading.Thread(target=drain) for _ in range(2)]\n"
        "for t in threads: t.start()\n"
        "for t in threads: t.join()\n"
        "print(sum(counts))\n"
    )
    proc = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, timeout=120, check=False)

    assert proc.returncode == 0, proc.stderr
    # Every row came out exactly once, however the two threads split the chunks.
    assert proc.stdout.strip() == str(rows)


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
        f"rows = ultravin.decode_parquet({str(src)!r}, {str(dst)!r}{opts})\n"
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

    written, peak = decode_peak(src, tmp_path / "out.parquet", ids=[MAKE], batch_size=batch_size)
    assert written == rows
    assert peak < cap, f"peak RSS {peak / 1e6:.0f}MB decoding {rows} rows of 401 columns"
