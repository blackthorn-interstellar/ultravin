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

The decoder embeds `crates/ultravin-core/data/vpic.rkyv` (~82MB) via
`include_bytes!`. That file is a gitignored build product: a pure, deterministic
function of the pinned NHTSA dump recorded in `vpic/manifest.json` (month, source
URL, dump `sha256`, and the artifact's `blake3`).

The `build-data` job rebuilds it once and every wheel/sdist job reuses it. It is
self-verifying: it downloads the pinned dump, checks the `sha256`, runs
`vpic-import`, and asserts the emitted artifact's `blake3` matches the manifest.
The sdist embeds the artifact too (`[tool.maturin] include`), so a source install
(`pip install` with no matching wheel) works fully offline.

Tagged releases also attach that exact `vpic.rkyv` to the GitHub release. That
is the supported data channel for Rust-crate users — the crate itself can't
carry 82MB (crates.io size cap), and a build without the artifact embeds an
empty placeholder that `Db::embedded()` refuses to serve at runtime. Download
the asset and either `Db::open` it (`external-data` feature) or drop it at
`crates/ultravin-core/data/vpic.rkyv` before building to bake it in; verify its
`blake3` against the tag's `vpic/manifest.json`.

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
