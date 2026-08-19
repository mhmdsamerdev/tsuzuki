# Developing Tsuzuki

Everything you need to build, test, and contribute — kept short on
purpose. This is the "how do I get going" doc; the reasoning behind how
Tsuzuki works lives elsewhere (pointers below).

## 1. Set up the dev shell

All commands run inside `nix-shell`, not your host toolchain:

```
nix-shell
```

This pins a known-good Rust toolchain (rustup-managed) plus the linker
and libraries Tsuzuki needs. You'll see a `tsuzuki dev shell ready` line
once it's active. Re-run `nix-shell` each time you open a new terminal.

## 2. Build & test

```
cargo build
cargo test
```

The full test suite should stay green before you propose a change —
that's how regressions get caught here.

## 3. Lint & format

```
cargo fmt -p tsuzuki --check
cargo clippy -p tsuzuki --no-deps --all-targets -- -D warnings
```

`vendor/anitomy` is deliberately excluded from both — see the ground
rules below.

For the full day-to-day command list (running a single test, checking
without building, etc.), see [`COMMANDS.md`](../COMMANDS.md).

## 4. Read before you write code

Three docs, three different jobs:

- **[`README.md`](../README.md)** — the product design. What
  Tsuzuki is, why it exists, and the reasoning behind each feature.
  Read this once, for the "why build this at all" picture.
- **[`CLAUDE.md`](../CLAUDE.md)** — the living spec. Every non-obvious
  decision and *why* it's made that way, kept current on purpose. If
  you're about to change how something behaves, check here first — odds
  are the current behavior isn't an accident.

If a change you're making seems to contradict something in `CLAUDE.md`,
that's a signal to stop and think it through — not to quietly route
around it.

## 5. Ground rules

- **`vendor/anitomy` is read-only.** It's a vendored copy of a
  third-party parser, not code this project owns. Only touch it for a
  confirmed, reproduced parsing bug that's actually blocking Tsuzuki.
- **Don't guess — make the boring, explicit choice.** Tsuzuki's whole
  design leans on predictable, non-inferred behavior over anything
  clever. A change that adds heuristics, fuzzy matching, or
  auto-detection is very likely going against the grain here — see
  `CLAUDE.md`'s design principles before adding any.
- **Test against the real thing, not just internals.** Anything that
  touches a real on-disk file (`config.toml`, `state.toml`, mpv's
  `watch_later`) needs at least one test that runs the actual compiled
  binary against a real temp filesystem — a passing unit test against
  internal functions alone isn't enough.

## 6. Sending a change

Small commits, imperative mood (`fix: ...`, `feat: ...`). Open a pull
request once tests, `fmt`, and `clippy` are all clean — no extra process
beyond that.
