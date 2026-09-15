# Reusable owner slots and storage V2 diagnostics

## Result

Worker-owned reusable slots materially improved bounded, globally ordered full-result delivery. At 12 decode workers and B200, the immutable pilot measured 942,780 VIN/s versus a bracketed shared-B12000 mean of 802,462 (+17.5%). A later window reproduced 943,160 VIN/s versus a bracketed mean of 819,682 (+15.1%). At eight workers, B200 measured 856,818 versus 749,745 (+14.3%). No production default changed.

| Path | Workers | Batch | VIN/s | Shared bracket | Gain | Busy cores | Instructions/VIN | Peak RSS |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| reusable slots, pilot | 12 | 200 | 942,780 | 802,462 | +17.5% | 10.90 | 74,944 | 2.017 GB |
| reusable slots, confirmation | 12 | 200 | 943,160 | 819,682 | +15.1% | 10.88 | 74,785 | 2.018 GB |
| reusable slots | 8 | 200 | 856,818 | 749,745 | +14.3% | 7.98 | 74,675 | 1.994 GB |

The bounded 12-worker follow-up measured B100 at 937,573, B200 at 943,160, and B400 at 925,569 VIN/s in one shared-control bracket. B200 remained best. The slots path uses 12 decoder/owner threads plus the caller/ordered consumer. The earlier ordered prototype used those threads plus a dedicated cleanup thread, so the slots experiment removes that extra thread and returns nested destruction to the allocating worker.

## Slot architecture and limits

Each worker owns five fixed B200 slots in the 12-worker experiment. A slot preallocates its outer `Vec<Option<DecodeResult>>`; the worker writes decoded rows into their input positions, then publishes only `(input batch ID, owner ID, slot ID)`. The consumer buffers these small descriptors, reads slots in global input order, and marks them recyclable. The original worker clears every nested result and only then drops the credit permit. Workers drain recycling between decode tasks, while admission is blocked, and after the final input claim.

One coordinator condition variable covers admission changes, recycle readiness, and cancellation. A worker rechecks cleanup and admission around the shared generation before sleeping, avoiding split-condition deadlocks. Cancellation wakes all owners. Consumer callbacks are caught while the slot lock remains held; the slot is restored to recyclable state and unlocked before unwinding. Decode panics are caught while the owner retains the lock, so partially filled options can be cleared without poisoning the slot.

The global admission limit is 12,000 rows across construction, publication, the reorder buffer, consumption, and awaiting owner cleanup. The 12-worker B200 run physically allocates 60 slots and peaked at 11,200 admitted rows. This is a row bound rather than a byte bound.

The reusable allocation is the outer slot vector; nested strings, error vectors, element vectors, and computed strings are still created for each result. The main measured hypothesis is owner-local nested destruction, with outer-vector reuse as a smaller secondary effect. The experiment does not isolate those effects.

Phase fields require care. `consumer_delivery_seconds` in the immutable measured binary is the entire consumer loop, including waits for publishers, rather than serial callback cost. `aggregate_worker_recycle_scan_and_cleanup_seconds` includes slot scans, mutex acquisition, and destruction; it is not allocator-only time. The recursive completed-output diagnostic measured 152.6 MB for slots versus 174.1 MB for shared, but the slots figure excludes in-progress construction and empty preallocated slots. Whole-process RSS is the comparable memory signal and was essentially flat (about 2.020 GB versus 2.014 GB in the accounting runs).

Eight focused tests cover full-field parity for mixed invalid and duplicate inputs with a partial tail, forced reverse completion, nested destruction before credit release, blocked admission and cleanup progress, cancellation, consumer panic restoration, a caught mid-batch partial fill, and repeated actual pipeline passes through final draining. The caught mid-batch test validates the same catch-and-clear primitive used by the worker; it is not an injected failure through the entire worker loop.

The next architectural experiment, if pursued, is to partition exactly 60 B200 slots across owners (seven or eight each at eight workers; five each at 12), use an atomic input claim, and rely on owner-local recycle progress. The fixed physical quota supplies the row bound, which may allow removal of the per-batch global admission mutex. That hypothesis was not implemented here.

## Storage V2

The restored slab V2 consolidates only each shard's element `Vec`. Header strings, error-code vectors, and owned computed strings remain per row, and temporary raw results are retained within a shard while the exact element count is computed.

| Path | Shard | VIN/s | Gain vs owned bracket | Busy cores | Instructions/VIN | Peak RSS |
|---|---:|---:|---:|---:|---:|---:|
| owned bracket mean | — | 796,292 | — | 9.99 | 79,003 | 2.015 GB |
| slab V2 | 64 | 818,518 | +2.8% | 10.13 | 79,998 | 2.025 GB |
| slab V2 | 256 | 744,785 | -6.5% | 9.80 | 80,206 | 2.089 GB |
| slab converted to owned | 64 | 631,335 | -20.7% | 9.84 | 82,386 | 2.204 GB |

The 1m-row allocation diagnostic counted 36.80 allocations/VIN for owned, 35.84 for slab-64, and 35.83 for slab-256. Completed-output owned bytes were effectively equal at about 173.6–173.8 MB. These byte counts exclude temporary raw storage during construction; process RSS includes it. The probe's counting allocator adds an enabled-state atomic load to allocation calls in every mode, so these storage rates should only be compared within the same binary and should not be compared directly with the mimalloc-only ordered/slots rates.

Shard 64 has a small throughput gain with slightly higher RSS and instruction count. Shard 256 and compatibility conversion regress. This does not support production activation or a broader eight-worker matrix.

## Reproduction artifacts

All throughput measurements used the canonical 20m unique corpus (SHA-256 `0d6224e99d0a7f241e3dcd052ce973c8baea774feb0831db0071423de726bd9a`), frozen clock `1788220800000000`, year 2026, mimalloc, a full warm pass, and one timed full pass longer than ten seconds. Runs were serial.

- `scripts/bench/reusable_slots_2026_09_14.json`: immutable B200 pilot and bracket.
- `scripts/bench/reusable_slots_followup_2026_09_14.json`: bounded batch-size, eight-worker, and memory follow-ups.
- `target/bench/reusable-slots-probe`: measured slots binary, SHA-256 `169b8dacfb7078ab291cd9092ce0a748ee9db9ee1c53f9955b7fd3fb41a686ea`.
- `target/bench/reusable-slots-probe.rs`: exact measured slots source, SHA-256 `be5c33354bef3f5b8f2b10a74ccf96b9f3e43441a3ea5f79d3fe3a22b0495d3e`.
- `scripts/bench/batch_storage_v2_2026_09_14.json`: V2 throughput and allocation diagnostics.
- `target/bench/storage-probe-v2`: measured V2 binary, SHA-256 `b393522f5e086ca07be0fae7ac3bf78a36051b2562d36ed46ff6384ceeadeb14`.
- `target/bench/storage-v2-source/`: exact measured V2 implementation, probe, instrumentation, Cargo metadata, and lockfile snapshots with per-file hashes recorded in the JSON.
