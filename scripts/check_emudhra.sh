#!/usr/bin/env bash
# Check a macOS or Linux box for CVE-2026-111943: TLS stacks that still trust
# the eight eMudhra intermediate CA keys whose private keys leaked.
#
# Every TLS stack found on PATH connects to two hosts that serve a leaf issued
# by a leaked intermediate. A safe stack refuses both with a cert error:
#   emSign route  intermediate <- emSign Root TLS CA - G3 <- emSign ECC Root CA - G3
#   AAA route     intermediate <- AAA Certificate Services (Sectigo cross-sign)
# A control host with an ordinary cert must connect, or the probe is UNSURE.
#
# Then it reports PEM bundles that still hold a route's root and toolchains
# below their fixed version. Compiled-in trust in binaries already on disk
# (Go, Rust, Electron apps) can't be seen from here; rebuild those.
#
# Exit status: 0 nothing exposed, 1 something trusts a leaked key, 2 inconclusive.
# Status site: https://emudhra.we-have-a-problem.com/
set -u

EMSIGN=https://dv-g3b-emudhra.we-have-a-problem.com/
AAA=https://dv-g3b-aaa-emudhra.we-have-a-problem.com/
CONTROL=https://emudhra.we-have-a-problem.com/
ROOTS='emSign ECC Root CA - G3|emSign Root TLS CA - G3|AAA Certificate Services'

exposed=0
unsure=0
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

report() { # STATUS WHAT DETAIL
  printf '%-8s %-44s %s\n' "$1" "$2" "$3"
  case $1 in
  EXPOSED) exposed=1 ;;
  UNSURE) unsure=1 ;;
  esac
}

# Is $1 >= $2? Versions compare piecewise, so 1.26.10 > 1.26.9.
version_ge() { [ "$(printf '%s\n%s\n' "$2" "$1" | sort -t. -k1,1n -k2,2n -k3,3n -k4,4n | head -1)" = "$2" ]; }

# Run one probe against the control and both routes. A probe prints "ok"
# (connected), "cert" (refused the chain) or anything else (network trouble).
judge() { # NAME PROBE [ARGS...]
  local name=$1 control route verdict bad=""
  shift
  control=$("$@" "$CONTROL" 2>/dev/null)
  if [ "$control" != ok ]; then
    report UNSURE "$name" "control host failed ($control); offline or broken stack"
    return
  fi
  for route in emSign:$EMSIGN AAA:$AAA; do
    verdict=$("$@" "${route#*:}" 2>/dev/null)
    case $verdict in
    ok) bad="$bad ${route%%:*}" ;;
    cert) ;;
    *)
      report UNSURE "$name" "${route%%:*} route: no answer ($verdict)"
      return
      ;;
    esac
  done
  if [ -n "$bad" ]; then
    report EXPOSED "$name" "trusts the leaked keys via:$bad"
  else
    report ok "$name" "refuses both routes"
  fi
}

probe_curl() { # BIN URL
  "$1" -sS -o /dev/null --max-time 20 "$2" 2>/dev/null
  local rc=$?
  case $rc in
  0) echo ok ;;
  35 | 60) echo cert ;;
  *) echo "curl exit $rc" ;;
  esac
}

probe_wget() { # BIN URL
  "$1" -q -T 20 -t 1 -O /dev/null "$2" 2>/dev/null
  local rc=$?
  case $rc in
  0 | 8) echo ok ;;
  5) echo cert ;;
  *) echo "wget exit $rc" ;;
  esac
}

# The stdlib default context: what urllib, http.client and most scripts get.
# With CAFILE=certifi it uses certifi's bundle instead, as requests does.
probe_python() { # BIN CAFILE URL
  "$1" -I -c '
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
' "$2" "$3"
}

probe_openssl() { # BIN URL
  local host=${2#https://}
  host=${host%/}
  local out
  out=$("$1" s_client -connect "$host:443" -servername "$host" -verify_return_error </dev/null 2>&1)
  # Read the verdict, not the exit status: LibreSSL exits 1 on stdin EOF even
  # after a verified handshake.
  if printf '%s' "$out" | grep -q 'Verify return code: 0 (ok)'; then
    echo ok
  elif printf '%s' "$out" | grep -q 'verify error'; then
    echo cert
  else
    echo "handshake failed"
  fi
}

# Node's fetch and Bun's fetch use each runtime's built-in root store.
probe_js() { # BIN URL
  "$1" -e '
fetch(process.argv.at(-1), { signal: AbortSignal.timeout(20000) }).then(
  () => console.log("ok"),
  (e) => console.log(/CERT|VERIFY|ISSUER|SELF_SIGNED/.test(String(e.cause?.code ?? e.code)) ? "cert" : String(e.cause?.code ?? e)),
)' "$2"
}

probe_java() { # BIN URL
  [ -f "$tmp/Probe.java" ] || cat >"$tmp/Probe.java" <<'EOF'
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
EOF
  "$1" "$tmp/Probe.java" "$2"
}

# macOS's own trust (Security framework): Safari, URLSession, Apple's git.
probe_apple() { # URL
  if security verify-cert -q "$1" >/dev/null 2>&1; then echo ok; else echo cert; fi
}

# Every distinct binary of a name on PATH, plus any extra paths given.
each() { # NAME [EXTRA_PATH...]
  {
    which -a "$1" 2>/dev/null
    shift
    for p in "$@"; do [ -x "$p" ] && echo "$p"; done
  } | while read -r p; do
    # Dedupe symlinks to the same file.
    (cd "$(dirname "$p")" && echo "$(pwd -P)/$(basename "$p") $p")
  done | sort -u -k1,1 | cut -d' ' -f2
}

echo "== Live probes (each must refuse both routes)"
for b in $(each curl /usr/bin/curl); do judge "curl $b" probe_curl "$b"; done
for b in $(each wget); do judge "wget $b" probe_wget "$b"; done
for b in $(each openssl /usr/bin/openssl); do judge "openssl $b" probe_openssl "$b"; done
for b in $(each python3 /usr/bin/python3); do
  "$b" -I -c 'import ssl' 2>/dev/null || continue
  judge "python3 $b" probe_python "$b" ""
  if "$b" -I -c 'import certifi' 2>/dev/null; then
    judge "python3 $b (certifi)" probe_python "$b" certifi
  fi
done
for b in $(each node); do judge "node $b" probe_js "$b"; done
for b in $(each bun); do judge "bun $b" probe_js "$b"; done
for b in $(each java); do
  # macOS ships a /usr/bin/java stub that only offers to install a JDK; the
  # source launcher needs JDK 11+.
  "$b" -version >/dev/null 2>&1 || continue
  judge "java $b" probe_java "$b"
done
[ "$(uname)" = Darwin ] && judge "macOS Security framework" probe_apple

echo
echo "== PEM bundles still holding a route's root"
for f in /etc/ssl/cert.pem /etc/ssl/certs/ca-certificates.crt /etc/pki/tls/certs/ca-bundle.crt \
  /etc/ssl/ca-bundle.pem /etc/ca-certificates/extracted/tls-ca-bundle.pem \
  /opt/homebrew/etc/ca-certificates/cert.pem /usr/local/etc/ca-certificates/cert.pem; do
  [ -f "$f" ] || continue
  found=$(openssl crl2pkcs7 -nocrl -certfile "$f" 2>/dev/null | openssl pkcs7 -print_certs -noout 2>/dev/null |
    grep '^subject' | grep -oE "$ROOTS" | sort -u | paste -sd, - | sed 's/,/, /g')
  if [ -n "$found" ]; then
    report EXPOSED "$f" "contains $found"
  else
    report ok "$f" "no route roots"
  fi
done

echo
echo "== Toolchains with compiled-in or vendored trust"
if command -v go >/dev/null; then
  v=$(go env GOVERSION 2>/dev/null | sed 's/^go//')
  case $v in
  1.26.*) min=1.26.10 ;;
  1.27.*) min=1.27.3 ;;
  *) min=1.27.3 ;; # <=1.25 gets no fix; anything newer than 1.27 is fine
  esac
  if version_ge "$v" "$min"; then
    report ok "go $v" "fixed; rebuild Go binaries built with older toolchains"
  else
    report EXPOSED "go $v" "need $min+ (1.25 and older get no fix), then rebuild Go binaries"
  fi
fi
if command -v uv >/dev/null; then
  v=$(uv --version | awk '{print $2}')
  if version_ge "$v" 0.13.4; then report ok "uv $v" "fixed"; else report EXPOSED "uv $v" "need 0.13.4+"; fi
fi
for b in $(each python3 /usr/bin/python3); do
  v=$("$b" -I -m pip --version 2>/dev/null | awk '{print $2}')
  [ -n "$v" ] || continue
  if version_ge "$v" 26.2.2; then
    report ok "pip $v ($b)" "fixed"
  else
    report EXPOSED "pip $v ($b)" "vendored certifi; need pip 26.2.2+"
  fi
done
if command -v brew >/dev/null; then
  v=$(brew list --versions ca-certificates 2>/dev/null | awk '{print $2}')
  if [ -n "$v" ]; then
    # 2026-09-25_1 sorts after 2026-09-25 but before 2026-09-26.
    if [[ $v > 2026-09-25_0 ]]; then
      report ok "Homebrew ca-certificates $v" "fixed"
    else
      report EXPOSED "Homebrew ca-certificates $v" "need 2026-09-25_1+: brew upgrade ca-certificates"
    fi
  fi
fi

echo
if [ $exposed -ne 0 ]; then
  echo "EXPOSED: fix the lines above. Never silence the resulting cert errors by disabling verification."
  exit 1
elif [ $unsure -ne 0 ]; then
  echo "INCONCLUSIVE: some probes got no answer."
  exit 2
fi
echo "OK: nothing found that trusts the leaked eMudhra keys."
