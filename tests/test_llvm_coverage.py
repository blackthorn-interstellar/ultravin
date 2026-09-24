"""scripts.coverage allowance keys, independent of a llvm-cov run."""

from scripts.coverage import allowance_key


def test_allowance_key_drops_the_crate_prefix() -> None:
    assert allowance_key("ultravin::checkdigit::check_digit") == "checkdigit::check_digit"
    assert allowance_key("<ultravin::errors::Charset>::freeze::{closure#1}") == "<errors::Charset>::freeze::{closure#1}"


def test_allowance_key_keeps_trait_impls() -> None:
    name = "<ultravin::errors::ValidChars as core::fmt::Display>::fmt"
    assert allowance_key(name) == "<errors::ValidChars as core::fmt::Display>::fmt"


def test_allowance_key_collapses_generic_instantiations() -> None:
    name = (
        "ultravin::errors::used_key_positions::<core::iter::adapters::map::Map<core::slice::iter::Iter<"
        "ultravin::decode::DecodingItem>, ultravin::errors::compute_errors_with_context::{closure#0}>>"
    )
    assert allowance_key(name) == "errors::used_key_positions"
