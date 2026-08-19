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

Each subsection below is meant to be readable alone — grep for the topic
you need rather than reading top to bottom.

### Series identity = depth-1 directory under a tracked root
A series is *always* the immediate child of a tracked root — never
detected by content, never configurable per-series. Everything below
depth 1 (season folders, release-tag folders) is opaque to Tsuzuki as a
category and only feeds the natural-sort tree. A layout that doesn't fit
(e.g. a `Rewatches/` layer) gets registered as its own root
(`tsuzuki add ~/Anime/Rewatches`) — composable roots is the general
escape hatch; don't add a depth-detection flag for one folder shape.

### State vs. config are two separate files
`~/.config/tsuzuki/config.toml` (human-tunable, hand-edited, versionable
in dotfiles) and `~/.local/share/tsuzuki/state.toml` (machine-written,
atomic). Don't merge them.

### State file: single flat TOML, atomic write, not SQLite, not sharded
State is small and bounded (dozens of series) — full-parse-on-every-
invocation is cheap, so there's no case for SQLite or mpv's per-file
sharding. **Every write is temp-file-then-rename**, through one shared
atomic-write helper — a kill mid-write must never leave a half-written
file; never add a second write path that writes in place.

**A moved/renamed series folder is indistinguishable from deletion — on
purpose.** No path-history or rename detection anywhere: series identity
is the literal directory name, so a folder at a new path/name is simply a
new, never-before-seen series. The old record goes orphaned exactly like a
genuine deletion; the new location appears as an untracked series with no
history. Never add rename-matching by heuristic (name similarity, file
count) — that's the fuzzy inference §2 rules out everywhere.

**An orphaned `state.toml` record (series deleted from disk) stays inert
until explicitly cleaned — never auto-removed.** `start`/`list` can't
misrepresent it (they cross-reference a live directory walk). The
automatic resume picker must degrade gracefully if it points at a deleted
file (skip it, never crash) — but the record itself isn't silently
deleted just because its path fails to resolve; that would be inference.
Cleanup is `doctor`'s job: it detects the orphan and hands back
`tsuzuki doctor --fix --forget-orphans` (requires `--fix`, enforced by
clap; prints exactly what it's about to delete; bulk not per-series, since
what it clears is already confirmed-dead). A root that's itself
unreachable (unmounted drive, permission denied) is excluded from
per-series orphan detection entirely — doctor checks root-level validity
first — so a simple unplugged drive never floods false-positive orphans.

### mpv `watch_later` reading is launcher-independent — tracking never needs an IPC socket, but resuming does spawn mpv
*Tracking* doesn't depend on Tsuzuki having launched mpv — it only reads
`watch_later` files after the fact, so it works identically regardless of
what opened the file. *Resuming* (`tsuzuki`/`resume`/`start`) is a
separate, core feature: it spawns `mpv <file> --start=<seconds>`
fire-and-forget, then lets go. What's ruled out is holding an ongoing IPC
socket or controlling an already-running mpv instance — never a bare
spawn-and-release.
- **`write-filename-in-watch-later-config=yes` is a hard prerequisite** —
  without it, entries are only unrecoverable MD5 hashes of the path.
  `doctor` checks for this explicitly, never assumes it's set.
- **Redirect entries (`# redirect entry`)** — produced by opening a
  directory/playlist in mpv, no real position — are filtered out.
- **Candidate `watch_later` paths are a fixed list per OS/install method**
  (native/Flatpak/Snap), merged freshest-file-wins — never environment
  detection. See `docs/design.md` §4 for the path list; keep `doctor`'s
  checks in sync with it.
- **Absence of a `watch_later` entry is never, by itself, proof of
  completion** — see "Completion detection has a real blind spot" below.

### Optional mpv companion script — per-file preferred source when present, watch_later fallback always intact
`contrib/mpv/tsuzuki.lua`, installed by the user into mpv's `scripts/`
directory, is exactly as launcher-independent as `watch_later` — mpv
loads it for every invocation regardless of what launched it. It writes
small text logs atomically to `$XDG_DATA_HOME/tsuzuki/mpv-log` (resolved
by mpv's own process). No IPC, no background process. `script_log.rs`
mirrors `mpv.rs`'s `scan_watch_later` shape (parse-one-entry, skip
malformed/redirect entries without aborting, filter to tracked roots via
the same helper).

**Strictly opt-in, never required.** A user who never installs it keeps
the exact pre-script behavior — nothing regresses.

**The directory name is fixed and Tsuzuki-owned, but which real directory
it resolves to has install-method variance, same as `watch_later`.**
Flatpak remaps `XDG_DATA_HOME` for every sandboxed app to its own private
per-app data dir (verified directly: `flatpak run --command=sh io.mpv.Mpv
-c 'echo $XDG_DATA_HOME'` → `~/.var/app/io.mpv.Mpv/data`) — the script's
own env read is correct, the bug was Tsuzuki assuming its unsandboxed
`dirs::data_dir()` and mpv's sandboxed one always agree. (Flatpak's
`host:ro` also rules out "use `$HOME` instead" — that mount is read-only
in the sandbox.) Fixed the same way `watch_later` is:
`script_log::mpv_log_dirs()` returns a small candidate list (native + the
verified Flatpak remap), merged freshest-file-wins across directories.
Still not a `config.toml` field. No Snap candidate — unverified against a
real install, and this project doesn't add candidate paths on a guess.

**An independent candidate source, unioned with `watch_later` — not mere
enrichment.** `reconcile.rs`'s `merge_candidates` scans both sources per
path, so a script-log entry with no `watch_later` entry at all (a clean
binge with the script installed — mpv never writes an entry for a file
that reaches a true EOF) still surfaces as real activity. This is what
actually closes the "clean binge leaves no trace" blind spot for anyone
with the script installed.

**Per-path source selection is mtime-based, not "script always wins."**
The fresher entry file wins; an exact tie goes to the script entry,
deterministically — `watch_later` can legitimately be fresher (e.g. a
`resume`d file quit again after the script last wrote).

**Classification is per source.** A `watch_later` winner classifies
exactly as before. A script winner with `reason=="eof"` is `Completed`
unconditionally, no proximity check. Any other script reason with a real
position goes through the normal `resume_seconds`/
`completion_remaining_seconds` classify(), fed the script's own `duration`
when present (skipping the Symphonia/AVI probe), else falling back to
`duration::probe_duration` on the file itself rather than leaving
`Unknown` — the file is real and on disk regardless of source, and
probing costs the same low-hundreds-of-microseconds it always does.
`reason=="redirect"` is discarded at parse time.

**Script-log entries persist forever — mpv never deletes them**, unlike
`watch_later`. So the "only write state.toml on a genuine change" diffing
rule must hold for script readings too, or an unchanged entry would look
like fresh activity on every invocation and could silently undo a prior
`pause`/`drop`.

**Why union-with-fallback, not a full replacement:** a user without the
script must still get full, correct `watch_later`-only tracking, never
zero tracking.

### Thresholds are fixed seconds, both configurable, defaults 120s / 180s
`resume_seconds = 120` (below this: start from 0), `completion_remaining_
seconds = 180` (this close to the end: mark complete). Every tunable must
have a corresponding config field — a hardcoded value that isn't
configurable is inference in disguise.

### Exclusions run before ordering, always
`tsuzuki exclude` removes files/dirs from the tree *before* natural sort
runs, never as a post-filter. If ordering and exclusion logic are ever
merged into one pass, verify exclusions still fully remove nodes pre-sort.

### Exclusion persistence and disambiguation
Exclusions persist in `config.toml` (not `state.toml`) — explicit,
human-declared, same category as roots. Stored form: the relative path
plus the owning root's own persisted literal form (never a resolved
absolute path). `exclude` validates the path exists before excluding
(matches `add`'s validate-now behavior). Excluding a whole series
(depth-1 dir itself) is rejected — not-adding or a future `drop` is the
right tool. Same relative path under more than one root resolves via
`--root <path>`, not an interactive prompt — `exclude` is a rare setup
action, so scriptability wins over keystrokes here (contrast the resume
picker, which makes the opposite tradeoff for the opposite reason).

- **Idempotency comparison must be normalized**, not a raw string match —
  resolve/canonicalize both sides before comparing (trailing slash,
  `..`-resolution, symlink aliasing), exactly like `add`'s own check. If
  `roots.rs`'s and `exclude.rs`'s normalization ever diverge, that's a bug.
- **The path argument must be validated as relative before any `.join()`**
  — `PathBuf::join` silently discards its base on an absolute path, so an
  unvalidated leading `/` could resolve against the filesystem root.
  Reject absolute `path` up front.
- **Exclusion enforcement checks the resolved (symlink-following) path,
  not the walked entry's literal path** — a symlink outside an excluded
  dir but pointing into it must still be excluded.

### Anitomy only annotates, never reorders
`anitomy-rs` parses episode *identity* for MAL sync. When parsing fails,
fall back to ordinal position — silently, since it's expected/common.
Always resolves to *some* usable episode number.

### Anitomy is vendored, not a live dependency — hands off except real bugs
`vendor/anitomy/` is Rapptz/anitomy-rs (MPL-2.0, commit
`c5271be1d9fe71a49e93e98ac3f982b38e6e27b6`), renamed `tsuzuki-anitomy` to
avoid a crates.io name collision with an unrelated C++-backed `anitomy`
crate. Vendored (not a live git dep) for safety against upstream
disappearing and full edit control. Its original `Cargo.toml` pinned an
unmerged `phf` fork for case-insensitive lookups; that support has since
shipped in `phf 0.12`, so the vendored copy now depends on plain
crates.io `phf 0.12` — verified all of anitomy-rs's own regression tests
still pass. Result: Tsuzuki has **no** remaining git-only dependencies,
so `cargo install`/crates.io publish are both viable. Treat `vendor/` as
read-only — the only acceptable edit is a confirmed, reproduced parsing
bug actually blocking Tsuzuki, as its own isolated, deliberate change.

### Anitomy must be called with a bare filename, never a full/relative path
Verified by direct testing: a path with directory components (e.g.
`"Season 1/ep02.mkv"`) silently loses the episode number, swallowed into
the title. Always call `.file_name()` first. Season subfolders are
normal, not an edge case — getting this wrong breaks parsing for a large
fraction of real libraries.

### Anitomy-candidate suggest-worthy Type values
Every `Type` keyword is internally marked "ambiguous," so that alone
doesn't distinguish anything. The actual suggest list: `OVA`, `Special`,
`Specials`, `Movie`, `Gekijouban`, `ONA`, `OAD`, `OAV` (case-insensitive).
`TV` is excluded (denotes the mainline episode category — the opposite of
exclude-worthy); `SP` is excluded (documented real-title collision in
Anitomy's own source, e.g. `"...SP Professional"`). Don't shrink this
list back to a smaller "starting guess" version.

### CLI bare-invocation contract
Every command group with subcommands (`config`, `sync`) has an explicit,
documented behavior for no-subcommand invocation — never clap's generic
"subcommand required" error. A group with a specified default action
(`sync` bare = run the sync) must keep that reachable without a
subcommand. A group with no single default (`config`) prints help and
exits 0. Malformed input (bad flags, unknown subcommands, missing
required args) stays on clap's own error output — don't extend Tsuzuki's
error layer beyond this one gap.

### Video file filtering within a series tree
The ordering tree only includes files matching `[roots] video_extensions`
(default `["mkv", "mp4", "avi", "webm", "mov"]`) — sidecar files
(subtitles, `.nfo`, cover art) would otherwise shift natural-sort
positions. A narrow, explicit, user-overridable exception to "no content
classification anywhere" — plausibly-a-video-file, not "is this an
episode." Directories are never filtered by this list.

### `tsuzuki add` validation and idempotency
`add` validates existence/directory-ness at add-time, not deferred to
`doctor`. Re-adding an already-tracked root is idempotent (exit 0),
printing which case occurred.

### Roots are persisted in the form the user typed, never rewritten to an absolute path
`~` expansion happens only in memory (validation, idempotency
comparison) — the value written to `config.toml` is exactly what the
user gave `add`. Canonicalizing on write would break config portability
across machines (different usernames/home paths). If a future need
requires an absolute path, expand at point of use, never persist it.

### Symlinks are followed everywhere, with cycle protection
A symlinked series folder or episode file behaves exactly like a real
one — every filesystem walk (enumeration, tree building, root
comparison) uses stat semantics, never lstat. Since this makes cycles
possible, any recursive walk tracks canonicalized real paths already
visited and stops descending into a repeat — not optional once symlinks
are followed at all.

### video_extensions entries are normalized on read (leading dot stripped)
`.mkv` and `mkv` are treated identically — `config.rs` strips a leading
`.` on load, in memory only (on-disk file is never rewritten by the
read). Canonical documented form stays the bare extension.

### Series selection: interactive by default, explicit when scripted — `resume`/`start`
No explicit series argument + a real TTY → a real interactive arrow-key
selector (↑/↓, Enter), not "type a number." The one place in the tool
where losing scriptability is the right tradeoff — it's the daily
one-command reflex path. But the picker must never be the *only* way to
complete the action: both commands also accept an explicit series name
(with `--root` to disambiguate, same mechanism as `exclude`/`pause`/
`drop`), always usable from a script. Non-TTY with no explicit series:
never hang — list choices with indexes and exit without acting (non-zero
exit). An explicit series argument always bypasses the picker, TTY or
not.

### `current` was cut — folded into `status`
Added no information `status` can't already show (one row for a single
`in_progress` series, a full list plus a note otherwise). Cut entirely
rather than kept as a redundant wrapper.

### Anitomy-candidate confirmation stays list-only, not interactive
Bare `tsuzuki exclude` lists suggested candidates plus the exact
`tsuzuki exclude "<path>"` command for each — no interactive walkthrough,
no separate indexed `--confirm` command. Re-running with an explicit path
reuses Phase 2's already-validated logic rather than adding new
cross-invocation state (an index from a prior listing would only be valid
if nothing changed on disk in between — a guarantee this project avoids
relying on).

### Clippy's strict gate is scoped to `-p tsuzuki --no-deps`, not `--workspace`
`vendor/anitomy` is a workspace member (needed for `cargo build`) but is
off-limits code — never held to Tsuzuki's own lint bar. `cargo build
--workspace` still checks the whole thing compiles; only the `-D
warnings` clippy gate is scoped down. See COMMANDS.md for the exact
command.

### Threshold precedence when position + completion checks overlap (short files)
On a short enough file, a single position can satisfy both
`resume_seconds` (noise) and `completion_remaining_seconds` (near-end) at
once. `mpv.rs`'s `classify()` has the position check win → `StartFromZero`,
never `Completed`. Reasoning: the wrong-call-cost is asymmetric (falsely
claiming progress vs. a possible few-second rewatch), and the
independent absence/duration completion signal still catches these cases
separately. A judgment call, not a design.md spec — a one-line change in
`classify()` if real usage disagrees.

**`start`'s listing has three distinct display words, one per state.**
Never-started (no record) → **"Start."** Paused → **"Resume"** (real
saved position). Dropped (manual, or the one detectable "finished the
whole series" auto-signal) → **"Rewatch?"** — manual-drop and
detected-completion collapse to the same display/state; the check is only
"does a record exist and in what status," never why. A `paused` record
must never show "Rewatch?". No fourth `completed` state exists — see next
section for why one isn't reliably detectable most of the time.

### Completion detection has a real blind spot for `watch_later` alone — absence of a bookmark is never, by itself, proof of anything
mpv's own `--resume-playback` (on by default) deletes a file's bookmark
the instant it's *loaded*, regardless of what happens next — and never
rewrites one for a file left mid-playback via a playlist command before
being quit while still loaded. So "no entry for the current episode" is
produced *identically* by a genuine completion and an ordinary abandon
(skip to a different episode, quit under `resume_seconds` there or none
at all) — provably indistinguishable from `watch_later` state alone. An
earlier rule ("absence + a real prior position ⇒ complete") silently
discarded real abandoned-episode progress and could auto-`Drop` a whole
series off a one-minute peek at its finale.

**The only signals that ever justify moving the cursor or status: a real
(≥`resume_seconds`) `Resume` position found anywhere forward, or a
`Completed` classification found anywhere forward.** Both are positive
readings from an entry that still exists. Absence contributes nothing —
not weighed, not a tiebreaker. This widens the accepted "clean binge
leaves no trace" blind spot: a series finished with zero further activity
afterward no longer auto-resolves to `Dropped` — it stays put until an
explicit `drop` or genuine forward evidence. The ordinary way people
finish an episode (quitting on/near it) still produces a `Completed`
reading before deletion can happen, so normal auto-advance/auto-drop is
unaffected. Closing this from `watch_later` alone would require real-time
playback observation, which this project chose not to build.

**Closable, not from `watch_later`, but from the optional companion
script:** its `reason=="eof"` signal (unioned into `reconcile.rs`) is
treated as unconditional `Completed`, so a clean binge with the script
installed now does auto-advance/auto-drop correctly. This doesn't weaken
the base rule — for a file with no script-log entry, absence is still
never proof of anything.

### When the CURRENT tracked episode itself completes: advance to the next one, or auto-`Dropped` if there is no next one
This is automatic in this one narrow, evidence-based case only — not the
fuzzy inference §2 rules out, since it only fires on a direct, confirmed
signal (the actual current tracked file genuinely completing).

When a `Completed` classification is found anywhere in the forward scan
from the current tracked episode on, reconcile resolves the cursor one
step further, not just records the fact:
- **Next episode exists** → cursor advances to it at position 0 (not a
  claim it was watched — same forward-looking default as a brand-new
  series' episode 1). Leaving the cursor on a now-finished episode would
  walk straight back into the freeze-at-the-end bug already fixed once.
  If that next episode separately has its own real bookmark already
  (binge), the ordinary forward-scan logic still applies from there.
- **No next episode** (completed one was the actual last file) → series
  auto-transitions to `Dropped`, no manual `drop` needed — this is the
  mechanism behind `start`'s "Rewatch?" display.

**Applies identically when no `state.toml` record exists yet.** A
never-tracked series whose only observed activity is a `Completed`
reading (including script `eof` — exactly the "clean first-time binge,
script installed" case) needs a cursor to advance; since none exists, a
placeholder record at index-0 (the same baseline `Dropped`/never-started
records already share) is created purely as the landing spot this rule
then resolves — so a first-time single-episode binge correctly lands
`Dropped`, and a multi-episode one lands `InProgress` at episode 2. A
genuinely `Dropped` record is excluded from this — it stays a dead end
only a real `Resume` reactivates.

### `pause`/`drop` never zero the stored position — reconcile's diffing depends on it
`drop`/`pause` never touch mpv's own `watch_later` file, so the real
bookmark is still there unchanged afterward. If they zeroed the stored
position, reconcile's stored-vs-mpv diff would manufacture a fake
mismatch and read it as new activity — silently undoing the drop on the
next invocation with zero user action. "Start fresh" for a
dropped/reactivated series is handled entirely at playback-resolution
time (index 0), never by mutating what's stored.

### Forward-scan is strictly forward — a paused series doesn't reactivate from an earlier real bookmark
A real bookmark on an episode *earlier* than a series' known position
must never reactivate or move a `Paused` record — reconcile only scans
forward. Otherwise an unrelated nostalgic rewatch of an early episode
could silently corrupt a paused position much further along. `Dropped`
and never-started records use an index-0 baseline, so by definition
nothing counts as "behind" them.

### Cursor movement: noise never touches saved state, completion check is per-series, a single run chains fully
- **A sub-`resume_seconds` reading is invisible to `state.toml`, in both
  directions** — never creates a record, never overwrites an existing
  real one. mpv has no concept of Tsuzuki's threshold: if a series has a
  real 15-minute position and the user briefly reopens and quits at 45s,
  mpv's own file now says 45s. Blindly propagating that would let one
  accidental short peek erase real progress — the same category of
  mistake §2's MAL rule guards against, applied locally. Only another
  real observation can move a position.
- **Completion's one real signal — `classify()`'s duration-based
  `Completed` result — is checked across the whole forward scan**, not
  just the current episode; absence is never a second signal alongside it
  (see above). Duration probing only runs on entries with a real
  position and costs ~212µs release — not a caching concern even across
  a multi-step chain.
- **Per-series, not "check the single freshest file"** — multiple series
  can be `in_progress` at once, so each is forward-scanned independently
  for its own `Completed` reading.
- **A single invocation resolves however many chained transitions
  happened since the last check** — binge-watching (episode 1 completing
  *and* the user already minutes into episode 3) must resolve in one
  call, not one step per call.
- **An absent OS-default candidate directory stays silent (normal); an
  explicit `watch_later_dir` override that doesn't resolve must be
  surfaced.** The override is something the user explicitly typed — if it
  doesn't exist, silence would hide a likely mistake indefinitely. The
  scan result carries a warning as data (this module has no
  command-facing output of its own); the first real command layer is
  responsible for displaying it — don't let `doctor` end up the only
  place it ever surfaces.

### Completion threshold needs episode duration — mpv's watch_later files don't have it
`watch_later` entries only ever store position, never duration. No
external program (`ffprobe` shell-out contradicts local-first/startup-
latency) — use a compiled-in Rust dependency instead. `symphonia` (pure
Rust, MPL-2.0) reads the container header via content-probe for
metadata/probe only, never decoding frames; covers MKV/WebM/MP4/MOV, not
AVI. **Dispatch is by content-probe, never by file extension** —
`video_extensions` is open-ended and user-configurable, so an
extension-keyed table would silently stop covering new formats. Only if
Symphonia's probe fails to identify the file at all does a small
hand-written AVI `avih` RIFF-header fallback get tried, still not
extension-gated. **Unreadable duration is surfaced with a reason, never
silently dropped to `None`** — but only skips the completion check for
that file (resume-from-position is unaffected).

### auto_pause_days defaults to 0 (never auto-pause)
Deliberate. A nonzero default would guess intent (break vs. abandoned vs.
waiting for episodes to air) — the inference this project avoids
everywhere else.

### `state.toml` record identity, root reference, and `updated_at`
Keyed by **both** tracked root and literal series name — never name
alone, since composable roots allow the same name under two roots with
independent progress. The root reference stores that root's own
persisted literal config form (same mechanism as exclusions), never a
resolved path. Each record carries `updated_at`, needed for
`auto_pause_days` to mean anything — it **only moves on a genuine,
persisted state change**, never on a mere scan or a sub-`resume_seconds`
observation.

### `pause`/`drop`: root disambiguation, no-record errors, and the allowed transitions
Same `--root` disambiguation as `exclude`. **Both error, rather than
silently creating a record, when the target has no real progress to act
on** — covers "never started" and "already dropped" identically, one
rule not two. **Allowed transitions:** `in_progress → paused`,
`in_progress → dropped`, `paused → in_progress`, `paused → dropped`.
`dropped → paused` does not exist — dropped is a dead end reachable only
by opening an episode directly (→ `in_progress`, never through `pause`).
Re-invoking `pause`/`drop` on an already-`paused`/`dropped` series is
idempotent (exit 0, no-op).

### Launching mpv: fire-and-forget, cheap existence check before spawn, install-method detection
Spawning doesn't block waiting for mpv to exit, and doesn't re-scan
`watch_later` on exit — reconciliation happens on the next invocation
like any other read. **Native vs. Flatpak is resolved by a cheap
existence check (PATH lookup), never attempt-and-catch a failed spawn** —
a failed spawn is real wasted process-launch overhead against the
startup-latency rationale. `[mpv] command_override` (empty = auto-detect)
lets a user with both installed pin the exact command, skipping detection
entirely. Unresolvable or a failed spawn is a plain command error, never
a panic.

### Every state-touching command reconciles against current mpv reality first, through one shared function
`list`, `status`, `pause`, `drop`, and the resume/`start` picker all run
the same "resolve `state.toml` against what mpv currently says" step
first, through one shared function — guarantees identical behavior by
construction rather than five commands agreeing today and drifting apart
later.

### `list` ordering and episode-number display
Grouped by status — `in_progress`, `paused`, `dropped`, never-started —
alphabetical within each group, never "whatever iteration order falls
out." Episode number uses Anitomy's parsed number when available, falling
back to ordinal position.

### Output convention: warnings on stderr, real output on stdout; non-completion exits non-zero
Matches the ripgrep/cargo convention. A command that *informs* without
*completing* the requested action (e.g. a non-interactive picker listing
choices without picking one) exits non-zero, so `tsuzuki && next-thing`
correctly sees nothing was resumed.

### Every phase needs at least one real-binary, real-XDG-path integration test
Every test validated internal functions against a redirected temp path —
none invoked the compiled binary against real `dirs::config_dir()`/
`dirs::data_dir()` resolution. A real bug in path resolution could pass
every unit test while breaking 100% of real usage. At least one test per
phase touching persistence spawns the real binary
(`env!("CARGO_BIN_EXE_tsuzuki")`) against a genuine temp `$HOME`/XDG
environment — required going forward for any phase reading/writing a
real on-disk file.

### MAL `client_id` is a built-in, publicly committed credential — not a per-user registration requirement
`mal::DEFAULT_CLIENT_ID` in `src/mal.rs` is committed to source in the
clear, on purpose. Tsuzuki's OAuth flow is Authorization Code + PKCE for
a public/native-client app type — MAL doesn't treat `client_id` as
confidential for this type, only `client_secret` is, and there is no
`client_secret` field anywhere in this codebase (never will be — PKCE is
chosen specifically so one isn't needed). A `client_id` alone can't
authenticate the app; it only starts a flow that still needs a real user
login/approval plus a fresh PKCE `code_verifier`. Precedent: Taiga
(erengy/taiga) commits its own `client_id` the same way. **Never
"helpfully" redact, rotate, or remove this as a leaked credential**
without re-reading this section — that would regress to requiring every
user to register their own MAL app, which is the wrong tradeoff this
already corrected once. `[mal] client_id` in `config.toml` is an optional
override (empty → `DEFAULT_CLIENT_ID` via the single `mal::resolve_
client_id` seam every caller goes through); `sync login`/`doctor` work
against a completely untouched config.

If `DEFAULT_CLIENT_ID` is ever rotated, verify the new registration is
genuinely secret-less before committing it — MAL's "web" app type demands
a `client_secret` even when PKCE parameters are sent; only a properly
registered secret-less app type works. See `DEFAULT_CLIENT_ID`'s doc
comment in `mal.rs` for the exact (safe, no-account-risk) verification
method.

### MAL link storage, `mal_episode_awareness`, and the field-name trap
Three decisions, all in `src/sync.rs` (module doc comment has the full
reasoning):
- **A series → MAL-id link lives in `state.toml`, not `config.toml`**, as
  `[[mal_links]]` keyed like `SeriesRecord`. The deciding question is
  authorship, not portability — a link is always Tsuzuki-derived/written
  (exact-match auto-link or `sync link`), same category as
  `SeriesRecord` itself.
- **`SeriesRecord.mal_episode_awareness` is a separate field from
  `episode_number`, never reused** — `reconcile.rs` unconditionally
  overwrites `episode_number` from local file identity on every pass, so
  a MAL-driven number stored there would vanish next command. The merge
  comparison's "local best-known" (`sync::local_effective_episode`) takes
  the higher of `mal_episode_awareness` and the *watched* count derived
  from `episode_number` (see "MAL push number is the last watched
  episode, not the tracked cursor" below — it's not the raw
  `episode_number` itself) — without folding a prior pull back in, a
  later run could re-read a stale lower local number and push it back
  down over a number MAL already has.
- **MAL's `my_list_status` field name is asymmetric, a real trap**: a
  list/anime response reports `num_episodes_watched`; the `PATCH
  /v2/anime/{id}/my_list_status` request body field is
  `num_watched_episodes` (words swapped, confirmed against Taiga's source
  and the unofficial MAL API spec). Getting this wrong is silent — MAL
  ignores an unrecognized field rather than erroring, so the episode
  count would just never update. Re-verify this exact swap first if
  `push_episode_count` is ever touched.

### MAL push number is the last watched episode, not the tracked cursor
Found in Phase 9 dogfooding: `SeriesRecord.episode_number` is a *cursor* —
the episode currently being watched (a real mid-episode position), or the
one just advanced to at position 0 after the previous episode completed
(see "When the CURRENT tracked episode itself completes" above) — never
a confirmed-finished count. MAL's own field means "episodes already
watched," so pushing the raw cursor number overstated progress by one:
mid-episode 9 was reported to MAL as 9 watched, not 8.
`sync::local_effective_episode` now feeds the merge
`episode_number.saturating_sub(1)` in the ordinary case, before folding
in `mal_episode_awareness` as before.

**One narrow, evidence-based exception, not a special case bolted on:**
`apply_completion_advance` (`reconcile.rs`) has no next episode to
advance a just-completed series to, so it leaves the cursor sitting on
the finale it just confirmed complete and transitions straight to
`Dropped` — the one state where the cursor's own episode genuinely *is*
the watched count already, not one short of it. This is identifiable
without guessing, from data already on the record: `Dropped` with a real
(nonzero) `position_seconds` is exactly that path's own signature — the
same nonzero near-the-end position `classify()` used to confirm
completion in the first place. A `Dropped` record with `position_seconds
== 0.0` never touched its cursor episode at all (auto-advanced to a
fresh next episode, then dropped before opening it — or dropped at the
index-0 baseline) and still gets the ordinary `-1`. `Paused` gets the
ordinary `-1` unconditionally — pausing never implies the paused-on
episode finished.

This still doesn't perfectly resolve every case (e.g. a manual
`tsuzuki drop` moments after quitting meaningfully-but-not-fully into an
episode still reads as "watched" under the `Dropped`-with-nonzero-position
rule) — an accepted, narrow edge case in the same spirit as §7's already-
documented local/MAL numbering-drift gap in `docs/design.md`, not
something worth a new persisted "was this episode actually completed"
field for.

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
