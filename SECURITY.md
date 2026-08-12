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
files at runtime in its default configuration. The interesting attack surface is
therefore memory safety in the decoder given a hostile 17-character input, and
artifact parsing for embedders who enable the non-default `external-data`
feature to load their own `.rkyv` file.

Everything under `scripts/` is development and benchmarking tooling. It is not
published to PyPI or crates.io and is not part of the supported surface; see
[docs/SCANNER-NOTES.md](docs/SCANNER-NOTES.md) for the findings a scanner
routinely raises there and why they are benign.
