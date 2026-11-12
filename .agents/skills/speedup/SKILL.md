---
name: speedup
description: Make ultravin dramatically faster. Use when asked to speed up ultravin, or via /speedup [target].
---

Make ultravin 100x faster (or the target given) on the headline benchmark in
`docs/BENCHMARKS.md`, with identical output.

Divide today's cycles per VIN by the target. That is your budget. Design the
decoder that fits it, then build the largest piece of that design you can prove
correct. Measure, keep or revert, repeat.

- `make answerkey-verify` proves the answers are unchanged.
- `PERFORMANCE_IMPROVEMENTS.md` records what was already tried and how to measure.
