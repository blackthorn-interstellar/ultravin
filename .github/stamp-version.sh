#!/usr/bin/env bash
# Stamp a release-tag version (vX.Y.Z) into the workspace Cargo.toml and Cargo.lock.
# The repo permanently carries version 0.0.0; CI runs this before building wheels.
# The version flows everywhere: maturin reads it for the wheel/sdist (pyproject's
# dynamic version), and the py crate exposes it as ultravin.__version__.
set -euo pipefail

TAG="${1:?usage: stamp-version.sh vX.Y.Z}"
[[ "$TAG" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)$ ]] || {
    echo "release tag must be vX.Y.Z, got: $TAG" >&2
    exit 1
}
V="${BASH_REMATCH[1]}"

# Workspace version (the three crates inherit it via version.workspace = true).
# No trailing $ anchors: tolerate CRLF checkouts on Windows runners.
sed -i.bak "s/^version = \"0.0.0\"/version = \"$V\"/" Cargo.toml

# Cargo.lock pins each workspace crate's version; stamp all three so --locked
# builds (e.g. from the sdist) resolve.
for crate in ultravin-core ultravin-build ultravin-py; do
    sed -i.bak -e "/^name = \"$crate\"/{" -e n -e "s/^version = \"0.0.0\"/version = \"$V\"/" -e '}' Cargo.lock
done
rm -f Cargo.toml.bak Cargo.lock.bak

grep -q "^version = \"$V\"" Cargo.toml || { echo "failed to stamp Cargo.toml" >&2; exit 1; }
for crate in ultravin-core ultravin-build ultravin-py; do
    grep -A1 "^name = \"$crate\"" Cargo.lock | grep -q "^version = \"$V\"" \
        || { echo "failed to stamp $crate in Cargo.lock" >&2; exit 1; }
done
echo "stamped version $V"
