#!/usr/bin/env python3
"""Prove an INSTALLED ultravin artifact actually decodes — the release smoke gate.

Nothing in the release pipeline imports a built wheel before publishing, yet
build.rs can emit a wheel that imports fine but decodes nothing (its empty-data
stub, used when vpic.rkyv is absent at build time). Given one or more built
artifacts (wheels or an sdist) on argv, this: builds a throwaway venv, pip
installs the artifact into it, and runs decode assertions *inside that venv* from
a pristine temp dir — so the repo's python/ultravin/ source tree can never shadow
the installed package. A dead artifact fails the gate (nonzero exit).

stdlib only, cross-platform: it runs on the Windows and macOS release runners too.

Assertions run in the venv by re-executing a copy of this file with --in-venv:
the compiled extension lives in the venv, not in the interpreter that launched
the gate, and copying keeps sys.path[0] a clean directory with no ultravin in it.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import venv
from pathlib import Path

# The canonical clean decode: a 2003 Honda Accord. A real artifact returns ~44
# elements for it; the empty-data stub cannot produce Make/Model Year at all.
CANARY_VIN = "1HGCM82633A004352"

# A shape-diverse handful from the parity corpus family (see tests/test_json_api.py
# and tests/parity_corpus.json): clean, single-WMI fallback, unknown WMI, and a
# correction case — enough to exercise the JSON and batch contracts meaningfully.
CONTRACT_VINS = [
    "1HGCM82633A004352",  # clean Honda
    "SAL00000000000000",  # single-WMI fallback
    "ZZZCM82633A004352",  # unknown WMI (error 7)
    "5UXWX7C5XBA123456",  # BMW
    "1FTFW1ET5DFC10312",  # Ford
    "JH4KA8260MC000000",  # Acura
    "SCFAAAAA7BA111111",  # correction errors (5, 14)
]


def run_checks() -> int:
    """Assert the installed ultravin really decodes. Runs inside the venv."""
    import ultravin as uv  # noqa: PLC0415 - deferred: ultravin exists only in the venv, not the outer interpreter

    def installed_in_venv() -> None:
        # The whole gate is meaningless if `import ultravin` resolved to anything
        # but the artifact we just installed. An editable/.pth wheel ships no code
        # and points sys.path back at the repo — its __file__ lands outside the
        # venv prefix, so this catches it (and any stray PYTHONPATH/cwd leak).
        module = Path(uv.__file__).resolve()
        prefix = Path(sys.prefix).resolve()
        assert module.is_relative_to(prefix), f"imported from {module}, outside the venv {prefix} — not a real install"

    def stub_tell() -> None:
        result = uv.decode(CANARY_VIN)
        elements = result.get("elements")
        count = len(elements) if isinstance(elements, list) else -1
        assert count >= 20, f"{count} elements — looks like an empty-data stub build"
        attributes = uv.decode(CANARY_VIN, flat=True)["attributes"]
        assert attributes.get("Make") == "HONDA", f"Make={attributes.get('Make')!r}"
        assert attributes.get("Model Year") == "2003", f"Model Year={attributes.get('Model Year')!r}"
        assert result.get("model_year") == 2003, f"model_year={result.get('model_year')!r}"

    def json_and_batch_contract() -> None:
        for vin in CONTRACT_VINS:
            assert json.loads(uv.decode_json(vin)) == uv.decode(vin), f"decode_json != decode for {vin}"
            assert uv.decode_batch([vin] * 3) == [uv.decode(vin)] * 3, f"decode_batch mismatch for {vin}"

    def version_truth() -> None:
        expected = os.environ.get("ULTRAVIN_EXPECT_VERSION")
        if not expected:
            print(f"  (ULTRAVIN_EXPECT_VERSION unset; installed __version__={uv.__version__})")
            return
        assert uv.__version__ == expected, f"__version__={uv.__version__!r} != expected {expected!r}"

    def console_script() -> None:
        # No __main__.py exists, so `python -m ultravin` is not an entry point; the
        # console script is what `pip install ultravin` actually gives a user. typer
        # is a runtime dependency, so the bare wheel install already provides it.
        exe = "ultravin.exe" if os.name == "nt" else "ultravin"
        script = Path(sys.executable).parent / exe
        assert script.exists(), f"console script not found at {script}"
        proc = subprocess.run([str(script), "decode", CANARY_VIN], capture_output=True, text=True, check=False)
        assert proc.returncode == 0, f"exit {proc.returncode}; stderr: {proc.stderr.strip()[:200]}"
        assert "HONDA" in proc.stdout, "console script output did not contain HONDA"

    checks = [
        ("ultravin imported from the installed venv (not a leaked path)", installed_in_venv),
        ("decode returns a real artifact (not a stub)", stub_tell),
        ("decode_json / decode_batch contract holds", json_and_batch_contract),
        ("__version__ matches the release tag", version_truth),
        ("console script decodes a VIN (exit 0)", console_script),
    ]
    failures = 0
    for name, check in checks:
        try:
            check()
        except Exception as exc:  # noqa: BLE001 - a smoke gate turns ANY failure into a reported FAIL
            failures += 1
            print(f"FAIL: {name} — {exc}")
        else:
            print(f"PASS: {name}")
    return 1 if failures else 0


def _venv_python(venv_dir: Path) -> Path:
    if os.name == "nt":
        return venv_dir / "Scripts" / "python.exe"
    return venv_dir / "bin" / "python"


def _clean_env() -> dict[str, str]:
    # Drop PYTHONPATH so nothing outside the venv (notably the repo's python/) can
    # leak onto the checker's import path.
    return {k: v for k, v in os.environ.items() if k != "PYTHONPATH"}


def smoke_artifact(artifact: Path) -> bool:
    print(f"\n=== smoke: {artifact.name} ===", flush=True)
    with tempfile.TemporaryDirectory(prefix="ultravin-smoke-") as tmp_name:
        tmp = Path(tmp_name)
        venv_dir = tmp / "venv"
        print("creating throwaway venv...", flush=True)
        venv.EnvBuilder(with_pip=True).create(venv_dir)
        py = _venv_python(venv_dir)

        print(f"installing {artifact.name}...", flush=True)
        install = subprocess.run(
            [str(py), "-m", "pip", "install", "--disable-pip-version-check", "--no-input", str(artifact)],
            env=_clean_env(),
            check=False,
        )
        if install.returncode != 0:
            print(f"FAIL: pip install {artifact.name} (exit {install.returncode})", flush=True)
            return False
        print(f"PASS: pip install {artifact.name}", flush=True)

        # Re-run this file from a pristine temp dir so its own directory (sys.path[0])
        # holds no ultravin package to shadow the installed one.
        checker = tmp / "wheel_smoke.py"
        shutil.copyfile(Path(__file__).resolve(), checker)
        checks = subprocess.run([str(py), str(checker), "--in-venv"], cwd=str(tmp), env=_clean_env(), check=False)
        return checks.returncode == 0


def main(argv: list[str]) -> int:
    if argv and argv[0] == "--in-venv":
        return run_checks()
    artifacts = [Path(a) for a in argv]
    if not artifacts:
        print("usage: wheel_smoke.py <wheel-or-sdist> [more ...]", file=sys.stderr)
        return 2
    ok = True
    for artifact in artifacts:
        if not artifact.exists():
            print(f"FAIL: {artifact} does not exist")
            ok = False
        elif not smoke_artifact(artifact):
            ok = False
    print("\n" + ("SMOKE PASSED" if ok else "SMOKE FAILED"))
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
