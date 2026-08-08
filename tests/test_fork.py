"""The batch paths must still work in a process forked from a warmed parent.

`fork()` copies only the calling thread, so a rayon pool built in the parent
arrives in the child with its bookkeeping intact and its worker threads gone —
the child's first `par_iter` then queues a job nobody will ever steal and blocks
forever. That is the ordinary shape of a fork-based Python deployment (gunicorn
prefork after a warmup decode, `multiprocessing` with the `fork` start method),
so warm the pool in the parent, fork, and assert the child still finishes.

The child arms `signal.alarm` and `os._exit`s; SIGALRM's default action kills it,
so a regression shows up here as a signalled child rather than a hung test run.
"""

from __future__ import annotations

import os
import signal

import pytest
import ultravin as uv

from tests.vin_samples import VINS

FORK_BATCH = VINS * 20

CHILD_TIMEOUT_SECONDS = 10


@pytest.mark.skipif(not hasattr(os, "fork"), reason="fork() is POSIX-only")
def test_decode_batch_survives_fork() -> None:
    # Warm the pool in the parent: without this the child would build its own and
    # the test could not fail.
    assert len(uv.decode_batch(FORK_BATCH)) == len(FORK_BATCH)

    pid = os.fork()
    if pid == 0:
        signal.alarm(CHILD_TIMEOUT_SECONDS)
        try:
            ok = len(uv.decode_batch(FORK_BATCH)) == len(FORK_BATCH) and uv.decode_batch_json(FORK_BATCH).startswith(
                "["
            )
        except BaseException:  # noqa: BLE001 - the child must never unwind into pytest
            os._exit(1)
        os._exit(0 if ok else 1)

    _, status = os.waitpid(pid, 0)
    if os.WIFSIGNALED(status):
        sig = signal.Signals(os.WTERMSIG(status)).name
        hint = " (batch decode hung in the forked child)" if os.WTERMSIG(status) == signal.SIGALRM else ""
        pytest.fail(f"forked child killed by {sig}{hint}")
    assert os.WEXITSTATUS(status) == 0
