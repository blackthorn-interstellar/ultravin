"""Extract and validate a Grok session's schema-constrained payload.

Prints the payload compact on stdout so it survives one $GITHUB_OUTPUT line.

Exits non-zero, with the diagnosis on stderr, when a session that was asked for
structured output did not deliver one. That check has to live here rather than
in the caller: grok 1.0.4 can treat a stray line of assistant text as the
terminal answer, cancel its own pending tool calls, and still exit 0 — so a
reviewer that never ran `git diff` and never produced a verdict is
indistinguishable from a healthy run by exit code alone. The `end` event knows
better, and every consumer of this output is a fail-closed gate.

The `end` event's field is `structuredOutput` (camelCase); grok's own docs call
it `structured_output`, which is wrong for 1.0.4.
"""

import json
import sys


def last_end_event(path: str) -> dict | None:
    """The session's terminal `end` event, or None if it never emitted one."""
    end = None
    with open(path) as transcript:
        for line in transcript:
            try:
                event = json.loads(line)
            except ValueError:
                continue
            if event.get("type") == "end":
                end = event
    return end


def main() -> int:
    end = last_end_event(sys.argv[1])
    if end is None:
        print("the session emitted no `end` event (transcript truncated, or grok died)", file=sys.stderr)
        return 1

    problems = []
    if end.get("structuredOutputError"):
        problems.append(str(end["structuredOutputError"]))
    if end.get("structuredOutput") is None:
        problems.append("structuredOutput was null")
    if end.get("stopReason") != "end_turn":
        problems.append(f"stopReason was {end.get('stopReason')!r}, expected 'end_turn'")
    if problems:
        print("; ".join(problems), file=sys.stderr)
        return 1

    print(json.dumps(end["structuredOutput"], separators=(",", ":")))
    return 0


if __name__ == "__main__":
    sys.exit(main())
