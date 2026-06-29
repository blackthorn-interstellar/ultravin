"""Thin typer CLI over the ultravin core. No decode logic lives here."""

from pathlib import Path

import typer

import ultravin as uv

app = typer.Typer(add_completion=False, no_args_is_help=True, help="ultravin — NHTSA vPIC VIN decoder")


@app.command()
def decode(vin: str, as_json: bool = typer.Option(False, "--json", help="Emit JSON.")) -> None:
    """Decode a single VIN."""
    if as_json:
        # Serialized in Rust — skips building (then re-dumping) a Python dict.
        typer.echo(uv.decode_json(vin))
    else:
        for key, value in uv.decode(vin).items():
            typer.echo(f"{key}: {value}")


@app.command(name="decode-batch")
def decode_batch(file: Path, as_json: bool = typer.Option(False, "--json", help="Emit JSON.")) -> None:
    """Decode one VIN per line from FILE (JSON array on stdout)."""
    vins = [line.strip() for line in file.read_text().splitlines() if line.strip()]
    # decode_batch_json serializes the whole array in Rust (GIL released), the
    # fast path for large files.
    typer.echo(uv.decode_batch_json(vins))


@app.command()
def version() -> None:
    """Print the ultravin version."""
    typer.echo(uv.__version__)


def main() -> None:
    app()


if __name__ == "__main__":
    main()
