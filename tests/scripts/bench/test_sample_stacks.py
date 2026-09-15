from __future__ import annotations

import pytest

from scripts.bench.sample_stacks import StackSample, parse_sample


def report(body: str) -> str:
    return f"header\nCall graph:\n{body}\nSort by top of stack, same collapsed (when >= 5):\nignored"


def test_inclusive_tree_becomes_conserved_exclusive_full_stacks() -> None:
    stacks = parse_sample(
        report(
            """    10 Thread_42
    + 10 root  (in app) + 4  [0x1]
    +   7 work  (in app) + 8  [0x2]
    +   ! 4 leaf_a  (in app) + 12  [0x3]
    +   ! 2 leaf_b  (in app) + 16  [0x4]
    +   3 __psynch_cvwait  (in libsystem_kernel.dylib) + 8  [0x5]"""
        ),
        {"leaf_a": "crate::leaf_a"},
    )

    assert sum(stack.samples for stack in stacks) == 10
    assert {(stack.frames, stack.samples) for stack in stacks} == {
        (("root", "work"), 1),
        (("root", "work", "crate::leaf_a"), 4),
        (("root", "work", "leaf_b"), 2),
        (("root", "__psynch_cvwait"), 3),
    }
    assert {stack.thread_id for stack in stacks} == {"42"}
    waits = [stack for stack in stacks if stack.occupancy == "known_wait"]
    assert [(stack.frames[-1], stack.samples) for stack in waits] == [("__psynch_cvwait", 3)]


def test_known_blocking_leaf_is_classified_but_wait_ancestor_is_not() -> None:
    stacks = parse_sample(
        report(
            """    5 Thread_7
    + 5 wait_until_cold
    +   3 active_child
    +     2 __psynch_mutexwait"""
        )
    )

    by_frames = {stack.frames: stack for stack in stacks}
    assert by_frames[("wait_until_cold",)].occupancy == "sampled"
    assert by_frames[("wait_until_cold", "active_child")].occupancy == "sampled"
    assert by_frames[("wait_until_cold", "active_child", "__psynch_mutexwait")].occupancy == "known_wait"
    assert sum(stack.samples for stack in stacks) == 5


def test_single_path_thread_without_branch_glyphs_preserves_real_leaf_names() -> None:
    stacks = parse_sample(
        report(
            """    730 Thread_15990392
      730 thread_start  (in libsystem_pthread.dylib) + 8  [0x1]
        730 _pthread_start  (in libsystem_pthread.dylib) + 136  [0x2]
          730 rayon_worker  (in app) + 380  [0x3]
            730 swtch_pri  (in libsystem_kernel.dylib) + 8  [0x4]
Total number in stack (recursive counted multiple, when >=5):
        99999 collapsed_statistic_that_is_not_a_tree_node"""
        )
    )

    assert stacks == [
        StackSample(
            thread_id="15990392",
            frames=("thread_start", "_pthread_start", "rayon_worker", "swtch_pri"),
            samples=730,
            occupancy="known_wait",
        )
    ]


def test_rejects_impossible_inclusive_counts_and_missing_graph() -> None:
    with pytest.raises(ValueError, match="negative exclusive samples"):
        parse_sample(
            report("""    2 Thread_1
    + 2 parent
    +   3 child""")
        )
    with pytest.raises(ValueError, match="no Call graph"):
        parse_sample("not a sample report")
