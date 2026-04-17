# Building aros-kernel

## Native build

Requires Rust stable (≥ 1.85 — `edition = "2024"` in Cargo.toml).

```bash
cargo build --release
```

Produces `target/release/aros-kernel` (~4MB stripped, statically-linked except for libc/libsqlite-c).

Verify:

```bash
./target/release/aros-kernel --help
./target/release/aros-kernel recommend
```

## Distributable binaries

Release tarballs are built by CI for four targets:

| Target | Runner | Toolchain |
|---|---|---|
| `x86_64-unknown-linux-gnu` | `ubuntu-latest` | native |
| `aarch64-unknown-linux-gnu` | `ubuntu-latest` | `cross` (Docker) |
| `x86_64-apple-darwin` | `macos-13` | native |
| `aarch64-apple-darwin` | `macos-14` | native |

Triggered by pushing a `v*` tag (`git tag v0.1.0 && git push origin v0.1.0`) or by
manual `workflow_dispatch` from the Actions tab (dry-run mode produces artifacts
without publishing a release).

Each run uploads `aros-kernel-<target>.tar.gz` (contains the binary, `README.md`,
and `LICENSE`). Tag-triggered runs additionally publish a GitHub Release with
auto-generated release notes.

## Local cross-compile (advanced)

For ad-hoc cross-compile from macOS to Linux:

```bash
cargo install cross --locked
cross build --release --target x86_64-unknown-linux-gnu
cross build --release --target aarch64-unknown-linux-gnu
```

Requires Docker. `rusqlite = { features = ["bundled"] }` builds SQLite from C
source inside the cross-rs container, so no host C toolchain setup is needed.

## Release flow

1. Bump `version` in `Cargo.toml`, update `CHANGELOG.md` if present.
2. `git commit -am "release: v0.1.0"`
3. `git tag v0.1.0 && git push origin main v0.1.0`
4. CI builds all four targets and publishes a GitHub Release automatically.
5. Before tagging, sanity-check via `workflow_dispatch` (dry-run) to catch breakage
   without cutting a release.

## Troubleshooting

- **`error: the 'rustfmt' binary, normally provided by the 'rustfmt' component, is not applicable`** — install the target explicitly: `rustup target add <triple>`.
- **`linker cc not found` on Linux aarch64 without `cross`** — use `cross` (Docker) or install `gcc-aarch64-linux-gnu` + set `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc`.
- **macOS universal binary** — run `lipo -create -output aros-kernel target/x86_64-apple-darwin/release/aros-kernel target/aarch64-apple-darwin/release/aros-kernel` after building both Darwin targets.
