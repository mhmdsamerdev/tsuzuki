**Editing this file: KEEP IT SHORT. No logs, no diff notes, no change history. It only tells a new AI session how I work and what this project is and its key architecture. Don't update it unless I ask.**

# Tsuzuki

Local-first Rust CLI (続き). It remembers what you were watching in mpv (local files, and ani-cli streams) and resumes it with `tsuzuki`.
Not a player, library manager, or MAL replacement. Maintenance mode: fixes, small enhancements, dependency upkeep. Product pitch: `README.md`. Commands and dev setup: `docs/development.md`.
`README.md` is for general users: don't document niche setups there (custom mpv, Snap).

## How to work
- Read this file fully before touching code.
- Check a change against "Hard invariants" first. Most "obvious improvements" here are the heuristics this project rejects.
- If behavior is unspecified, stop and ask. Don't guess.
- Gate before proposing a change: tests pass, `cargo fmt -p tsuzuki --check`, and `cargo clippy -p tsuzuki --no-deps --all-targets -- -D warnings` are clean.
- Everything runs inside `nix-shell`. Missing tooling goes in `shell.nix`, never on the host.
- Don't add dependencies without asking.

## Hard invariants
- Never modify or rename user files.
- For local files the filesystem is the library. Never add labels, tags, or a library database; the folder layout on disk is the only source of what exists and how it's grouped. ani-cli shows are the one exception (see "ani-cli source").
- Never guess. Every tracked or excluded path is explicit.
- Tsuzuki doesn't launch mpv itself or hold an IPC socket to observe playback.
- MAL sync only ever writes a higher episode number, never a lower one, even on retries or partial failures.
- For local files, natural sort is the only authority for episode order. Anitomy only labels identity for MAL sync.
- Absence of an mpv-script log entry proves nothing. Only a real position or a `Completed` reading moves state.

## Engineering choices
- Local-first. No network at startup; only `sync` and `update` touch it, through `crate::http`. No async runtime.
- State is one flat TOML, separate from the hand-edited `config.toml`. Every state write goes through the single atomic temp-file-then-rename helper. `config.toml` is a hand-edited file written only by `add`/`exclude` with a plain write.
- mpv is read-only: the only input is the `contrib/mpv/tsuzuki.lua` script's log (required; freshest entry per file wins). No IPC. Resuming spawns mpv fire-and-forget.
- One resolver (`mpv::resolve`, from `[mpv] install`) picks the mpv target. Launch, the script check, `doctor` and `--fix` use only it; nothing else detects mpv. Log reading deliberately merges every known log dir (native, Flatpak) regardless of target.
- The mpv script is enforced only by `tsuzuki`/`start` (refuse to launch when the target's script file is missing) and `doctor`. No runtime warnings anywhere else. Snap isn't supported (its paths can't be known); Snap users use `custom`.
- Commands must feel instant, and their cost must not grow with history or library size. Do each filesystem lookup or scan once per command and reuse it. Don't trade correctness or the invariants above for speed, and don't add caches or dependencies for it.
- Pure Rust only, no C/C++ dependencies. `vendor/anitomy` is read-only (edit only for a reproduced, blocking parsing bug). Distribution is prebuilt binaries via cargo-dist only.

## Architecture
- A series is the depth-1 directory under a tracked root. Its name is the directory name verbatim. Odd layouts become their own root.
- Thresholds are flat seconds (`resume_seconds`, `completion_remaining_seconds`), never percentages. Every tunable has a config field.
- A moved or deleted series folder is a new series. The old record stays inert until `doctor --fix --forget-orphans`. No rename detection.
- `Completed` is set only by reconcile when the final episode finishes; no command sets it, `pause`/`drop` reject it. Paused and dropped series are excluded from sync. `pause`/`drop` never zero the stored position.
- Every state-touching command reconciles with mpv through the one shared function, then acts.
- Series-ness is decided only by `series::is_series_dir`. No ad-hoc `join(..).is_dir()` for series.
- Symlinks are followed everywhere, with cycle protection. Roots are stored as the user typed them.
- `mal::DEFAULT_CLIENT_ID` is public on purpose (PKCE, no secret). Never redact or remove it.

## ani-cli source
- Opt-in: `tsuzuki add ani-cli` enables `[ani_cli]` in config. Before that nothing is tracked, read or checked.
- Tsuzuki never starts, lists or imports an ani-cli show. A record exists only because an mpv-script log entry (stream URL, title ending ` Episode <int>`) exactly matched a title in ani-cli's history. History is read-only and never evidence of watching.
- Series = show title under the reserved root `ani-cli://`. The episode number comes from the logged title: no ordering, no Anitomy.
- Resume runs ani-cli in the foreground via its search path (`-e`, never `-c`), with `ANI_CLI_PLAYER` from `mpv::resolve` and `--start` via `ANI_CLI_PLAYER_FLAGS`. Unfinished: same episode and position. Finished: the next episode. Tsuzuki never writes ani-cli's history and makes no network requests itself.
- `Completed` only when the finished episode equals the MAL `total_episodes` saved by `sync`. Unknown or 0 never completes. A `Completed` show isn't offered for resume; new playback reactivates it, as for folders.
- A record whose title leaves ani-cli's history is inert until `doctor --fix --forget-orphans`.
- Only mpv is tracked. Other players are out of scope.

## Code style
- Match the surrounding code: naming, idiom, comment density.
- Errors are plain messages and a non-zero exit, never panics. Warnings go to stderr, real output to stdout.
- Output symbols: `✓` ok, `⚠` needs attention, `✗` failed. One line per item; show changes and required actions, not unchanged items.

## Testing rules
Every test guards a named invariant. Rules are checkable, not taste.

**Writing**
- Expected values are hand-written from the spec. Never compute them by calling the code under test or re-implementing it in the test.
- Bug fix: write the failing test first, run it against the unfixed code, and confirm it fails before fixing.
- Behavior change: update or delete every test that covered the old behavior in the same change. A test must fail if the behavior is reverted.
- Name: `{subject}_{scenario}_{expected}`, e.g. `series_walk_orders_season_dir_files_before_loose_ova`. Never `test_x` or `it_works`.
- Assert whole values (`SeriesRecord`, classification enums, rendered lines), not single fields. `assert!(x.is_ok())` or `is_some()` alone is banned; unwrap or match and compare the payload.
- Each non-trivial function gets a happy, an error, and an edge case. No test-count quotas.
- Never infer a status from incidental fields (position, mtime). Tests build records through the real command or reconcile at least once, never only by hand-setting `status`.
- Every predicate on a status or threshold is tested at each status and at 0, below, at and above the boundary, plus the excluded cases. Vary fields that must be irrelevant and assert the decision is unchanged. Status tables use an exhaustive `match` (no `_`).
- Fixtures vary layout, not just values: for anything touching roots or series, include nested roots, a root that is also a child of another root, symlinked roots, and same-named sibling roots.
- Unit tests stay inline (`#[cfg(test)] mod tests`). Integration tests go in `tests/`.

**Isolation**
- Filesystem work uses `tempfile` only. Never the real `$HOME`, real mpv, or the network.
- Never `std::env::set_var` in-process. Integration tests pass `HOME`/`XDG_*` to the child process.
- No `sleep` as synchronization. Set mtimes explicitly with `File::set_modified`. Poll with a deadline only when waiting on a child process.
- Compare collections order-independently or use `BTreeMap`. Never `HashMap` in assertions.
- Live-network tests are `#[ignore = "reason"]`.

**Real binary**
- Anything that reads or writes `config.toml`, `state.toml` or `mpv-log` needs at least one test that runs the real binary (`env!("CARGO_BIN_EXE_tsuzuki")`) against a temp `HOME`/XDG.

**Invariants that must always have a test** (add one if missing)
- Topology: a directory equal to a tracked root (canonical path) is never a series, at every site that decides series-ness (pickers, positional/`--root` resolution, pause/drop, sync, reconcile, exclude).
- Ordering: directories before a level's loose files; numbered natural sort; exclusions applied before sort; symlink cycle terminates.
- Script-log parsing: redirect entries skipped; malformed entries skipped without aborting; a missing or outdated mpv script reported by `doctor`.
- mpv target: every `install` value; auto order (native, then Flatpak) for each combination present; pinned-but-missing errors, never falls back; `/snap/bin` mpv is not native. `tsuzuki`/`start` block on a missing target script and never spawn mpv; other commands stay silent; doctor checks and fixes only the target; logs merge regardless of target.
- Thresholds: a position below `resume_seconds` never creates or overwrites state; `classify_entry()` position check wins over completion on short files; script `eof` is `Completed`; absence alone never moves state.
- MAL: never write a lower episode; paused and dropped excluded; push count is the watched count (cursor − 1, or the cursor for `Completed`); only `Completed` marks a MAL entry finished; status moves forward only.
- `exclude`/`pause`/`drop`: legal transitions only; no-record error; idempotent repeat; absolute exclude path rejected; `pause`/`drop` keep the stored position.
- State: write is atomic (no partial file); unchanged script-log entries don't bump `updated_at`.

**Verification**
- Run `cargo test`, then `cargo mutants --in-diff <diff-of-change>`. Report surviving mutants and either add tests that kill them or say why each is equivalent. Don't claim "done" without that report.
- Grep every site that decides the same concept as the change, and confirm each is tested against the Hard invariants.

## Code comments
- Short, clear, focused; explain only the code they sit on. No diff/change notes ("added", "now", "was").
- If you meet an existing comment that breaks this, fix it without asking.

## Git
- After any change, suggest a commit message.
- Only commit when asked. Messages short, focused, conventional-style (`feat:`, `fix:`, `docs:`), never mention AI or add co-author lines.
- NEVER `git tag`. Tags trigger GitHub/PyPI releases; the user does that manually.

## CHANGELOG.md
Every user-visible change gets one very short bullet under `## [Unreleased]`.
