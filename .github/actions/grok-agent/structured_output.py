"""Extract a Grok session's schema-constrained payload from its JSONL transcript.

The `end` event carries `structuredOutput` (camelCase — the CLI's docs call it
`structured_output`, but 1.0.4 emits camelCase in both `json` and
`streaming-json`). Printed compact so it survives a single $GITHUB_OUTPUT line;
prints nothing when the session produced no payload, which leaves the consuming
gate to fail closed.
"""

import json
import sys


def main() -> None:
    payload = ""
    with open(sys.argv[1]) as transcript:
        for line in transcript:
            try:
                event = json.loads(line)
            except ValueError:
                continue
            if event.get("type") == "end" and event.get("structuredOutput") is not None:
                payload = json.dumps(event["structuredOutput"], separators=(",", ":"))
    print(payload)


if __name__ == "__main__":
    main()
