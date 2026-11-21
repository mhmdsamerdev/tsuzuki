# Contributing to Tsuzuki

Tsuzuki is small and in maintenance mode. Bug fixes, small improvements and
dependency upkeep are welcome.

## Before you start

- For a new feature or a change in behavior, open an issue first.
- The design is strict on purpose. Read the "Hard invariants" in
  [`AGENTS.md`](AGENTS.md) before changing behavior.
- Don't add dependencies without discussing it in an issue.

## Reporting a bug

Include:

- `tsuzuki --version`
- the output of `tsuzuki doctor`
- `mpv-launch.log`, if mpv failed to start. It sits next to `state.toml`
  (`~/.local/share/tsuzuki/` by default) and is overwritten on each launch.

## Setup

Linux only. Clone the repo and run `nix-shell`, which pins the toolchain and
linker. A recent stable Rust works too.

## Before you open a PR

These are the same checks CI runs:

```
cargo test --workspace
cargo fmt -p tsuzuki --check
cargo clippy -p tsuzuki --no-deps --all-targets -- -D warnings
```

`vendor/anitomy` is read-only third-party code, so fmt and clippy skip it.

Handy extras:

```
cargo nextest run <name>    # one test by name (nix-shell only)
cargo test --release --test perf perf_warm -- --ignored --nocapture
```

Run the perf benchmark if you touched reconcile, overview or script_log.

## Tests

- A bug fix comes with a test that fails without the fix.
- Test through the real binary in `tests/` when a change touches
  `config.toml`, `state.toml` or the mpv log.
- Tests use temp directories only: never your real `$HOME`, real mpv or the
  network. The full rules are in [`AGENTS.md`](AGENTS.md).

## Sending a PR

- Keep it small and focused.
- Use short commit messages like `fix: ...`, `feat: ...` or `docs: ...`.
- For anything users will notice, add one short bullet under `## [Unreleased]`
  in [`CHANGELOG.md`](CHANGELOG.md).
- Contributions are licensed under GPLv3, like the rest of the project.

## Maintainers: releasing

1. Bump the version in `Cargo.toml`, then run `cargo check` to update
   `Cargo.lock`.
2. Move the `[Unreleased]` entries in `CHANGELOG.md` under the new version.
3. Run build, test, fmt and clippy, then
   `cargo build --release && ./target/release/tsuzuki --version`.
4. Merge to `main`, then from an up-to-date `main`:

   ```
   git tag -a vX.Y.Z -m "vX.Y.Z"
   git push origin vX.Y.Z
   ```

   The tag starts the release workflow. Check that it passes and the release
   has both Linux builds and the install script.
