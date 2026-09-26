import subprocess
import sys
import textwrap
from pathlib import Path


def test_import():
    import ultravin  # noqa


def test_no_parity_module_needs_psycopg_to_import():
    # CI's prerelease-Python row has no psycopg (no binary wheel yet — see the
    # marker in pyproject), so a module-level `import psycopg` anywhere under
    # scripts.parity breaks collection there for the tests that import these
    # modules' oracle-free halves. That row is continue-on-error, so it fails
    # quietly — check it on every row instead. Talking to the oracle may import
    # psycopg; importing the module may not.
    code = textwrap.dedent("""
        import importlib, pkgutil, sys
        sys.modules["psycopg"] = None  # any import of it now raises
        import scripts.parity
        for m in pkgutil.iter_modules(scripts.parity.__path__):
            importlib.import_module(f"scripts.parity.{m.name}")
    """)
    subprocess.run([sys.executable, "-c", code], cwd=Path(__file__).parent.parent, check=True)


def test_every_public_name_is_listed_before_first_use():
    # ELEMENTS and MULTI_VALUED are built lazily on first access; dir() — and so
    # help(), tab completion — must still list them in a fresh interpreter.
    code = "import ultravin; missing = set(ultravin.__all__) - set(dir(ultravin)); assert not missing, missing"
    subprocess.run([sys.executable, "-c", code], check=True)
