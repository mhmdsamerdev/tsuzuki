# Tsuzuki — Design Document

> *Continue where you left off.*

---

## 1. Overview

**Tsuzuki** — from the Japanese "続き," meaning "continuation" — is a local-first CLI tool that remembers what you were watching and lets you pick up exactly where you left off, with one command.

It is not a media player, not a media library manager, and not a MyAnimeList replacement. Those problems are already solved. Tsuzuki exists to answer one narrow question that nothing else quite does: *what was I watching, where did I stop, and can you take me back there instantly?*

You watch an episode in mpv, quit, and forget about it. Tomorrow, you type `tsuzuki`. The right file opens, at the right timestamp. No searching your folders, no remembering episode numbers, no thinking. The tool's entire job is to remove that small daily friction — nothing more, nothing less.

---

## 2. Philosophy

**Local-first.** The local machine is the source of truth. No cloud, no required accounts, no internet connection needed for the core experience. Everything keeps working offline.

**CLI-first.** The command line is the primary interface. Anything else — status bars, launchers, desktop integrations — is an optional layer built on top, never a requirement.

**Filesystem-first.** The user's filesystem is already their media library. Tsuzuki never tries to replace or reorganize it — it only remembers progress against structure that already exists.

**Explicit over inference.** This is the principle everything else in this doc actually enforces. Tsuzuki never guesses — not which folders matter, not which files are episodes, not which series a filename belongs to. Every point where the tool could be "smart" is deliberately replaced with a small, one-time, explicit action from the user instead. A little manual setup is the accepted cost of the tool staying simple, predictable, and honest everywhere else.

**Boring over clever.** Every design decision favors a fixed, obvious rule over an adaptive or heuristic one — flat thresholds instead of percentages, natural sort instead of content parsing, directory structure instead of classification. Boring is a compliment.

**Unix philosophy.** Tsuzuki does one job well and composes with other tools. It doesn't try to absorb the responsibilities of mpv, MyAnimeList, or a file manager — it sits between them.

---

## 3. Core Concepts

**Tracked roots.** Tsuzuki only knows about directories the user explicitly registers:

```bash
tsuzuki add ~/Anime
```

Anything outside a tracked root — a family video, a tutorial, a one-off download — is invisible to Tsuzuki by construction, not by detection. There is no content classification anywhere in the system.

**Series identity.** A series is an immediate child directory of a tracked root — no deeper, no shallower, no classification involved:

```
Anime/
├── Monster/
│   └── [1080p HEVC][Crunchyroll]/
│       └── ep01.mkv
├── One Piece/
│   ├── Season 1/
│   │   ├── ep01.mkv
│   │   └── ep02.mkv
│   └── Season 2/
│       └── ep01.mkv
```

Everything below depth 1 is opaque to Tsuzuki as a *category* — release-tag folders, season folders, whatever a release group or the user named things. Season folders are not a special case; they're just another layer below depth 1, exactly like a release-tag folder. `One Piece/Season 1` and `One Piece/Season 2` are one series with one continuous cursor, not two — "continuing One Piece" is one thing to the user regardless of how files are split across folders.

**Series naming.** The series name is the directory name itself, verbatim — no parsing, no bracket-stripping. People reliably name the *top-level* show folder sensibly; the noisy names live deeper, where Tsuzuki never has to read them.

**Composable roots.** If a folder structure has an extra layer of categorization the depth-1 rule can't see through — a `Rewatches/` folder, for instance — the fix is registering that layer as its own root:

```bash
tsuzuki add ~/Anime
tsuzuki add ~/Anime/Rewatches
```

The same rule applies independently to each registered root. `Anime/Monster` and `Anime/Rewatches/Monster` become two distinct tracked series with separate progress — correct, since first-watch and rewatch progress shouldn't share a cursor.

**Episode ordering within a series.** Files are ordered by a full recursive natural sort of the path tree — directories compared naturally against siblings first, then filenames within each directory, also naturally. This is what makes `Season 1/ep02` correctly precede `Season 2/ep01` without Tsuzuki ever knowing what a "season" is. (Full detail in §5.)

**Exclusions.** Some files or folders structurally belong under a series directory but aren't part of its continuation — OVAs, specials, side movies. These aren't detected automatically; there's no reliable filesystem signal for "this is a special," since that's a fact about the story, not the file. Instead, the user marks them explicitly, using a path relative to a tracked root:

```bash
tsuzuki exclude "One Piece/Specials"
# resolves to ~/Anime/One Piece/Specials
```

Excluding a directory excludes everything under it, recursively. Excluding a single file covers a stray file sitting alongside real episodes. If the same relative path exists under more than one tracked root (possible with composable roots), Tsuzuki doesn't guess — it lists the matches and asks the user to specify which one. Either way, excluded content is skipped by ordering and "next episode" logic entirely, but remains playable manually and still trackable if opened directly.

Running `tsuzuki exclude` with no path surfaces Anitomy-suggested candidates (see §5) — files whose parsed type looks like OVA/Special/Movie — for the user to confirm. Nothing is ever excluded automatically.

---

## 4. Playback Tracking

Tsuzuki doesn't launch mpv itself or hold an IPC socket to observe playback. mpv already solves the position-tracking problem: whenever a file is quit mid-playback, mpv writes a `watch_later` entry containing the resume timestamp, regardless of how it was launched — terminal, file manager, or Tsuzuki.

**Prerequisite: `write-filename-in-watch-later-config` must be enabled.** By default, mpv identifies each entry only by an MD5 hash of the file's path — a one-way hash, unrecoverable. Tsuzuki requires one line in the user's `mpv.conf`:

```
write-filename-in-watch-later-config=yes
```

With this set, every entry gets a comment line containing the real path, followed by the position:

```
# /home/user/Anime/Dungeon Meshi/Dungeon Meshi - S01E03.mkv
start=565.231000
```

This is a one-time setup step, checked (not assumed) by `tsuzuki doctor` — if the flag is missing, `doctor` reports it plainly.

Tsuzuki's read step:

1. Scan the `watch_later` location for entries.
2. **Skip redirect entries.** Opening a directory or playlist in mpv (e.g. right-click → open with mpv) produces intermediate entries containing only `# redirect entry`, no real position — these are internal pointers, not playback state, and are filtered out.
3. Parse the path from the comment line and the timestamp from `start=`.
4. Filter to only paths under a tracked root.
5. Take the most recently modified matching entry — that's "what you were watching."

Scoping does the filtering in step 4 for free. A file outside every tracked root still gets a `watch_later` entry from mpv, but Tsuzuki simply ignores it.

**Location varies by OS and install method.** Rather than detecting the environment, Tsuzuki checks a fixed list of known candidate paths and merges by freshest-file-wins:

- **Linux (native):** `~/.local/state/mpv/watch_later/` (mpv ≥ 0.33), `~/.cache/mpv/watch_later/` (older)
- **Linux (Flatpak):** the same two paths under `~/.var/app/io.mpv.Mpv/...`
- **Linux (Snap):** `~/snap/mpv/current/.local/state/mpv/watch_later/`
- **macOS:** same XDG-style paths as native Linux, unverified until targeted
- **Windows:** `%APPDATA%\mpv\watch_later\`, unverified until targeted

A manual override (`watch_later_dir` in config) covers users who've customized `watch-later-directory` in their own `mpv.conf`. v1 targets Linux only. A `tsuzuki doctor` check also flags Flatpak mpv's potential filesystem-permission gap (may not have access to tracked roots unless the user granted broad home access).

**Resume threshold.** A saved position under **2 minutes** is treated as noise (an accidental open, not a real bookmark) — playback starts from 0 instead of resuming. At or above 2 minutes, playback resumes exactly at the saved position.

**Completion threshold.** An episode is marked completed — advancing the cursor — when the remaining time is **3 minutes or less**. This combines with a free secondary signal: mpv doesn't write a `watch_later` entry at all when a file was played essentially to the end, so the *absence* of an entry for a file that was clearly opened is treated as completion too.

Both thresholds are fixed numbers, not percentages, and both are configurable (§8).

**VLC is not supported in v1.** Its resume feature is opt-in (must be manually enabled in VLC's own preferences), stores position in a shared recents file with a different format per OS (including a binary plist on macOS), and has a known reliability gap at the start of playback. If added later, it would plug into the same abstraction mpv uses — a source yielding `(path, timestamp)`.

---

## 5. Episode Ordering

**The rule: hierarchical natural sort of the full relative path tree.**

Plain alphabetical sort fails past 9 episodes — `"Episode 10"` sorts before `"Episode 2"` under naive character comparison. **Natural sort** compares embedded numeric runs as numbers, not characters — the same mechanism behind `ls -v` and most file managers.

Applied hierarchically — directories compared naturally against siblings first, then files within each directory, depth-first:

```
One Piece/
├── Season 1/
│   ├── ep01.mkv
│   └── ep02.mkv
└── Season 2/
    └── ep01.mkv
```

Order: `Season 1/ep01`, `Season 1/ep02`, `Season 2/ep01` — one general rule, no season-specific logic.

**Natural sort remains the sole authority for order.** It's deterministic and never fails, which is why it stays load-bearing.

**Episode number vs. episode order.** These are usually the same thing but not always — a series missing a downloaded episode (files 1, 2, 3, 5, 6 — no 4) has five files in correct *order*, but the file at position 4 is really episode 5. Ordinal position alone can't see this gap.

**Anitomy closes that gap, without replacing ordering.** For each file, Anitomy (`anitomy-rs`, a pure-Rust port — no C++ toolchain dependency) attempts to parse the actual episode number from the filename. When it succeeds, that number is treated as the file's true episode identity, used wherever an actual episode number matters — chiefly MAL sync (§7). When Anitomy fails to parse a file, ordinal position is the fallback. Ordering itself is never decided by Anitomy — it only annotates files already correctly ordered by natural sort.

**Fallback for non-numeric names.** Files or folders with no embedded number fall back to plain alphabetical ordering among themselves.

**Exclusions are applied before ordering.** Anything marked via `tsuzuki exclude` is removed from the tree entirely before the sort runs.

**What this deliberately doesn't attempt:** any understanding of narrative order or content beyond bounded, single-file episode-number extraction. It trusts ascending path order matches watch order — true the overwhelming majority of the time. When it isn't, the fix is an exclusion or manual correction, not a smarter sort.

---

## 6. Cold Start

Tsuzuki's premise is continuation — but that means it has nothing to offer the first time a series is watched. Left unaddressed, this would force the user into a file explorer just to begin anything new.

**Resolution: `tsuzuki start`** — a listing of series outside the automatic resume flow, not a discovery feature:

```bash
tsuzuki start
```

```
Available to start:

  Apothecary Diaries   (12 files, never started)
  Monster               (74 files, paused at ep. 9)

Start which one?
```

Covers three cases, unified by "not currently in the automatic `in_progress` picker":

- **Never started** → opens at episode 1.
- **Dropped** → same as never-started, since progress was reset (§8).
- **Paused** → resumes at its saved position, not episode 1 — the one case where `start` genuinely resumes rather than begins fresh.

No metadata, no posters, no scraping — just a filtered enumeration of directory names and state Tsuzuki already has.

**The scope boundary this sets:** Tsuzuki removes the friction of *continuing*, not the friction of *choosing*. Deciding what to start next is a human decision; what's removed is the need to leave the terminal to locate the option.

---

## 7. MyAnimeList Sync

Sync is optional — nothing about core tracking or resuming depends on MAL being configured.

**Prerequisite: one-time OAuth authorization.** `tsuzuki sync login` walks through MAL's OAuth flow once.

**Linking a series without manual IDs.** Matching against MAL's global database would be open-ended inference; instead, Tsuzuki matches against the user's own MAL list, which returns each entry's main title, English title, Japanese title, and any synonyms.

1. First `sync` touching a given series: fetch the user's MAL list.
2. Normalize both sides (case, brackets, whitespace) and check for an **exact match** between the local folder name and *any* of an entry's known titles — not fuzzy similarity against one title.
3. **Match found** → link automatically, cache the ID locally.
4. **No exact match** → show the closest candidates by title (similarity ranks what's *displayed*, never decides a match) — user picks one. That folder name is saved as a new synonym, so it's an exact match next time.
5. **Nothing on the list** → tell the user plainly; they add it on MAL, or use `tsuzuki sync link <series>` as a manual override.

**Two-way, non-destructive merge.** A push-only sync is harmful — if the user watched ahead elsewhere and updated MAL directly, a naive push would overwrite MAL *backward*. The rule:

> **Take the higher episode number, in either direction. Never overwrite down.**

- MAL ahead of local → local awareness updates to match. No timestamp/position is implied for episodes Tsuzuki never observed.
- Local ahead of MAL → push via `PATCH /v2/anime/{id}/my_list_status`, field `num_episodes_watched`.
- Equal → no-op.

The pushed number uses Anitomy-parsed episode identity when available (§5), ordinal position otherwise — keeping sync accurate for series with numbering gaps.

**This directly answers "forced to click through episodes."** If MAL says episode 12 but local history only knows up to 9, Tsuzuki doesn't make the user play through 10–11 to catch up — it trusts the higher number and treats 13 as next. If the user wants to actually rewatch 10–11, they open those files directly in mpv, already a fully legitimate way to interact with the system.

`pause`/`drop` (§8) can optionally mirror to MAL's own `on_hold`/`dropped` statuses, keeping both sides consistent.

**Known unsolved edge case:** local and MAL numbering can still diverge beyond what Anitomy catches — split-cour renumbering, dub/sub differences. The local-to-MAL correspondence is fixed at link time; further drift is a manual re-link.

---

## 8. Configuration

Every tunable value in this doc is genuinely configurable — a fixed default that can't be changed is inference wearing a different disguise.

**Note on this section:** the underlying *values* (2-minute resume threshold, 3-minute completion threshold) and the *principle* that everything should be overridable were established earlier in the design process. The file format, location, field names, and `tsuzuki config` subcommands are formalized here.

**Config file — the source of truth**, following XDG convention:

```
~/.config/tsuzuki/config.toml
```

TOML, not JSON — with 8+ tunable values, a hand-editable file benefits from inline comments, which JSON can't express. TOML is also what Cargo itself uses.

```toml
# ~/.config/tsuzuki/config.toml

[thresholds]
resume_seconds = 120                 # below this, restart from 0 instead of resuming
completion_remaining_seconds = 180   # this close to the end counts as watched
auto_pause_days = 0                  # 0 = never auto-pause. Nonzero = pause after N idle days

[roots]
paths = ["~/Anime", "~/Anime/Rewatches"]
video_extensions = ["mkv", "mp4", "avi", "webm", "mov"]  # only these extensions count as episodes when building a series' ordering tree

[mpv]
watch_later_dir = ""                 # empty = use OS-default candidate list

[mal]
client_id = ""
```

**`tsuzuki config` is a thin CLI layer over the same file** — same relationship git has to `.gitconfig`:

```bash
tsuzuki config get resume_seconds
tsuzuki config set resume_seconds 90
tsuzuki config list
tsuzuki config edit
```

**Series status and the resume picker.** A series with a saved position is `in_progress` by default. Three explicit states, no silent expiry:

- **`in_progress`** — appears in the bare `tsuzuki`/`resume` picker.
- **`paused`** (`tsuzuki pause <series>`) — position kept, removed from the picker, appears in `tsuzuki start` at its saved position. Opening any episode directly in mpv naturally re-populates progress and pulls it back to `in_progress` — no separate "unpause" command needed.
- **`dropped`** (`tsuzuki drop <series>`) — position reset. Reappears in `tsuzuki start` exactly like a never-started series.

**No default auto-expiry.** `auto_pause_days = 0` means an `in_progress` series never automatically loses that status, no matter how long it's untouched — a fixed timeout would be a guess about intent (abandoned vs. on a break vs. waiting for more episodes to air) that this design avoids everywhere else. If the user opts in by setting `auto_pause_days` to a nonzero value, an idle `in_progress` series is automatically moved to `paused` — never `dropped`, since that's destructive and stays a manual-only action regardless of config.

**The resume picker itself:**

- **Exactly one `in_progress` series** → bare `tsuzuki` resumes it directly, no picker — preserves the one-command demo.
- **More than one** → shows a short choice list.
- **Zero** → falls through to `tsuzuki start` behavior.

**Config vs. state — two separate files.** Config is what a human tunes; state is what Tsuzuki writes as it runs. State lives at:

```
~/.local/share/tsuzuki/state.toml
```

A single flat TOML file — not sharded per-series (mpv shards because it tracks an unbounded, unpredictable filesystem-wide set; Tsuzuki's state is small and bounded, dozens of series at most, cheap to fully parse on every invocation) and not SQLite (no real concurrent access, no transactional need, and a binary file works against the "Data Ownership" principle of staying inspectable). Written **atomically** — new content to a temp file, then renamed over the original — so a crash or kill mid-write can never leave a corrupted half-written state file.

---

## 9. Commands Reference

| Command | Purpose |
|---|---|
| `tsuzuki` | Resume the current series. Single `in_progress` series → resumes directly. Multiple → shows a picker. None → falls through to `start` behavior. |
| `tsuzuki resume` | Explicit alias for the above. |
| `tsuzuki status` | Show what's currently `in_progress`. |
| `tsuzuki current` | Show which series bare `tsuzuki` would resume right now. |
| `tsuzuki list` | List all tracked series and their progress/status. |
| `tsuzuki add <path>` | Register a tracked root. |
| `tsuzuki start` | List series outside the automatic picker — never-started, dropped (open at ep. 1), and paused (resume at saved position). |
| `tsuzuki pause <series>` | Remove from the resume picker, keep saved position. |
| `tsuzuki drop <series>` | Reset progress; series reappears in `start` as if new. |
| `tsuzuki exclude [path]` | Exclude a file or directory from ordering. Bare invocation surfaces Anitomy-suggested candidates for confirmation. |
| `tsuzuki sync login` | One-time MAL OAuth authorization. |
| `tsuzuki sync link <series>` | Manually resolve a series-to-MAL-ID mapping when exact-title matching fails. |
| `tsuzuki sync` | Run the two-way, non-destructive sync against MAL. |
| `tsuzuki config get/set/list/edit` | Read or write config values, or open the config file directly. |
| `tsuzuki doctor` | Environment health checks — mpv config flag, watch_later reachability, Flatpak permissions, tracked-root validity, MAL auth status. |

**CLI invocation contract.** Every command group with subcommands
(`config`, `sync`) must have an explicit, documented behavior for being
invoked with no subcommand — never left to clap's generic "subcommand
required" error, which is accurate but reads as an accident rather than a
decision. Two cases:

- **The group already has a specified default action** — `tsuzuki sync`
  with no subcommand *is* "run the two-way sync" (above); `login` and
  `link <series>` are additional subcommands alongside that default, not
  the only valid ways to invoke `sync`. The bare form must remain
  reachable in the CLI structure, not accidentally blocked by requiring
  a subcommand.
- **The group has no single default meaning** — `tsuzuki config` bare
  (none of `get`/`set`/`list`/`edit` is "the" default) prints help and
  exits 0.

This does not extend to malformed input — unknown flags, missing
required positional arguments, unknown subcommand names all keep clap's
own error and usage output. That output is already accurate and matches
the convention of the wider Rust CLI ecosystem (ripgrep, cargo, fd); the
contract above only closes the one gap where clap's default reads as
unintentional rather than as a real design decision.

---

## 10. Tech Stack & Distribution

**Language: Rust.** The deciding factor is startup latency, not raw computation — nothing here is compute-bound. A compiled Rust binary starts in low single-digit milliseconds versus Python's fixed interpreter overhead, felt on every invocation of a tool whose entire pitch is a one-command reflex action. Matches the project's own inspiration set (ripgrep, fd, zoxide, atuin), and its type system reinforces the exhaustive, explicit-states modeling this design already leans on throughout.

**Key dependencies:** `anitomy-rs` (pure Rust, no C++ toolchain needed), `serde`/`toml`, an HTTP client for MAL's API, a natural-sort comparator.

**Testing:** `cargo test`, built into the toolchain. `assert_cmd` for end-to-end command tests against the compiled binary; `tempfile` for fabricated directory structures. Most of the suite is close to mechanical, since this doc's rules are already stated as explicit boundary values.

**Distribution — prebuilt binaries as the primary path, not `cargo install`.** `cargo install` only reaches people with the Rust toolchain already installed — a small subset of this tool's actual audience. The path that matches: **`cargo-dist`**-generated GitHub Release binaries plus a one-line install script (`curl ... | sh`, the same pattern as rustup and Homebrew). `cargo install`/crates.io stays available as a secondary path; a `maturin`-built PyPI wheel offers a third door (`pip install tsuzuki`) using the same precompiled binary, no client-side compilation in any path. A Homebrew tap once there's adoption.

**Platform scope for v1: Linux only.** OS-candidate-path logic is parameterized from day one, but macOS/Windows remain unverified until targeted.

---

## 11. Build Plan

**Phase 0 — Scaffolding & data model**
Cargo project, clap CLI skeleton with all subcommands stubbed (even if they just print "not implemented"), the two config/state files: config.toml (read/write, defaults, XDG path) and state.toml (atomic write via temp-file-then-rename). This is boring but everything else hangs off it, and it's small enough to verify in one pass.

**Phase 1 — Tracked roots & episode ordering**
tsuzuki add, series-identity-as-depth-1-directory, and the hierarchical natural sort over the path tree. This is pure filesystem logic with zero external I/O (mpv, network) — ideal for tempfile-fabricated directory trees in tests, and it's the algorithmic heart of the tool. Get this rock solid before anything depends on it.

**Phase 2 — Exclusions**
tsuzuki exclude, path resolution against tracked roots, the ambiguity case (same relative path under multiple composable roots), and wiring exclusions to run before the Phase 1 sort. Small, but genuinely separate logic from ordering itself.

**Phase 3 — Anitomy integration**
Wire in anitomy-rs, parse episode numbers, fallback to ordinal position, and the anitomy-suggested-candidates flow for bare tsuzuki exclude. Isolated because it's a single external dependency with a narrow job (annotate, never reorder).

**Phase 4 — mpv watch_later reading**
Candidate-path list per OS/install method, parsing entries (including skipping redirect entries), resume/completion thresholds, filtering to tracked roots. This is the first phase touching another program's on-disk format, so it deserves its own focused pass — and it's easy to test with fabricated watch_later files, no real mpv needed.

**Phase 5 — State machine & core commands**
This is the payoff phase: in_progress/paused/dropped, the resume picker (0/1/many logic), and tsuzuki / resume / status / current / list / start / pause / drop. Everything from Phases 1–4 gets tied together here into the actual one-command demo. I'd stop and manually dogfood after this phase — it's the point where the tool becomes real.

**Phase 6 — doctor**
Full environment checks (mpv config flag, watch_later reachability, Flatpak permission gap, tracked-root validity, MAL auth status once Phase 7 exists). Doing it last means it can genuinely check everything rather than being built incrementally and forgotten.

**Phase 7 — MAL sync**
OAuth login, exact-match linking against the user's own list, manual sync link, the two-way non-destructive merge, pause/drop mirroring. This is the highest-risk phase (network, external API, auth) — isolating it means a flaky OAuth flow can't destabilize everything you already validated.

**Phase 8 — Distribution & end-to-end tests**
cargo-dist config, install script, assert_cmd end-to-end tests against the compiled binary, PyPI wheel via maturin. Genuinely last — there's no point packaging a binary that's still changing shape.

---

## 12. Open Questions

*(none currently — resolved as they came up)*
