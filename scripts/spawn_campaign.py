"""Daemonize the campaign supervisor (double-fork + setsid) so it fully detaches
from this shell/agent and keeps running across the session. Run from the repo
root: `uv run python scripts/spawn_campaign.py`. Logs go to campaign/supervisor.out.
"""

import os
import sys
from pathlib import Path

Path("campaign").mkdir(exist_ok=True)

if os.fork() > 0:
    sys.exit(0)  # original parent returns to the caller immediately
os.setsid()  # new session: detach from the controlling terminal + process group
if os.fork() > 0:
    os._exit(0)  # session leader exits so the daemon can't reacquire a terminal

out = open("campaign/supervisor.out", "a")  # noqa: SIM115
os.dup2(out.fileno(), 1)
os.dup2(out.fileno(), 2)
os.dup2(open(os.devnull).fileno(), 0)  # noqa: SIM115
os.execvp("bash", ["bash", "scripts/campaign-supervisor.sh"])
