"""Parity harness: VIN generation, differential oracle/ultravin runs, diffing.

The decode logic is NOT here. This package only *exercises* ultravin against the
unmodified Postgres oracle (vpic.spvindecode) and reports field-for-field diffs,
so the W2 gap can be quantified and frozen into a self-contained regression test.
"""
