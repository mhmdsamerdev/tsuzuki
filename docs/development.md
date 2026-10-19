# Developing Tsuzuki

Design rules and conventions are in [`AGENTS.md`](../AGENTS.md). Read it before changing behavior.

## Setup

All commands run inside `nix-shell` (pins the Rust toolchain, and linker):

```
nix-shell
```

## Build, test, lint

```
cargo build
cargo nextest run   # quiet, one summary line; shows only failures
cargo test          # fallback; also the only runner for doctests
cargo fmt -p tsuzuki --check
cargo clippy -p tsuzuki --no-deps --all-targets -- -D warnings
```

Clippy and fmt skip `vendor/anitomy` on purpose.

```
cargo nextest run <name>           # one test, by name substring
cargo nextest run --test cli       # end-to-end CLI suite
cargo nextest run --run-ignored ignored-only   # live-network tests
./target/debug/tsuzuki status   # run the dev build
```

## Sending a change

Add a line to [`CHANGELOG.md`](../CHANGELOG.md) under `[Unreleased]` for anything user-visible. Open a PR once tests, fmt and clippy pass.

## Releasing

1. Bump the version in `Cargo.toml`, then run `cargo check` to update `Cargo.lock`.
2. Move `[Unreleased]` entries in `CHANGELOG.md` under the new version.
3. Run `cargo build --workspace`, `cargo test --workspace`, fmt and clippy (above), then `cargo build --release && ./target/release/tsuzuki --version`.
4. Merge to `main`, then from an up-to-date `main`:

```
git tag -a vX.Y.Z -m "vX.Y.Z"
git push origin vX.Y.Z
```

The tag starts the release workflow. Check that it passes and the release has both Linux builds and the install script.
