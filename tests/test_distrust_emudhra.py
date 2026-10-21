import base64

from scripts.distrust_emudhra import without_roots


def pem(body: bytes) -> bytes:
    b64 = base64.encodebytes(body)
    return b"-----BEGIN CERTIFICATE-----\n" + b64 + b"-----END CERTIFICATE-----\n"


def test_drops_only_the_routes_roots():
    keep = pem(b"\x30\x82 CN=ISRG Root X1")
    emsign = pem(b"\x30\x82 CN=emSign ECC Root CA - G3")
    aaa = pem(b"\x30\x82 CN=AAA Certificate Services")
    bundle = b"# ISRG Root X1\n" + keep + b"\n" + emsign + aaa
    kept, dropped = without_roots(bundle)
    assert kept == keep
    assert dropped == {"emSign ECC Root CA - G3": emsign, "AAA Certificate Services": aaa}


def test_a_clean_bundle_drops_nothing():
    keep = pem(b"\x30\x82 CN=ISRG Root X1")
    assert without_roots(keep) == (keep, {})
