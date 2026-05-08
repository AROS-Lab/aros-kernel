aros-kernel local distribution artifacts
=========================================

This directory holds locally-produced, ad-hoc distributable binaries.
The canonical, signed release artifacts come from the CI release workflow
(.github/workflows/release.yml) on tag push — those are the ones to
prefer for downstream consumers.

The binaries here are committed (gitignore notwithstanding) so that
they can be handed to other machines without rebuilding, mirroring the
pattern set by the existing darwin binary.

Targets present
---------------

  aros-kernel-v0.1.0-aarch64-apple-darwin
    Built natively on the host Mac mini (M-series).

  aros-kernel-v0.1.0-x86_64-unknown-linux-gnu
    Cross-compiled via Docker (rust:1-slim-bookworm, --platform linux/amd64).
    Verified --help executes inside Docker amd64.

  aros-kernel-v0.1.0-aarch64-unknown-linux-gnu
    Cross-compiled via Docker (rust:1-slim-bookworm, --platform linux/arm64).
    Build verified; runtime smoke-test deferred (no aarch64 Linux host).

All binaries are stripped. SHA256 sums in SHA256SUMS — verify with:

    shasum -a 256 -c SHA256SUMS    # macOS
    sha256sum -c SHA256SUMS         # Linux

Linux dynamic dependencies: glibc + libc — runs on Debian 12 / Ubuntu 22.04+
out of the box. (rusqlite is bundled, so no system libsqlite3 is needed.)

This file and the binaries here are not part of a tagged release — no
git tag, no GitHub Release, no Cargo.toml version bump has been issued.
Tagging is reserved for Eddie's manual decision per Nirmana governance.
