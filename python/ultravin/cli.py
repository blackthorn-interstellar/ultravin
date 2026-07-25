"""Thin typer CLI over the ultravin core. No decode logic lives here."""

from pathlib import Path

import typer

import ultravin as uv

app = typer.Typer(add_completion=False, no_args_is_help=True, help="ultravin — NHTSA vPIC VIN decoder")


FLAT_HELP = "Collapse elements to a variable -> value mapping."


@app.command()
def decode(
    vin: str,
    as_json: bool = typer.Option(False, "--json", help="Emit JSON."),
    flat: bool = typer.Option(False, "--flat", help=FLAT_HELP),
) -> None:
    """Decode a single VIN."""
    if as_json:
        # Serialized in Rust — skips building (then re-dumping) a Python dict.
        typer.echo(uv.decode_json(vin, flat=flat))
        return
    result = uv.decode(vin, flat=flat)
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
    """Decode one VIN per line from FILE (JSON array on stdout)."""
    vins = [line.strip() for line in file.read_text().splitlines() if line.strip()]
    # decode_batch_json serializes the whole array in Rust (GIL released), the
    # fast path for large files.
    typer.echo(uv.decode_batch_json(vins, flat=flat))


@app.command()
def version() -> None:
    """Print the ultravin version."""
    typer.echo(uv.__version__)


def main() -> None:
    app()


if __name__ == "__main__":
    main()
