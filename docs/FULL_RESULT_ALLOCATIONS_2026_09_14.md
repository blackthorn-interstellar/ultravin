# Full-result allocation reduction — September 14, 2026

Full native results now preserve borrowed database text for element `value`,
`attribute_id`, and `keys`, using `Cow<str>` like the existing `source` field.
Computed values remain owned, and scrubbing still allocates when necessary.
This removes string copies during projection and the corresponding serial frees
when the caller drops a batch. It benefits the normal library path; cleanup
remains in the benchmark timer.

For the canonical VIN `1HGCM82633A004352`, 44 output elements retain borrowed
text for 34 values, 34 attribute IDs, and 43 keys: 111 formerly owned string
buffers avoided for this example. The saving varies by VIN.

## Paired measurement

Apple M2 Max, 12 workers, automatic batching, the same five million unique VINs
and frozen clock. Three fresh-process pairs alternate old/new execution order.
Each trial warms a complete pass, then times a complete pass including
calibration, full-result construction, and cleanup. No CPU load was added.

| Build | Median VIN/s | Range VIN/s | Median time for five million | Median peak RSS |
|---|---:|---:|---:|---:|
| Before | 271,656 | 270,732–277,394 | 18.41 s | 2,551.0 MiB |
| Borrowed full-result text | 461,565 | 449,709–462,649 | 10.83 s | 2,555.5 MiB |

**Throughput increased 69.9%.** The fastest pass took 10.81 seconds, satisfying
the ten-second unique-input requirement. Peak process RSS was essentially
unchanged in this test; it includes the input corpus, decoder caches, and
allocator-retained memory as well as live results.

[Raw samples, input manifest, batch histories, and executable hashes](../scripts/bench/allocation_12cores_2026_09_14.json).

## Compatibility and validation

Full-result fields, values, ordering, provenance, and serialization are unchanged.
Python output formats are unchanged. For Rust source consumers, the three fields
change from `String` to `Cow<str>`: use `.as_ref()` for text, `.into_owned()` when
an owned `String` is required, and `.into()` when constructing fields. Borrows
are tied to the backing `Db`; embedded data has a static lifetime. `FlatResult`
retains its existing owned strings.

`make checku` passed, including 918 Python tests and all Rust checks. Tests cover
full JSON parity, borrowed fields, computed owned values, external database
lifetimes, and unchanged flat conversion. All three throughput-example memory
tests passed, including exclusion of borrowed text from owned-buffer estimates.

## Reproduce

Save locked release builds before and after the change as distinct executables:

```sh
uv run --frozen python -m scripts.bench.contention \
  --before-binary target/bench/allocation-before-throughput \
  --new-binary target/bench/allocation-after-throughput \
  --burners 0 --rounds 3 --workers 12 \
  --output target/bench/allocation-paired.json
```

The runner verifies the corpus and executable hashes and enforces the minimum
unique-input duration. The README four-core chart remains its separately dated
measurement; this report measures the all-core full-native path.

The subsequent [core-count sweep](CORE_SCALING_2026_09_14.md) measures the
optimized build at 1, 2, 4, 8, and 12 workers, using automatic batching throughout.
