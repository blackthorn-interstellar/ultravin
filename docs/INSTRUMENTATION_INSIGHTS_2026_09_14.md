# Native multicore instrumentation: September 14, 2026

The clearest newly identified scaling cost is **worker activation and coordination
between phases, especially cleanup**. At twelve workers, some workers join a
phase late or never participate, while others finish earlier. The sampled mutex
waits are in Rayon's sleep/wakeup machinery. Output extraction is only about
0.3% of sampled batch wall time.

Regenerate the interactive worker timeline with `scripts/bench/worker_timeline.py` (it writes `docs/figures/native-worker-timeline.html`, which is not committed).
Select the twelve-worker, 12,000-row profile and batch 448: workers 0 and 4 begin
decoding roughly ten milliseconds after the first workers; worker 7 does no
cleanup in that batch. Other batches identify different late workers. This is
not one permanently slow thread. Blue bars show decoder tasks, purple bars
cleanup, and orange bars setup/sort/extraction. Bars measure wall time, including
any OS descheduling; empty intervals within a sampled batch have no recorded
worker task. The default shows a middle sampled batch. Unrecorded gaps between
sampled batches are unknown.

The function flame graph (regenerated per [NATIVE_BOTTLENECKS_2026_09_14.md](NATIVE_BOTTLENECKS_2026_09_14.md)) provides the matching
kind of call-level detail from earlier captures of the same production engine.
It is a separate capture, not time-synchronized to the new worker timeline.

## Worker scaling and where participation is lost

The adjacent traced runs processed ten million unique VINs, full ordered managed
results, in 12,000-row batches after a complete warm pass:

| Measurement | 8 workers | 12 workers |
|---|---:|---:|
| Whole-pass throughput | 646,909 VIN/s | 662,775 VIN/s |
| Process CPU time per VIN, measured inside the timed pass | 10.987 µs | 12.592 µs |
| Average process CPU seconds per elapsed second | 7.108 | 8.346 |
| Decode task-span occupancy | 95.5% | 82.8% |
| Decode time before first task / no task | 2.8% | 10.1% |
| Decode completion-tail gap | 1.5% | 6.9% |
| Cleanup task-span occupancy | 88.6% | 68.5% |
| Cleanup time before first task / no task | 4.2% | 17.4% |
| Cleanup completion-tail gap | 6.3% | 13.1% |

Twelve workers deliver **2.5% more throughput for 14.6% more CPU time per VIN**.
The occupancy/gap rows describe worker slots during the sampled parallel phase,
weighted by phase duration across fourteen sampled batches. Their denominator is
workers × phase wall duration. They are not CPU utilization. Unused workers are
counted in the before-first-task category; the remaining small gap is between
recorded tasks. The trace validates row conservation, disjoint task spans per
worker, and containment inside each parallel phase.

Late participation is the larger measured gap in twelve-worker decoding, but
completion tails also matter, particularly during cleanup. Across the earlier
stack samples, explicit worker wait/yield observations increased from **6.75% to
25.20%**. All classified wait stacks belong to Rayon sleep/wakeup/idle paths;
none have an allocator ancestor. There are 30 mutex-wait observations at eight
workers and 130 at twelve. Of the latter, 114 include explicit wake methods and
16 occur under the sleep path. This connects the pool's wait machinery to the
observed participation gaps; it does not establish how much of a late start was
OS scheduling delay versus Rayon wakeup delay.

## Costs worth prioritizing

Mean stage wall durations in sampled twelve-worker, 12,000-row batches:

| Stage | Mean wall duration |
|---|---:|
| Slot and worklist setup | 0.202 ms |
| Locality sort, itself parallel | 0.491 ms |
| Parallel decode and full-result construction | 15.257 ms |
| Extract results into the returned vector | 0.056 ms |
| Parallel cleanup | 2.651 ms |

Extraction is too small to explain the missing multicore throughput. Sorting
also scales in these samples: its duration falls from 0.811 ms at eight workers
to 0.491 ms at twelve. Decode/construction and cleanup remain the large phases.

Allocation remains substantial. A freshly rebuilt TLS allocation probe over
one million warmed unique VINs measured **38.17 allocations and 8.38 reallocations
per VIN**, requesting a cumulative **31,745 bytes per VIN**. These bytes count
allocation/reallocation requests, not live memory, copied bytes, or measured
DRAM traffic. This sequential diagnostic excludes batch containers and pool
bookkeeping. In the stack captures, allocator leaf observations comprise
15.4% and 17.8% of non-wait observations at eight and twelve workers. Reducing
allocation work remains worthwhile; these measurements do not identify an
allocator mutex as the source of the additional waits.

Larger batches help participation and CPU efficiency. With 48,000-row batches,
twelve-worker sampled decode occupancy rises from 82.8% to 92.9%, and decode's
before-first-task gap falls from 10.1% to 2.9%. CPU time per VIN falls from 12.592
to 11.412 µs. Eight-worker CPU time falls from 10.987 to 10.171 µs. The larger
batches have only four sampled batches each, and host load varied; these are
mechanism diagnostics, not a claim that 48,000 is universally optimal. Cleanup
still leaves roughly 29% of twelve-worker slots outside recorded tasks.

## Hardware and host load

`powermetrics` collected ten one-second intervals after each warmup, recording
process instructions/cycles, performance-core CPU time, efficiency-core CPU
time, hardware active residency, and thermal state. Those intervals are shorter
than the complete timed passes; counter-window rates are not divided by
whole-pass VIN counts to manufacture instructions/VIN.

At batch 12,000, performance-core IPC stayed near **2.30–2.34** across the initial
runs; efficiency-core IPC was **1.17–1.20**. This machine has eight performance
cores and four efficiency cores. Twelve workers therefore do not receive twelve
copies of the single-core benchmark's execution resources. IPC alone does not
identify cache misses or establish a memory-bandwidth ceiling.

Competing work strongly changes the result. The unchanged production binary's
eight-worker throughput moved from **311k to 563k VIN/s** as competing builds
subsided. The corresponding counter windows recorded approximately 9.7 versus
4.7 live background CPU-core equivalents. In the calmer initial eight-worker
comparison, increasing the batch from 12,000 to 48,000 raised throughput from
563k to 628k VIN/s, while performance-core IPC rose from 2.33 to 2.52.

Thermal pressure remained nominal. The tool's `ALL_TASKS` field had exited-task
accounting spikes exceeding physical CPU capacity, so analysis excludes that
aggregate and retains live-task CPU accounting plus hardware residency instead.
Direct DTrace scheduler probes were unavailable under the machine's System
Integrity Protection; Instruments tracing also requires a full Xcode installation,
which this machine lacks. No security setting was changed.

## Next optimization target

First, measure a design that keeps workers participating across decode and
cleanup and reduces sleep/wakeup transitions, while retaining the existing
ordered-result contract and bounded live memory. The highest-value question is
whether those leading gaps and cleanup gaps can be removed without expensive
spinning or extra contention. Second, reduce result-construction allocations
and remaining reallocations. The batch predictor should be evaluated against
available CPU capacity and phase activation costs, not just configured worker
count. The new instrumentation makes each of these changes directly testable.

## Reproduction and validation

The `stage-trace` feature is off by default. The diagnostic binary preserves
Rayon's original splitting and records per-folder wall spans only for sampled
batches, using separate worker buffers rather than one shared recording lock.
`ULTRAVIN_STAGE_TRACE_EVERY=0` disables collection in the exact same binary;
`64` records batch ordinals 0, 64, 128, and so on. Decode and cleanup ordinals
align in this probe's FIFO managed-result lifecycle, not arbitrary API consumers.
All captured runs had zero dropped events.

Collection-on/off CPU time per VIN differed by under 0.3% at each worker count
in these single pairs. Eight-worker throughput was 643k without collection and
647k with it. The twelve-worker control coincided with renewed background load
and fell to 480k, so its wall-time difference is not an instrumentation-overhead
estimate. This is not a statistical bound on tracing overhead.

```sh
cargo build --release --locked -p ultravin --example pipeline_probe --features stage-trace
RAYON_NUM_THREADS=12 ULTRAVIN_STAGE_TRACE_EVERY=64 \
  target/release/examples/pipeline_probe \
  target/bench/multicore-corpus.txt 10 12 12000 sequential_budget > trace.json
UV_FROZEN=1 uv run python -m scripts.bench.worker_timeline trace.json
```

Artifacts preserve binary/corpus hashes and exact commands:

- [Hardware captures](../scripts/bench/hardware_profile_2026_09_14.json) and [summary](../scripts/bench/hardware_summary_2026_09_14.json).
- [Stage captures and same-binary controls](../scripts/bench/stage_profile_2026_09_14.json) and [validated stage summary](../scripts/bench/stage_summary_2026_09_14.json).
- [Generic-aware stack attribution](../scripts/bench/native_stack_insights_2026_09_15.json).
- [Fresh allocation counts](../scripts/bench/instrumentation_allocations_2026_09_14.json).
