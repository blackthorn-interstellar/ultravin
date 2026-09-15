# Atomic slot coordination diagnostic

## Result

Replacing the reusable-slots probe's global admission mutex with a physical
slot quota and atomic batch claim did not materially improve throughput at 12
workers and B200. Atomic admission with the existing ordered map measured
945,317 VIN/s versus a bracketed immutable-slots mean of 940,774 (+0.5%). In a
reversed confirmation, atomic-map measured 941,343 versus 942,743 VIN/s for
slots (-0.1%). The fixed ready-descriptor ring's single sample measured
931,820 VIN/s (-1.0%); it demonstrated no win, but one sample does not
establish a regression beyond normal variance. These results do not justify a
production change or a wider worker-count grid.

| Path | VIN/s | Gain vs slots bracket | Busy cores | Instructions/VIN | Peak RSS |
|---|---:|---:|---:|---:|---:|
| immutable slots, opening | 943,280 | — | 10.84 | 74,912 | 2.016 GB |
| atomic quota + ordered map | 945,317 | +0.5% | 10.80 | 74,832 | 2.019 GB |
| atomic quota + fixed ring | 931,820 | -1.0% | 10.38 | 74,937 | 2.020 GB |
| immutable slots, closing | 938,268 | — | 10.83 | 74,901 | 2.018 GB |
| atomic-map, reversed confirmation | 941,343 | -0.1% paired | 10.79 | 74,869 | 2.016 GB |
| immutable slots, confirmation | 942,743 | — | 10.83 | 74,867 | 2.016 GB |

The reversed pair confirms that the map result is within control drift.
Instructions per VIN were also effectively unchanged. In its single sample,
the ring's lower busy-core count came with lower throughput and substantially
more aggregate worker wait/publish time (7.62 s versus 3.67 s for atomic-map),
so it provides no evidence of reduced useful work.

## Design and bounds

The probe allocates up to `floor(12000 / batch_size)` physical slots, capped
by the number of input batches. At
B200 this is 60 slots: five per owner with 12 workers, or quotas of eight,
eight, eight, eight, seven, seven, seven, seven with eight workers. A worker
must reserve one of its own free slots before claiming a monotonically
increasing input-batch ID. This makes the physical allocation the admission
bound and removes the timing path's global admission mutex, condition
variable, credit reference-count traffic, and live-row counter updates.
Optional byte accounting retains separate global bookkeeping and was not
enabled in throughput runs.

Each slot retains its own payload mutex. Owner-local condition variables only
signal recycle readiness, so decoding and nested result destruction do not
hold an owner-wide lock. The consumer marks a slot recyclable after ordered
delivery and wakes that owner. Final-claim owners continue recycling until all
their slots are free. Cancellation sets one atomic flag and wakes every owner.
Nested results remain worker-owned through destruction, and every slot keeps a
preallocated `Vec<Option<DecodeResult>>` outer buffer.

The fixed ring has one descriptor entry per physical slot. Every claimed but
not yet emitted batch owns a distinct physical slot, claims are contiguous,
and IDs below the next emitted ID are stale. Therefore a valid received ID is
less than 60 positions ahead of the next emitted ID at B200. The ring stores
the full ID as its generation and rejects out-of-range windows, duplicate
entries, and stale index reuse instead of overwriting metadata. Its metadata
is bounded by the physical slot quota.

## Validation and artifacts

Eight focused tests cover repeated actual owner loops through many ring and
map wraps with final draining, timeout-bounded deadlock detection, exact
physical quotas, a zero-quota owner, full-field parity for invalid, duplicate,
and partial-tail inputs, forced reverse ring completion, caught mid-batch
partial-fill cleanup, and consumer-panic cancellation that wakes every owner.
The mid-batch failure test exercises the worker's fill/catch/clear primitive;
it is not injected through the complete worker command loop. The example also
passes Clippy with warnings denied.

Root review extracted the startup quota calculation into a helper so the
quota test exercises the actual setup code, including tiny and uneven
partitions. This change followed measurement and does not alter the timed
worker loop; the exact measured source remains archived below. Final
`UV_FROZEN=1 make checku` and all eight focused example tests passed.

All measurements used the canonical 20m unique corpus (SHA-256
`0d6224e99d0a7f241e3dcd052ce973c8baea774feb0831db0071423de726bd9a`),
frozen clock `1788220800000000`, year 2026, mimalloc, a full warm pass, and one
timed full pass longer than ten seconds. Runs were serial. No production
default changed.

- `scripts/bench/slot_coordination_2026_09_14.json`: progressive four-run bracket, environment, counters, RSS, commands, corpus manifest, and artifact hashes.
- `scripts/bench/slot_coordination_confirmation_2026_09_14.json`: progressive reversed atomic-map/slots confirmation referencing the original validated corpus metadata and immutable hashes.
- `scripts/bench/slot_coordination_confirm.py`: confirmation runner, SHA-256 `3348d06df997aa02d12a3aed7db530d5f3fe6dacec1020c30cf3c69f1ef40dc4`.
- `target/bench/slot-coordination-probe-2026-09-14`: immutable measured binary, SHA-256 `6be13cc4ffdb028905888850a1d0a9312cc51ede9a08aa63b87c49d5b367e560`.
- `target/bench/slot-coordination-probe-2026-09-14.rs`: exact measured source, SHA-256 `600084b2783d65d85ae716fb26538667a8296d399cbb150dbc2f3ae21fc36de7`.
- `scripts/bench/slot_coordination.py`: runner, SHA-256 `a2cdcd87433057275d9c61ccc254c8c8806fb1453af87b582147393d81ccb037`.
