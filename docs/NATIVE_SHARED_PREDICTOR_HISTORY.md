# Retired native shared-batch predictor

This describes the earlier shared-batch execution path and its recorded
measurements. Native automatic streaming now uses the
[worker-slot predictor](NATIVE_WORKER_AUTO_2026_09_15.md).

The retired native model is `ultravin-native-batch-v2`. Its fit contains 64 timed
samples across 32 worker/batch settings, each lasting at least ten seconds over unique input: 1, 4, 8, and 12
workers at batch sizes from 256 through 16,384 rows. Batch size 9,000 was held
out of fitting. Held-out mean absolute percentage error is 2.99%; leave-one-
batch-out error is 4.84%. Five coefficients are active (`a`, `b`, `d1`, `l0`,
and `l1`); nonnegative least squares selected zero for `h0`, `h1`, and `d0`.
With those fitted zeros, the native speed input scales estimated throughput;
at fixed worker count, output width, and memory, it does not change the selected
batch size. Live tuning still measures the current job.

Its reference single-core speed is **142,713.934 VIN/s**, pooled from 20
calibration offsets measured on the same host. Pooling avoids assigning
worker-process scheduling jitter to the model's CPU-speed input. The measured
native output-width estimates range from 15,799.875 to 16,513.6875 bytes per
row. The model rounds the conservative upper value to a 256-byte boundary,
**16,640 bytes per row**. Those estimates cover 321,240 distinct rows and
2,590,176 sampled draws. See the [fit](../scripts/bench/native_managed_fit_2026_09_14.json)
and [width measurements](../scripts/bench/native_managed_width_2026_09_14.json).

The source grid predates the final year-borrow engine change. A paired full-pass
validation of the final engine measured automatic-mode median gains of 23.1%
at four workers, 35.9% at eight, and 24.1% at twelve against the preceding
managed-worker binary. Median peak RSS changed from 1.81 to 1.43 GiB, 2.45 to
1.48 GiB, and 3.19 to 1.69 GiB, respectively. The v2 predictor initially chose
15,784, 11,719, and 11,018 rows; the preceding model chose ranges of 822–856,
1,152–1,310, and 1,368–1,506 rows. Each result is the median of two reversed
fresh-process pairs over the complete 10-million-row unique corpus. See the
[raw automatic scaling comparison](../scripts/bench/multicore_auto_scaling_2026_09_14.json).

The final eight-worker median was 656,900 VIN/s, above the twelve-worker median
of 630,092 VIN/s. This full-result workload therefore did not scale further on
the host's four efficiency cores. Shared-host scheduling and allocation,
cleanup, and memory-system pressure all remain possible limits; the measurements
do not demonstrate saturated memory bandwidth. One-worker validation is still
pending, so scaling relative to one worker is not yet available.

The [September 13 native report](NATIVE_PREDICTOR_FIT_2026_09_13.md)
remains as dated historical evidence: its earlier model used 75 short-corpus
runs, a 5,000-row fitted ceiling, a 125,845 VIN/s reference, and a 9,571.94-byte
width. Its reported four-worker automatic comparison describes that recorded
build and is not evidence for `ultravin-native-batch-v2`.

Earlier Rust callers selected `BatchFormat::Native` when constructing predictive
`BatchFeedback`; that combination now returns an error to prevent applying a
worker-slot plan to a shared-batch loop. The benchmark exercises that controller, including its
calibration cost and later tuning:

```sh
RAYON_NUM_THREADS=4 cargo run --locked -p ultravin --example throughput --release -- \
  target/bench/multicore-corpus.txt 10 batch full auto
```

`predict_batch_size(output="native", ...)` exposes the same model in Python.
Calling `decode_batch` directly still decodes the supplied batch as supplied.
The native `throughput ... full auto` example now calls the public
`decode_batch_managed_at` API. Its `BatchResults` owner frees large batches on
the decoder pool, and the measured interval includes that synchronous cleanup.
Earlier recorded builds used ordinary `Vec` results with caller-thread cleanup.
Native predictions and runtime tuning can explore up to the measured 16,384-row
domain when observed throughput supports larger batches; the live output-width
memory limit still takes precedence. The worker and single-core speed inputs
support other machines; hardware portability is modeled from this machine's
measurements, not yet validated on a second host.

