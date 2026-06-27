"""Launch the campaign supervisor fully detached (its own session) so it keeps
running across this shell/agent session. Run from anywhere:

    uv run python scripts/spawn_campaign.py

`start_new_session=True` puts the supervisor in a new session/process-group, so
it survives the launching process being reaped or its process group killed.
Output goes to campaign/supervisor.out.
"""

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
(ROOT / "campaign").mkdir(exist_ok=True)

out = open(ROOT / "campaign" / "supervisor.out", "a")  # noqa: SIM115
subprocess.Popen(  # noqa: S603
    ["bash", str(ROOT / "scripts" / "campaign-supervisor.sh")],
    cwd=str(ROOT),
    stdin=subprocess.DEVNULL,
    stdout=out,
    stderr=subprocess.STDOUT,
    start_new_session=True,
)
print("supervisor spawned (detached)", file=sys.stderr)
