# Security Policy

## Supported versions

The latest release only. Fixes ship in a new release; older versions are not
backported.

## Reporting a vulnerability

Report privately through GitHub, not a public issue:

**https://github.com/blackthorn-interstellar/ultravin/security/advisories/new**

Useful in a report: affected version, what an attacker gains, and the smallest
input or steps that reproduce it. A VIN string or artifact that triggers the
bug is worth more than a description of it.

You should get an acknowledgement within a few days and a fix or an explanation
of why it is not a vulnerability once the report has been assessed. There is no
bug bounty — this is an unfunded project, and the thanks are the only reward on
offer. Credit in the advisory on request.

## Scope notes

Ultravin is an offline library: it decodes VIN strings against a data artifact
baked into the binary. It opens no sockets, spawns no processes, and reads no
files at runtime in its default configuration.

Two areas are squarely in scope. First, memory safety in the decoder given a
hostile 17-character input. Second, artifact parsing: `Db::from_bytes` is public
in a default build — no cargo feature required — so an embedder can hand it
attacker-controlled bytes. A crash, out-of-bounds read, or unsoundness reachable
through either is a vulnerability worth reporting.

The non-default `external-data` feature additionally exposes `Db::open`, whose
mmap TOCTOU behaviour is documented, accepted, and out of scope; see
[docs/SCANNER-NOTES.md](docs/SCANNER-NOTES.md) section (f) for the reasoning and
for how the validated load path works.

Everything under `scripts/` is development and benchmarking tooling. It is not
published to PyPI or crates.io and is not part of the supported surface; see
[docs/SCANNER-NOTES.md](docs/SCANNER-NOTES.md) for the findings a scanner
routinely raises there and why they are benign.
