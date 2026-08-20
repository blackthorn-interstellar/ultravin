"""Validate one Grok session's transcript and print what the next stage needs.

A schema-constrained lane runs grok twice, so this script has two modes:

  --mode investigation  the stage that actually reviews, run WITHOUT a schema.
                        Prints its final assistant text (tail-truncated) so the
                        second stage can formalize it.
  --mode verdict        the stage run WITH --json-schema. Prints the payload
                        compact on stdout so it survives one $GITHUB_OUTPUT line.

Both exit non-zero, with the diagnosis on stderr, when the session did not
deliver. That check has to live here rather than in the caller: grok 1.0.4 can
treat a stray line of assistant text as the terminal answer, cancel its own
pending tool calls, and still exit 0 — so a reviewer that never ran `git diff`
is indistinguishable from a healthy run by exit code alone. Every consumer of
this output is a fail-closed gate.

The zero-tool-call check belongs to `investigation` alone: grok-4.6 handed a
schema will often satisfy it on turn 1 with a placeholder and no investigation,
which is why the investigating stage no longer gets a schema at all. The
formalizing stage is *expected* to call nothing.

The `end` event's field is `structuredOutput` (camelCase); grok's own docs call
it `structured_output`, which is wrong for 1.0.4. That event carries no final
text in 1.0.4 — under `--output-format streaming-json` the answer arrives as
`text` deltas — so the final message is reassembled from the deltas that follow
the last tool call.
"""

import argparse
import json
import sys

# The formalizing stage only needs the verdict, which grok writes last. A long
# review can run to hundreds of KB of prose; the tail is the part that matters.
FINAL_TEXT_MAX_CHARS = 30000
TRUNCATION_MARKER = "[earlier investigation output truncated — the tail follows]\n\n"


def scan_transcript(path: str) -> tuple[dict | None, int, str]:
    """The terminal `end` event (None if never emitted), the tool-call count, and the final text."""
    end = None
    tool_calls = 0
    text: list[str] = []
    with open(path) as transcript:
        for line in transcript:
            try:
                event = json.loads(line)
            except ValueError:
                continue
            kind = event.get("type")
            if kind == "end":
                end = event
            elif kind == "tool_call":
                tool_calls += 1
                # Anything said before the last tool call is commentary mid-review,
                # not the answer.
                text.clear()
            elif kind == "text":
                text.append(str(event.get("data", "")))
    return end, tool_calls, "".join(text)


def tail(text: str) -> str:
    if len(text) <= FINAL_TEXT_MAX_CHARS:
        return text
    return TRUNCATION_MARKER + text[-FINAL_TEXT_MAX_CHARS:]


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate a grok transcript and print the next stage's input.")
    parser.add_argument("transcript", help="Path to a streaming-json JSONL transcript.")
    parser.add_argument(
        "--mode",
        choices=("investigation", "verdict"),
        default="verdict",
        help="investigation: print the final assistant text. verdict: print the structured output.",
    )
    args = parser.parse_args()

    end, tool_calls, streamed_text = scan_transcript(args.transcript)
    if end is None:
        print("the session emitted no `end` event (transcript truncated, or grok died)", file=sys.stderr)
        return 1

    problems = []
    if end.get("stopReason") != "end_turn":
        problems.append(f"stopReason was {end.get('stopReason')!r}, expected 'end_turn'")

    if args.mode == "investigation":
        # A later CLI may put the whole message on the `end` event; prefer it.
        final_text = end["text"] if isinstance(end.get("text"), str) and end["text"].strip() else streamed_text
        if tool_calls == 0:
            problems.append("session made 0 tool calls — a review produced without any investigation is invalid")
        if not final_text.strip():
            problems.append("session ended with no assistant text to formalize")
        payload = tail(final_text)
    else:
        if end.get("structuredOutputError"):
            problems.append(str(end["structuredOutputError"]))
        if end.get("structuredOutput") is None:
            problems.append("structuredOutput was null")
        payload = "" if problems else json.dumps(end["structuredOutput"], separators=(",", ":"))

    if problems:
        print("; ".join(problems), file=sys.stderr)
        return 1

    print(payload)
    return 0


if __name__ == "__main__":
    sys.exit(main())
