"""scripts.coverage demangle keys, independent of a llvm-cov run."""

from scripts.coverage import demangle


def test_demangle_strips_crate_disambiguator_prefixes() -> None:
    assert demangle("qzY::N4i::ultravin::checkdigit::check_digit") == "checkdigit::check_digit"
    assert demangle("ultravin::decode::decode_core_into") == "decode::decode_core_into"


def test_demangle_v0_mangling_after_the_crate_marker() -> None:
    assert demangle("_RNvNtCs3qzYG3N4izx_8ultravin10checkdigit11check_digit") == "checkdigit::check_digit"
    assert demangle("_RNvNtCs3qzYG3N4izx_8ultravin6decode16decode_core_into") == "decode::decode_core_into"
