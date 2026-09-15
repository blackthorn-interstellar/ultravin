"""Convert macOS ``sample`` call graphs into weighted full stacks.

``sample`` reports inclusive counts. :func:`parse_sample` subtracts direct-child
counts from every node and returns one record per positive exclusive weight.
Weights represent sampled thread occupancy; they are not CPU percentages.
"""

from __future__ import annotations

import json
import re
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Annotated

import typer

THREAD_RE = re.compile(r"^    (?P<count>\d+) Thread_(?P<thread>\d+)(?:\s|$)")
# Branch glyphs disappear when a thread has a single path, leaving indentation
# made entirely of spaces. Every depth still occupies two prefix columns.
NODE_RE = re.compile(r"^(?P<prefix>[ +!|:]{6,}?)(?P<count>\d+) (?P<frame>.+)$")
FRAME_SUFFIX_RE = re.compile(r"\s+\(in [^)]+\)(?:\s+\+\s+[^[]+)?(?:\s+\[[^]]+\])?$|\s+\[[^]]+\]$")
WAIT_LEAVES = (
    "__psynch_cvwait",
    "__psynch_mutexwait",
    "__semwait_signal",
    "kevent",
    "mach_msg2_trap",
    "poll",
    "select",
    "swtch_pri",
    "ulock_wait",
)


@dataclass(frozen=True)
class StackSample:
    """An exclusive sample count for one thread and full root-to-leaf stack."""

    thread_id: str
    frames: tuple[str, ...]
    samples: int
    occupancy: str


@dataclass
class _Node:
    frame: str
    inclusive: int
    children: list[_Node]


def _frame(text: str, demangle: dict[str, str]) -> str:
    raw = FRAME_SUFFIX_RE.sub("", text).strip()
    return demangle.get(raw, raw)


def _occupancy(frame: str) -> str:
    return "known_wait" if any(frame == name or frame.startswith(f"{name} ") for name in WAIT_LEAVES) else "sampled"


def parse_sample(text: str, demangle: dict[str, str] | None = None) -> list[StackSample]:
    """Parse the ``Call graph`` section and conserve each thread's sample total.

    A demangle mapping may replace exact raw frame names. Only explicit blocking
    leaf functions are classified as waits; a Rayon wait ancestor with active
    descendants remains ordinary sampled occupancy.
    """

    demangle = demangle or {}
    in_graph = False
    thread_id: str | None = None
    thread_total = 0
    roots: list[_Node] = []
    stack: list[tuple[int, _Node]] = []
    result: list[StackSample] = []

    def finish_thread() -> None:
        nonlocal thread_id, thread_total, roots, stack
        if thread_id is None:
            return
        current_thread_id = thread_id
        top_total = sum(node.inclusive for node in roots)
        if top_total > thread_total:
            message = f"thread {thread_id} child counts exceed its total"
            raise ValueError(message)

        emitted = 0

        def emit(node: _Node, parents: tuple[str, ...]) -> None:
            nonlocal emitted
            child_total = sum(child.inclusive for child in node.children)
            if child_total > node.inclusive:
                message = (
                    f"negative exclusive samples below {node.frame!r}: "
                    f"inclusive={node.inclusive}, direct_children={child_total}"
                )
                raise ValueError(message)
            frames = (*parents, node.frame)
            exclusive = node.inclusive - child_total
            if exclusive:
                result.append(StackSample(current_thread_id, frames, exclusive, _occupancy(node.frame)))
                emitted += exclusive
            for child in node.children:
                emit(child, frames)

        for root in roots:
            emit(root, ())
        unattributed = thread_total - top_total
        if unattributed:
            result.append(StackSample(current_thread_id, ("[thread root]",), unattributed, "sampled"))
            emitted += unattributed
        if emitted != thread_total:
            message = f"thread {thread_id} sample conservation failed"
            raise ValueError(message)
        thread_id = None
        thread_total = 0
        roots = []
        stack = []

    for line in text.splitlines():
        if line == "Call graph:":
            in_graph = True
            continue
        if not in_graph:
            continue
        if line.startswith(("Total number in stack", "Sort by top of stack", "Binary Images:")):
            break
        thread_match = THREAD_RE.match(line)
        if thread_match:
            finish_thread()
            thread_id = thread_match.group("thread")
            thread_total = int(thread_match.group("count"))
            continue
        node_match = NODE_RE.match(line)
        if not node_match or thread_id is None:
            continue
        prefix = node_match.group("prefix")
        if (len(prefix) - 6) % 2:
            message = f"invalid call-graph indentation: {line!r}"
            raise ValueError(message)
        depth = (len(prefix) - 6) // 2
        node = _Node(_frame(node_match.group("frame"), demangle), int(node_match.group("count")), [])
        while stack and stack[-1][0] >= depth:
            stack.pop()
        if depth == 0:
            roots.append(node)
        elif not stack or stack[-1][0] != depth - 1:
            message = f"missing parent at call-graph depth {depth}"
            raise ValueError(message)
        else:
            stack[-1][1].children.append(node)
        stack.append((depth, node))
    finish_thread()
    if not in_graph:
        message = "sample report has no Call graph section"
        raise ValueError(message)
    return result


def main(
    input_path: Annotated[Path, typer.Argument(exists=True, dir_okay=False)],
    output: Annotated[Path, typer.Option("--output", "-o")],
    demangle_map: Annotated[Path | None, typer.Option("--demangle-map", exists=True, dir_okay=False)] = None,
) -> None:
    """Write exclusive weighted stacks as JSON for a flamegraph or treemap."""

    mapping = json.loads(demangle_map.read_text()) if demangle_map else {}
    if not isinstance(mapping, dict) or not all(
        isinstance(key, str) and isinstance(value, str) for key, value in mapping.items()
    ):
        message = "demangle map must be a JSON string-to-string object"
        raise typer.BadParameter(message)
    stacks = parse_sample(input_path.read_text(), mapping)
    output.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "weight_semantics": "sampled thread occupancy counts, not CPU percentages",
                "stacks": [asdict(stack) for stack in stacks],
            },
            indent=2,
        )
        + "\n"
    )


if __name__ == "__main__":
    typer.run(main)
