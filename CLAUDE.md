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

**A moved or renamed series folder is indistinguishable from deletion, and
this is intentional, not a gap.** Tsuzuki has no path-history or rename
detection anywhere — series identity is the literal directory name (§3/
design.md §3, "no parsing, no aliasing"), so a folder at a new path or new
name is, by that same rule, simply a different, never-before-seen series
as far as Tsuzuki is concerned. The old record becomes exactly as orphaned
as a genuine deletion (handled identically, below); the new location
appears in `series_in_root`'s walk as a brand-new, untracked series with no
history. Do not add rename detection later "for convenience" — matching a
new folder back to an old record by any heuristic (name similarity, file
count, whatever) is precisely the fuzzy inference §2 rules out everywhere
else.

**A `state.toml` record whose series has been deleted from disk stays
inert until explicitly cleaned, never auto-removed.** `start`/`list`
already can't show a deleted series wrong, structurally — they cross-
reference `state.toml` against a live walk of the tracked root (`series_in_root`,
Phase 1), and a deleted folder simply can't appear in that walk regardless
of what `state.toml` still says. The real risk is narrower: the automatic
resume picker (bare `tsuzuki`) resolves a specific file an `in_progress`
record points to — if that record's series was deleted, the file's gone
too, and this must degrade gracefully (skip it, never crash, never treat
it as resumable), same "one bad entry doesn't abort the whole scan"
precedent used everywhere else in this project. But the stale record
itself must NOT be silently deleted the moment its path fails to resolve —
that would be inference (assuming a missing path means the user wants the
record forgotten) in a project that avoids exactly that everywhere else.
Cleanup is `doctor`'s job (Phase 6): it must detect an orphaned
`state.toml` record (tracked once, no longer resolvable on disk) and hand
back the exact command to clear it — same list-and-hand-back-the-command
pattern §3 already uses for exclude ambiguity and Anitomy suggestions —
never automatic. The exact cleanup command's shape isn't decided yet; that's
Phase 6 scope, not now. Don't let this silently vanish into "tracked-root
validity" being read as root-level only — it must include per-series
validity within a still-tracked root, not just whether the root itself
still exists.

### mpv `watch_later` reading is launcher-independent — tracking never needs an IPC socket, but resuming does spawn mpv
**Corrected wording — the previous phrasing here ("Tsuzuki never launches
mpv") was misleading and, read literally, would have blocked Phase 5's
whole reason to exist.** What's actually true, and what matters: *tracking*
doesn't depend on Tsuzuki being the one that launched mpv — it only reads
`watch_later` files after the fact, so it works identically whether a file
was opened by Tsuzuki, a file manager, or the terminal. That's a completely
separate question from *resuming*, which is a core feature: `tsuzuki` /
`resume` / `start` genuinely do spawn `mpv <file> --start=<seconds>` (a
plain fire-and-forget process launch, then immediately let go) — otherwise
the tool's entire one-command pitch (§1) is meaningless. What's actually
ruled out is holding an ongoing IPC socket or requiring control over an
already-running mpv instance — never a bare spawn-and-release. This means:
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
- **Absence of a `watch_later` entry is never, by itself, proof of
  completion** — see §3's "Completion detection has a real, permanent
  blind spot" for why (mpv's own `--resume-playback` deletes a file's
  bookmark on load regardless of what happens next, so absence is
  produced identically by a genuine completion and by an ordinary
  abandon). Don't reintroduce "no entry ⇒ watched" reasoning here; that
  was a real, shipped bug.

### Optional mpv companion script — per-file preferred source when present, watch_later fallback always intact
A small, Tsuzuki-authored Lua script (`contrib/mpv/tsuzuki.lua`) placed in
mpv's `scripts/` directory is exactly as launcher-independent as
`watch_later` itself — mpv loads every installed script for every
invocation regardless of what launched it, and the script only ever writes
small text files to its own log directory (`$XDG_DATA_HOME/tsuzuki/mpv-log`,
atomically, temp-file-then-rename). No IPC socket, no background process,
nothing added to this section's own "never holds an ongoing connection to
an already-running mpv instance" constraint. Reading this log is
`script_log.rs`'s job — its scan shape (parse-one-entry, skip malformed/
`reason=="redirect"` entries without aborting, filter to tracked roots via
the same symlink-aware helper `watch_later` parsing already uses) mirrors
`mpv.rs`'s own `scan_watch_later` deliberately.

**Strictly opt-in, never a hard requirement.** Installing it is a manual,
documented, one-time step — the same shape as the existing
`write-filename-in-watch-later-config=yes` prerequisite, not a new command
surface. A user who never installs it keeps the exact behavior shipped
before this script existed; nothing regresses, nothing new is required for
the tool to keep working. The log directory itself is fixed
(`$XDG_DATA_HOME/tsuzuki/mpv-log`, entirely Tsuzuki-owned) and deliberately
**not** a `config.toml` field — unlike `watch_later_dir`, there's no
install-method variance to work around here, so there's nothing for a user
to need to override.

**An independent candidate source, unioned with `watch_later` — not mere
enrichment.** `reconcile.rs`'s candidate discovery scans both sources and
merges them per source path (`merge_candidates`), so a file with a
script-log entry but no `watch_later` entry at all (a clean binge, watched
straight through with the script installed — mpv writes no `watch_later`
entry for a file that reaches a true, uninterrupted EOF) still surfaces as
real activity. This is what actually closes the "clean binge leaves no
trace" blind spot below for anyone who has the script installed — union,
not fallback-only, is what makes that true.

**Per-path source selection is mtime-based, not "script always wins."**
When a path has entries from both sources, the fresher one wins by the
underlying entry file's own mtime; an exact tie goes to the script entry,
deterministically (same precedent as the OS-candidate merge's own
exact-mtime-tie rule). This matters because `watch_later` entries can
legitimately be fresher — e.g. a `resume`d file quit again after the
script-log entry was last written.

**Classification is per source, once a winner is picked.** A
`watch_later`-sourced winner classifies exactly as before, unchanged
(`resume_seconds`/`completion_remaining_seconds`, Symphonia/AVI duration
probing). A script-sourced winner with `reason=="eof"` is treated as
`Completed` unconditionally — a direct, unambiguous signal, never run
through the `completion_remaining_seconds` proximity check at all. A
script-sourced winner with any other reason (`stop`/`quit`/`error`/
`unknown`) and a real position is classified exactly like a `watch_later`
reading (the same `resume_seconds` noise threshold, the same
`completion_remaining_seconds` proximity check) — just fed the script's
own `duration` directly when it has one, skipping the Symphonia/AVI probe
entirely for that file, per this same section's original design. **When
the script's own `duration` field is empty** (mpv's `duration` property
observer never fired for that session, e.g. quit almost immediately) —
**falls back to `duration::probe_duration` on the file itself, exactly as
a `watch_later`-sourced reading always has**, rather than leaving duration
`Unknown` and skipping the completion check. The file is real and on disk
regardless of which source won; probing it costs the same
low-hundreds-of-microseconds it always has (§3's "Completion threshold
needs episode duration" already established this is cheap, not a caching
concern), and there's no reason a script-sourced reading should know
*less* about a file's completion state than a `watch_later`-sourced one
would for the exact same file. A `reason=="redirect"` entry is discarded
at parse time, never a candidate at
all — the script-log equivalent of `watch_later`'s own redirect-entry
filtering. A script entry that errors, or simply never gets written for a
file, is indistinguishable from "script not installed" for that file — no
special error state, matching the existing "one bad entry doesn't abort
the whole scan" precedent used everywhere else in this project.

**Script-log entries persist forever on disk — mpv never deletes them.**
Unlike `watch_later`, which mpv's own `--resume-playback` clears on load,
a script-log entry just sits there unchanged indefinitely once written.
This means the existing "only write to `state.toml` on a genuine content
change vs. what's currently stored" diffing rule has to hold for
script-sourced readings too, or an unchanged entry seen on every
subsequent invocation would look like fresh activity forever — most
concretely, it must never silently undo a prior `pause`/`drop` the way an
un-zeroed `watch_later` bookmark could before that bug was fixed (see
"`pause`/`drop` never zero the stored position" above).

**Why union-with-fallback instead of replacing `watch_later` reading
outright:** a full replacement would mean a user without the script gets
*zero* tracking instead of full, correct `watch_later`-only tracking — a
materially worse out-of-box floor than the one-line
`write-filename-in-watch-later-config` ask ever was. Union-with-fallback
gets the accuracy win (closes the "clean binge leaves no trace" blind spot
entirely for anyone who installs it, eliminates the absence-vs-abandon
ambiguity at the root rather than just making it safe) without raising the
floor for anyone who doesn't.

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

**Idempotency comparison must be normalized, not a raw string match.**
`"One Piece/Specials"` and `"One Piece/Specials/"` (or any other
equivalent form `add`'s own idempotency check already treats as the same
path — trailing slash, `..`-resolution, symlink aliasing) must be
recognized as the same exclusion. Resolve/canonicalize both sides before
comparing, exactly like `add` does — this was previously stated as
already matching `add`'s behavior but didn't in practice; if these two
checks (roots.rs's and exclude.rs's) ever diverge again, that's a bug,
not an acceptable inconsistency.

**The path argument to `exclude` must be validated as relative before
any join.** `PathBuf::join` silently discards its base entirely when
given an absolute path — so an unvalidated leading `/` in user input
(typo or otherwise) can resolve against the filesystem root instead of
the tracked root, landing on something completely unrelated. Reject an
absolute `path` argument outright with a clear error before it ever
reaches a `.join()` call; never let a malformed argument silently
resolve to an unintended location.

**Exclusion enforcement checks the resolved (symlink-following) path,
not the walked entry's own literal path.** Consistent with "symlinks are
followed everywhere" (Phase 1) — a symlink located outside an excluded
directory but pointing *into* excluded content must still be excluded,
because what it resolves to is excluded, even though its own path in the
tree isn't. Checking only the entry's literal path lets a symlink alias
silently bypass an exclusion.

### Anitomy only annotates, never reorders
`anitomy-rs` parses episode *identity* for MAL sync. When parsing fails,
fall back to ordinal position — silently, no error surfaced to the user
for this specific fallback since it's expected/common. Anitomy failing to
parse a filename is not a bug to fix upstream; it's the designed fallback
path, and it must always resolve to *some* usable episode number.

### Anitomy is vendored, not a live dependency — hands off except real bugs
`vendor/anitomy/` is Rapptz/anitomy-rs's source (MPL-2.0), copied in at
commit `c5271be1d9fe71a49e93e98ac3f982b38e6e27b6`, renamed to
`tsuzuki-anitomy` to avoid confusion with the unrelated, C++-backed
`anitomy` crate on crates.io. It was vendored — not pulled as a live git
dependency — for safety against the upstream repo disappearing or going
stale (its own commit history is genuinely sparse) and full edit control,
since Tsuzuki only needs a narrow slice of what it parses. Its original
`Cargo.toml` pinned a fork of `phf` (`edef1c/rust-phf`) for case-
insensitive lookup support that wasn't yet released upstream — that
support has since shipped in `phf 0.12`, so the vendored copy's
`Cargo.toml` uses plain `phf = { version = "0.12", features = ["macros",
"uncased"] }` from crates.io, not the fork. Verified by rebuilding
against the swap and confirming all of anitomy-rs's own regression tests
still pass unchanged. This means Tsuzuki's dependency graph has **no**
remaining git-only dependencies — `cargo install tsuzuki` and a full
crates.io publish are both viable once desired, no longer blocked.
`vendor/anitomy/` is treated as read-only, off-limits code: don't
refactor, "clean up," or restyle it in a session working on anything
else. The only acceptable reason to touch it is a confirmed, reproduced
bug in its parsing behavior that's actually blocking Tsuzuki — and even
then, treat it as a deliberate, isolated change with its own report, not
a drive-by edit alongside unrelated work.

### Anitomy must be called with a bare filename, never a full/relative path
Verified by direct testing: `anitomy::parse()` silently mis-parses when
given a path with directory components (e.g. `"Season 1/ep02.mkv"` — the
episode number is lost entirely, swallowed into the title). Always call
`.file_name()` (or equivalent) before passing anything to Anitomy. This
matters a lot in practice — season subfolders are a normal, expected
structure (§3), not an edge case, so getting this wrong would silently
break episode parsing for a large fraction of real libraries.

### Anitomy-candidate suggest-worthy Type values
The full `ElementKind::Type` keyword table treats every value —
including `TV` and `SP` — as equally "ambiguous" internally, so
ambiguity alone doesn't distinguish which values should trigger a
suggestion. The actual list Tsuzuki suggests on: `OVA`, `Special`,
`Specials`, `Movie`, `Gekijouban`, `ONA`, `OAD`, `OAV` (case-insensitive).
`Gekijouban` is included even though it wasn't in the original phase
scope — it's a confirmed real value, semantically identical to `Movie`.
`TV` and `SP` are deliberately excluded: `TV` denotes the mainline
episode category (suggesting it would mean suggesting regular episodes
for exclusion), and `SP` has a documented real-title collision noted in
Anitomy's own source. If this list is ever revisited, don't shrink it
back to "just the phase-3 prompt's original list" — that list was a
starting guess, not the settled answer; this one is.

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

### Series selection: interactive by default, explicit when scripted — `resume`/`start`, Phase 5
`tsuzuki resume` and `tsuzuki start` share one selection mechanism, decided
now so it isn't improvised under time pressure later. Invoked with no
explicit series argument, and a real TTY is available: a real interactive
arrow-key selector (↑/↓, Enter — via a crate like `dialoguer`/`inquire`),
not "type a number." This is the one place in the tool where losing
scriptability is the right tradeoff — it's the daily one-command reflex
path the whole tool exists for, so minimizing keystrokes matters more than
non-interactive use here. But — this was a real gap in the earlier version
of this note — the picker must not be the *only* way to complete the
action: both commands also accept an explicit series name (with `--root`
when the same name exists under more than one tracked root, same mechanism
as `exclude`/`pause`/`drop`), so the action can always be completed
non-interactively and used reliably from scripts, without ever depending on
a TTY existing at all. In a non-TTY context with no explicit series given:
never hang waiting for input — list the available choices with indexes and
exit without performing the action (non-zero exit, see "informed but did
not complete the action" below). An explicit series argument always bypasses
the picker entirely and proceeds directly, TTY or not. The interactive
picker is a convenience layered on top of a fully scriptable command, never
a requirement of the command's actual semantics.

### `current` was cut — folded into `status`
An earlier draft of the command list (design.md §9) had `tsuzuki current`
as a separate command ("which series would bare `tsuzuki` resume right
now"). Revisited during Phase 5 planning: it added no information `status`
can't already show — a single `in_progress` series is `status`'s one row;
multiple are `status`'s full list, which can just note that running
`tsuzuki` bare will prompt among them. Cut entirely rather than kept as a
redundant thin wrapper — one fewer command surface, zero lost capability.

### Anitomy-candidate confirmation stays list-only, not interactive
Bare `tsuzuki exclude` (no path) lists Anitomy-suggested OVA/Special/
Movie candidates plus the exact `tsuzuki exclude "<path>"` command for
each — it does not run an interactive y/n walkthrough, and there is no
separate indexed `--confirm` follow-up command. Same reasoning as the
`--root` disambiguation case (§3): `exclude` stays non-interactive by
design, and re-running it with an explicit path reuses Phase 2's already-
validated logic rather than adding a new command surface or new
cross-invocation state (an index from a prior "list" call would only be
valid if nothing on disk changed in between — a fragile guarantee this
project avoids relying on elsewhere).

### Clippy's strict gate is scoped to `-p tsuzuki --no-deps`, not `--workspace`
`vendor/anitomy` is a workspace member (needed for `cargo build` to work
at all) but is off-limits code (see the hands-off policy above) — it was
never held to Tsuzuki's own lint bar and doing so now would mean either
restyling vendored third-party code for no functional reason, or having
every future phase's verification fail against pre-existing, unrelated
lints. `cargo build --workspace` still checks the whole thing compiles;
only the strict `-D warnings` clippy gate is scoped down. See
COMMANDS.md for the exact command.

### Threshold precedence when position + completion checks overlap (short files)
On a short enough file, a single saved position can satisfy both
`resume_seconds` (small enough to count as noise) and
`completion_remaining_seconds` (close enough to that file's own ending) at
once — the two zones only stay separate on episode-length video. Current
code (`src/mpv.rs`'s `classify()`) has the position check win: such a case
resolves to `StartFromZero`, never `Completed`. Reasoning: getting this
wrong the other way silently claims progress the user may not have actually
watched, where getting it wrong this way only costs a possible rewatch of a
few seconds — and the real completion signal for a fully-watched file (no
`watch_later` entry at all) still catches these cases independently once
Phase 5 exists, so erring conservative here doesn't lose real detection.
This is this session's judgment call, not something docs/design.md specifies
— if real usage says the other order feels more correct, this is a one-line
change in `classify()`, not a structural one; revisit if it doesn't hold up.

**`start`'s listing has three distinct display words, one per state — this
was caught by literally simulating a session, not obvious from the state
mechanics alone.** Never-started (no `state.toml` record) → **"Start."**
Paused → **"Resume"** (at its saved position) — genuinely different from
the other two; a paused series has a real position waiting, not a reset
one. Dropped (manually, or via the one detectable "finished the whole
series" signal — see the binge-watching trace gap below) → **"Rewatch?"**
— manual-drop and detected-completion collapse to the same display and the
same underlying state, since both are genuinely the same situation from the
user's side: reopening any episode naturally re-populates progress either
way (§8), and this needs zero new persisted data to tell them apart — the
display only checks "does a record exist, and what state is it in," never
"why." A `paused` record must never be mislabeled "Rewatch?" — that word is
specifically for a reset/dropped position, not a genuinely resumable one.
Deliberately no fourth `completed` state was added — see "Completion
detection has a real, permanent blind spot" below for why one wouldn't even
be reliably detectable most of the time.

### Completion detection has a real blind spot for `watch_later` alone — absence of a bookmark is never, by itself, proof of anything
Absence of a `watch_later` entry for the series' currently-tracked episode
must never be read as positive evidence of completion, full stop — no
exceptions, no "but the record's old stored position was real" special
case. This was a real, shipped bug, found by dogfooding: mpv's own
`--resume-playback` (on by default, independent of anything Tsuzuki does
or which program launched mpv) deletes a file's `watch_later` bookmark the
instant that file is *loaded*, if one already exists for it — and mpv
never rewrites a fresh one for a file left mid-playback via a playlist
command (e.g. `playlist-next`) before it's ever quit while still loaded.
The result: "no entry for the current episode" is produced *identically*
by a genuine, played-to-true-EOF completion and by an ordinary abandon —
open an already-bookmarked episode, skip ahead to a different one, quit
having spent under `resume_seconds` there (or none at all). These two
situations are provably indistinguishable from `watch_later` state alone.
Treating "absence + the record's last stored position was real" as
sufficient proof of completion (an earlier version of this rule did)
silently discarded real progress on the abandoned episode and could even
auto-`Drop` the whole series off a one-minute peek at its finale.

**The only signals that ever justify moving the cursor or changing status:
a real (≥`resume_seconds`) `Resume` position found anywhere forward in the
series, or a `Completed` classification (see below) found anywhere
forward** — both are positive readings taken from an entry that still
exists, unaffected by the deletion-on-load problem. Absence contributes
nothing to the decision either way; it's not weighed, corroborated, or
treated as a tiebreaker — it simply fails to produce either signal. This
does widen the already-accepted "clean binge leaves no trace" blind spot
(mpv writes no entry at all for a file played straight through to a true,
uninterrupted EOF): a series finished this way, with *nothing* else ever
observed afterward, on any subsequent episode, no longer auto-resolves to
`Dropped` — it stays exactly where it was until an explicit `tsuzuki drop`
or genuine forward evidence appears. That's the honest tradeoff: the
ordinary way people actually finish an episode — quitting on or near it,
which mpv *does* capture as a `Completed` reading before anything gets a
chance to delete it — is completely unaffected and keeps auto-advancing/
auto-dropping exactly as before. Closing this gap completely from
`watch_later` state alone (detecting a true, silent EOF with zero
corroborating signal) would require real-time observation of playback,
which is explicitly the thing this project chose not to build. This is not
a failure to fix; it's the tool honestly reporting the boundary of what it
can know from `watch_later` alone.

**The gap is closable, not from `watch_later`, but from the optional
companion script.** `contrib/mpv/tsuzuki.lua` observes the same `end-file`
event `watch_later` never gets to write anything meaningful for on a clean
EOF, and records a direct `reason=="eof"` signal instead of leaving
absence to be interpreted. For anyone with the script installed, that
signal is unioned into `reconcile.rs`'s candidate discovery (see "Optional
mpv companion script" above) and treated as an unconditional `Completed` —
so a clean binge genuinely does now auto-advance/auto-`Drop` correctly,
closing exactly the blind spot this section describes. This doesn't
weaken the rule above in any way: for a file with no script-log entry
(script never installed, or played before the script existed), absence of
a `watch_later` entry is still never, by itself, proof of anything — the
script only ever adds a positive signal where one previously didn't
exist; it never makes absence itself meaningful.

### When the CURRENT tracked episode itself completes: advance to the next one, or auto-`Dropped` if there is no next one
**This is a real, previously-underspecified rule, not something already
fully built — stated explicitly now because an earlier, too-brief mention
of it (buried in the display-wording section below) was read too narrowly
and produced a real, incorrect claim that nothing here is ever automatic.**
It is automatic, in this one specific, narrow, evidence-based case — and
this is not the fuzzy inference §2 rules out elsewhere, because it only
ever fires on a direct, confirmed signal (the actual current tracked file,
genuinely completing), never a guess.

The rule, precisely: when a `Completed` classification (see "Completion
detection has a real, permanent blind spot" above and "Cursor movement"
below) is found anywhere in the forward scan from the current tracked
episode on — confirming it, or a later episode, genuinely done — reconcile
does not just record that fact and stop. It also resolves the cursor
exactly one step further:
- **If a next episode exists** (in natural-sort order, per the series'
  actual files on disk) — the cursor advances to it, at position 0. This
  is not a claim that the next episode was watched; it's the same
  forward-looking default already used for a brand-new series (`start`
  offers episode 1 without claiming anything about history) applied one
  step later in the sequence. It has to work this way — leaving the
  cursor pointed at a now-finished episode would mean the next resume
  attempt walks straight back into the exact freeze/resume-at-the-end
  problem already fixed once. If that next episode separately turns out to
  already have its own real bookmark (a binge already happened), the
  existing forward-scan/furthest-real-bookmark logic still applies from
  there — this rule only supplies the *starting point* for that scan when
  nothing else does.
- **If there is no next episode** (the completed one was the actual last
  file in the series) — the series transitions to `Dropped`, automatically,
  no manual `tsuzuki drop` required. This is the "one detectable 'finished
  the whole series' signal" already referenced in the display-wording
  section below — that section only described the *display* consequence
  ("Rewatch?"); this section is the actual mechanism producing it. No new
  state or persisted marker is needed for this either — it's the same
  `Dropped` state manual dropping already produces, reached automatically
  instead of by command.

**This rule applies identically when no `state.toml` record exists yet.**
Found by dogfooding the real binary during Phase 6, not by design ahead of
time: a brand-new, never-before-tracked series whose only observed
activity is a `Completed` classification — including a script `reason ==
"eof"` reading, i.e. exactly the "clean binge, watched straight through
the very first time, script installed" case this project's own companion-
script rationale is about — used to produce **no record at all**, silently.
The rule above was implemented as "advance the CURRENT tracked episode,"
which implicitly assumed a tracked episode (an existing record) already
had to exist; a series with no record yet has no cursor to advance, so the
completion signal was simply dropped on the floor. The fix: a never-
tracked series uses the exact same index-0 baseline `Dropped`/never-started
records already share for the ordinary `Resume` forward-scan (see "Forward-
scan is strictly forward" below) — a placeholder record is created purely
as the landing spot this rule's own advance-or-`Drop` logic then resolves,
so a brand-new single-episode series watched straight through correctly
lands `Dropped` (not invisible), and a brand-new multi-episode series
watched straight through episode 1 correctly lands `InProgress` at episode
2. A genuinely `Dropped` record is **not** given this same treatment —
it stays excluded here exactly as it already was, since `Dropped` is a
deliberate dead end only a real `Resume` reactivates (no `dropped ->
paused` transition exists), not something a stray `Completed` reading
should auto-undo.

### `pause`/`drop` never zero the stored position — reconcile's diffing depends on it
Found by Phase 5's dogfood pass, not by design ahead of time — worth
stating plainly so it isn't "cleaned up" by a later refactor that doesn't
know why it's there. `drop` (and `pause`) must never zero or clear a
record's stored position/path. Reconcile's whole mechanism is diffing
"what's stored" against "what mpv currently says" to detect genuine new
activity (see below) — but `drop` never touches mpv's own `watch_later`
file, so the real bookmark is still sitting there unchanged after a drop.
Zeroing the stored side manufactures a fake mismatch, which reconcile
correctly (by its own logic) reads as new activity — silently undoing the
drop on the very next invocation, with zero user action. The actual
"start fresh" behavior for a dropped/paused-then-reactivated series is
handled entirely separately, at playback-resolution time (index 0 for
dropped/never-started, per the state's own rules below) — never by
mutating what's stored. If a future change touches `pause`/`drop`, this
must not regress; it already did once.

### Forward-scan is strictly forward — a paused series doesn't reactivate from an earlier real bookmark
A real (≥`resume_seconds`) bookmark appearing on an episode *earlier* than
a series' current known position must never reactivate or move a `Paused`
record — reconcile only ever scans forward from the last known position,
never backward. This matters concretely: someone pauses a long series at
episode 50, then months later rewatches an unrelated earlier episode (say
episode 3) purely for nostalgia — if a backward bookmark could reactivate
things, that unrelated rewatch would silently corrupt the paused position
at episode 50. Forward-only prevents this by construction. `Dropped` and
never-started records don't hit this at all — both use an index-0
baseline (per "reappears exactly like never-started," design.md §8), so
by definition nothing in the series counts as "behind" them; any real
bookmark anywhere in a dropped/never-started series is forward from that
baseline and correctly reactivates it.

### Cursor movement: noise never touches saved state, completion check is per-series, a single run chains fully
Settles the open question left after Phase 4 about how the cursor advances
when the user manually skips ahead. The full mechanism, worked out by
stress-testing scenarios rather than asserting it from feel:

**A sub-`resume_seconds` reading is invisible to `state.toml`, not
"treated as start-from-zero."** This is a sharper rule than it first
sounds, for a concrete reason: mpv itself has no concept of Tsuzuki's noise
threshold — if a series already has a real, saved position (say 15 minutes
into an episode), and the user briefly reopens that same file and quits
after 45 seconds, mpv's own `watch_later` file now physically says 45
seconds, clobbering the real 15-minute mark that was there a moment ago. If
Tsuzuki blindly propagated whatever it just read, one accidental short peek
would silently erase real progress — the same category of mistake §2's MAL
sync rule already exists to prevent ("never write a smaller number... has
to hold even in edge cases"), just against local state instead of MAL. So:
a sub-threshold reading never creates a new `state.toml` record, and —
critically — never overwrites an existing real (≥`resume_seconds`) one
either. Only another real observation can ever move a series' saved
position, in either direction.

**Completion has exactly one real signal — a duration-based near-the-end
reading — and it's checked across the whole remaining forward scan, not
just at the series' current episode.** `classify()`'s `Completed` result
(built in Phase 4 — a real, ≥`resume_seconds` position with
`duration - position ≤ completion_remaining_seconds`, e.g. quitting at
58:30 of a 60:00 episode during the credits) is the only thing that
confirms an episode done for cursor-advancement purposes. Absence is
*not* a second, equally-weighted signal alongside it — see "Completion
detection has a real, permanent blind spot" above for why: mpv deletes a
file's own bookmark on load regardless of what happens next, so absence
alone can't be trusted to mean completion rather than an ordinary abandon.
Reconcile calls `classify()` (reusing Phase 4's `probe_duration`/
`mpv::classify()` exactly as built — no new duration logic) on every entry
in the scan that has a real position, from the current episode through the
rest of the series; a `Completed` result found *anywhere* in that range is
enough to enter completion-advance, because `apply_completion_advance`
independently re-derives completion at each step it walks through using
the same rule. `StartFromZero`/`Resume` results behave as an ordinary
real/noise reading, unaffected. This costs nothing extra to worry about:
duration probing only ever runs on entries that already have a real
position, and Phase 4 already measured the actual cost directly — 212µs
(release) to probe one file — negligible even across a full multi-step
chain, not a caching concern.

**The completion check is per-series, not "check the single
globally-freshest file."** Phase 4's `most_recent_entry` only identifies
one thing: the single most-recently-touched file across everything scanned.
That's not sufficient on its own for Phase 5, because `state.toml` has to
track multiple `in_progress` series simultaneously (design.md §8's resume
picker exists specifically because more than one can be in progress at
once) — a series watched Monday and one watched Tuesday are both still
legitimately in progress today. So each run does two separate jobs, not
one: (a) for every series already `in_progress`, forward-scan *that
series' own* files for a `Completed` reading, independent of what's newest
elsewhere; (b) separately, check whether any real (≥`resume_seconds`)
`Resume` activity happened anywhere, which is what can move a cursor or
start a new `in_progress` record.

**A single invocation must resolve a full state transition, not just one
step of one.** Binge-watching is a normal usage pattern, not an edge case:
quit episode 1 near its end (a real `Completed` reading) and be several
real minutes into episode 2 before `tsuzuki` is ever run again. One run
must chain through the whole thing — recognize episode 1's completion,
advance the cursor to episode 2, and pick up episode 2's already-real
position — not stop after the first step and require a second invocation
to "catch up." (If episode 2's own position is real enough to stand on its
own — a `Resume` reading — the ordinary forward scan finds it directly,
without ever needing to confirm episode 1's completion at all; either path
must resolve in one call.) Don't implement this as a single comparison per
run; it has to resolve however many steps actually happened since the last
check.

**An absent OS-default candidate directory stays silent — an explicit
`watch_later_dir` override that doesn't resolve must be surfaced, not
silent.** These are not the same situation and must not be treated
identically. An OS-default candidate being missing (e.g. no Flatpak mpv
installed) is normal and expected — silence is correct there. But
`watch_later_dir` is something the user explicitly typed, asserting "look
here instead" — if that path doesn't exist, it's almost certainly a
mistake, and staying silent means the user has no way to discover why
nothing is ever found, possibly indefinitely. The fix: when the override is
set and doesn't resolve, the scan result carries a warning (the configured
path, distinguishable from the "nothing found" case) rather than being
indistinguishable from zero-bookmarks-anywhere — same "carry the reason
forward, don't collapse to a bare empty result" pattern as
`DurationOutcome::Unknown`. This still doesn't error or block any command —
the daily one-command reflex must keep working even with a bad config value
— but the warning must actually reach the user somewhere. Since this module
has no command-facing output of its own (same as everything else in this
phase), the warning is *returned as data*, not printed here; Phase 5 (the
first phase with real command output) is responsible for actually
displaying it once it exists. Don't let `doctor` (Phase 6) end up as the
only place this ever surfaces — `doctor` is for catching setup problems
proactively before they bite; a bad override should also be visible
reactively, the moment it actually affects a real command.

### Completion threshold needs episode duration — mpv's watch_later files don't have it
`completion_remaining_seconds` (§8) requires knowing how much of an episode is
*left* — but mpv's `watch_later` entries only ever store the resume position
(`start=`), never the file's total duration (verified directly against mpv's
own option docs: `--watch-later-options` never includes anything duration-
like, since duration isn't a settable "option" mpv tracks per-file). Tsuzuki
has to get duration itself, from the video file's own container header — not
from mpv.

**No external program (no `ffprobe` shell-out).** Requiring a separately
installed program contradicts local-first/no-moving-parts and Tsuzuki's own
startup-latency rationale (§5/§10) — a Rust dependency compiled into the
binary is strictly better here: works everywhere the binary works, no PATH
lookup, no "is it installed" failure mode.

**`symphonia` (pure Rust, MPL-2.0) reads the header, used for metadata/probe
only — never asked to decode actual audio/video frames.** Confirmed directly
against Symphonia's own docs: it has real demuxers for MKV/WebM and ISO/MP4
(mp4/mov use the same box structure), covering most real libraries as one
dependency. It does **not** support `avi`.

**Dispatch is by content-probe, never by file extension.** Hand the file to
Symphonia's own format-auto-detection first — never a hardcoded "if `.mkv`/
`.webm`/`.mp4` use Symphonia, if `.avi` use X" branch. `video_extensions` is
user-configurable and open-ended (§8) — an extension-keyed dispatch table
would silently stop covering new formats the moment someone adds one to their
config that Symphonia already supports. Only if Symphonia's probe fails to
identify the file at all does a small hand-written fallback reader (for
`avi`'s simple RIFF `avih` header — total-frames × microseconds-per-frame,
no dependency needed for one legacy field) get tried as the next rung, not
as an extension-gated special case.

**Unreadable duration is surfaced, never silently dropped.** If neither
Symphonia's probe nor the fallback reader can determine a file's duration,
the completion-by-remaining-time check is skipped for that file only (the
resume threshold still works fine from position alone — a real `Resume`
reading is unaffected; only a `Completed` classification specifically
needs duration, and per §3's "Completion detection has a real, permanent
blind spot," absence is never an independent fallback for it). But the
*reason* duration couldn't be read is preserved in the return value,
not collapsed to a bare `None` — Phase 4 has no user-facing command of its
own yet, so it's Phase 5 (or `doctor`, Phase 6) that decides where the user
actually sees this, same pattern as Anitomy's parse-fallback in Phase 3:
built correctly now, surfaced later, never silently discarded now.

### auto_pause_days defaults to 0 (never auto-pause)
This is deliberate, not a placeholder. A nonzero default would be a guess
about user intent (on a break vs. abandoned vs. waiting for episodes to
air) — exactly the category of inference this project avoids everywhere
else. Don't "helpfully" pick a nonzero default later; 0 is correct.

---

### `state.toml` record identity, root reference, and `updated_at`
A series record is keyed by **both** its tracked root and its literal
series name — never name alone. Composable roots (§3 above) mean the same
series name can legitimately exist under more than one tracked root at
once (`Anime/Monster` vs. `Anime/Rewatches/Monster`), and they must stay
distinct records with independent progress, same as everywhere else this
project handles composable roots. The root reference stores that root's own
**persisted literal config form** — exactly the same mechanism exclusions
already use (§3, "Exclusion persistence and disambiguation") — never a
resolved absolute path or a synthesized ID; this preserves the same
portability guarantee roots and exclusions already have (§3, "Roots are
persisted in the form the user typed"). Each record also carries an
`updated_at` timestamp, needed to give `auto_pause_days` any real meaning
at all — nothing currently tracks how idle a series actually is.
**`updated_at` only moves on a genuine, persisted state change** — never
on a mere scan or invocation, and never on a sub-`resume_seconds`
observation, since those are already explicitly forbidden from touching
saved state at all (see "Cursor movement" above). It measures real idle
time, not how often the command happened to run.

### `pause`/`drop`: root disambiguation, no-record errors, and the allowed transitions
`pause <series>` and `drop <series>` use the exact same `--root`
disambiguation mechanism `exclude` already established, when the same
series name exists under more than one tracked root — not a new, second
disambiguation UX.

**Both error, rather than silently creating a record, when the target has
no real progress to act on** — this covers two cases identically: a series
with no `state.toml` record at all (never started), and one that's already
`dropped` (progress already reset, nothing left to pause). `pause`/`drop`
act on existing progress; they don't manufacture a starting point. This is
one rule, not two special cases — "is there anything real to act on,"
consistently applied.

**Allowed transitions:** `in_progress → paused`, `in_progress → dropped`,
`paused → in_progress`, `paused → dropped`. **`dropped → paused` does not
exist** — dropped is a dead end reachable only by opening an episode
directly, which (per §8) naturally re-populates progress and goes straight
back to `in_progress`, never through `pause`. Re-invoking `pause` on an
already-`paused` series, or `drop` on an already-`dropped` one, is
idempotent — exit 0, no-op — same precedent as `add` re-registering an
already-tracked root; a script re-running a harmless action shouldn't look
like a failure.

### Launching mpv: fire-and-forget, cheap existence check before spawn, install-method detection
Resuming (`tsuzuki`/`resume`/`start` picking an episode) spawns
`mpv <file> --start=<seconds>` and immediately lets go — it does not block
waiting for mpv to exit, and it does not re-scan `watch_later` the moment
mpv exits either. Consistent with the passive, after-the-fact
`watch_later` architecture (§3, "mpv `watch_later` reading is
launcher-independent"): state reconciliation happens the next time Tsuzuki
is invoked, exactly like every other read of mpv's state, not as a special
case tied to having just launched something.

**Native vs. Flatpak mpv is resolved by a cheap existence check, never by
attempting one and catching failure before trying the other.** A
`PATH` lookup is a fast filesystem check, not a spawn — this must never
become "try spawning native mpv, catch the failure, then try the Flatpak
invocation," since a failed spawn attempt is real, wasted process-launch
overhead directly against the tool's own startup-latency rationale (§5).
Check existence once, spawn only the one that's actually there. A new
`[mpv] command_override` config field (empty string default = auto-detect;
same empty-means-default shape as `watch_later_dir`) lets a user with both
installed pin the exact launch command explicitly (e.g.
`"flatpak run io.mpv.Mpv"`), skipping detection entirely on every future
invocation. If neither resolves, or the spawn itself still fails for some
other reason, that's a plain, clear command error — never a panic, and
`doctor` (Phase 6) checking mpv availability proactively doesn't make this
the *only* place a launch failure is ever surfaced.

### Every state-touching command reconciles against current mpv reality first, through one shared function
`list`, `status`, `pause`, `drop`, and the resume/`start` picker must all
run the same "resolve `state.toml` against what mpv currently says"
step before doing anything else — through one shared function, not five
commands each quietly implementing their own freshness check. This
guarantees identical behavior by construction (e.g. `pause <series>`
correctly sees a series that secretly already completed since the last
check, before acting on stale info) rather than by every command happening
to agree today and silently drifting apart later.

### `list` ordering and episode-number display
`list`'s ordering is fixed and deterministic, never "whatever iteration
order falls out": grouped by status first — `in_progress`, then `paused`,
then `dropped`, then never-started — alphabetical by series name within
each group. Episode number shown in `list`/`start` (design.md §6/§9's
"ep. 9" mockups) uses Anitomy's parsed episode number when available,
falling back to ordinal position — same precedent as everywhere else
Anitomy annotates without deciding order (§3), applied to display.

### Output convention: warnings on stderr, real output on stdout; non-completion exits non-zero
Any surfaced warning (a `ScanWarning`, a `DurationOutcome::Unknown` reason,
etc. — see Phase 4's decisions above) goes to stderr; normal command
output goes to stdout — matches the ripgrep/cargo convention this project
already follows elsewhere (§3, "CLI bare-invocation contract"), and keeps
piping/scripting against stdout clean. A command that *informs* the user
of something but doesn't *complete* the requested action exits non-zero —
this isn't a new rule, it's the same precedent `exclude`'s ambiguity case
already set (§3): a non-interactive picker listing choices without picking
one falls in the same bucket, so a script chaining `tsuzuki && next-thing`
correctly sees that nothing was actually resumed.

### Every phase needs at least one real-binary, real-XDG-path integration test
Found during Phase 5's dogfood investigation, worth standing policy going
forward: every existing test in this project, across every phase, validated
internal functions against a redirected temp path (`tempdir().join(...)`)
— none ever invoked the actual compiled binary against real
`dirs::config_dir()`/`dirs::data_dir()` resolution. This is a real,
standing blind spot: a genuine bug in real-path resolution (wrong env var,
wrong directory, a swallowed error in the real write path) could pass every
unit test while breaking 100% of real usage, and nothing would catch it
short of a human running the actual binary by hand. It happened not to be
hiding a real bug when this was investigated — but the gap itself was real,
and it's cheap to close permanently: at least one test per phase that
touches persistence should spawn the real binary (`env!
("CARGO_BIN_EXE_tsuzuki")`) against a genuine temp `$HOME`/XDG environment,
not just call internal functions with an injected path. Retrofit this for
existing phases opportunistically; require it going forward for any new
phase that reads or writes a real on-disk file.

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
  strategy is built to avoid), **symphonia** (pure Rust, metadata/probe use
  only — reading episode duration for the completion threshold; see the
  "Completion threshold needs episode duration" decision above for why and
  its avi fallback).
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
