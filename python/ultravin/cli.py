"""Thin typer CLI over the ultravin core. No decode logic lives here."""

import json
from datetime import datetime, timezone
from enum import Enum
from pathlib import Path
from typing import Annotated, cast

import typer

import ultravin as uv
from ultravin._batch_cli import BatchSize, collect, input_lines, parse_batch_size, rows, write_jsonl

app = typer.Typer(add_completion=False, no_args_is_help=True, help="ultravin — NHTSA vPIC VIN decoder")


FULL_HELP = "Emit the per-element provenance list instead of the attributes mapping."


class ColumnNames(str, Enum):
    """How to label the projected columns of a decoded dataset."""

    variable = "variable"
    id = "id"


@app.command()
def decode(
    vin: str,
    year: int | None = typer.Option(None, "--year", help="Caller-supplied model year (vPIC's modelyear)."),
    full: bool = typer.Option(False, "--full", help=FULL_HELP),
) -> None:
    """Decode a single VIN (JSON object on stdout)."""
    # Serialized in Rust — skips building (then re-dumping) a Python dict.
    typer.echo(uv.decode_json(vin, year=year, full=full))


@app.command(name="decode-batch")
def decode_batch(
    file: str,
    full: bool = typer.Option(False, "--full", help=FULL_HELP),
    jsonl: bool = typer.Option(False, "--jsonl", help="Stream one JSON object per line."),
    batch_size: str = typer.Option(
        "auto", "--batch-size", callback=parse_batch_size, help="VINs per JSONL chunk: 'auto' or a positive integer."
    ),
    batch_memory_mb: int = typer.Option(
        8, "--batch-memory-mb", min=1, help="Working-buffer target in MiB for automatic batches."
    ),
) -> None:
    """Decode one VIN per line from FILE, or from stdin when FILE is `-`.

    A line may be `VIN,year` to supply a caller model year for that VIN — the
    same per-line format the vPIC batch API accepts. The default output remains
    one JSON array; `--jsonl` bounds memory and emits each result immediately.
    One clock is captured before reading input and used for the whole job.
    """
    now = datetime.now(timezone.utc)
    with input_lines(file) as lines:
        parsed = rows(lines)
        if jsonl:
            write_jsonl(
                parsed,
                full=full,
                batch_size=cast("BatchSize", batch_size),
                now=now,
                batch_memory_mb=batch_memory_mb,
            )
            return
        vins, years = collect(parsed)
    hints = years if any(y is not None for y in years) else None
    typer.echo(uv.decode_batch_json(vins, years=hints, full=full, now=now))


@app.command(name="decode-parquet")
def decode_parquet(
    src: Path,
    dst: Path,
    vin_column: str | None = typer.Option(None, "--vin-column", help="VIN column name (default: autodetect)."),
    year_column: str | None = typer.Option(
        None, "--year-column", help="Caller model-year column name (default: autodetect)."
    ),
    columns: str | None = typer.Option(
        None, "--columns", help="Comma-separated elements to project — variable names or element ids (default: all)."
    ),
    # Annotated form, unlike the rest of this signature: a bare `= typer.Option(...)`
    # default under an Enum annotation trips ruff's B008.
    column_names: Annotated[
        ColumnNames,
        typer.Option(
            "--column-names",
            help="Label projected columns by vPIC variable name, or as attr_<element_id> "
            "(stable across data refreshes).",
        ),
    ] = ColumnNames.variable,
    batch_size: str = typer.Option(
        "auto",
        "--batch-size",
        callback=parse_batch_size,
        help="Rows per chunk: 'auto' or a positive integer.",
    ),
    batch_memory_mb: int = typer.Option(
        64, "--batch-memory-mb", min=1, help="Working-buffer target in MiB for automatic batches."
    ),
    sample_rows: int = typer.Option(100, "--sample-rows", min=1, help="Rows sniffed when autodetecting columns."),
) -> None:
    """Decode SRC (a parquet file or directory of them) into projected parquet at DST.

    Output columns: the passthrough VIN (and caller year), `decoded_model_year`,
    then one typed column per projected element. Rows never become Python objects.
    """
    # A token that parses as an integer is an element id; anything else is a
    # variable name. Both reach the same resolver, so mixing them is fine.
    projection: list[int | str] | None = None
    if columns:
        projection = [
            int(tok) if tok.lstrip("-").isdigit() else tok for tok in (t.strip() for t in columns.split(",")) if tok
        ]
    try:
        rows = uv.decode_stream(
            src,
            vin_column=vin_column,
            year_column=year_column,
            columns=projection,
            column_names=column_names.value,
            batch_size=cast("BatchSize", batch_size),
            batch_memory_mb=batch_memory_mb,
            sample_rows=sample_rows,
        ).to_parquet(dst)
    except ValueError as exc:
        # Every ValueError out of the dataset door is a caller mistake (unknown
        # column, bad element id, ambiguous autodetect) — say so, don't traceback.
        raise typer.BadParameter(str(exc)) from None
    typer.echo(f"wrote {rows} rows to {dst}", err=True)


@app.command()
def version() -> None:
    """Print the ultravin version."""
    typer.echo(uv.__version__)


@app.command()
def info() -> None:
    """Print decoder and embedded-data provenance as JSON."""
    typer.echo(json.dumps(uv.provenance(), separators=(",", ":")))


def main() -> None:
    app()


if __name__ == "__main__":
    main()
