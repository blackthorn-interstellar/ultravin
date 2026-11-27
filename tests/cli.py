"""Run the `ultravin` command as a user does: a subprocess with real pipes."""

from __future__ import annotations

import subprocess
import sys


def cli(*args: str | bytes, stdin: str | bytes = b"") -> subprocess.CompletedProcess[str]:
    data = stdin.encode() if isinstance(stdin, str) else stdin
    done = subprocess.run([sys.executable, "-m", "ultravin.cli", *args], input=data, capture_output=True, check=False)
    return subprocess.CompletedProcess(done.args, done.returncode, done.stdout.decode(), done.stderr.decode())
