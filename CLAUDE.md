# CLAUDE.md — Tsuzuki

Read this in full at the start of every session before touching code. It is
the authoritative spec. If something here conflicts with what you remember
from an earlier session, this file wins — it's kept current on purpose.

Companion file: **`PROGRESS.md`** tracks which phase we're on and what's
actually done. This file (`CLAUDE.md`) never contains status updates —
only decisions and the reasoning that protects them. Check `PROGRESS.md`
first to know where to resume; check `CLAUDE.md` to know how to build it.

---

## 1. What this is

**Tsuzuki** ("continuation," 続き) is a local-first Rust CLI that remembers
what the user was watching in mpv and resumes it with one command:
`tsuzuki`. It is not a media player, library manager, or MAL replacement —
it answers exactly one question: *what was I watching, where did I stop,
and can you take me back there instantly?*

Full product rationale lives in `docs/design.md` (the original design doc).
This file only holds what changes how you should write code — read
`docs/design.md` once at project start for the "why we're building this at
all" narrative; come back here for every implementation decision.

---

## 2. Non-negotiable design principles

These are load-bearing. Don't "improve" around them without flagging it to
the user first — they are the reason the tool stays simple and predictable.

- **Never infer, always require an explicit action.** No content
  classification, no fuzzy matching decides anything automatically, no
  heuristic guesses which files are episodes. If you find yourself writing
  logic that guesses user intent, stop — that's very likely the wrong
  design for this project specifically.
- **Fixed thresholds, not percentages.** Resume/completion cutoffs are
  flat seconds, not `%` of runtime. Don't refactor toward percentage-based
  logic even if it looks "smarter" — different episode lengths would make
  it inconsistent, which is exactly what the flat-threshold rule avoids.
- **Natural sort is the sole authority for episode order**, always,
  everywhere. Anitomy-parsed episode numbers annotate identity for sync —
  they never reorder anything. If a bug looks like "wrong episode order,"
  the fix belongs in the sort, not in giving Anitomy a say over ordering.
- **Two-way MAL sync only ever takes the higher episode number.** Never
  write a smaller number to MAL, in either direction. A push-only or
  overwrite-down sync silently destroys progress the user made elsewhere —
  this has to hold even in edge cases (partial fetch failures, retries).

---

## 3. Architecture decisions (with reasoning)

### Series identity = depth-1 directory under a tracked root
A series is *always* the immediate child of a tracked root — never
detected by content, never configurable per-series. Season folders,
release-tag folders, anything below depth 1 is opaque to Tsuzuki as a
category and only feeds the natural-sort tree. If a folder layout doesn't
fit (e.g. a `Rewatches/` layer), the answer is registering that layer as
its own root (`tsuzuki add ~/Anime/Rewatches`) — not adding a
depth-detection flag. Composable roots is the *general* escape hatch;
don't add a special case for one folder shape when this already covers it.

### State vs. config are two separate files
`~/.config/tsuzuki/config.toml` (human-tunable, defaults, comments) and
`~/.local/share/tsuzuki/state.toml` (machine-written, atomic). Don't merge
them — config is meant to be hand-edited and versioned in dotfiles repos;
state is not.

### State file: single flat TOML, atomic write, not SQLite, not sharded
Tsuzuki's state is small and bounded (dozens of series, not thousands of
files) — full-parse-on-every-invocation is cheap, so there's no case for
SQLite's complexity or mpv's per-file sharding. **Every write must be
temp-file-then-rename**, never an in-place write — a kill mid-write must
never leave a half-written state file. Any new state-writing code path
must go through the same atomic-write helper; don't add a second write
path that writes in place "just this once."

### mpv `watch_later` reading, not IPC or launching mpv
Tsuzuki never launches mpv or holds an IPC socket. It only reads mpv's own
`watch_later` files after the fact. This means:
- **`write-filename-in-watch-later-config=yes` is a hard prerequisite** —
  without it, entries are only MD5 hashes of the path, which are
  one-way and unrecoverable. `doctor` must check for this explicitly,
  never assume it's set.
- **Redirect entries (`# redirect entry`) must be filtered out** — they
  come from opening a directory/playlist in mpv and contain no real
  position. Silently including them would corrupt "most recent" logic.
- **Candidate `watch_later` paths are a fixed list per OS/install method**
  (native/Flatpak/Snap), merged freshest-file-wins — never environment
  detection. See `docs/design.md` §4 for the exact path list; don't let
  it drift out of sync with `doctor`'s checks.
- Absence of a `watch_later` entry for a file that was clearly opened is
  itself a completion signal (mpv doesn't write one if played to the
  end) — don't treat "no entry" as "never watched" without checking this.

### Thresholds are fixed seconds, both configurable, defaults 120s / 180s
`resume_seconds = 120` (below this: start from 0, not a real bookmark),
`completion_remaining_seconds = 180` (this close to the end: mark
complete). Every tunable in the design doc must have a corresponding
`[thresholds]`/`[roots]`/`[mpv]`/`[mal]` config field — a hardcoded value
that isn't configurable is inference in disguise; don't add one without
also wiring it into `config.toml` and `tsuzuki config`.

### Exclusions run before ordering, always
`tsuzuki exclude` removes files/dirs from the tree *before* the natural
sort runs, not as a post-filter on results. If ordering logic and
exclusion logic ever get combined into one pass for a "cleaner" refactor,
verify exclusions still fully remove nodes pre-sort — don't let excluded
files leak into natural-sort comparisons even if their final output
position ends up correct by luck.

### Exclusion persistence and disambiguation
Exclusions persist in `config.toml` (not `state.toml`) — they're an
explicit, human-declared, one-time rule, same category as tracked roots,
not something Tsuzuki derives while running. Like roots (above), the
stored form must stay portable: store the relative path plus a reference
to which tracked root it belongs to (using that root's own persisted
literal form as the key), never a resolved absolute path baked to one
machine. `exclude` validates the resolved path exists on disk before
excluding, matching `add`'s validate-now behavior — an exclude of a
typo'd path should error, not silently accept garbage. Excluding an
entire series folder (a depth-1 directory itself) is rejected as invalid
usage — not tracking it, or a future `drop`, is the correct tool for
that; allowing `exclude` to also remove a whole series would be a third,
confusing way to make one disappear. When the same relative path exists
under more than one tracked root, resolution is a `--root <path>` flag,
not an interactive prompt — `exclude` is a rare setup action, not the
daily reflex path, so staying scriptable (usable in a script/dotfiles
install, non-interactive) wins over minimizing keystrokes here. Contrast
this deliberately with the resume picker (§8 in docs/design.md), where
the opposite tradeoff was made for the opposite reason.

### Anitomy only annotates, never reorders
`anitomy-rs` parses episode *identity* for MAL sync. When parsing fails,
fall back to ordinal position — silently, no error surfaced to the user
for this specific fallback since it's expected/common. Anitomy failing to
parse a filename is not a bug to fix upstream; it's the designed fallback
path, and it must always resolve to *some* usable episode number.

### CLI bare-invocation contract
Every command group with subcommands (`config`, `sync`) must have an
explicit, documented answer for what happens when it's invoked with no
subcommand — never clap's generic "subcommand required" error, which is
accurate but reads as accidental rather than decided. If the group
already has a specified default action (`sync` bare = run the sync,
per `docs/design.md` §9), that action must stay reachable without a
subcommand — don't model it as a required-subcommand enum that
accidentally blocks the primary command. If a group has no single
default meaning (`config` — `get`/`set`/`list`/`edit` are all equally
valid, none is "the" default), bare invocation prints help and exits 0.
This does **not** extend to malformed input (bad flags, unknown
subcommands, missing required args) — that stays on clap's own
error/usage output. Clap's default error handling is mature and matches
the rest of the Rust CLI ecosystem (ripgrep, cargo, fd); reimplementing
it would be exactly the kind of self-inflicted complexity "boring over
clever" (§2) exists to avoid. Don't extend Tsuzuki's own error-message
layer beyond the one gap this closes.

### Video file filtering within a series tree
The ordering tree under a series only includes files matching a
configurable video-extension allowlist (`[roots] video_extensions`,
default `["mkv", "mp4", "avi", "webm", "mov"]`) — sidecar files common in
release folders (`.srt`/`.ass` subtitles, `.nfo`, cover art, samples)
would otherwise shift natural-sort positions and corrupt "next episode"
logic. This is a narrow exception to "no content classification
anywhere" (§2/§3) — it's plausibly-a-video-file, not "is this an episode
of this story," and it's explicit + user-overridable rather than a
silent heuristic, consistent with §8's "every tunable is genuinely
configurable." Directories are never filtered by this list — only files.

### `tsuzuki add` validation and idempotency
`add` validates the path exists and is a directory at add-time, exiting
non-zero with a clear error if not — doesn't defer this to `doctor`.
Adding an already-tracked root is not an error: it's idempotent, exits 0,
and prints which case occurred (already tracked vs. newly added) so the
user gets confirmation either way without a script treating a harmless
re-run as a failure.

### Roots are persisted in the form the user typed, never rewritten to an absolute path
`~` expansion happens only in memory, for validation and for the
already-tracked comparison — the path written back to `config.toml` is
whatever the user gave `add`, unchanged. Rewriting `~/Anime` to a
canonicalized absolute path on write would silently break config
portability across machines (different usernames/home paths), which
directly contradicts config.toml's stated purpose (§8: hand-edited,
versionable in a dotfiles repo) and the `~`-form example already shown
there. If a future phase needs an absolute path (e.g. for filesystem
scanning), expand at the point of use — never persist the expanded form.

### Symlinks are followed everywhere, with cycle protection
A symlinked series folder or symlinked episode file behaves exactly like
a real one — "the user's filesystem is already their library" (§2)
includes symlinks as a legitimate, ordinary part of that filesystem, not
something to silently hide. This must be applied consistently: any
filesystem-walking code (series enumeration, episode-tree building, root
validation/comparison) uses stat semantics (follows symlinks), never
lstat semantics (which would silently make symlinked content invisible
with no error). Because following symlinks makes cycles possible (a
symlink pointing back into its own ancestry), any recursive walk must
track canonicalized real paths already visited and stop descending into
a repeat — this guard is not optional once symlinks are followed at all.

### video_extensions entries are normalized on read (leading dot stripped)
A config entry of `.mkv` and `mkv` are treated identically — config.rs
strips a leading `.` if present when loading `video_extensions`. Without
this, a natural, common way to hand-type the list (`.mkv` instead of
`mkv`) would silently match nothing, with no error surfaced — exactly
the kind of silent failure the project avoids everywhere else. The
canonical stored/documented form stays the bare extension (no dot), per
§8's example; the normalization only exists to make the dotted form also
work rather than fail silently.

### Resume picker is interactive (arrow-key select), not a numbered prompt — Phase 5
Deciding this now so it isn't re-litigated later: the multi-`in_progress`
resume picker (§8) uses a real interactive selector (↑/↓, Enter — via a
crate like `dialoguer` or `inquire`), not "type a number." This is the
one place in the tool where losing scriptability is the right tradeoff —
it's the daily one-command reflex path the whole tool exists for, so
minimizing keystrokes matters more than non-interactive use here. A
non-TTY context (piped/scripted invocation) must fall back to listing
choices with an index rather than hanging on a prompt that can't render.
Not built until Phase 5; noted here so the crate choice and UX aren't
improvised under time pressure later.

### auto_pause_days defaults to 0 (never auto-pause)
This is deliberate, not a placeholder. A nonzero default would be a guess
about user intent (on a break vs. abandoned vs. waiting for episodes to
air) — exactly the category of inference this project avoids everywhere
else. Don't "helpfully" pick a nonzero default later; 0 is correct.

---

## 4. Dev environment

**All build/test/lint commands run inside `nix-shell`**, not against the
host's system Rust or system gcc. `shell.nix` at the repo root pins the
toolchain (rustup-managed rustc/cargo, gcc as linker, pkg-config + openssl
for the eventual MAL HTTP client, cargo-dist for Phase 8). This exists
because the host's package manager had a transient gcc/glibc version
mismatch when this project started — nix-shell was the fix, and it's
stayed the standard because it also means anyone (including a future
Claude Code session on a different machine) gets an identical toolchain
without touching system packages. Don't suggest `sudo dnf install` for
build tooling; if something's missing from the shell, it belongs in
`shell.nix`, not installed on the host.

If a command in a session transcript or terminal output shows `cargo`/
`rustc` not found, the fix is `nix-shell` (re-)entry, not a fresh Rust
install.

Day-to-day commands (build/test/lint/dist) are in `COMMANDS.md` — that
file is a cheatsheet, not a spec; don't duplicate its contents here.

## 5. Tech stack specifics

- **Rust**, stable toolchain. Startup latency is the entire reason for
  this language choice (sub-tens-of-ms binary start vs. interpreter
  overhead) — don't introduce anything at startup that reintroduces
  meaningful latency (e.g. eager network calls before the CLI is
  responsive; MAL calls must stay lazy/on-demand).
- **clap** for the CLI, **serde + toml** for config/state, **anitomy-rs**
  (pure Rust — this is a hard requirement, not a preference; a C++-backed
  binding reintroduces a toolchain dependency the whole distribution
  strategy is built to avoid).
- **Testing:** `cargo test` + `assert_cmd` (end-to-end against the
  compiled binary) + `tempfile` (fabricated directory trees for ordering/
  exclusion tests — never touch the real filesystem or a real mpv
  install). Phases 1–3 (ordering, exclusions, Anitomy) should be testable
  with zero real I/O; if a test in these phases needs real mpv or network
  access, that's a sign the logic isn't properly isolated yet.
- **Distribution target for v1: Linux only.** OS-candidate-path logic
  should stay parameterized (a lookup table, not an `if cfg!(target_os)`
  scattered everywhere) so macOS/Windows paths can be filled in later
  without restructuring — but don't spend effort verifying them now.

---

## 6. Build phases & session workflow

Phases are defined in full in `docs/design.md` §11 (Phase 0 through
Phase 8). Reasoning for the phase order and what belongs in each is
already there — don't duplicate it here, just follow it.

**Gate discipline (strict):** don't start the next phase until the
current phase's own tests are written and passing. If you hit a design
question mid-phase that isn't resolved by `docs/design.md`, stop and ask
rather than guessing and moving on — an unresolved ambiguity compounds if
later phases build on top of a guess.

**Dogfood checkpoint after Phase 5:** this is called out in the design
doc as the point the tool becomes real end-to-end. Actually pause there
and confirm with the user before continuing to Phase 6 — don't treat it
as just another phase boundary.

**Commits:** the user commits manually. At the end of each unit of work,
propose a single, simple, conventional commit message (imperative mood,
e.g. `feat: add tracked-root registration and validation`) — don't commit
yourself, and don't propose a multi-paragraph message.

**Progress tracking:** update `PROGRESS.md` (not this file) as phases
complete — current phase, what's done, what's next, any open question
blocking further work. Keep it short; it's a status file, not a log of
every session's blow-by-blow.

---

## 7. Doc maintenance

This file follows the living-spec discipline: only decisions-with-reasoning
and non-obvious gotchas belong here. Before adding anything new:

1. Check it doesn't already contradict an existing line — grep first.
2. If it's a one-off fix, ask whether it's really a class of bug; if so,
   write the general rule, not the specific patch.
3. If a decision changes, delete the old text, don't append a correction
   next to it.
4. If a section stops protecting against anything (pure narrative now),
   cut it — git history already has the story.
