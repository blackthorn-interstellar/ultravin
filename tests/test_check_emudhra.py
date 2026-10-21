import pytest

from scripts import check_emudhra
from scripts.check_emudhra import CONTROL, ROUTES, go_minimum, judge, on_path, verdict_openssl, version


def fake(answers: dict[str, str]):
    return lambda url: answers[url]


def test_a_stack_refusing_both_routes_is_ok():
    probe = fake({CONTROL: "ok", ROUTES["emSign"]: "cert", ROUTES["AAA"]: "cert"})
    assert judge("x", probe) == ("ok", "x", "refuses both routes")


def test_a_stack_trusting_either_route_is_exposed():
    probe = fake({CONTROL: "ok", ROUTES["emSign"]: "ok", ROUTES["AAA"]: "cert"})
    assert judge("x", probe) == ("EXPOSED", "x", "trusts the leaked keys via: emSign")


@pytest.mark.parametrize(
    "answers",
    [
        {CONTROL: "TimeoutError"},
        {CONTROL: "ok", ROUTES["emSign"]: "curl exit 6", ROUTES["AAA"]: "cert"},
    ],
)
def test_no_answer_is_unsure_not_safe(answers):
    assert judge("x", fake(answers))[0] == "UNSURE"


def test_a_probe_that_cannot_run_is_unsure():
    def probe(url):
        raise FileNotFoundError(url)

    assert judge("x", probe) == ("UNSURE", "x", "probe failed (FileNotFoundError)")


@pytest.mark.parametrize(
    ("output", "verdict"),
    [
        ("depth=0 CN = host\nVerify return code: 0 (ok)\npoll error", "ok"),
        ("verify error:num=20:unable to get local issuer certificate", "cert"),
        ("connect: Connection refused", "handshake failed"),
    ],
)
def test_openssl_is_judged_by_its_verdict_not_exit_status(output, verdict):
    assert verdict_openssl(output) == verdict


def test_versions_compare_piecewise():
    assert version("1.26.10") > version("1.26.9")
    assert version("0.13.4 (7456b60bc 2026-10-20)") == (0, 13, 4)


@pytest.mark.parametrize(
    ("go", "minimum"),
    [("1.26.3", "1.26.10"), ("1.27.0", "1.27.3"), ("1.25.9", "1.27.3"), ("1.28.0", "1.27.3")],
)
def test_go_minimum(go, minimum):
    assert go_minimum(go) == minimum


def test_on_path_finds_each_distinct_binary_once(tmp_path, monkeypatch):
    a, b = tmp_path / "a", tmp_path / "b"
    a.mkdir()
    b.mkdir()
    (a / "curl").write_text("")
    (a / "curl").chmod(0o755)
    (b / "curl").symlink_to(a / "curl")
    (b / "wget").write_text("")  # not executable
    monkeypatch.setenv("PATH", f"{a}:{b}")
    assert on_path("curl") == [str(a / "curl")]
    assert on_path("wget") == []


def test_bundle_roots_reads_subjects(monkeypatch):
    certs = [
        {"subject": ((("commonName", "emSign ECC Root CA - G3"),),)},
        {"subject": ((("organizationName", "x"),), (("commonName", "ISRG Root X1"),))},
    ]
    monkeypatch.setattr(check_emudhra.ssl.SSLContext, "load_verify_locations", lambda self, cafile: None)
    monkeypatch.setattr(check_emudhra.ssl.SSLContext, "get_ca_certs", lambda self: certs)
    assert check_emudhra.bundle_roots("bundle.pem") == {"emSign ECC Root CA - G3"}
