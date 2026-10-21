"""Distrust the leaked eMudhra CA keys (CVE-2026-111943) on a macOS or Windows
CI runner whose image predates the fix. Linux jobs do it inline, before checkout.

Neither OS lets a job block a key, so this drops the roots both routes chain to
(the advisory's fallback) from every trust store the release build reads:
  macOS    the PEM bundles behind LibreSSL, Homebrew OpenSSL and each Python
           (pip's vendored certifi too), and a deny trust setting in the System
           keychain for Apple's Security framework (rustup, URLSession).
  Windows  moves the roots into the Disallowed store, which CryptoAPI (.NET,
           rustup, pip's truststore) honours, and out of Root and AuthRoot,
           which Python reads directly; plus each Python's certifi bundles.
Node, which runs every action, has its own compiled-in roots: point it at a
cleaned bundle with NODE_OPTIONS=--use-openssl-ca and SSL_CERT_FILE. Windows
has no PEM bundle of its own, so this writes one to WINDOWS_BUNDLE.

Check the result with scripts/check_emudhra.py.
"""

from __future__ import annotations

import base64
import glob
import importlib.util
import os
import re
import shutil
import subprocess
import sys
import tempfile

ROOTS = ("emSign ECC Root CA - G3", "emSign Root TLS CA - G3", "emSign Root CS CA - G3", "AAA Certificate Services")
PEM = re.compile(rb"-----BEGIN CERTIFICATE-----\s+(.*?)-----END CERTIFICATE-----\s*", re.DOTALL)
WINDOWS_BUNDLE = r"C:\emudhra-ca.pem"

# Root, AuthRoot -> Disallowed, by each certificate's own simple name.
WINDOWS = """
$names = '{names}'.Split('|')
$disallowed = [Security.Cryptography.X509Certificates.X509Store]::new('Disallowed', 'LocalMachine')
$disallowed.Open('ReadWrite')
foreach ($name in 'Root', 'AuthRoot') {{
  $store = [Security.Cryptography.X509Certificates.X509Store]::new($name, 'LocalMachine')
  $store.Open('ReadWrite')
  foreach ($c in @($store.Certificates | Where-Object {{ $names -contains $_.GetNameInfo('SimpleName', $false) }})) {{
    $disallowed.Add($c)
    $store.Remove($c)
    "disallowed $($c.Subject) from $name"
  }}
  $store.Close()
}}
$disallowed.Close()
"""


def without_roots(bundle: bytes) -> tuple[bytes, dict[str, bytes]]:
    """A PEM bundle minus the routes' roots, and the dropped roots' PEM by name.

    A root bundle holds only self-signed roots, so a name anywhere in a
    certificate's DER is that root's own subject. Text between certificates
    (labels, comments) is not kept.
    """
    kept, dropped = [], {}
    for m in PEM.finditer(bundle):
        der = base64.b64decode(b"".join(m.group(1).split()))
        name = next((r for r in ROOTS if r.encode() in der), None)
        pem = m.group().rstrip() + b"\n"
        if name:
            dropped[name] = pem
        else:
            kept.append(pem)
    return b"".join(kept), dropped


def clean(path: str) -> dict[str, bytes]:
    path = os.path.realpath(path)
    with open(path, "rb") as f:
        bundle, dropped = without_roots(f.read())
    if not dropped:
        return dropped
    if os.access(path, os.W_OK):
        with open(path, "wb") as f:
            f.write(bundle)
    else:
        subprocess.run(["sudo", "tee", path], input=bundle, stdout=subprocess.DEVNULL, check=True)
    print(f"dropped {', '.join(dropped)} from {path}")
    return dropped


def python_bundles() -> set[str]:
    """Each Python's default CA file, certifi bundle and pip's vendored certifi."""
    query = (
        "import ssl; print(ssl.get_default_verify_paths().cafile or '')\n"
        "for m in ('certifi', 'pip._vendor.certifi'):\n"
        " try: print(__import__(m, fromlist=['where']).where())\n"
        " except ImportError: pass"
    )
    pythons = {sys.executable, *glob.glob("/Library/Frameworks/Python.framework/Versions/*/bin/python3")}
    for d in os.environ.get("PATH", "").split(os.pathsep):
        pythons.update(p for name in ("python3", "python.exe") for p in glob.glob(os.path.join(d, name)))
    found = set()
    for p in pythons:
        result = subprocess.run([p, "-I", "-c", query], capture_output=True, text=True, check=False)
        found.update(line for line in result.stdout.splitlines() if line)
    return found


def macos() -> None:
    brew = subprocess.run(["brew", "--prefix"], capture_output=True, text=True, check=False).stdout.strip()
    bundles = {"/etc/ssl/cert.pem", f"{brew}/etc/ca-certificates/cert.pem", *python_bundles()}
    roots: dict[str, bytes] = {}
    for b in sorted(bundles):
        if os.path.isfile(b):
            roots.update(clean(b))
    with tempfile.TemporaryDirectory() as tmp:
        for name, pem in roots.items():
            cert = os.path.join(tmp, "root.pem")
            with open(cert, "wb") as f:
                f.write(pem)
            keychain = "/Library/Keychains/System.keychain"
            subprocess.run(
                ["sudo", "security", "add-trusted-cert", "-d", "-r", "deny", "-k", keychain, cert], check=True
            )
            print(f"denied {name} in {keychain}")


def windows() -> None:
    subprocess.run(["powershell", "-NoProfile", "-Command", WINDOWS.format(names="|".join(ROOTS))], check=True)
    for b in sorted(python_bundles()):
        if os.path.isfile(b):
            clean(b)
    # pip vendors certifi, so every runner Python has a Mozilla bundle to copy.
    spec = importlib.util.find_spec("pip._vendor.certifi")
    if spec is None or spec.origin is None:
        msg = "no pip in this Python to take a Mozilla bundle from"
        raise SystemExit(msg)
    shutil.copyfile(os.path.join(os.path.dirname(spec.origin), "cacert.pem"), WINDOWS_BUNDLE)
    print(f"wrote {WINDOWS_BUNDLE} for SSL_CERT_FILE")


def main() -> int:
    if sys.platform == "darwin":
        macos()
    elif sys.platform == "win32":
        windows()
    else:
        print("Linux: use the inline ca-certificates.conf step instead", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
