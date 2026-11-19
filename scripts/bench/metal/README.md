# Full VIN decoding on the M2 Max GPU

This branch experiment runs the complete Rust `decode_json_at` function on Metal. The GPU receives the input VIN, optional caller year and clock, selects the vPIC rules, resolves competing results, applies corrections, and emits the complete JSON bytes. Every result contains the ordered element records and their provenance.

The CPU control uses the release build of ultravin's parallel `decode_batch_json_at`. The hybrid job gives disjoint VINs to the CPU pool and GPU concurrently, then writes their results in input order. The runner scans the inputs once to size the GPU buffer. Each timed job reopens the same input and writes a JSON array. Times include input packing, dispatch and readback for GPU rows, plus writing and closing the output with the normal OS page cache. GPU initialization is timed separately. Lazy shader compilation is charged to the first job that requests each pipeline. The runner checks SHA-256 across modes and trials after stopping the timer.

The published 6.91M VIN/s benchmark writes two compact Parquet files and reuses clean results across VINs. The timings here apply to full JSON. [Headline benchmark](../../../docs/BENCHMARKS.md#headline-disk-to-disk-november-14-2026-commit-5ab93e6)

## Measured on an Apple M2 Max

Eight CPU workers and the 30-core GPU decoded separate VINs concurrently. On three runs of 65,536 rows, assigning 1,024 rows to Metal changed the median from 2.661 s to 2.639 s. The 0.9% difference was smaller than the run-to-run variation. Every mode wrote the same 912,334,465 bytes.

| Full-JSON workload | CPU only | Metal only, interpreter | CPU + Metal | GPU share in combined job |
| --- | ---: | ---: | ---: | ---: |
| 1,024 distinct NY Honda VINs, one run | 0.0253 s | 1.436 s | 0.993 s | 512 / 1,024 |
| Same 1,024 VINs repeated 64 times, median of three | 2.661 s | 91.615 s | 2.639 s | 1,024 / 65,536 |
| 16 VINs sampled across sorted NY registrations, one run | 0.0863 s | 134.497 s | 118.303 s | 8 / 16 |

The large run reached 24,626 VIN/s on CPU, 715 VIN/s on Metal and 24,837 VIN/s combined. CPU-only times ranged from 2.659 to 5.383 s, and combined times ranged from 2.625 to 3.018 s. Each job read its source file and closed its output file. GPU startup took about 60 s before timing. The Honda cohort shares a warmed manufacturer and model year. VINs that need other indexes can spend much longer building those indexes in each GPU lane.

The separate `--compiled` trial used 64 Honda VINs of the same model and year. GPU setup took 61 s. We stopped the first GPU-only job after it spent more than 15 minutes compiling and executing shader pipelines without returning a batch. A throughput figure for that run is unavailable. A one-row check on an empty VIN produced 2,496 matching bytes on compiled shaders in 175.920 s across 1,157 dispatches.

The committed measurements include the repository benchmark VINs, separate samples of shuffled NY and synthetic VINs, and edge cases covering missing or explicit years, Unicode, partial VINs and invalid check digits. The early single-run smoke and field measurements assigned Metal the first half of each block. The large run and current runner assign it the final slice. This choice affects timings on mixed inputs. Each mode produced the same SHA-256 on every completed workload. [Trial times and output hashes](results-m2-max.json)

## How it runs

The same small Rust guest builds as a native library and a WebAssembly module with the pinned vPIC archive embedded. A hardware Metal adapter runs the GPU guest through wasmgpu and wgpu. The interpreter executes the guest instructions inside a compute shader. The `--compiled` option lowers basic blocks to GPU shader instructions. In that mode the host schedules a new pipeline when execution crosses into a different compilation unit and checks the GPU's interpreted and compiled instruction counters.

Startup initializes the decoder's lazy database indexes with one fixed Honda VIN on the GPU and clears the output. Each lane starts with that shared image and computes its result from scratch. Copy-on-write 4 KiB pages hold its stack, new indexes and mutable data. Lanes can share the 80 MiB vPIC archive. A batch that runs out of private pages splits into smaller GPU jobs. Additional indexes built by a lane live for that batch. This favors inputs that use the already-initialized schemas.

A 1,024-VIN dispatch submits 32 threadgroups of 32. Metal controls their placement on the chip's 30 GPU cores. The experiment measures completed records and wall time. [wasmgpu](https://pypi.org/project/wasmgpu/0.0.2/) · [Apple's Metal compute guide](https://developer.apple.com/documentation/metal/performing-calculations-on-a-gpu)

## Reproduce

Use an Apple Silicon Mac with a visible Metal device, the repo's Rust toolchain and the matching `crates/ultravin/data/vpic.rkyv` file or `ULTRAVIN_DATA` path. Run from the repository root.

```sh
rustup target add wasm32-unknown-unknown --toolchain 1.99.1
cargo build --locked --release -p ultravin --no-default-features --example metal_guest \
  --target aarch64-apple-darwin --target wasm32-unknown-unknown
uv run --no-project --script scripts/bench/metal/bench.py \
  crates/ultravin/benches/vins.txt --limit 32 --lanes 32
```

For the NY and synthetic samples, first generate the existing headline corpora under `target/bench`. The sampler selects sixteen evenly spaced VINs from each full file and a separate group of 1,024 real Honda registrations. The larger test repeats that group 64 times. [NY corpus setup](../../../docs/BENCHMARKS.md#real-fleet-new-york-registrations-november-12-2026-commit-a758e6d) · [Synthetic corpus setup](../../../docs/BENCHMARKS.md#headline-throughput-september-25-2026-commit-56cf2e9)

```sh
uv run --frozen -- python scripts/bench/metal/samples.py
uv run --no-project --script scripts/bench/metal/bench.py \
  target/metal-inputs/ny-honda-1024.txt target/metal-inputs/ny-sorted-16.txt \
  target/metal-inputs/ny-shuffled-16.txt target/metal-inputs/synthetic-16.txt \
  target/metal-inputs/edge.jsonl --limit 0 --lanes 1024 --out target/metal-field
uv run --no-project --script scripts/bench/metal/bench.py \
  target/metal-inputs/ny-honda-repeated-65536.txt --limit 0 --lanes 1024 \
  --hybrid-gpu-rows 1024 --block-rows 65536 --rounds 3 --out target/metal-balanced
uv run --no-project --script scripts/bench/metal/bench.py \
  target/metal-inputs/ny-honda-compiled.txt --compiled --limit 0 --lanes 64 --gpu-timeout 900 \
  --out target/metal-compiled
```

Text files contain one VIN per line. A `.jsonl` input contains one `[vin, year]` pair per line, with `null` for an unspecified year. The output directory holds each mode's JSON and `timings.json`, including the CPU and GPU row counts. The CPU pool defaults to eight workers. The optional `--gpu-timeout` stops a batch at a dispatch boundary, journals its progress and exits with code 3. The sampler includes `empty-vin.txt` for a single-row compiled check.
