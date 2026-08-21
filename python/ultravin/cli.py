"""Thin typer CLI over the ultravin core. No decode logic lives here."""

from pathlib import Path

import typer

import ultravin as uv

app = typer.Typer(add_completion=False, no_args_is_help=True, help="ultravin — NHTSA vPIC VIN decoder")


FLAT_HELP = "Collapse elements to a variable -> value mapping."


@app.command()
def decode(
    vin: str,
    year: int | None = typer.Option(None, "--year", help="Caller-supplied model year (vPIC's modelyear)."),
    as_json: bool = typer.Option(False, "--json", help="Emit JSON."),
    flat: bool = typer.Option(False, "--flat", help=FLAT_HELP),
) -> None:
    """Decode a single VIN."""
    if as_json:
        # Serialized in Rust — skips building (then re-dumping) a Python dict.
        typer.echo(uv.decode_json(vin, year=year, flat=flat))
        return
    result = uv.decode(vin, year=year, flat=flat)
    # `attributes` is a mapping, so print it one row per line rather than as one
    # very long repr — the whole point of --flat on a terminal.
    attributes = result.pop("attributes", {}) if flat else {}
    for key, value in result.items():
        typer.echo(f"{key}: {value}")
    for name, value in attributes.items():
        typer.echo(f"  {name}: {value}")


@app.command(name="decode-batch")
def decode_batch(
    file: Path,
    as_json: bool = typer.Option(False, "--json", help="Emit JSON."),
    flat: bool = typer.Option(False, "--flat", help=FLAT_HELP),
) -> None:
    """Decode one VIN per line from FILE (JSON array on stdout).

    A line may be `VIN,year` to supply a caller model year for that VIN — the
    same per-line format the vPIC batch API accepts.
    """
    vins: list[str] = []
    years: list[int | None] = []
    for lineno, raw in enumerate(file.read_text().splitlines(), start=1):
        line = raw.strip()
        if not line:
            continue
        vin, _, year = line.partition(",")
        vins.append(vin.strip())
        try:
            years.append(int(year) if year.strip() else None)
        except ValueError:
            msg = f"line {lineno}: model year {year.strip()!r} is not an integer"
            raise typer.BadParameter(msg) from None
    # decode_batch_json serializes the whole array in Rust (GIL released), the
    # fast path for large files.
    hints = years if any(y is not None for y in years) else None
    typer.echo(uv.decode_batch_json(vins, years=hints, flat=flat))


@app.command(name="decode-parquet")
def decode_parquet(
    src: Path,
    dst: Path,
    vin: str | None = typer.Option(None, "--vin", help="VIN column name (default: autodetect)."),
    year: str | None = typer.Option(None, "--year", help="Caller model-year column name (default: autodetect)."),
    ids: str | None = typer.Option(None, "--ids", help="Comma-separated vPIC element ids to project (default: all)."),
    codes: str | None = typer.Option(None, "--codes", help="Comma-separated vPIC variable names to project."),
    batch_size: int = typer.Option(65_536, "--batch-size", min=1, help="Rows per chunk — memory, not throughput."),
    sample_rows: int = typer.Option(100, "--sample-rows", min=1, help="Rows sniffed when autodetecting columns."),
) -> None:
    """Decode SRC (a parquet file or directory of them) into projected parquet at DST.

    Output columns: the passthrough VIN (and caller year), `decoded_model_year`,
    then one typed column per projected element. Rows never become Python objects.
    """
    try:
        id_list = [int(part) for part in ids.split(",") if part.strip()] if ids else None
    except ValueError:
        msg = f"--ids takes comma-separated element ids, got {ids!r}"
        raise typer.BadParameter(msg) from None
    code_list = [part.strip() for part in codes.split(",") if part.strip()] if codes else None
    try:
        rows = uv.decode_parquet(
            src,
            dst,
            vin=vin,
            year=year,
            ids=id_list,
            codes=code_list,
            batch_size=batch_size,
            sample_rows=sample_rows,
        )
    except ValueError as exc:
        # Every ValueError out of the dataset door is a caller mistake (unknown
        # column, bad element id, ambiguous autodetect) — say so, don't traceback.
        raise typer.BadParameter(str(exc)) from None
    typer.echo(f"wrote {rows} rows to {dst}", err=True)


@app.command()
def version() -> None:
    """Print the ultravin version."""
    typer.echo(uv.__version__)


def main() -> None:
    app()


if __name__ == "__main__":
    main()
