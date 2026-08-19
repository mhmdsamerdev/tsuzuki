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

Excluding a directory excludes everything under it, recursively; excluding a file covers a stray file alongside real episodes. If the same relative path exists under more than one tracked root, Tsuzuki lists every matching absolute path and the exact `--root` invocation to disambiguate — never a guess:

```
$ tsuzuki exclude "One Piece/Specials"
Ambiguous: "One Piece/Specials" exists under more than one tracked root:

  1. /home/user/Anime/One Piece/Specials
  2. /home/user/AltAnime/One Piece/Specials

Re-run with --root to specify which one:
  tsuzuki exclude "One Piece/Specials" --root ~/Anime
  tsuzuki exclude "One Piece/Specials" --root ~/AltAnime
```

`--root` stays script-friendly on purpose — this is a rare setup action, not the daily reflex path, so scriptability wins over interactivity here (contrast the resume picker, §8). Excluded content is skipped by ordering/"next episode" logic entirely, but remains playable manually and still trackable if opened directly. `exclude` validates the path exists before excluding, matching `add`'s validate-now behavior. Excluding an entire series folder is rejected — not tracking it, or a future `drop`, is the tool for that.

Bare `tsuzuki exclude` (no path) surfaces Anitomy-suggested candidates (§5) — files whose parsed type looks like OVA/Special/Movie — as a plain list plus the exact `tsuzuki exclude "<path>"` command for each, never an interactive walkthrough. Re-running with an explicit path is the confirmation step; there's no separate confirm/index command.

---

## 4. Playback Tracking

Tsuzuki's *tracking* doesn't depend on being the one that launched mpv — it only reads `watch_later` files after the fact, so it works identically whether a file was opened by Tsuzuki, a file manager, or the terminal. mpv already solves the position-tracking problem: whenever a file is quit mid-playback, it writes a `watch_later` entry with the resume timestamp, regardless of how it was launched. *Resuming*, however — `tsuzuki`/`resume`/`start` opening an episode — is a real, core feature: Tsuzuki spawns `mpv <file> --start=<seconds>` directly, fire-and-forget, then lets go. What's ruled out is holding an ongoing IPC socket or needing control over an already-running mpv instance — not launching mpv at all.

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

**Optional: a Tsuzuki-owned mpv companion script.** A small Lua script (`contrib/mpv/tsuzuki.lua`, installed by the user into mpv's own `scripts/` directory — the same launcher-independent loading mechanism as `watch_later` itself, and the same manual-one-time-setup shape as the `write-filename-in-watch-later-config` prerequisite above) can optionally give Tsuzuki a direct signal per file instead of an inferred one: mpv's own `duration` property (read straight from its demuxer, no local probing needed) and the real reason playback ended (`eof` — genuinely reached the end — distinct from `stop`, which is what skipping to another file produces). Strictly additive: a file with no script-log entry falls back to exactly the `watch_later`-based pipeline above, unchanged. Never required for Tsuzuki to work.

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

**Mixed level (a directory containing both subdirectories and its own loose files).** Directories are grouped and ordered before this directory's own files at the same level — not interleaved into one flat natural-sort pass. E.g. a series root with a stray OVA file sitting next to a `Season 1/` folder orders the `Season 1/` contents before the loose file, regardless of what natural-sort comparison between the directory name and the filename would suggest on its own. This follows directly from "directories... first, then files" — decided explicitly here since the worked example above doesn't exercise the mixed case.

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
- **Dropped** → same underlying mechanics as never-started (open at episode 1, opening any episode naturally re-populates progress), but the listing itself says "Rewatch?" rather than "Start," since a dropped series was, at some point, actually watched — including the one case Tsuzuki can detect a series was watched to its true end (its final episode's bookmark disappeared with nothing left to advance to; see §8). No new state or persisted marker exists for this — the display only checks whether a `state.toml` record exists at all for that series, not why.
- **Paused** → resumes at its saved position, not episode 1 — the one case where `start` genuinely resumes rather than begins fresh.

A series folder that's been moved, renamed, or deleted from a tracked root is indistinguishable to Tsuzuki from a fresh, never-tracked series appearing in its place — series identity is the literal directory name, with no path-history or rename detection anywhere (§3). Any prior progress becomes an orphaned record rather than following the move; see §8 for how that's handled.

No metadata, no posters, no scraping — just a filtered enumeration of directory names and state Tsuzuki already has.

**The scope boundary this sets:** Tsuzuki removes the friction of *continuing*, not the friction of *choosing*. Deciding what to start next is a human decision; what's removed is the need to leave the terminal to locate the option.

---

## 7. MyAnimeList Sync

Sync is optional — nothing about core tracking or resuming depends on MAL being configured.

**Scope boundary: episode progress only.** MAL sync only ever moves episode-progress numbers between the two sides — it never pushes a status change (`on_hold`/`dropped`/etc.) to MAL. Status (`paused`/`dropped`/`in_progress`) stays a purely local Tsuzuki concept.

**Prerequisite: one-time OAuth authorization.** `tsuzuki sync login` walks through MAL's OAuth flow once.

**No per-user MAL app registration required.** Tsuzuki ships a built-in, publicly committed `client_id` (`mal::DEFAULT_CLIENT_ID`), so `sync login` works immediately against an untouched `config.toml`. Safe because the OAuth flow is Authorization Code + PKCE for a public/native client — `client_id` isn't confidential for this app type, and there's no `client_secret` anywhere in this codebase to leak (same precedent as Taiga, erengy/taiga). `[mal] client_id` remains an explicit override for anyone running their own fork against their own registered app. Full reasoning and the "don't rotate this without re-verifying" gotcha: CLAUDE.md §3.

**Linking a series without manual IDs.** Matching against MAL's global database would be open-ended inference; instead, Tsuzuki matches against the user's own MAL list, which returns each entry's main title, English title, Japanese title, and any synonyms.

1. First `sync` touching a given series: fetch the user's MAL list.
2. Normalize both sides (case, brackets, whitespace) and check for an **exact match** between the local folder name and *any* of an entry's known titles — not fuzzy similarity against one title.
3. **Match found** → link automatically, cache the ID locally.
4. **No exact match, and the closest candidate clears `fuzzy_match_threshold`** → a single "did you mean...?" line naming that one candidate, plus the exact `tsuzuki sync link "<series>" --mal-id <id>` command to confirm it — never a ranked list, never a raw similarity percentage (similarity ranks what's *displayed*, never decides a match, and a weak candidate is noise, not help). The user still has to run that command explicitly; nothing auto-links.
5. **No exact match, and nothing clears the threshold** → a plain "no matching series found," no suggestion shown at all.
6. **Nothing on the list** → tell the user plainly; they add it on MAL, or use `tsuzuki sync link <series> --mal-id <id>` as a manual override.

**`sync` output is one line per series, action-oriented.** A successful merge reads `✓ <series> — episode <n> synced`; a case needing manual linking reads `⚠ <series> — ...` with an indented `→ tsuzuki sync link ...` command when there's something to run; a real failure reads `✗ <series> — sync failed: <reason>`. The run ends with one compact summary line (`Sync complete: 1 synced, 2 need manual linking`) — categories with a zero count are omitted rather than padding the line. Nothing about *how* a match was found (similarity scores, candidate counts, which title field matched) belongs in this output — that's internal reasoning, not a result or an action the user needs to take.

**Two-way, non-destructive merge.** A push-only sync is harmful — if the user watched ahead elsewhere and updated MAL directly, a naive push would overwrite MAL *backward*. The rule:

> **Take the higher episode number, in either direction. Never overwrite down.**

- MAL ahead of local → local awareness updates to match. No timestamp/position is implied for episodes Tsuzuki never observed.
- Local ahead of MAL → push via `PATCH /v2/anime/{id}/my_list_status`. **Request-body field is `num_watched_episodes`** — MAL's API is asymmetric: a read reports the count as `num_episodes_watched`, but the PATCH body expects `num_watched_episodes` (words swapped, verified against Taiga's source and the unofficial API spec). Easy to get wrong silently — MAL ignores an unrecognized field rather than erroring, so the count would just never update.
- Equal → no-op.

The pushed number uses Anitomy-parsed episode identity when available (§5), ordinal position otherwise — keeping sync accurate for series with numbering gaps.

**The pushed/compared number is the last *watched* episode, not the tracked cursor.** A `state.toml` record's episode number names whatever episode Tsuzuki is currently sitting on — mid-watch, or just-advanced-to at position 0 — not one confirmed finished. MAL's field means episodes already watched, so the number fed into the merge is the cursor's episode number minus one, with one narrow exception documented in `sync::local_effective_episode`'s own doc comment (CLAUDE.md's "MAL push number is the last watched episode, not the tracked cursor").

**This directly answers "forced to click through episodes."** If MAL says episode 12 but local history only knows up to 9, Tsuzuki doesn't make the user play through 10–11 to catch up — it trusts the higher number and treats 13 as next. If the user wants to actually rewatch 10–11, they open those files directly in mpv, already a fully legitimate way to interact with the system.

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
client_id = ""               # optional override; empty = use Tsuzuki's built-in client_id (see §7 — no MAL app registration required by default)
redirect_port = 8734        # local OAuth-callback listener port used by `tsuzuki sync login`
fuzzy_match_threshold = 0.8  # minimum title similarity (0.0-1.0) before `sync` suggests a "did you mean...?" — below this, no suggestion is shown at all
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

**How the cursor actually moves.** A saved position below `resume_seconds` is invisible to `state.toml` entirely, in both directions — it never creates a new record and never overwrites an existing real one. This has to be strict: mpv has no concept of Tsuzuki's noise threshold, so a brief accidental reopen of an already-bookmarked file would otherwise silently erase real progress (the same category of mistake §7's MAL rule guards against, applied locally). Only another real (≥`resume_seconds`) observation can ever move a series' position. Because more than one series can be `in_progress` at once, and binge-watching in one sitting is normal, a single `tsuzuki` invocation resolves the whole chain of what happened since the last check, not just one step of it. Full mechanics: CLAUDE.md's "Cursor movement" and "Completion detection" sections.

**The one thing this can't see: a clean binge leaves no trace at all.** mpv only writes a `watch_later` entry when a file is quit *mid*-playback — played straight through, nothing is ever written to later find absent. Direct, accepted cost of never holding an IPC socket rather than observing playback live. (Closable for anyone with the optional companion script installed — §4.)

**A deleted or moved-away series' record doesn't disappear on its own** — that would be Tsuzuki guessing the user wants it forgotten. It sits inert until `doctor` (§9) surfaces it explicitly.

`auto_pause_days = 0` means an `in_progress` series never automatically loses that status — a fixed timeout would guess intent (abandoned vs. on a break). Nonzero opts an idle series into auto-`paused`, never auto-`dropped` (destructive, stays manual-only).

**The resume picker itself:**

- **Exactly one `in_progress` series** → bare `tsuzuki` resumes it directly, no picker — preserves the one-command demo.
- **More than one** → an interactive arrow-key selector (↑/↓ to move, Enter to confirm) — not a type-a-number prompt. This is the one place in the tool where an interactive-only UX is the right call: it's the daily-use reflex path the whole tool is built around, so minimizing keystrokes matters more here than scriptability does (contrast `tsuzuki exclude`'s ambiguity case, §3, which stays script-friendly via a `--root` flag instead, since that's a rare setup action). Requires a real TTY; a non-interactive context (piped/scripted `tsuzuki`) falls back to listing the choices and their index instead of hanging on a prompt that can't render.
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
| `tsuzuki` | Resume the current series. Single `in_progress` series → resumes directly. Multiple → interactive picker (arrow-key select) if a TTY is available, otherwise lists choices with an index and exits without resuming. None → falls through to `start` behavior. |
| `tsuzuki resume [series] [--root <root>]` | Explicit alias for the above. Accepts an optional explicit series name (with `--root` to disambiguate a name shared across composable roots) to bypass the picker and complete non-interactively — always usable from a script, never dependent on a TTY. |
| `tsuzuki status` | Show what's currently `in_progress` — every such series, its progress, and (if more than one) a note that running bare `tsuzuki` will prompt among them. Subsumes the earlier separate `current` command, cut as redundant during Phase 5 planning. |
| `tsuzuki list` | List all tracked series and their progress/status — grouped `in_progress` → `paused` → `dropped` → never-started, alphabetical within each group. |
| `tsuzuki add <path>` | Register a tracked root. |
| `tsuzuki start [series] [--root <root>]` | List series outside the automatic picker — never-started, dropped/completed (shown as "Rewatch?"), and paused (resume at saved position). Same explicit-argument/interactive-picker behavior as `resume`. |
| `tsuzuki pause <series> [--root <root>]` | Remove from the resume picker, keep saved position. Errors (does not silently create a record) if the target has no real progress to pause — never started, or already dropped. |
| `tsuzuki drop <series> [--root <root>]` | Reset progress; series reappears in `start` as if new (labeled "Rewatch?" either way). Same no-record-to-act-on error as `pause`. |
| `tsuzuki exclude [path]` | Exclude a file or directory from ordering. Bare invocation surfaces Anitomy-suggested candidates for confirmation. |
| `tsuzuki sync login` | One-time MAL OAuth authorization. |
| `tsuzuki sync link <series> --mal-id <id> [--root <root>]` | Manually resolve a series-to-MAL-ID mapping when exact-title matching fails. The given id is verified against the user's own MAL list before being saved. |
| `tsuzuki sync` | Run the two-way, non-destructive sync against MAL. |
| `tsuzuki config get/set/list/edit` | Read or write config values, or open the config file directly. |
| `tsuzuki doctor [--fix] [--forget-orphans]` | Environment health checks — mpv-launch resolvability (native/Flatpak/configured override), the mpv.conf config flag, watch_later reachability (including a configured `watch_later_dir` override that doesn't resolve), Flatpak permissions, the tsuzuki.lua companion script's install/version status, its log directory's reachability, tracked-root validity, and orphaned per-series `state.toml` records (a series deleted/moved/renamed out from under a still-tracked root). MAL auth status is excluded — Phase 8 doesn't exist yet. Each check reports OK/WARN/FAIL; exits non-zero only if at least one FAIL is present. `--fix` applies every currently-safe, non-destructive fix (installing/updating tsuzuki.lua) — never edits mpv.conf. `--forget-orphans` (requires `--fix`) permanently clears every currently-confirmed orphaned record in one pass, printing exactly what it deletes first; there is no separate, per-series, or standalone cleanup command. |

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

**Key dependencies:** `anitomy` (vendored from Rapptz/anitomy-rs as `vendor/anitomy`, renamed `tsuzuki-anitomy` — pure Rust, no C++ toolchain needed; see CLAUDE.md for the vendoring rationale and hands-off policy), `serde`/`toml`, an HTTP client for MAL's API, a natural-sort comparator.

**Testing:** `cargo test`, built into the toolchain. `assert_cmd` for end-to-end command tests against the compiled binary; `tempfile` for fabricated directory structures. Most of the suite is close to mechanical, since this doc's rules are already stated as explicit boundary values.

**Distribution — prebuilt binaries as the primary path, not `cargo install`.** `cargo install` only reaches people with the Rust toolchain already installed — a small subset of this tool's actual audience. The path that matches: **`cargo-dist`**-generated GitHub Release binaries plus a one-line install script (`curl ... | sh`, the same pattern as rustup and Homebrew). `cargo install`/crates.io stays available as a secondary path; a `maturin`-built PyPI wheel offers a third door (`pip install tsuzuki`) using the same precompiled binary, no client-side compilation in any path. A Homebrew tap once there's adoption. With the vendored Anitomy parser updated to depend on crates.io `phf 0.12` instead of its original unmerged fork (see CLAUDE.md), Tsuzuki's dependency graph has no remaining git-only dependencies, so all three distribution paths are viable without qualification.

**Platform scope for v1: Linux only.** OS-candidate-path logic is parameterized from day one, but macOS/Windows remain unverified until targeted.

---

## 11. CLI UX 

**Signal over noise**: only show information that helps the user understand the result or take action.

**Actionable output**: whenever user intervention is needed, show the exact command they can run (minimize noise as much as possible).

**Clear status at a glance**: use concise, consistent success/warning states (✓, ⚠) so the result is scannable.

**Concise success paths**: successful operations should take up as little space as possible.

**No implementation details**: e.g.: similarity percentages, “closest matches,” and internal matching behavior aren't useful unless they affect the user's decision.

**Consistent structure**: each result follows a predictable pattern: status → result → action if needed.

**Useful summary**: end with a compact aggregate result rather than making the user count individual outcomes.

## 12. Build Plan

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
This is the payoff phase: in_progress/paused/dropped, the resume picker (0/1/many logic), and tsuzuki / resume / status / list / start / pause / drop. Everything from Phases 1–4 gets tied together here into the actual one-command demo. I'd stop and manually dogfood after this phase — it's the point where the tool becomes real.

**Phase 6 — Optional mpv companion script**
An opt-in Lua script (`contrib/mpv/tsuzuki.lua`) giving Tsuzuki a direct per-file signal (real duration, real completion reason) instead of an inferred one, with the existing watch_later-based pipeline as the unconditional per-file fallback. Isolated because it's strictly additive and off by default — nothing before or after this phase depends on it existing.

**Phase 7 — doctor**
Full environment checks (mpv config flag, watch_later reachability, Flatpak permission gap, tracked-root validity, MAL auth status once Phase 7 exists). Doing it last means it can genuinely check everything rather than being built incrementally and forgotten.

**Phase 8 — MAL sync**
OAuth login, exact-match linking against the user's own list, manual sync link, the two-way non-destructive merge. This is the highest-risk phase (network, external API, auth) — isolating it means a flaky OAuth flow can't destabilize everything you already validated.

**Phase 9 — Polishment, Distribution, end-to-end tests**
CLI UX Polish, cargo-dist config, install script, assert_cmd end-to-end tests against the compiled binary, PyPI wheel via maturin. Genuinely last — there's no point packaging a binary that's still changing shape.

---

## 13. Open Questions

*(none currently — resolved as they came up)*
