# CLAUDE.md

## Language

- Think / reason in English.
- Write all user-facing responses in Japanese (日本語で回答する).

## Build & test

This is a Rust workspace (ultra-low-latency remote desktop). The Ubuntu host's
system libraries (e.g. pipewire) are too old to build directly — use the Debian
bookworm dev container:

```sh
./scripts/dev-container.sh bash -c 'cargo build --workspace --target x86_64-unknown-linux-gnu'
./scripts/dev-container.sh bash -c 'cargo test  --workspace --target x86_64-unknown-linux-gnu'
```

- `crates/media-win` is Windows-only (`#[cfg(windows)]`). It compiles to an
  empty shell on Linux, so its real code is **only** verified by CI's
  `windows-latest` job — not by any local build.

## Before pushing

CI's rustfmt check runs on a Windows runner and is strict; the dev container's
rustfmt drifts from it. **Always** run this before every push and commit the
result:

```sh
./scripts/dev-container.sh bash -c 'cargo fmt --all'
```

`cargo build` / `cargo clippy` passing does **not** imply rustfmt-clean.
