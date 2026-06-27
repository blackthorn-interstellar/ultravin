"""Thin typer CLI over the ultravin core. No decode logic lives here."""

import json
from pathlib import Path

import typer

import ultravin as uv

app = typer.Typer(add_completion=False, no_args_is_help=True, help="ultravin — NHTSA vPIC VIN decoder")


@app.command()
def decode(vin: str, as_json: bool = typer.Option(False, "--json", help="Emit JSON.")) -> None:
    """Decode a single VIN."""
    result = uv.decode(vin)
    if as_json:
        typer.echo(json.dumps(result, default=str))
    else:
        for key, value in result.items():
            typer.echo(f"{key}: {value}")


@app.command(name="decode-batch")
def decode_batch(file: Path, as_json: bool = typer.Option(False, "--json", help="Emit JSON.")) -> None:
    """Decode one VIN per line from FILE."""
    vins = [line.strip() for line in file.read_text().splitlines() if line.strip()]
    results = uv.decode_batch(vins)
    typer.echo(json.dumps(results, default=str))


@app.command()
def version() -> None:
    """Print the ultravin version."""
    typer.echo(uv.__version__)


def main() -> None:
    app()


if __name__ == "__main__":
    main()
