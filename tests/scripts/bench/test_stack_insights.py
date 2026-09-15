from __future__ import annotations

import pytest

from scripts.bench.stack_insights import ALLOCATOR, _leaf_category, _outer_symbol, _wait_category


@pytest.mark.parametrize(
    "symbol",
    [
        "_mi_page_free_collect",
        "mi_free_generic_mt",
        "mi_malloc_aligned",
        "malloc",
        "madvise",
        "<alloc::raw_vec::RawVec<ultravin::DecodeResult>>::grow_one",
    ],
)
def test_allocator_symbols_are_classified(symbol: str) -> None:
    outer = _outer_symbol(symbol)

    assert ALLOCATOR.search(outer)
    assert _leaf_category(symbol, wait=False) == "allocator"


def test_outer_symbol_removes_nested_closure_names() -> None:
    symbol = (
        "<rayon_core::job::StackJob<rayon_core::latch::SpinLatch, "
        "ultravin::batch_at<ultravin::DecodeResult, <ultravin::RawResult>::full>::{closure#0}> "
        "as rayon_core::job::Job>::execute"
    )

    outer = _outer_symbol(symbol)

    assert outer == "<rayon_core::job::StackJob as rayon_core::job::Job>::execute"
    assert _leaf_category(symbol, wait=False) == "other_runtime_or_system"


def test_outer_symbol_preserves_ufcs_function_name() -> None:
    symbol = "<ultravin::RawResult>::full"

    assert _outer_symbol(symbol) == symbol
    assert _leaf_category(symbol, wait=False) == "full_result_construction"


def test_wait_category_distinguishes_rayon_wake_from_allocator_ancestry() -> None:
    wake_frames = [
        "<rayon_core::registry::WorkerThread>::wait_until_cold",
        "<rayon_core::sleep::Sleep>::wake_any_threads",
        "__psynch_mutexwait",
    ]
    allocator_frames = ["_mi_page_free_collect", "__psynch_mutexwait"]

    assert _wait_category(wake_frames[-1], wake_frames) == "rayon_sleep_wake"
    assert _wait_category(allocator_frames[-1], allocator_frames) == "allocator_ancestor"
