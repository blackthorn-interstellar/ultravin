# Independent batch architecture diagnostic

All runs used frozen clock `1788220800000000`, full native `DecodeResult` materialization and destruction, mimalloc, locality sorting inside each batch, and persistent worker threads. Every-field equality and output order were checked outside the timed region. The production decoder API was not changed.

## Result

Persistent workers dynamically claiming independent batches are substantially faster than the current shared Rayon B12000 path when result consumption and cleanup stay on the worker. On the 20m unique corpus, B10 reached 1,076,722 VIN/s at 8 workers versus the bracketed shared mean of 747,232 (+44.1%), and 1,230,946 VIN/s at 12 workers versus the bracketed shared mean of 781,683 (+57.5%). A second immutable-binary 12-worker B10 run reached 1,233,550 VIN/s, confirming the key result.

| Architecture | Workers | Worker batch | VIN/s | Busy cores | Instructions/VIN | Peak RSS |
|---|---:|---:|---:|---:|---:|---:|
| shared B12000, mean of brackets | 8 | 12000 shared | 747,232 | 7.68 | 76,490 | 2.031 GB |
| independent local sink | 8 | 10 | 1,076,722 | 7.99 | 76,874 | 1.980 GB |
| independent local sink | 8 | 100 | 1,003,924 | 7.99 | 76,930 | 1.983 GB |
| independent local sink | 8 | 1000 | 815,795 | 7.98 | 77,144 | 2.007 GB |
| shared B12000, mean of brackets | 12 | 12000 shared | 781,683 | 9.81 | 77,466 | 2.054 GB |
| independent local sink | 12 | 10 | 1,230,946 | 10.83 | 76,808 | 1.999 GB |
| independent local sink | 12 | 100 | 1,121,182 | 10.69 | 76,981 | 2.005 GB |
| independent local sink | 12 | 1000 | 918,232 | 10.60 | 77,218 | 2.040 GB |

These are single screening samples for each local-sink configuration; the shared values are two bracketing samples. Each timed pass covered the whole 20m unique corpus and exceeded ten seconds. The input file is 360 MB; its in-memory strings and vectors consume substantially more. Peak RSS includes input loading, warmup, caches, and output, so it is not a measure of live output alone and cannot be compared directly with the earlier 10m-corpus experiment.

The local-sink architecture intentionally has no ordered downstream delivery. Each worker atomically claims the next whole batch, decodes it sequentially into an input-ordered full result vector, black-box consumes it, and destroys it locally before claiming more work. It measures the compute/dispatch ceiling for the requested independent-worker architecture, rather than a finished ordered API.

## Ordered bounded handoff diagnostic

The first experiment used bounded ordered delivery with at most one live result batch per worker. The coordinator returned each owned vector to its worker for cleanup, and the worker could receive another job only once that batch reached the head of the ordered stream. Two reversed rounds ran on the 10m unique corpus.

| Architecture | Workers | Worker batch | VIN/s | Instructions/VIN | Worker wall outside decode/cleanup | Peak RSS | Max active job rows |
|---|---:|---:|---:|---:|---:|---:|---:|
| shared B12000 | 8 | 12000 shared | 749,170 | 76,476 | n/a | 1.194 GB | 12000 |
| strict ordered handoff | 8 | 10 | 573,706 | 78,728 | 37.3% | 1.026 GB | 80 |
| strict ordered handoff | 8 | 100 | 605,197 | 75,204 | 34.3% | 1.029 GB | 800 |
| strict ordered handoff | 8 | 1000 | 701,746 | 75,064 | 13.7% | 1.106 GB | 8000 |
| shared B12000 | 12 | 12000 shared | 801,302 | 77,184 | n/a | 1.282 GB | 12000 |
| strict ordered handoff | 12 | 10 | 585,972 | 78,709 | 56.6% | 1.039 GB | 120 |
| strict ordered handoff | 12 | 100 | 631,761 | 75,211 | 51.0% | 1.049 GB | 1200 |
| strict ordered handoff | 12 | 1000 | 736,344 | 75,061 | 36.1% | 1.172 GB | 12000 |

This strict window recreates head-of-line blocking: fast workers cannot run ahead while an earlier batch is outstanding. Its poor throughput is therefore not the ceiling of independent decoding. The original ordered binary also retained parity vectors during timing (up to 4,006 additional results for B1000). The active-job bounds above exclude those retained validation results, while measured RSS includes them. The source now drops them before warmup. The worker-wall remainder includes dispatch and handoff work as well as waiting; it is not a direct scheduler-wait counter.

The optimum depends on the delivery architecture. B1000 performed best under the strict ordered window and amortizes its handoffs over more VINs, while B10 won with atomic claiming and worker-local cleanup. The local-sink path used less CPU time per VIN with broadly similar instruction counts. Reduced memory stalls are a plausible explanation, but these counters do not establish the cause. A production ordered design should test decoupling worker scheduling from output order with a larger bounded reorder window and explicit credits returned after worker-local destruction.

## Artifacts and validation

- [Ordered raw samples](../scripts/bench/independent_batches_2026_09_14.json)
- [Worker-local sink, bracketed controls, and confirmation](../scripts/bench/independent_sink_2026_09_14.json)
- Ordered probe and runner: `crates/ultravin/examples/independent_batch_probe.rs`, `scripts/bench/independent_batches.py`
- Local-sink probe and runner: `crates/ultravin/examples/independent_sink_probe.rs`, `scripts/bench/independent_sink.py`
- 20m corpus SHA-256: `0d6224e99d0a7f241e3dcd052ce973c8baea774feb0831db0071423de726bd9a`
- Sink binary SHA-256: `f132e6b05b931cafbcccfc8f11d982195685250790b2fb023ead260d3cac79d2`
- Corrected control binary SHA-256: `e06b5e780664756e810c61c3baadc74fe5d07339ae6ed9a2ae8957f432315746`

`UV_FROZEN=1 make checku` passed after the final source changes: 232 Rust tests and 950 Python tests.
