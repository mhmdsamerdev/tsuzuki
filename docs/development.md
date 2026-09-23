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

To use the freshly built CLI without installing it, replace `tsuzuki` in
any command with `./target/debug/tsuzuki`. For example:

```
./target/debug/tsuzuki status
```

## 3. Lint & format

```
cargo fmt -p tsuzuki --check
cargo clippy -p tsuzuki --no-deps --all-targets -- -D warnings
```

`vendor/anitomy` is deliberately excluded from both — see the ground
rules below.

Running a single test, checking without building, and other day-to-day
shortcuts:

```
cargo test <test_name>          # run one test by (substring of) name
cargo check -p tsuzuki           # type-check without building
cargo test --test cli            # just the end-to-end CLI suite
```

## 4. Command reference

Every command Tsuzuki has, what it does, and the flags that matter. If
you're adding or changing a command, this table (and CLAUDE.md's "CLI
bare-invocation contract") is what needs to stay accurate alongside it.

| Command | Purpose |
|---|---|
| `tsuzuki` | Resume the current series. Single `in_progress` series → resumes directly. Multiple → interactive picker (arrow-key select) if a TTY is available, otherwise lists choices with an index and exits without resuming. None → falls through to `start` behavior. |
| `tsuzuki resume [series] [--root <root>]` | Explicit alias for the above. Accepts an optional explicit series name (with `--root` to disambiguate a name shared across composable roots) to bypass the picker and complete non-interactively — always usable from a script, never dependent on a TTY. |
| `tsuzuki status` | Show what's currently `in_progress` — every such series, its progress, and (if more than one) a note that running bare `tsuzuki` will prompt among them. |
| `tsuzuki list` | List all tracked series and their progress/status — grouped `in_progress` → `paused` → `dropped` → never-started, alphabetical within each group. |
| `tsuzuki add <path>` | Register a tracked root. |
| `tsuzuki start [series] [--root <root>]` | List series outside the automatic picker — never-started, dropped/completed (shown as "Rewatch?"), and paused (resume at saved position). Same explicit-argument/interactive-picker behavior as `resume`. |
| `tsuzuki pause <series> [--root <root>]` | Remove from the resume picker, keep saved position. Errors (does not silently create a record) if the target has no real progress to pause — never started, or already dropped. |
| `tsuzuki drop <series> [--root <root>]` | Reset progress; series reappears in `start` as if new (labeled "Rewatch?" either way). Same no-record-to-act-on error as `pause`. |
| `tsuzuki exclude [path]` | Exclude a file or directory from ordering. Bare invocation surfaces Anitomy-suggested candidates for confirmation. |
| `tsuzuki sync login` | One-time MAL OAuth authorization. |
| `tsuzuki sync link <series> --mal-id <id> [--root <root>]` | Manually resolve a series-to-MAL-ID mapping when exact-title matching fails. The given id is verified against the user's own MAL list before being saved. |
| `tsuzuki sync` | Run the two-way, non-destructive sync against MAL. |
| `tsuzuki config` | Show the resolved config (real file path, defaults filled in) and a hint pointing at `edit`. |
| `tsuzuki config edit` | Open config.toml in `$EDITOR`, blocking until it exits, then re-validate it. |
| `tsuzuki doctor [--fix] [--forget-orphans]` | Environment health checks — mpv-launch resolvability, the mpv.conf config flag, watch_later reachability, Flatpak permissions, the tsuzuki.lua companion script's install status, tracked-root validity, orphaned per-series `state.toml` records, and MAL auth status. `--fix` applies every currently-safe, non-destructive fix (installing/updating tsuzuki.lua) — never edits mpv.conf. `--forget-orphans` (requires `--fix`) permanently clears every currently-confirmed orphaned record in one pass, printing exactly what it deletes first. Exits non-zero only if at least one check fails. |

**Bare invocation of a subcommand group is never an accident.** `config`
and `sync` both have a real default action reachable without a
subcommand — `tsuzuki sync` bare runs the sync, `tsuzuki config` bare
shows the resolved config — rather than clap's generic "subcommand
required" error. Malformed input (bad flags, unknown subcommands, missing
required args) still uses clap's own error output.

## 5. Read before you write code

Two docs, two different jobs:

- **[`README.md`](../README.md)** — the product pitch. What
  Tsuzuki is, why it exists, and the reasoning behind each feature.
  Read this once, for the "why build this at all" picture.
- **[`CLAUDE.md`](../CLAUDE.md)** — the living spec. Every non-obvious
  decision and *why* it's made that way, kept current on purpose. If
  you're about to change how something behaves, check here first — odds
  are the current behavior isn't an accident.

If a change you're making seems to contradict something in `CLAUDE.md`,
that's a signal to stop and think it through — not to quietly route
around it.

## 6. Ground rules

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

## 7. Sending a change

Small commits, imperative mood (`fix: ...`, `feat: ...`). Add a line to
[`CHANGELOG.md`](../CHANGELOG.md)'s `[Unreleased]` section for anything
user-visible. Open a pull request once tests, `fmt`, and `clippy` are all
clean — no extra process beyond that.
