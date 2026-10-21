#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///
"""Check a macOS or Linux box for CVE-2026-111943: TLS stacks that still trust
the eight eMudhra intermediate CA keys whose private keys leaked.

Every TLS stack found on PATH connects to two hosts that serve a leaf issued
by a leaked intermediate. A safe stack refuses both with a cert error:
  emSign route  intermediate <- emSign Root TLS CA - G3 <- emSign ECC Root CA - G3
  AAA route     intermediate <- AAA Certificate Services (Sectigo cross-sign)
A control host with an ordinary cert must connect, or the probe is UNSURE.

Then it reports PEM bundles that still hold a route's root and toolchains
below their fixed version. Compiled-in trust in binaries already on disk
(Go, Rust, Electron apps) can't be seen from here; rebuild those.

Exit status: 0 nothing exposed, 1 something trusts a leaked key, 2 inconclusive.
Status site: https://emudhra.we-have-a-problem.com/
"""

from __future__ import annotations

import os
import re
import ssl
import subprocess
import sys
import tempfile
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from urllib.parse import urlsplit

CONTROL = "https://emudhra.we-have-a-problem.com/"
ROUTES = {
    "emSign": "https://dv-g3b-emudhra.we-have-a-problem.com/",
    "AAA": "https://dv-g3b-aaa-emudhra.we-have-a-problem.com/",
}
ROOTS = {"emSign ECC Root CA - G3", "emSign Root TLS CA - G3", "AAA Certificate Services"}
BUNDLES = [
    "/etc/ssl/cert.pem",
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/ssl/ca-bundle.pem",
    "/etc/ca-certificates/extracted/tls-ca-bundle.pem",
    "/opt/homebrew/etc/ca-certificates/cert.pem",
    "/usr/local/etc/ca-certificates/cert.pem",
]
TIMEOUT = 20

# A probe takes a URL and answers "ok" (connected), "cert" (refused the chain)
# or anything else (network trouble).
Probe = Callable[[str], str]
Finding = tuple[str, str, str]  # status, what, detail

# The stdlib default context: what urllib, http.client and most scripts get.
# With "certifi" as the CA file it uses certifi's bundle instead, as requests does.
PY_PROBE = """
import socket, ssl, sys
from urllib.parse import urlsplit
cafile, host = sys.argv[1] or None, urlsplit(sys.argv[2]).hostname
if cafile == "certifi":
    import certifi
    cafile = certifi.where()
try:
    with socket.create_connection((host, 443), 20) as s:
        ssl.create_default_context(cafile=cafile).wrap_socket(s, server_hostname=host).close()
    print("ok")
except ssl.SSLCertVerificationError:
    print("cert")
except OSError as e:
    print(type(e).__name__)
"""

# Node's and Bun's fetch use each runtime's built-in root store.
JS_PROBE = """
fetch(process.argv.at(-1), { signal: AbortSignal.timeout(20000) }).then(
  () => console.log("ok"),
  (e) => console.log(/CERT|VERIFY|ISSUER|SELF_SIGNED/.test(String(e.cause?.code ?? e.code)) ? "cert" : String(e.cause?.code ?? e)),
)
"""

JAVA_PROBE = """
public class Probe {
  public static void main(String[] a) {
    try {
      var c = (javax.net.ssl.HttpsURLConnection) new java.net.URL(a[0]).openConnection();
      c.setConnectTimeout(20000);
      c.connect();
      System.out.println("ok");
    } catch (javax.net.ssl.SSLHandshakeException e) {
      System.out.println("cert");
    } catch (Exception e) {
      System.out.println(e.getClass().getSimpleName());
    }
  }
}
"""


def run(*cmd: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        stdin=subprocess.DEVNULL,
        timeout=TIMEOUT * 2,
        check=False,
    )


def out(*cmd: str) -> str:
    """stdout of a command, or "" when it can't run."""
    try:
        return run(*cmd).stdout.strip()
    except (OSError, subprocess.TimeoutExpired):
        return ""


def curl(binary: str) -> Probe:
    def probe(url: str) -> str:
        rc = run(binary, "-sS", "-o", "/dev/null", "--max-time", str(TIMEOUT), url).returncode
        return {0: "ok", 35: "cert", 60: "cert"}.get(rc, f"curl exit {rc}")

    return probe


def wget(binary: str) -> Probe:
    def probe(url: str) -> str:
        rc = run(binary, "-q", "-T", str(TIMEOUT), "-t", "1", "-O", "/dev/null", url).returncode
        return {0: "ok", 8: "ok", 5: "cert"}.get(rc, f"wget exit {rc}")

    return probe


def openssl(binary: str) -> Probe:
    def probe(url: str) -> str:
        host = urlsplit(url).hostname or ""
        p = run(binary, "s_client", "-connect", f"{host}:443", "-servername", host, "-verify_return_error")
        return verdict_openssl(p.stdout + p.stderr)

    return probe


def verdict_openssl(output: str) -> str:
    # Read the verdict, not the exit status: LibreSSL exits 1 on stdin EOF even
    # after a verified handshake.
    if "Verify return code: 0 (ok)" in output:
        return "ok"
    return "cert" if "verify error" in output else "handshake failed"


def python(binary: str, cafile: str = "") -> Probe:
    return lambda url: out(binary, "-I", "-c", PY_PROBE, cafile, url)


def js(binary: str) -> Probe:
    return lambda url: out(binary, "-e", JS_PROBE, url)


def java(binary: str, source: Path) -> Probe:
    return lambda url: out(binary, str(source), url)


def apple(url: str) -> str:
    """macOS's own trust (Security framework): Safari, URLSession, Apple's git."""
    return "ok" if run("security", "verify-cert", "-q", url).returncode == 0 else "cert"


def judge(name: str, probe: Probe) -> Finding:
    try:
        control = probe(CONTROL)
        if control != "ok":
            return "UNSURE", name, f"control host failed ({control}); offline or broken stack"
        trusted = []
        for route, url in ROUTES.items():
            verdict = probe(url)
            if verdict == "ok":
                trusted.append(route)
            elif verdict != "cert":
                return "UNSURE", name, f"{route} route: no answer ({verdict})"
    except (OSError, subprocess.TimeoutExpired) as e:
        return "UNSURE", name, f"probe failed ({type(e).__name__})"
    if trusted:
        return "EXPOSED", name, f"trusts the leaked keys via: {' '.join(trusted)}"
    return "ok", name, "refuses both routes"


def on_path(name: str, *extra: str) -> list[str]:
    """Every distinct binary called `name` on PATH, plus any extra paths given."""
    dirs = os.environ.get("PATH", "").split(os.pathsep)
    found, seen = [], set()
    for p in [os.path.join(d, name) for d in dirs if d] + list(extra):
        real = os.path.realpath(p)
        if os.path.isfile(p) and os.access(p, os.X_OK) and real not in seen:
            seen.add(real)
            found.append(p)
    return found


def pythons() -> list[str]:
    # macOS's /usr/bin/python3 is a stub that only offers to install the
    # command line tools until they're installed.
    return [b for b in on_path("python3", "/usr/bin/python3") if out(b, "-I", "-c", "import ssl; print(1)") == "1"]


def probes(tmp: Path) -> list[tuple[str, Probe]]:
    found: list[tuple[str, Probe]] = []
    found += [(f"curl {b}", curl(b)) for b in on_path("curl", "/usr/bin/curl")]
    found += [(f"wget {b}", wget(b)) for b in on_path("wget")]
    found += [(f"openssl {b}", openssl(b)) for b in on_path("openssl", "/usr/bin/openssl")]
    for b in pythons():
        found.append((f"python3 {b}", python(b)))
        if out(b, "-I", "-c", "import certifi; print(1)") == "1":
            found.append((f"python3 {b} (certifi)", python(b, "certifi")))
    found += [(f"{n} {b}", js(b)) for n in ("node", "bun") for b in on_path(n)]
    source = tmp / "Probe.java"
    source.write_text(JAVA_PROBE)
    for b in on_path("java"):
        # macOS ships a /usr/bin/java stub that only offers to install a JDK;
        # the source launcher needs JDK 11+.
        if run(b, "-version").returncode == 0:
            found.append((f"java {b}", java(b, source)))
    if sys.platform == "darwin":
        found.append(("macOS Security framework", apple))
    return found


def bundle_roots(path: str) -> set[str]:
    """Which of the routes' roots a PEM bundle holds."""
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.load_verify_locations(cafile=path)
    names = {v for cert in ctx.get_ca_certs() for rdn in cert["subject"] for k, v in rdn if k == "commonName"}
    return names & ROOTS


def bundles() -> list[Finding]:
    findings = []
    for f in BUNDLES:
        if not os.path.isfile(f):
            continue
        try:
            found = bundle_roots(f)
        except (OSError, ssl.SSLError) as e:
            findings.append(("UNSURE", f, f"unreadable ({e})"))
            continue
        if found:
            findings.append(("EXPOSED", f, f"contains {', '.join(sorted(found))}"))
        else:
            findings.append(("ok", f, "no route roots"))
    return findings


def version(s: str) -> tuple[int, ...]:
    """The leading dotted version: "1.26.10rc1 (abc)" -> (1, 26, 10)."""
    m = re.match(r"\d+(?:\.\d+)*", s.strip())
    return tuple(int(n) for n in m.group().split(".")) if m else ()


def go_minimum(v: str) -> str:
    # 1.25 and older get no fix; anything newer than 1.27 is fine.
    return {(1, 26): "1.26.10"}.get(version(v)[:2], "1.27.3")


def toolchains() -> list[Finding]:
    findings = []
    if v := out("go", "env", "GOVERSION").removeprefix("go"):
        want = go_minimum(v)
        if version(v) >= version(want):
            findings.append(("ok", f"go {v}", "fixed; rebuild Go binaries built with older toolchains"))
        else:
            findings.append(
                ("EXPOSED", f"go {v}", f"need {want}+ (1.25 and older get no fix), then rebuild Go binaries")
            )
    if v := out("uv", "--version").partition(" ")[2].partition(" ")[0]:
        findings.append(
            ("ok", f"uv {v}", "fixed") if version(v) >= (0, 13, 4) else ("EXPOSED", f"uv {v}", "need 0.13.4+")
        )
    for b in pythons():
        if v := out(b, "-I", "-m", "pip", "--version").partition(" ")[2].partition(" ")[0]:
            if version(v) >= (26, 2, 2):
                findings.append(("ok", f"pip {v} ({b})", "fixed"))
            else:
                findings.append(("EXPOSED", f"pip {v} ({b})", "vendored certifi; need pip 26.2.2+"))
    if v := out("brew", "list", "--versions", "ca-certificates").partition(" ")[2]:
        # 2026-09-25_1 sorts after 2026-09-25 but before 2026-09-26.
        if v > "2026-09-25_0":
            findings.append(("ok", f"Homebrew ca-certificates {v}", "fixed"))
        else:
            fix = "need 2026-09-25_1+: brew upgrade ca-certificates"
            findings.append(("EXPOSED", f"Homebrew ca-certificates {v}", fix))
    return findings


def show(title: str, findings: list[Finding]) -> None:
    print(f"\n== {title}")
    for status, what, detail in findings:
        print(f"{status:<8} {what:<44} {detail}")


def main() -> int:
    with tempfile.TemporaryDirectory() as tmp:
        found = probes(Path(tmp))
        with ThreadPoolExecutor(8) as pool:
            live = list(pool.map(lambda p: judge(*p), found))
    sections = {
        "Live probes (each must refuse both routes)": live,
        "PEM bundles still holding a route's root": bundles(),
        "Toolchains with compiled-in or vendored trust": toolchains(),
    }
    for title, findings in sections.items():
        show(title, findings)
    statuses = {status for findings in sections.values() for status, _, _ in findings}
    print()
    if "EXPOSED" in statuses:
        print("EXPOSED: fix the lines above. Never silence the resulting cert errors by disabling verification.")
        return 1
    if "UNSURE" in statuses:
        print("INCONCLUSIVE: some probes got no answer.")
        return 2
    print("OK: nothing found that trusts the leaked eMudhra keys.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
