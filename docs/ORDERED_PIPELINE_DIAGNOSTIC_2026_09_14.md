# Bounded ordered independent-worker diagnostic

## Result

Independent persistent workers can retain a throughput gain after full ordered delivery, but the gain is much smaller than the worker-local-sink ceiling and uses one additional cleanup thread. With a global 12,000-live-result-row budget, B200 was best: 811,325 VIN/s at 8 decode workers and 861,785 VIN/s at 12. These exceed their same-run bracketed shared-B12000 means by 8.6% and 8.8%. A separate 12-worker confirmation measured 862,330 versus a nearby shared control at 807,816 (+6.7%). The production API was not changed.

| Architecture | Decode workers | Batch | VIN/s | Busy cores | Instructions/VIN | Peak RSS |
|---|---:|---:|---:|---:|---:|---:|
| shared B12000, bracket mean | 8 | 12000 | 746,857 | 7.61 | 76,260 | 2.01 GB |
| ordered independent | 8 | 200 | 811,325 | 8.77 | 75,349 | 2.00 GB |
| shared B12000, bracket mean | 12 | 12000 | 792,074 | 9.74 | 77,249 | 2.01 GB |
| ordered independent | 12 | 200 | 861,785 | 10.40 | 75,651 | 2.03 GB |
| shared confirmation | 12 | 12000 | 807,816 | 10.00 | 77,038 | 2.02 GB |
| ordered confirmation | 12 | 200 | 862,330 | 10.38 | 75,674 | 2.03 GB |

The thread budgets differ. “12 workers” means 12 decode workers in the ordered prototype, plus one persistent cleanup worker and the caller/ordered consumer. The shared control uses 12 Rayon workers plus its caller. Busy-core measurements expose the practical CPU difference; the ordered B200 confirmation used about 0.38 more average core than its nearby control. This diagnostic therefore supports the architecture as promising, but does not establish an equal-thread production speedup.

## Architecture and bound

Workers claim a whole batch under one mutex/condition-variable admission point. Admission assigns input positions in increasing order and reserves row credits before decoding, so a later input batch cannot reserve capacity ahead of a lower input batch that has not yet been admitted. Each worker sequentially calls `decode_full` for every VIN, using the same first-eight-byte locality ordering as the earlier probes, restores input order into batch slots, publishes the owned full `DecodeResult` vector with its input position, and immediately asks for more work. It never waits merely because its own previous output is not yet deliverable.

The consumer buffers completed batches by position, black-box consumes every full result in global input order, and moves the vector to a persistent cleanup worker. The credit permit follows the vector and returns its rows only after destruction. The consumer, publication queue, reorder map, and cleanup queue together can own at most 12,000 admitted rows. RAII returns credits on abandoned ownership; cancellation wakes admission waiters; worker decode failure is signaled to the consumer. The final source adds integration coverage for forced reverse completion, partial tails, mixed/duplicate/invalid VINs, decode failure, consumer failure, bounded credits, and termination. Those correctness changes postdate the immutable measured binary and do not change its normal-path scheduling design.

The measured executable is preserved at `target/bench/ordered-pipeline-probe`; its SHA-256 is recorded in the raw confirmation and memory artifacts, and the base matrix records the measured source hash. The measured source itself was not archived before the post-benchmark correctness hardening, so the final source is not an exact source snapshot of that executable. Reproduction of the exact measurements depends on the preserved binary; a future rerun should rebuild from and record the hardened source.

## Batch-size sweep and bottleneck

| Batch | 8-worker VIN/s | 12-worker VIN/s |
|---:|---:|---:|
| 10 | 647,855 | 585,640 |
| 100 | 796,023 | 706,333 |
| 200 | **811,325** | **861,785** |
| 400 | 785,728 | 828,098 |
| 600 | 759,987 | 827,568 |
| 800 | 779,891 | 797,586 |
| 1000 | 379,511 | 661,200 |

B10 pays excessive scheduling cost. In the initial grid, 8-worker B1000 measured 379,511 VIN/s with only 4.77 busy cores: admission/publish time rose to 141.9 aggregate worker-seconds, versus 1.25 seconds for B200 in the later sweep, while instruction counts stayed close. B1000 was not repeated, and the 12-worker controls in that initial window drifted from 724k to 630k; treat the magnitude as an unconfirmed low-utilization sample rather than a stable batch-size cliff. The extended B200–B800 sweep ran in a later measurement window with its own controls.

Cleanup is the remaining ceiling. For 12-worker B200, the single cleanup worker spent 22.56 seconds destroying the 20m outputs, equivalent to roughly 887k outputs/s, close to the observed 862k end-to-end rate. Separate 10m-corpus pilots compared one and four cleanup workers; four increased contention and did not improve throughput. This does not show that ordering itself is expensive. The local-sink probe freed results on their allocating workers and reached 1.23m VIN/s. A next design should return delivered ownership to an owner-addressed recycle queue and let each decode worker drain its own cleanup queue independently when new admission is blocked. That preserves allocating-worker locality without making new work depend on delivery of that worker's prior batch.

## Memory accounting

All throughput runs enforce the same 12,000 live-result-row budget as shared B12000 and report whole-process peak RSS. Because both paths materialize the same full result type on the same fixed corpus, rows are the available pre-decode admission unit. They are not a hard byte cap: strings and element counts vary by VIN.

A separate recursive diagnostic, excluded from throughput comparisons, counted vector capacity and every owned `String`, `Vec`, and owned `Cow` allocation. Shared B12000 peaked at 174,050,277 output-owned bytes for one chunk. Ordered B200 peaked at 173,153,674 bytes among completed/published results. The latter excludes admitted results still being constructed, so it is a validation signal rather than total live-output memory. RSS includes the 20m input strings, embedded database, allocator state, caches, stacks, and outputs; ordered B200 measured about 2.03 GB versus about 2.01 GB for the nearby shared runs.

All published passes used the 20m unique corpus (SHA-256 `0d6224e99d0a7f241e3dcd052ce973c8baea774feb0831db0071423de726bd9a`), frozen clock `1788220800000000`, year 2026, mimalloc, a full warm pass, and a complete timed pass longer than ten seconds. Runs were serial.

Artifacts:

- `scripts/bench/ordered_pipeline_2026_09_14.json`: B10/B100/B1000 and bracket controls.
- `scripts/bench/ordered_pipeline_sweep_2026_09_14.json`: B200/B400/B600/B800 and bracket controls.
- `scripts/bench/ordered_pipeline_confirm_2026_09_14.json`: nearby B200 confirmation.
- `scripts/bench/ordered_pipeline_memory_2026_09_14.json`: separate recursive owned-byte diagnostics.
- `crates/ultravin/examples/ordered_pipeline_probe.rs`: diagnostic implementation and tests.
