# ZIP dependency repair — 2026-10-16

The October 16 Nightly failure was Cargo failing to resolve ZIP 2 after all
matching releases were reported yanked. Migrate `ultravin-build` to ZIP 9.0.4,
with default features disabled and only `deflate` enabled, and regenerate the
workspace lockfile. Upstream's [support policy](https://github.com/zip-rs/zip2/blob/v9.0.4/SECURITY.md)
supports only the latest release. Older releases appearing unyanked again are
not supported alternatives. ZIP 9.0.4 requires Rust 1.88 or newer.

## Release-age exception

Admit **zip 9.0.4 only**, published on 2026-10-16 at 09:09:57 UTC, inside the
seven-day cooldown to restore fresh resolution on a supported upstream
release. This deliberate lockfile migration does not change the nightly
cooldown algorithm or its seven-day setting. No security checks, yanked-crate
denials, or advisory checks are relaxed.

All new transitive dependencies pass the seven-day window: `cfg_aliases`
0.2.2 (July 16), `nix` 0.31.3 (May 11), `typed-path` 0.12.3 (February 11),
and `zlib-rs` 0.6.8 (September 15). The latter is ZIP's current deflate backend;
the importer still accepts the same stored and deflated formats.

## Compatibility and PR #101

ZIP 9 filename decoding can fail. The importer propagates that error before
writing output. It still reads the first archive entry, keeps its decoded
logical filename in the manifest, reads stored and deflated SQL, and propagates
CRC/read errors. Regression tests compare archive imports with the same plain
SQL, including the resulting artifact bytes, and reject invalid/empty archives.

[PR #101](https://github.com/blackthorn-interstellar/ultravin/pull/101) contains
Python dependency updates and separate rustls security findings. The ZIP
migration does not address those advisories. Refresh that PR from master after
the fixes land so its nightly branch retains this migration; do not suppress
or dismiss its security failures on the strength of a successful ZIP build.

The preceding, separate rustls commit updates 0.23.45 to 0.23.46 to address
RUSTSEC-2026-0346, -0347, -0348, and -0349. That security fix has its own
one-time exception for **rustls 0.23.46**, published October 14 at 09:54:15 UTC.
The scanners must pass on the combined branch; the ZIP change is not evidence
that any TLS advisory is harmless.
