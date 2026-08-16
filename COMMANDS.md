# COMMANDS.md — day-to-day commands for Tsuzuki

Quick reference, not a spec. If a command's behavior needs explaining
*why*, that belongs in `CLAUDE.md` instead — this file is just "what do I
type."

## Entering the dev environment

Every command below assumes you're inside the Nix shell. Run this first,
every session, from the repo root:

```bash
nix-shell
```

You'll see a `tsuzuki dev shell ready — rustc x.x.x` line when it's set up
correctly. Leaving the shell (`exit`, or closing the terminal) drops the
toolchain from `PATH` — that's expected, just re-enter next time.

Optional quality-of-life: install `direnv`, add a `.envrc` containing
`use nix`, and the shell auto-activates on `cd` into the repo — no need
to type `nix-shell` manually.

## Building & running

```bash
cargo build              # debug build
cargo build --release    # optimized build
cargo run -- <args>      # e.g. cargo run -- add ~/Anime
cargo run --release -- <args>
```

## Testing

```bash
cargo test                     # run the full suite
cargo test <name>              # run tests matching a substring
cargo test -- --nocapture      # show println!/dbg! output during tests
cargo test --test <file_stem>  # run only one integration test file (tests/<file>.rs)
```

Given the strict phase-gate rule in `CLAUDE.md`, `cargo test` should be
green before moving to the next phase — not just the tests for the new
phase, the whole suite.

## Linting & formatting

```bash
cargo fmt                # auto-format
cargo fmt --check        # check formatting without modifying (CI-style)
cargo clippy             # lint
cargo clippy -- -D warnings   # treat warnings as errors (use before wrapping up a phase)
```

## Checking without building (fast feedback loop)

```bash
cargo check
```

## Dependency management

```bash
cargo add <crate>              # add a dependency
cargo add <crate> --dev        # add a dev-only dependency (e.g. tempfile, assert_cmd)
cargo update                   # update lockfile within semver constraints
cargo tree                     # inspect the dependency graph
```

## Working with a fabricated filesystem in tests

Phases 1–3 (ordering, exclusions, Anitomy) use `tempfile` to build fake
directory trees rather than touching real files. No special commands
here — it's just Rust code inside `#[test]` functions — but if a test
needs a scratch directory, prefer `tempfile::tempdir()` over anything
under the repo itself, so `cargo test` stays hermetic and parallel-safe.

## End-to-end tests against the compiled binary (Phase 8+)

```bash
cargo test --test cli    # assert_cmd-based tests, once added under tests/cli.rs
```

## Distribution (Phase 8 only)

```bash
cargo dist init     # one-time: generate cargo-dist config
cargo dist build    # build release artifacts locally
cargo dist plan     # preview what a release would produce
```

## Git

Commits are made by the user, not by Claude Code. At the end of a unit of
work, Claude proposes a single conventional commit message; review the
diff yourself before committing:

```bash
git status
git diff
git add -A
git commit -m "<proposed message>"
```
