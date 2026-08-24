# Release Process

Versions are determined solely by git tags. The workspace `Cargo.toml`
permanently says `0.0.0`; when a `vX.Y.Z` tag is pushed, CI stamps the tag
version into `Cargo.toml`/`Cargo.lock` (`.github/stamp-version.sh`) before
building, so the wheels, the sdist, and `ultravin.__version__` all pick it up.
There is no version-bump commit.

## Make a release

```bash
git tag v0.2.0
git push origin v0.2.0
```

That's it. `.github/workflows/release.yaml` runs the Rust gate (fmt/clippy/test),
materializes the embedded data artifact, builds wheels for every platform plus
the sdist, generates build-provenance attestations, and publishes to PyPI via
trusted publishing. Nothing is published if the gate or any build fails.

A `workflow_dispatch` run builds the full wheel matrix as artifacts **without**
publishing — untagged builds carry version `0.0.0` and the release job runs only
for tags. Use it to dry-run the matrix.

## The embedded data artifact

The decoder embeds `crates/ultravin/data/vpic.rkyv` (~82MB) via
`include_bytes!`. That file is a gitignored build product: a pure, deterministic
function of the pinned NHTSA dump recorded in `vpic/manifest.json` (month, source
URL, dump `sha256`, and the artifact's `blake3`).

The `build-data` job rebuilds it once and every wheel/sdist job reuses it. It is
self-verifying: it downloads the pinned dump, checks the `sha256`, runs
`vpic-import`, and asserts the emitted artifact's `blake3` matches the manifest.
The sdist embeds the artifact too (`[tool.maturin] include`), so a source install
(`pip install` with no matching wheel) works fully offline.

Tagged releases also attach that exact `vpic.rkyv` to the GitHub release. That
is the data channel for Rust-crate users — the crate itself can't carry 82MB
(crates.io size cap), and a build without the artifact embeds an empty
placeholder (written to `OUT_DIR` by `build.rs`) that `Db::embedded()` refuses
to serve at runtime. A crate user bakes the downloaded asset in with
`ULTRAVIN_DATA=/abs/path/vpic.rkyv` at build time (build.rs fully validates it
first) or mmaps it at runtime with `Db::open` (`external-data` feature); either
way they verify its `blake3` against the tag's `vpic/manifest.json`. User-facing
instructions: `crates/ultravin/README.md` (the crate's crates.io page).

## The Rust crate

`ultravin` is published to crates.io by the release workflow's `crate` job,
after PyPI and the GitHub release (so the artifact exists before the crate that
needs it). The gate job runs `cargo package -p ultravin` first, which
verify-builds the packaged crate with the placeholder artifact and default
features — exactly what publish does. `make crate-package` is the local
equivalent.

Auth is crates.io Trusted Publishing (OIDC via `rust-lang/crates-io-auth-action`),
so no token lives in repo secrets. It has to be bootstrapped once by hand:

1. Publish the first version locally with an owner's API token:
   `./.github/stamp-version.sh vX.Y.Z && cargo publish -p ultravin --allow-dirty`
   (then `git checkout Cargo.toml Cargo.lock`).
2. On crates.io → the crate → Settings → Trusted Publishing, add
   `blackthorn-interstellar/ultravin`, workflow `release.yaml`, no environment.

From then on every tag publishes. The job skips itself when the version is
already on crates.io, so a re-run of a tag's workflow is safe.

Data bumps are automated: a daily workflow detects new NHTSA dumps, integrates
them behind parity gates, and opens a classified PR (see
[DATA_REFRESH.md](DATA_REFRESH.md)). The manual equivalent:

```bash
make refresh MONTH=2026_07   # download + import + re-freeze corpus + parity gates
```

That rewrites `vpic/` (committed schema/procs/manifest), the gitignored
artifact, and `tests/parity_corpus.json`. Commit those changes; CI rebuilds the
artifact from the new pins.

## Wheels

abi3 (`pyo3/abi3-py310`): one wheel per platform serves Python 3.10+, so there is
no per-Python matrix. The crate is pure Rust (blake3 uses its `pure` feature), so
every cross target — aarch64/armv7/s390x/ppc64le/riscv64, gnu and musl — builds
with the default cross-gcc; no zig or per-arch C toolchain wrangling.

Local builds report version `0.0.0`; only tagged CI builds carry a real version.

## One-time setup: PyPI trusted publishing

The release job authenticates with OIDC; no API token is stored anywhere.
Configure once at https://pypi.org/manage/project/ultravin/settings/publishing/:

- Owner: `blackthorn-interstellar`
- Repository: `ultravin`
- Workflow: `release.yaml`
- Environment: (leave blank)

## Local build

```bash
make build-wheel        # wheel lands in target/wheels/
pip install target/wheels/ultravin-*.whl
ultravin version
```
