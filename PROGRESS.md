# PROGRESS.md — Tsuzuki build status

Status file, not a spec. Decisions and reasoning live in `CLAUDE.md` and
`docs/design.md` — this just tracks where the build currently stands.

**Current phase:** Phase 8a — MAL OAuth login, token storage/refresh, and
`doctor`'s MAL-auth check (implemented, verified — paused before Phase 8b
(linking/matching/two-way merge), per this task's own explicit "do not
proceed automatically" instruction)

**Done:**
- Phase 0 — Scaffolding & data model: Cargo binary crate (edition 2024),
  full clap CLI skeleton, config.toml + state.toml read/write (atomic for
  state), 8 passing unit tests.
- Phase 1 — Tracked roots & episode ordering:
  - `tsuzuki add <path>` is real (`src/roots.rs` + `main.rs` wiring):
    validates existence/directory-ness, idempotent re-add (`~` expansion
    + canonicalization happen only in memory for validation/comparison,
    so `~/Anime` / `/home/user/Anime` / a trailing slash all recognize
    the same root), correct exit codes (0 for success/already-tracked,
    non-zero for validation failure).
  - Roots are persisted to `config.toml` **exactly as the user typed
    them** — no expansion, no canonicalization applied to the stored
    value (corrected after an initial pass wrongly wrote canonicalized
    absolute paths; see CLAUDE.md's "Roots are persisted in the form the
    user typed").
  - `video_extensions` added to `Config.roots` (default `["mkv", "mp4",
    "avi", "webm", "mov"]`), matching design.md §8.
  - Ordering engine (`src/series.rs`), standalone and not yet wired to
    any command: `series_in_root()` (depth-1 enumeration, verbatim
    names) and `ordered_episodes()` (hierarchical natural sort via the
    `natord` crate — directories recursed depth-first in natural sibling
    order, then a directory's own video files, also natural order; only
    `video_extensions`-matching files count as leaves, case-insensitive;
    non-numeric names fall back to alphabetical). Confirmed correct
    including the mixed-level case (design.md §5's "Mixed level"
    paragraph).
  - `series.rs` now follows symlinks (stat semantics via `fs::metadata`,
    not `DirEntry::file_type()`'s lstat) for both `series_in_root()` and
    `ordered_episodes()`, with cycle protection in `ordered_episodes()`'s
    recursive walk (a `HashSet` of canonicalized real paths already
    visited, seeded with the series root itself) — fixes the symlink-
    blindness bug flagged during the gap audit. A broken symlink or
    stat-race is silently skipped rather than aborting the whole walk.
  - `config.rs` normalizes `video_extensions` on read — a leading `.` is
    stripped in memory (`.mkv` and `mkv` behave identically); the on-disk
    file is never silently rewritten, only `write()` changes it.
  - 42 unit tests total (37 → 42: symlinked-series-folder enumeration,
    symlinked-episode-file inclusion — both confirmed to genuinely fail
    against the pre-fix code, not just pass now — a symlink-cycle test
    bounded by a 5s timeout so a regression fails loudly instead of
    hanging CI, and two config.rs tests for dot-stripping normalization),
    all passing, zero clippy/fmt warnings.
  - 25 unit tests total (post-Phase-1) grew to 37 via a gap-audit pass
    covering the full `add` idempotency matrix — trailing slash + `~`,
    `..`-resolution, symlinked root, symlinked home directory, case-
    sensitivity — plus ordering boundary conditions: empty directory,
    non-video-only directory, no-subdirectories root, 3-level-deep
    recursion, empty `video_extensions`, and a leading-dot config entry.
    Manually verified against fabricated trees, including a scratch HOME
    proving `add ~/Anime` persists the literal string `~/Anime` and that
    idempotency detection matches correctly across `~`-form and
    absolute-form for the same root.

- Phase 2 — Exclusions:
  - `Config.exclusions: Vec<Exclusion>` added (`src/config.rs`), each
    entry storing a tracked root's own persisted literal form plus a
    relative path under it — `[[exclusions]]` array-of-tables in TOML,
    e.g. `root = "~/Anime"`, `path = "One Piece/Specials"`. Defaults to
    empty; `#[serde(default)]` so a pre-Phase-2 config.toml without the
    key still reads cleanly.
  - `tsuzuki exclude <path> [--root <root>]` is real (`src/exclude.rs` +
    `main.rs` wiring): resolves `<path>` against every tracked root
    containing it, matching the full decision table from CLAUDE.md/
    design.md §3 — zero matches errors, exactly one match validates and
    persists, multiple matches without `--root` print the exact numbered
    + re-invocation format from design.md §3 and exit non-zero, multiple
    matches with `--root` resolve directly, already-excluded is
    idempotent (exit 0), and excluding a depth-1 directory (a whole
    series) is rejected. `roots.rs`'s `expand_tilde`/`normalize` helpers
    were made `pub(crate)` so `exclude.rs` reuses the exact same
    tilde-expansion/canonicalization logic `add` uses, rather than
    duplicating it.
  - Bare `tsuzuki exclude` (no path) is still a stub this phase, per
    design.md §11 — Anitomy-suggested candidates land in Phase 3.
  - `series.rs`'s `ordered_episodes` is now a thin wrapper around a new
    `ordered_episodes_excluding(series_path, video_extensions, excluded)`
    — exclusion filtering happens at directory-entry collection time,
    before each level's `sort_by` call, not as a post-filter on the
    sorted result. Excluding a directory means `walk` simply never
    descends into it, which makes the recursive removal free. Not yet
    wired into any user-facing command (`list`/`status`/`start` are
    Phase 5, per scope).
  - 60 unit tests total (up from 42): config.rs exclusion persistence
    round-trip + missing-key-defaults-to-empty; exclude.rs's full
    decision table (single-root success, idempotent re-exclude,
    nonexistent-path error, whole-series rejection including the
    depth-1-file-is-not-a-whole-series distinction, two-root ambiguity
    listing both + exact Display-format assertion against design.md's
    example, `--root`-disambiguated resolution, unknown `--root` error,
    literal-tilde-form persistence, `excluded_paths_for_root` filtering);
    series.rs's directory-exclusion-is-recursive, single-file-exclusion,
    cross-series-isolation (excluding in one series doesn't touch an
    identically-structured sibling), and an equivalence test confirming
    `ordered_episodes` with no exclusions matches
    `ordered_episodes_excluding` with an empty list. All passing, zero
    clippy/fmt warnings.
  - Manually verified against a scratch `HOME` with two tracked roots
    (`~/Anime`, `~/AltAnime`) sharing `One Piece/Specials`: the
    no-`--root` case prints the exact ambiguous-match format from
    design.md §3 and exits non-zero; `--root ~/Anime` succeeds and
    persists `root = "~/Anime"` (not an expanded path) to config.toml;
    re-running the same command reports "already excluded" and exits 0;
    excluding a whole series (`One Piece`, `Monster`) is rejected with a
    clear message, exit non-zero.
  - Gap-audit pass found and reproduced three bugs (each as an
    `#[ignore]`d test): idempotency comparing raw strings instead of
    normalized paths (a trailing slash wasn't recognized as the same
    exclusion), an absolute `path` argument silently discarding the
    tracked root via `PathBuf::join`'s documented replace-on-absolute
    behavior, and exclusion enforcement checking exact real-path
    membership instead of ancestry (a symlink outside an excluded
    directory pointing into it could bypass the exclusion).
  - Correction pass fixed all three: `exclude.rs`'s already-excluded
    check now reuses `roots::normalize` (the same helper `add`'s
    idempotency check uses) instead of comparing raw strings; an
    absolute `path` argument is now rejected with a clear
    `ExcludeError::NotRelative` before any `.join()`; `series.rs`'s walk
    now checks each entry's resolved path against exclusion *ancestry*
    (`real_path == e || real_path.starts_with(e)`) instead of exact
    `HashSet` membership. All three previously-`#[ignore]`d tests are
    now un-ignored and passing as part of the normal suite — 72 tests
    total, 0 ignored.

- Phase 3 — Anitomy integration:
  - **Workspace wiring:** `[workspace]` (`members = [".", "vendor/anitomy"]`)
    added to the top-level `Cargo.toml`, plus `tsuzuki-anitomy = { path =
    "vendor/anitomy" }` as a real dependency. This surfaced a bug in the
    vendored crate's own `Cargo.toml` left over from its rename: its
    `[dev-dependencies]` still self-referenced the pre-rename name
    (`anitomy = { path = "." }`), which only breaks resolution once the
    crate is a workspace member (a plain path-dependency consumer never
    resolves another crate's dev-dependencies) — invisible until this
    task's workspace requirement exercised it for the first time.
    Fixed as a deliberate, isolated vendor edit (user-approved, scoped
    to "finishing the rename," not a parsing-behavior change): the
    dev-dependency key renamed to `tsuzuki-anitomy`, and
    `tests/parser.rs`'s four `anitomy::` references updated to
    `tsuzuki_anitomy::` to match. `cargo test -p tsuzuki-anitomy`
    (all of anitomy-rs's own regression tests, including the full
    JSON-driven `test_json_data` suite) reconfirmed passing after the
    fix. A `cargo fmt` run on the crate also reformatted two unrelated
    vendored source files (`src/parser.rs`, `src/utils.rs`) as a side
    effect — those were reverted; only the two files with real edits
    (`Cargo.toml`, `tests/parser.rs`) were kept.
  - **`src/anitomy.rs`** (new): `episode_identity(ordered_files:
    &[PathBuf]) -> Vec<EpisodeIdentity>` — annotates each already-ordered
    file (never reorders) with an episode number: Anitomy's parsed
    `Episode` element when present and numeric, else ordinal fallback
    position. Always calls `.file_name()` before handing anything to
    `tsuzuki_anitomy::parse` (verified by direct testing: a path with
    directory components silently loses the episode number otherwise —
    e.g. `"Season 1/ep02.mkv"` parsed whole produces no `Episode`
    element at all, swallowed into `Title`). No caller yet — Phase 7
    (MAL sync) wires it in — `#[allow(dead_code)]`, matching `series.rs`'s
    established not-yet-wired convention. 4 unit tests: parseable
    filename uses the parsed number; unparseable filename falls back to
    ordinal; a numbering gap (files 1,2,3,5,6, all parseable) uses the
    parsed numbers *including the gap*, not ordinal positions — the
    actual point of Anitomy per design.md §5; a directory-components
    regression test proving the `.file_name()` fix is real, not just
    present in the code.
  - **Bare `tsuzuki exclude` candidate suggestions** (`src/exclude.rs`'s
    `suggest_candidates`/`Candidate`, wired into `main.rs`): replaces
    the Phase-2 stub. Scans every tracked root's series trees via
    `series::series_in_root` + `series::ordered_episodes_excluding`
    (reusing Phase 1/2's already-tested traversal and exclusion
    filtering, rather than a new walker), parses each file's bare
    filename via Anitomy, and suggests files whose `Type` element
    case-insensitively matches a suggest-worthy list. Prints each
    candidate's root-relative path plus the exact `tsuzuki exclude
    "<path>"` command, per design.md §5's "list-and-hand-back-the-exact-
    command" pattern (same shape as the `--root` ambiguity case) — no
    interactive prompt, no new command surface. Zero candidates prints a
    plain message and exits 0.
  - **Suggest-worthy `Type` values — judgment call, flagged per the task
    brief's own request:** `OVA`, `Special`, `Specials`, `Movie`,
    `Gekijouban`, `ONA`, `OAD`, `OAV`. Verified directly against
    `vendor/anitomy/src/keyword.rs`: *every* `Type` keyword is marked
    `ambiguous` there (all require bracket-enclosure or specific context
    to be recognized at all) — so "ambiguous per the keyword table"
    doesn't by itself distinguish any of these from each other, contrary
    to the initial framing. `TV` and `SP` are excluded on more concrete
    grounds instead: `TV` denotes the ordinary/mainline broadcast
    category (the opposite of an exclude-worthy signal), and the keyword
    table itself carries an inline comment documenting a real `SP`-in-an-
    ordinary-title collision (`"Yumeiro Patissiere SP Professional"`).
    `Gekijouban` was added on top of the task brief's literal list: it's
    a confirmed real `Type` value (verified by direct testing,
    `Gekijouban` → `Type = "Gekijouban"`) carrying the same ambiguity
    classification as `Movie` and is its direct semantic synonym
    (theatrical release) — no principled reason to suggest one and not
    the other. Matching is case-insensitive since Anitomy preserves a
    filename's original casing in the parsed value (`"ova"` in a
    filename parses to `Type = "ova"`, not normalized).
  - **Verification scoping decision (user-approved):** `cargo clippy
    --workspace -- -D warnings` as originally specified is not
    achievable without editing vendor/anitomy's source for pure style
    lints (a `match`-replaceable-with-`?` in `parser.rs`, a
    `contains()`-vs-`iter().any()` in `tokenizer.rs` — pre-existing,
    never previously checked since `vendor/anitomy` wasn't a workspace
    member before this phase). Decided: the strict gate is scoped to
    `cargo clippy -p tsuzuki --no-deps -- -D warnings` (Tsuzuki's own
    code only); `cargo clippy -p tsuzuki-anitomy` stays available for
    visibility without blocking on lints nobody wrote for this project.
    This scoping should be treated as the standing convention for future
    phases' verification, not re-litigated each time.
  - 84 unit tests total across the workspace (80 in `tsuzuki` + 2 in
    `tsuzuki-anitomy`'s own unit tests + 1 JSON-driven regression suite),
    all passing, zero warnings under the scoped clippy gate, `cargo fmt
    --check` clean for every file this phase touched.
  - Manually verified against a scratch `HOME` with one tracked root
    containing a regular episode and an OVA-style-named file under a
    `Specials/` subdirectory: bare `tsuzuki exclude` lists only the OVA
    candidate with a correct, copy-pasteable `exclude "<path>"` command;
    running that exact command excludes it successfully via Phase 2's
    unchanged logic and persists to `config.toml`; re-running bare
    `exclude` afterward correctly reports zero candidates.

- Phase 4 — mpv `watch_later` reading:
  - **`src/duration.rs`** (new): episode duration probing, standalone and
    not yet wired to any command. `probe_duration(path) -> DurationOutcome`
    tries `symphonia`'s own content-based format probe first (no extension
    hint given at all — never dispatches by `.mkv`/`.mp4`/etc, only by
    file content, per CLAUDE.md), reading duration straight off
    `FormatReader::media_info()` (works regardless of track type, so no
    audio track is required). Falls back to a small hand-written AVI
    `avih` RIFF-header reader (`total_frames × microseconds_per_frame`)
    only when Symphonia's probe fails to identify the file — never gated
    on the `.avi` extension. `DurationOutcome::Known(f64)` /
    `Unknown(DurationError)` (`Io`/`Unreadable`) — an unreadable duration
    is always surfaced with a real reason, never collapsed to a bare
    `None`. `symphonia = { version = "0.6.1", default-features = false,
    features = ["mkv", "isomp4"] }` — scoped to exactly the containers
    `video_extensions` needs (mkv covers webm too; isomp4 covers
    mp4/mov); avi has no Symphonia support, hence the fallback.
  - **`src/mpv.rs`** (new): reads mpv's own `watch_later` files, standalone
    and not yet wired to any command. Candidate-path table
    (`WatchLaterCandidate { os, relative }`, matched against
    `std::env::consts::OS` — one platform check, not scattered `cfg!`)
    with design.md §4's exact 5 Linux candidates (native ×2, Flatpak ×2,
    Snap ×1); `watch_later_dir` config override replaces the candidate
    list entirely rather than merging with it. Entry parsing discards
    `# redirect entry` files and skips any malformed entry (missing
    `start=`, unparseable float, non-comment first line) without aborting
    the scan. Tracked-root filtering reuses `roots::normalize` (the same
    helper `add`/`exclude` use) rather than a second comparison path.
    Cross-directory duplicates are merged per-*path* (not per-directory)
    freshest-file-wins. `classify()` turns a position + `DurationOutcome`
    into `StartFromZero` / `Resume(f64)` / `Completed` against
    `resume_seconds`/`completion_remaining_seconds` — on the overlap edge
    case (a short file where both thresholds fire at once),
    `StartFromZero` wins, a deliberate, documented (CLAUDE.md) judgment
    call, not a coin flip.
  - `tests/fixtures/{short.mkv,short.mp4}` (new, ~4.4KB/4.8KB, 5.0s
    duration each): tiny real ffmpeg-generated fixtures, committed to the
    repo, used to test Symphonia's actual content-probe path against real
    container bytes rather than only fabricated ones. ffmpeg/ffprobe were
    only ever used locally to generate these bytes — neither is a Tsuzuki
    dependency or ever shelled out to at runtime.
  - Real-world timing (`probe_duration` against a real 20-minute/1200s
    synthetic mkv fixture, not committed): **212µs release / 653µs
    debug** — confirms duration probing is nowhere near a startup-latency
    concern (CLAUDE.md §5).
  - **Gap-audit pass** (separate review pass, not the implementation
    session): confirmed duration probing is genuinely **lazy** — nothing
    in `mpv.rs`'s own code calls `probe_duration`; `classify()` takes a
    `DurationOutcome` as a parameter rather than computing one, so Phase 5
    is the one that decides when (and for which single entry) to probe.
    Added 11 tests: corrupt/non-video files (zero-byte, plain text
    renamed `.mkv`) resolving cleanly to `Unknown` with no panic; a
    redirect entry with a trailing `start=` line still discarded as a
    redirect; a missing tracked root and a deleted source file both
    filtering cleanly with no panic; a 3-way merge proving freshest-wins
    isn't order-dependent; an exact-mtime-tie determinism test; and the
    threshold-overlap precedence case.
  - **Correction pass** (CLAUDE.md's "An absent OS-default candidate
    directory stays silent — an explicit `watch_later_dir` override that
    doesn't resolve must be surfaced, not silent"): `scan_watch_later`'s
    return type changed from `Vec<WatchLaterEntry>` to
    `ScanResult { entries, warning: Option<ScanWarning> }`. An override
    that doesn't exist on disk now returns `ScanWarning::WatchLaterDirNotFound`
    carrying the value exactly as configured (unexpanded, matching
    `AddError::NotFound`'s existing convention) alongside the (still
    empty) entries — not an error, the scan still completes. An absent
    OS-default candidate directory stays exactly as silent as before,
    proven by its own explicit test rather than inferred from the
    override case. Still zero printing anywhere in this module — the
    warning is returned data only; no command exists yet to display it.
  - 143 tests total (up from 84 at the end of Phase 3), all passing, zero
    warnings under the scoped gate (`cargo clippy -p tsuzuki --no-deps --
    -D warnings`, both with and without `--tests`), `cargo fmt -p tsuzuki
    --check` clean. Two whole-workspace `cargo fmt` runs accidentally
    reformatted the off-limits `vendor/anitomy/` during this phase (once
    in the implementation session, once in the gap-audit session) — both
    caught via `git status` before finishing and reverted. Any future
    session running `cargo fmt` on this repo must scope it to `-p
    tsuzuki`, never bare `cargo fmt`.
  - Not wired into any command, `state.toml` untouched — Phase 5 does
    both.
- Phase 5a — Planning & design (chat only, no code yet):
  - Resolved both open questions Phase 4 left blocking Phase 5: the exact
    absence-based-completion mechanism (per-series, checks only the one
    file `state.toml` remembers as current, no baseline = can't fire), and
    how the cursor advances when the user skips ahead manually.
  - Full `state.toml` schema settled: records keyed by (tracked root's
    literal persisted form, series name) — never name alone, since
    composable roots allow the same series name under two roots at once;
    `updated_at` moves only on genuine persisted state changes, never on a
    scan or a sub-`resume_seconds` reading.
  - Cursor-movement mechanics worked out by stress-testing concrete
    scenarios (not just asserted): a sub-`resume_seconds` reading is
    invisible to saved state in both directions — critically, it must
    never overwrite an existing *real* position, since mpv itself has no
    concept of Tsuzuki's noise threshold and would otherwise let one
    accidental short reopen silently erase real progress. Documented as a
    permanent, accepted architectural limit: a clean binge (never quitting
    mid-file) leaves mpv's `watch_later` zero trace at all — no amount of
    inference recovers information that was never written.
  - Deleted/moved/renamed series: treated identically to a fresh,
    untracked series appearing in its place (series identity is the
    literal directory name, no rename detection, ever). The old
    `state.toml` record goes inert, never auto-deleted — cleanup is
    pinned as a required `doctor` (Phase 6) check, not automatic.
  - Corrected a real doc bug: "Tsuzuki never launches mpv" was misleading
    — tracking is launcher-independent, but resuming does spawn mpv
    directly (fire-and-forget, no IPC socket held).
  - Full command-surface review (not just accepted as originally drafted):
    `current` was cut entirely (fully subsumed by `status`); `pause`/`drop`
    given explicit `--root` disambiguation (same mechanism as `exclude`),
    idempotent re-invocation, and a unified "no real progress to act on"
    error covering both never-started and already-dropped; allowed state
    transitions made explicit, `Dropped→Paused` ruled out on purpose.
  - mpv launch strategy settled: cheap existence check (never
    attempt-and-catch a failed spawn) to resolve native vs. Flatpak, new
    `[mpv] command_override` config field to skip detection entirely.
  - `list`'s ordering (grouped by status, alphabetical within group) and
    `start`'s three separate display words (Start/Resume/Rewatch?) locked
    in — the Resume/Rewatch? distinction was caught by literally
    simulating a terminal session end-to-end, not obvious from the state
    mechanics alone; an earlier pass had conflated paused with dropped.
  - Full session simulated by hand (fresh install → add → start → resume
    → binge → pause → drop → disambiguation case) against the settled
    design before writing Phase 5's prompt, specifically to catch gaps
    like the above before they became rebuilds.
  - New `scenario-stress-test` skill created as a byproduct — captures the
    "test the rule against real, adversarial numbers/sequences before
    trusting it" discipline that caught most of the above, for reuse on
    future decisions.
  - All of the above written into `CLAUDE.md`; `docs/design.md` updated
    for the product-level changes (commands table, Cold Start section).
    Phase 5's kickoff prompt is written and ready to hand off.

- Phase 5b — State machine & core commands (implementation):
  - One more design gap surfaced mid-implementation and resolved before
    writing `reconcile`: whether completion during reconcile should be
    absence-only or also duration-aware (design.md §4 says both; the
    kickoff prompt's own scope/test list only mentioned absence). Resolved:
    both signals feed the same cursor-advancement rule — `reconcile` calls
    `mpv::classify()` (Phase 4's `probe_duration`, unmodified) on any entry
    with a real position, and a `Completed` result is treated identically
    to an absent entry. Written into CLAUDE.md's "Cursor movement" section
    before implementation proceeded.
  - **`src/state.rs`** rewritten: `State.series` is now `Vec<SeriesRecord>`
    (was a `BTreeMap<String, SeriesState>` keyed by name alone) — keyed by
    (root's literal persisted form, series name), matching config.rs's
    `[[exclusions]]` array-of-tables shape. `SeriesRecord` adds
    `current_relative_path` (relative to root, not series dir — same
    portability convention as exclusions) and `updated_at` (unix seconds).
    Added `State::find`/`find_mut`, `now_unix()`, `resolve_root_arg()`.
  - **`src/reconcile.rs`** (new): the single shared "resolve against mpv
    reality" step. Scans `watch_later` once per call, resolves each
    candidate entry's (root, series name) via most-specific-root matching
    (so composable roots like `Anime` vs. `Anime/Rewatches` resolve
    correctly), then per series forward-scans natural-sort order from the
    right starting index (0 for never-tracked/dropped, the matched index
    otherwise) for the furthest file with a real, non-`Completed` bookmark.
    Only a genuine change (different file or different position) ever
    writes — this is what keeps `pause`/`drop` from being silently undone
    by the next reconcile seeing the same still-there mpv entry.
  - **`src/launch.rs`** (new): mpv resolution as an injectable pure function
    (`command_override` bypass → cheap native-on-`PATH` check → cheap
    Flatpak-install-dir check → typed `NotResolvable`) plus a real
    fire-and-forget `spawn`. New `[mpv] command_override` config field.
  - **`src/target.rs`** (new): resolves an explicit series name + optional
    `--root` to a tracked root, same 0/1/many shape `exclude`'s `--root`
    already established (reimplemented independently since `exclude.rs`'s
    own helper is private/off-limits).
  - **`src/pause_drop.rs`** (new): `pause`/`drop` transitions exactly per
    the allowed-transitions table. Caught and fixed a real bug during
    dogfood: `drop` originally zeroed `position_seconds`, which made the
    *next* reconcile see the still-unchanged real `watch_later` entry as
    "new activity" and silently flip the series back to `in_progress` —
    undoing the drop with no user action. Fix: `drop` no longer mutates the
    stored position/path at all (the "position reset" design.md describes
    is already achieved by `playback_target`'s Dropped special-case at
    launch time); locked in with a dedicated reconcile regression test
    (two reconcile calls in a row after a drop, bookmark unchanged, must
    stay dropped).
  - **`src/picker.rs`** (new): TTY-injectable interactive/non-interactive
    picker; non-TTY lists indexed choices and performs no action.
  - **`src/overview.rs`** (new): shared per-series display data for
    `list`/`status`/`start`, `list`'s grouping/ordering, and the
    Start/Resume/Rewatch? display-word logic (with a dedicated test
    asserting a paused series' line never contains "Rewatch").
  - **`main.rs`/`cli.rs`**: `resume`/`start` gained `[series] [--root]`;
    `pause`/`drop` gained `[--root]`; `current` removed entirely (cut per
    CLAUDE.md, fully subsumed by `status`); bare `tsuzuki` now really does
    the 0/1/many dispatch (0 → falls through to `start`, 1 → resumes
    directly, many → picker).
  - 197 tests total (up from 143 at the end of Phase 4), all passing;
    `cargo clippy -p tsuzuki --no-deps -- -D warnings` clean both with and
    without `--tests`; `cargo fmt -p tsuzuki --check` clean; `vendor/anitomy`
    untouched.
  - Manually dogfooded end-to-end against a fabricated `HOME` (real
    `tsuzuki` binary, fabricated `watch_later` entries, no real mpv
    install needed): add → start (non-TTY listing, then explicit pick) →
    fabricate a bookmark → bare `tsuzuki` resumes correctly → `pause` →
    confirmed it drops out of the automatic picker → `list` → `drop` a
    different series → confirmed `start` shows "Rewatch?" for that one
    specifically and "Resume" (never "Rewatch") for the still-paused one.
    This is where the drop/reconcile bug above was actually caught.

- Phase 5c — Gap audit (separate review pass, not the implementation
  session): hunted for what wasn't tested rather than confirming what was.
  Three required findings, all with real passing-test evidence, not just
  reasoning:
  1. Traced every `SeriesRecord` write path (pause, drop, reconcile's own
     write) for the drop-bug's *pattern*, not just the fixed instance.
     `pause`/`drop` only ever touch `status`/`updated_at` (now with
     dedicated tests asserting every other field is untouched); reconcile's
     own write always stores exactly what it just found, so it can't
     manufacture a diff against itself (proven by a new test: two reconcile
     calls back to back with nothing changed on disk, `updated_at` doesn't
     move on the second). No other instance of the bug class found.
  2. A `Paused` series with a genuinely new real bookmark on a *later*
     episode does correctly advance to `in_progress` at the new position —
     this was untested (only the "unchanged bookmark stays paused" and
     "earlier bookmark blocked" cases existed); added and confirmed passing.
  3. Multi-step chaining through *both* completion signals in one call
     (absence, then a duration-based `Completed` reading, then an ordinary
     real position) was previously only tested with each signal type in
     isolation; added and confirmed it resolves in a single `reconcile()`
     call.
  - General sweep found and fixed two real gaps (not required findings,
    but real): `launch.rs`'s native-mpv-on-`PATH` check used `is_file()`
    alone, which can't distinguish "exists" from "is actually executable"
    — fixed with a cheap permission-bit check on Unix (still just a
    `stat`, never a spawn), regression test added. `picker.rs`'s TTY
    branch called `dialoguer::Select` directly with no way to test the
    actual selection path at all — refactored to inject the selector
    (production passes a real `dialoguer_select` function; tests pass a
    fake one), closing a real coverage gap.
  - Also added: a consolidated overview.rs test asserting all four real
    states' display words together (not four separate tests that could
    each pass while wrong relative to each other); `main.rs` had *no*
    tests at all before this pass — added real `--root` disambiguation
    tests against two actually identically-named series under two real
    tracked roots for `pause`/`drop` and for the shared `explicit_play`
    path `resume`/`start` use; `is_tty` was refactored from an internal
    `stdin_is_tty()` call inside `resume_command`/`start_command` into a
    value threaded down from `dispatch` (queried once), specifically so
    "an explicit series argument bypasses the picker even with a TTY
    available" is a testable claim, not just one provable by reading the
    control flow — confirmed with a timeout-guarded test (same precedent
    as `series.rs`'s symlink-cycle test) so a real regression fails loudly
    rather than hanging.
  - Confirmed empirically (not just by reasoning) that no pre-Phase-5
    `state.toml` can exist in practice — `git log -p -- src/main.rs` shows
    the only `write()` call that ever existed before this phase wrote
    nothing but an empty default and was removed the same day, and no real
    `~/.local/share/tsuzuki/` exists on the development machine. Added a
    test covering the hypothetical anyway: an old-schema file fails
    deserialization cleanly (typed `Err`, never a panic or silent data
    loss) against today's reader.
  - Idempotency boundary ("`pause`/`drop` on an inconsistent stored
    record") isn't reachable — `Status` is an exhaustive 3-variant enum
    with no partial state possible in Rust's type system, and both
    functions match it exhaustively with no wildcard arm.
  - No stale tests found — every test/comment from the implementation
    session that mentions completion signals already reflects the
    corrected (absence + duration-`Completed`) design, since the
    clarifying question was asked and answered before `reconcile.rs` was
    written at all.
  - 211 tests total (up from 197), all passing; both clippy gates clean;
    `cargo fmt -p tsuzuki --check` clean; `vendor/` untouched.

- Phase 5d — Correction: config.toml forward-compatibility (found during
  the human dogfood pass): `tsuzuki add` against a real, older config.toml
  failed outright with "missing field `video_extensions`" — that field
  (Phase 1) had no serde default at all, so any config.toml predating it
  could never be read again. Full field-by-field audit of `Config` found
  this was not an isolated case: **every** top-level `Config` field except
  `exclusions` lacked `#[serde(default)]`, and `Thresholds`'s own
  `resume_seconds`/`completion_remaining_seconds` had the same
  no-default problem one level down, with the same aggravating factor as
  `video_extensions` — their correct defaults (120/180) are non-zero, so a
  *bare* `#[serde(default)]` would have "fixed" the crash into a silently
  wrong `0` instead of the documented value, not a real fix.
  - Fixed with two consistent defaulting layers throughout `config.rs`:
    each `Config` field now has `#[serde(default)]` (backed by a real
    `Default` impl per substruct) covering a whole section being absent,
    and any field whose correct default isn't already `Default::default()`
    (`video_extensions`, `resume_seconds`, `completion_remaining_seconds`)
    gets `#[serde(default = "fn")]` with a named function, covering that
    one key being absent from an otherwise-present section. `Config`'s
    manual `impl Default` was removed in favor of `#[derive(Default)]`,
    now that every substruct composes to the exact same documented values
    — one source of truth instead of two that could drift apart.
  - Full per-field audit (`paths`, `auto_pause_days`, `watch_later_dir`,
    `command_override`, `client_id`) confirmed their correct defaults are
    already `Default::default()`-equivalent (empty/zero/empty string) — a
    bare `#[serde(default)]` was already correct for those, no change
    needed beyond adding it where it was missing at the section level.
    `Exclusion`'s two fields (`root`/`path`) were deliberately left
    without defaults — both were introduced together in one commit with no
    incremental-field-add history, and there's no sensible default for a
    partially-specified exclusion entry.
  - Two new regression tests assert on the actual resulting values, not
    just "doesn't error": a config.toml missing `video_extensions`
    entirely defaults to exactly the documented 5-extension list; a
    config.toml missing the whole `[thresholds]` table *and* one missing
    just one key within an otherwise-present table both default
    `resume_seconds`/`completion_remaining_seconds` to 120/180, not 0.
  - Manually reproduced the exact broken shape from the dogfood report
    (a `[roots]` table with `paths` but no `video_extensions` key) against
    the real binary: `add` and `list` both now succeed; confirmed
    `video_extensions` actually resolved to the real (non-empty) default
    in memory by adding a real `.mkv` file and seeing `list` find it — not
    just that the command stopped crashing. On-disk config.toml stays
    untouched by the read, per the existing no-auto-rewrite convention.
  - 213 tests total (up from 211), all passing; both clippy gates clean;
    `cargo fmt -p tsuzuki --check` clean; `vendor/` untouched.

- Phase 5e — Correction: investigated a reported "`state.toml` never gets
  written" bug from the human dogfood pass — **did not reproduce**. Direct
  repro against the real release binary, including a fully clean `env -i`
  shell with no `XDG_*` overrides (matching a normal user's terminal
  exactly), confirmed `state.toml` is correctly created at
  `~/.local/share/tsuzuki/state.toml` (via `dirs::data_dir()`, exactly as
  `state::state_path()` is coded and documented) with the correct
  `in_progress` record, after `resume` picks up a genuine pre-existing
  real bookmark. The bug report's own verification command checked
  `~/.config/tsuzuki/state.toml` — that's `config.toml`'s directory
  (`dirs::config_dir()`), never state.toml's; state.toml has never lived
  there in any version of this code. No code defect found or fixed in the
  persistence path itself.
  - The report's second claim — the `resume`/`start` message showing only
    the series name and file path, no episode number or timestamp — **was
    real** and is fixed: `playback_target` (`main.rs`) now also resolves
    the correct episode number via `anitomy::episode_identity` (the exact
    mechanism `reconcile.rs` already uses, not a third formatting path)
    for every case including never-started/dropped, which has no
    record-side episode number to fall back on at all. `play_series`'s
    message is now `"{verb} \"{name}\" — episode {N} ({mm:ss}) — {file}"`.
    An early draft of this fix conflated "index resolved to 0 because the
    record's file was genuinely episode 1" with "index defaulted to 0
    because no usable record existed" (which would have silently shown
    position 0 even for a real paused/in-progress record at episode 1) —
    caught before landing by re-deriving the logic explicitly rather than
    trusting the first version; fixed to carry index and position as one
    resolved pair, defaulting together or not at all.
  - Closed the actual, real test-coverage gap this investigation
    surfaced: every one of the 213 unit tests constructs its own
    `state_path`/`config_path` via `tempdir().join(...)` and passes it
    directly as a function argument — none of them call
    `state::state_path()`/`config::config_path()` themselves, and none of
    them invoke the compiled binary or `main()`/`dispatch()` at all. A new
    `tests/state_persistence.rs` integration test spawns the real
    compiled binary (`env!("CARGO_BIN_EXE_tsuzuki")`) against a genuine
    temp `$HOME` with `XDG_CONFIG_HOME`/`XDG_DATA_HOME` explicitly cleared,
    exercising `add` → fabricated `watch_later` bookmark → `resume` →
    reading back real `~/.local/share/tsuzuki/state.toml`, plus a second
    test proving persistence survives across separate process invocations
    (`status`/`list` in fresh processes reading what an earlier `resume`
    process wrote). Verified this test would have failed pre-fix by
    literally reverting the message-format change and re-running it: the
    episode/timestamp assertions fail exactly as expected, while the
    persistence assertion still passes even pre-fix — direct, reproduced
    confirmation that persistence itself was never the actual bug.
  - 215 tests total (213 unit + 2 new integration), all passing; both
    clippy gates clean; `cargo fmt -p tsuzuki --check` clean; `vendor/`
    untouched. Manual repro re-run against the real release binary,
    matching the bug report's exact sequence: `state.toml` confirmed
    correctly absent at the (wrong) reported path and present with the
    correct record at the real path; resume message confirmed showing
    `episode 1 (09:20)`.

- Phase 5f — Correction: reconcile silently froze once a tracked file's
  real position crossed into `completion_remaining_seconds` of the
  episode's end. Confirmed repeatable and permanent across multiple real,
  separate quits, not a one-off timing fluke — a session watched further
  and quit again near the end three separate times, and the record stayed
  frozen at the same pre-threshold position every time.
  - **Root cause, found by tracing (not guessed at):**
    `reconcile_one_series`'s forward-scan loop only ever set its landing
    candidate (`best`) for a `PlaybackAction::Resume` classification;
    `Completed` fell through and set nothing. CLAUDE.md's own wording —
    "a `Completed` result is treated identically to an absent entry" — was
    read (and implemented) too literally: it correctly means neither
    should ever advance the cursor to a later episode on its own, but the
    code also discarded the `Completed` entry's *own real position*
    exactly like absence's genuine lack of one. Absence and `Completed`
    are not alike in what they carry — absence means mpv deleted the
    entry (nothing left to record); `Completed` means mpv is still
    reporting a real, current, changing position for the file already
    being tracked. Once nothing existed past a `Completed` entry, `best`
    stayed `None` and the whole reconcile call returned having recorded
    nothing at all — every single time, since the file's own real
    position kept re-classifying as `Completed` on every later call too.
  - **Fix:** the forward-scan now sets `best` for `Completed` as well as
    `Resume`, using the entry's own real position. Since `best` is
    unconditionally overwritten as the scan moves forward, a later real
    entry (of either classification) still always overrides an earlier
    one — multi-step binge chaining is unaffected, confirmed by the two
    existing chaining tests still passing unchanged. The only behavioral
    change: when nothing is found forward of a `Completed` entry, its own
    position is now the fallback landing point and gets recorded, instead
    of the whole observation being silently discarded.
  - CLAUDE.md's "Completion has two real signals" section and
    `reconcile.rs`'s own module/inline doc comments rewritten to make this
    distinction explicit — "identical for cursor-advancement purposes" is
    not "identical in every respect" — per the doc-maintenance rule
    (delete and replace imprecise text, don't append a correction next to
    it).
  - New test reproduces the exact real shape, not a single fabricated
    multi-file call: three separate `reconcile()` calls against the SAME
    tracked file — `Resume` (creates/updates), then `Completed`
    (must advance, was frozen pre-fix), then a second, later `Completed`
    reading (must advance again, not repeat the second call's value — the
    part a single before/after test wouldn't catch, since the bug recurred
    on every subsequent call). Confirmed failing pre-fix by temporarily
    reverting only the `match` change and re-running it — failed exactly
    as expected (frozen at the first value); passes with the fix restored.
  - **Independence from the separate `--start` launch-flag issue,
    investigated not assumed:** this bug lives entirely in `reconcile.rs`'s
    forward-scan `best`-selection — the layer that decides *what position
    gets stored*. The `--start` issue is in `launch.rs` — the layer that
    decides *what argument to pass mpv at launch time*, using whatever
    `reconcile` already decided. These are causally sequential, non-
    overlapping code paths; this fix touches only `reconcile.rs` (plus its
    own new test) and does not touch `launch.rs` at all. Confirmed
    unrelated, not assumed.
  - 216 tests total (214 unit + 2 integration), all passing; both clippy
    gates clean; `cargo fmt -p tsuzuki --check` clean; `vendor/` untouched.
    Manual repro against the real release binary using the project's own
    committed `tests/fixtures/short.mkv` (~5.0s, real symphonia-probed
    duration) with tuned thresholds to reach the completion window
    quickly: three separate real `resume` invocations (1.5s → 4.0s →
    4.3s) each correctly advanced the persisted position — no freeze.

- Phase 5g — Build: cursor advancement / auto-`Dropped` when the CURRENT
  tracked episode itself completes. The completion-freeze fix only made
  reconcile *record* a `Completed` reading; CLAUDE.md's later "When the
  CURRENT tracked episode itself completes" section specified the actual
  full behavior, which was genuinely missing: advance to the next episode
  at position 0, or auto-`Drop` if there is no next episode.
  - **This superseded, not extended, the freeze fix's own approach.**
    That fix landed a `Completed` reading at its own file's position
    (staying on the same episode). The correct behavior is different: a
    confirmed-complete episode should never land on itself at all — it
    should resolve the cursor one step further. Reverted the "Completed
    sets `best` at its own index" change; `Completed` (like absence) is no
    longer a landing candidate in the main forward-scan at all.
  - New `apply_completion_advance` (in `reconcile.rs`) runs only when the
    scan finds nothing real (`Resume`) anywhere and the series' own
    currently-tracked episode is confirmed complete — via absence (only
    counts with a real stored baseline, else a freshly-advanced/never-
    watched episode at position 0 would immediately "complete" again next
    call) or a real `Completed` reading (unconditional, since it's current
    real data). Walks forward past any further *consecutively* completed
    episode (a binge that quit near the end of more than one episode in a
    row), landing at position 0 of the first genuinely blank episode, or
    transitioning to `Dropped` if the walk runs out of episodes — all in
    one reconcile() call. Ordinary binge chaining (a real `Resume` position
    already sitting on a later episode) needed no new logic at all — the
    existing forward-scan already finds it directly, since it's still
    within the same scan range; `apply_completion_advance` only ever runs
    when that scan already came up empty. Auto-`Drop` never mutates the
    stored position/path, matching manual `drop`'s established rule, for
    the same reconcile-diffing-stability reason.
  - Rewrote the freeze-fix's own single-episode test: under the new rule, a
    single/last episode confirmed complete must auto-`Drop`, not update its
    own position and stay `InProgress` — the old expectation was
    superseded, not just extended.
  - Found and fixed a real collateral regression while re-running the full
    suite: three pre-existing `--root`-disambiguation tests in `main.rs`
    fabricated a bare `InProgress` record for a single-episode series with
    no matching mpv bookmark at all — now indistinguishable from "mpv
    deleted the bookmark, it's done," so reconcile correctly auto-dropped
    them before `pause`/`drop` ever ran, breaking those tests' original,
    unrelated intent. Fixed by fabricating a matching real bookmark in each,
    restoring a true reconcile no-op.
  - 219 tests total (217 unit + 2 integration), all passing; both clippy
    gates clean; `cargo fmt -p tsuzuki --check` clean; `vendor/` untouched.
    Manual repro against the real release binary with two real fixture
    episodes: episode 1 completing (absence) correctly advanced `Monster`
    to episode 2 at 0:00, still `in_progress`; episode 2 (the actual last
    episode) then also completing correctly auto-dropped it — confirmed
    via `list` (no longer `in_progress`), `start` (`"Rewatch?"`), and bare
    `tsuzuki` (0 in_progress, falls through to `start`'s listing).

- Phase 5.5 — Dogfood: three real bugs found trying the actual binary and
  real mpv by hand — the human dogfood pass Phase 5g's "Next up" flagged as
  still pending. None of these were caught by any prior gap-audit pass,
  since all three depend on real mpv behavior no fabricated-entry test had
  exercised.
  1. **`--start=<seconds>` inherited by whatever mpv played next in the
     playlist.** mpv treats a bare command-line option as a session-wide
     default, not a property of the one file it was given for — so once
     mpv advanced past the initially-resumed file (`playlist-next`, EOF
     auto-advance, an autoload script), the next file silently inherited
     the same start offset. **Fix** (`src/launch.rs`): `--start=` is now
     wrapped in mpv's own per-file option braces (`--{ --start=… file
     --}`), scoping it to just the one file `resume_playback` launches. No
     shell sits between this and `execve`, so `--{`/`--}` need no escaping
     here. New regression test spawns a fake `mpv` shell script and
     asserts the exact captured argv shape.
  2. **A tracked root nested inside another tracked root double-counted as
     a series.** Registering both `~/Anime` and `~/Anime/Rewatches`
     showed `Rewatches` twice: once correctly as its own root's series
     list, once incorrectly again as a "series" named `Rewatches` under
     `Anime`. **Fix** (`src/series.rs`): `series_in_root` now takes every
     currently-tracked root's own canonicalized real path and excludes any
     depth-1 child that matches one, using the same `roots::normalize`
     comparison `add`/`exclude` already use for root/exclusion idempotency
     — catches a symlinked or `..`-relative alias to a tracked root
     exactly like a literal path match would. All four call sites
     (`overview.rs`, `exclude.rs`, `mpv.rs`, `reconcile.rs`) updated to
     compute and pass this list. A `series.rs` test that had asserted the
     buggy behavior as "expected" was corrected, not just extended.
  3. **Absence of a `watch_later` entry silently treated as proof of
     completion — the serious one, real data loss.** Root cause, confirmed
     against mpv's own PR #8141 / issue #9623: mpv's `--resume-playback`
     (on by default, independent of who launches mpv) deletes a file's
     bookmark the instant it's *loaded*, regardless of what happens next —
     and mpv never rewrites a fresh one for a file left mid-playback via a
     playlist command before it's ever quit while still loaded. So "no
     entry for the current episode" was produced identically by a genuine
     completion and by an ordinary abandon (skip to a different episode,
     quit having spent under `resume_seconds` there, or none at all) —
     provably indistinguishable from `watch_later` state alone. The old
     rule (absence + the record's last stored position was real ⇒
     confirmed complete) silently discarded real progress on the abandoned
     episode and could even auto-`Drop` the whole series off a one-minute
     peek at its finale. **Fix** (`src/reconcile.rs`): removed the
     absence-based branch entirely. `reconcile_one_series` now tracks a
     single `any_completed_found` flag across the *whole* forward scan
     (not just the current episode) — only a real `Resume` position or a
     `Completed` classification found anywhere justifies moving the cursor
     or changing status; absence alone is a pure no-op. This does widen
     the already-accepted "clean binge leaves no trace" blind spot (a
     series finished via a true, silent EOF with *zero* further activity
     ever recorded no longer auto-`Drop`s) — accepted as the right
     tradeoff, since the ordinary way people finish an episode (quitting
     on or near it) still writes a `Completed` reading before deletion can
     happen, so auto-advance/auto-drop is unaffected for that common path.
     One test rewritten (it encoded the old buggy behavior as expected),
     one new regression test added reproducing the exact reported
     scenario; the other 7 completion-related tests were traced by hand
     against the new logic and confirmed to pass unchanged — a strict
     tightening, not a new heuristic. `CLAUDE.md`'s "Completion detection
     has a real, permanent blind spot," "When the CURRENT tracked episode
     itself completes," and "Cursor movement" sections corrected in place
     per its own doc-maintenance rule (deleted and replaced, not appended
     to).
  - Considered and explicitly rejected for finding 3: disabling mpv's
    `--resume-playback` globally via the user's own `mpv.conf` (breaks
    native resume for every other video the user watches, not just
    tracked anime — disproportionate); an IPC socket (only works when
    Tsuzuki is the launcher, contradicts "tracking is launcher-
    independent"); a third-party history-logging Lua script (a new, heavy
    dependency this project has never needed). Researched and confirmed
    viable but deliberately deferred: a small Tsuzuki-*owned* Lua script
    listening for mpv's `end-file` event with `reason=="eof"` (distinct
    from `"stop"`, which is exactly what `playlist-next` produces) would
    give a genuine positive completion signal, launcher-independent, read
    after the fact exactly like `watch_later` — written up as a follow-up
    in `/home/mhmdsamer/.claude/plans/so-the-question-is-humble-cupcake.md`,
    not blocking this fix.
  - 225 tests total (223 unit + 2 integration, up from 219), all passing;
    `cargo clippy -p tsuzuki --no-deps --all-targets -- -D warnings` clean;
    `vendor/anitomy` untouched.

- Phase 5.6 — Planning & design (chat only, no Rust code yet — matching
  the Phase 5a precedent): studied whether an mpv Lua script could
  simplify or replace parts of the watch_later-based tracking pipeline
  (full study: duration probing, candidate-path/merge complexity in
  `mpv.rs`, and the absence-based-completion ambiguity Phase 5.5 fixed
  were all identified as things a script makes strictly better or
  removable). Confirmed technically: a script is exactly as
  launcher-independent as `watch_later` (passive file writer, no IPC, no
  background process — mpv loads it identically regardless of what
  launched mpv). Decided: strictly opt-in, per-file preferred-source-
  with-fallback, never a hard requirement — a user who doesn't install it
  keeps today's exact behavior. `contrib/mpv/tsuzuki.lua` (new file, new
  `contrib/` directory) written and added to the repo; `CLAUDE.md`/
  `docs/design.md` updated with the decision and its reasoning.
  **Not yet wired into any Rust code** — `mpv.rs` doesn't read the
  script's log directory, `duration.rs`/`reconcile.rs` don't prefer it
  over the existing pipeline yet. That implementation is the next phase.

- Phase 6 — mpv companion script wiring (per-file preferred source when
  present, union with `watch_later`, watch_later fallback always intact):
  - **`src/script_log.rs`** (new): reads `$XDG_DATA_HOME/tsuzuki/mpv-log`
    (`mpv_log_dir()`, same base directory `state::state_path()` resolves
    `state.toml` under — fixed, deliberately not a `config.toml` field).
    `scan_script_log(dir, tracked_roots, home) -> Vec<ScriptLogEntry>`
    mirrors `mpv.rs`'s `scan_watch_later` shape deliberately: parses each
    flat entry file (`# <path>` / `duration=` / `position=` / `reason=`),
    discards `reason=="redirect"` entries and any malformed file without
    aborting the scan, and filters to tracked roots by reusing
    `mpv::is_under_a_tracked_root` (bumped from private to `pub(crate)` —
    the one, minimal touch to `mpv.rs` this phase needed) rather than
    forking the symlink-aware root-matching logic a second time.
  - **`src/reconcile.rs`**: candidate discovery is now a per-path union of
    both sources, not watch_later-only. New private `Candidate` enum
    (`WatchLater`/`ScriptLog`) and `merge_candidates()` pick a winner per
    source path by the underlying entry files' own mtime — strictly
    fresher wins, an exact tie goes to the script entry (deterministic,
    matching the OS-candidate merge's own tie precedent). New
    `classify_candidate()` fans out per source: a `watch_later` winner
    classifies exactly as before (unchanged `mpv::classify()` call); a
    script winner with `reason=="eof"` is `Completed` unconditionally, no
    `completion_remaining_seconds` check at all; any other script reason
    with a real position goes through the same `mpv::classify()` call,
    fed the script's own `duration` directly when present (skipping the
    Symphonia/AVI probe for that file entirely) or falling back to
    `duration::probe_duration` when the script never observed one;
    `reason=="redirect"` is discarded at parse time in `script_log.rs` and
    never reaches this layer at all. Everything downstream — the forward
    scan, `any_completed_found`, `apply_completion_advance`,
    auto-`Dropped` — is structurally unchanged, just now looks entries up
    in the merged `HashMap<PathBuf, Candidate>` instead of a
    `&[WatchLaterEntry]` slice.
  - **Testability without a new config field:** since the script-log
    directory is deliberately not configurable (unlike `watch_later_dir`,
    which tests redirect via `config.mpv.watch_later_dir`), the public
    `reconcile()` (always resolving the real directory via
    `script_log::mpv_log_dir()`) now delegates to a private
    `reconcile_with_script_log_dir(config, state, home, script_log_dir)`
    that tests call directly with a fabricated tempdir — `reconcile()`
    itself stays the only public entry point and is exercised for real by
    the new integration test below.
  - **`contrib/mpv/tsuzuki.lua`**: the one, small, contained change scope
    called for — `mkdir -p` was re-running on every single `end-file`
    event; gated behind a one-time `log_dir_created` flag (persists for
    the script's lifetime, i.e. once per mpv process launch). No other
    line of the script's logic, format, or hashing touched.
  - **`CLAUDE.md`** updated in place (not appended) per its own doc-
    maintenance rule: "Optional mpv companion script" rewritten from
    future/"studied and confirmed viable" tense to describe what's now
    actually built (union-not-enrichment, mtime-based source selection,
    per-source classification, the persists-forever diffing implication).
    "Completion detection has a real, permanent blind spot" retitled
    ("...blind spot for `watch_later` alone") and given a closing
    paragraph explaining the blind spot is now closable for anyone with
    the script installed, without weakening the underlying watch_later-
    only rule (absence is still never, by itself, proof of anything for a
    file with no script-log entry).
  - **Judgment call, flagged per the task brief's own request:** whether a
    script-sourced non-`eof` reading with no `duration` field should fall
    back to `duration::probe_duration` (this session's choice) or stay
    `Unknown`/skip the completion check entirely was not explicitly
    specified. Chose fallback-to-probe since the file is real and on disk
    regardless of source, and it's strictly more informative than treating
    a merely-not-yet-observed script duration as unreadable — flagged for
    review, not silently assumed correct.
  - 250 unit tests (up from 223 at the end of Phase 5.5) + 1 new
    integration test (`tests/script_log_integration.rs`, the required
    real-binary/real-XDG-path test for this phase) + the 2 existing
    `tests/state_persistence.rs` integration tests = 253 total, all
    passing; `cargo clippy -p tsuzuki --no-deps --all-targets --
    -D warnings` clean; `cargo fmt -p tsuzuki --check` clean (scoped to
    files this phase touched — `launch.rs` has a pre-existing,
    unrelated `cargo fmt` diff from before this phase, left untouched);
    `vendor/anitomy` confirmed untouched via `git diff --stat`.
  - New fabricated-entry tests cover every required scenario: the
    flagship script-only-eof-on-the-last-episode auto-`Drop` (zero
    watch_later trace at all — the actual blind-spot-closing claim),
    eof-on-a-non-last-episode cursor advancement, both-sources-present
    with watch_later fresher / script fresher / an exact mtime tie,
    `reason=="stop"` above and below the resume threshold, `redirect`
    discarded regardless of its position, a malformed script-log file
    alongside a valid one not aborting the scan, and reconcile called
    twice against an unchanged script-log entry that already reflects a
    prior drop confirming the drop isn't undone.

- Phase 6a — Correction: found by dogfooding the real binary against a
  scratch `$HOME` (running `/run catch-missing-tests`, not any automated
  test) — a real gap none of the 250+ fabricated-entry tests happened to
  exercise. A brand-new, never-before-tracked series whose only observed
  activity is a `Completed` classification — including a script `eof`
  reading, i.e. exactly the "clean binge, first viewing, script installed"
  case Phase 6's own flagship claim is about — silently produced **no
  `state.toml` record at all**. `list` kept showing "never started" forever
  even though the user watched the entire thing, for both a single-episode
  series (should auto-`Drop`) and a multi-episode one (should land
  `InProgress` at episode 2).
  - **Root cause, traced via `git log -p`, confirmed pre-existing (not a
    Phase 6 regression):** `reconcile_one_series`'s completion-advance
    branch (`src/reconcile.rs`, the "nothing real found, but something
    completed" case) was gated on `existing_index` being `Some(...)` since
    Phase 5g — the rule was implemented as "advance the CURRENT tracked
    episode," which silently assumed a tracked episode (an existing
    record) already had to exist. Phase 6's script `eof` path routed
    through this exact same gate, so it inherited the gap rather than
    introducing a new one — but it directly undercuts Phase 6's own
    headline claim for the single most obvious case (a series' very first
    viewing).
  - **Fix:** a never-tracked series (`existing_index == None`) now gets the
    same index-0 baseline treatment `Dropped`/never-started records already
    share for the ordinary `Resume` forward-scan (CLAUDE.md's "Forward-scan
    is strictly forward") — a placeholder record is created purely as the
    landing spot `apply_completion_advance` then resolves, landing
    `Dropped` or `InProgress` exactly as it would for an existing record. A
    genuinely `Dropped` record is deliberately excluded from this new
    treatment, unchanged from before — it stays a dead end only a real
    `Resume` reactivates, never a stray `Completed` reading.
  - CLAUDE.md's "When the CURRENT tracked episode itself completes"
    section updated in place (the stale "if this mechanism doesn't exist
    yet" hedge — true in Phase 5's own kickoff, false for years since —
    replaced with the actual fix and its reasoning, not appended next to
    the stale text).
  - 4 new regression tests: single-episode auto-Drop with no prior record,
    multi-episode advance-to-next-episode with no prior record, the same
    gap confirmed via an ordinary watch_later `Completed` reading (not
    script-specific), and an explicit guard proving a genuinely `Dropped`
    record still isn't reactivated by a fresh `Completed` reading. 254
    unit tests total (up from 250), all passing; both clippy/fmt gates
    clean; `vendor/anitomy` untouched. Manually reproduced against the real
    release binary using the exact previously-broken scenario (brand-new
    single-episode series, script `eof`, no prior record) both before and
    after the fix — confirmed silently invisible pre-fix, correctly
    `dropped`/`"Rewatch?"` post-fix.

- Phase 6b — Correction: investigated a live user report ("installed
  `tsuzuki.lua`, no logs ever appear in `~/.local/share/tsuzuki/`") against
  their real system and a genuine Flatpak `io.mpv.Mpv` install — reproduced
  and root-caused directly, not guessed at.
  - **Root cause, verified empirically:** `flatpak run --command=sh
    io.mpv.Mpv -c 'echo $XDG_DATA_HOME'` shows Flatpak overrides
    `XDG_DATA_HOME` for every sandboxed app to its own private per-app data
    directory (`~/.var/app/io.mpv.Mpv/data`), never the real
    `~/.local/share`. The script itself was working correctly — real,
    well-formed `eof`/`quit` entries were found sitting at
    `~/.var/app/io.mpv.Mpv/data/tsuzuki/mpv-log/`, genuinely written,
    permanently invisible to Tsuzuki's old single-fixed-path scan (which
    resolves `dirs::data_dir()` from its own, unsandboxed environment —
    the real `~/.local/share`). Also confirmed `host:ro` makes the real
    host home read-only inside the sandbox (`mkdir` under it fails with
    "Read-only file system"), ruling out "have the script use `$HOME`
    instead" as a fix on the script side.
  - This directly contradicted Phase 6's own original decision text
    ("unlike `watch_later`, there's no install-method variance to work
    around here") — the directory *name* is fixed and Tsuzuki-owned, but
    which real filesystem location it resolves to isn't, once two
    different processes' environments are doing the resolving.
  - **Fix:** `script_log.rs`'s single fixed directory became a small
    candidate list (`mpv_log_dirs()`), mirroring `mpv.rs`'s own
    `watch_later` install-method candidate table exactly: native
    (`dirs::data_dir()`, unchanged) plus the verified Flatpak remap.
    `scan_script_log` now merges across all candidate directories per
    source path, freshest-file-wins — the exact cross-directory merge
    precedent `watch_later` scanning already established. No Snap
    candidate added — its own `XDG_DATA_HOME` remapping behavior for a
    confined mpv hasn't been verified against a real install, and this
    project doesn't add candidate paths on a guess.
  - CLAUDE.md's "Optional mpv companion script" section corrected in place
    (the wrong "no install-method variance" claim replaced, not appended
    next to).
  - New tests: `mpv_log_dirs` includes the Flatpak candidate when home is
    known / degrades to native-only with no home; a same-source-path
    cross-directory merge test and a per-path-not-per-directory test
    mirroring `mpv.rs`'s own equivalents; a new real-binary integration
    test fabricating an entry *only* at the Flatpak-remapped path (nothing
    at the native path at all) and confirming Tsuzuki still finds it. 258
    unit tests total (up from 254) + the existing 2 + 2 integration tests
    = 262, all passing; both clippy/fmt gates clean; `vendor/anitomy`
    untouched. Manually reproduced against the user's own real system: the
    two genuine pre-existing log entries (Dungeon Meshi S01E14/S01E15,
    `eof`/`quit`) were confirmed sitting at the Flatpak-remapped path,
    invisible pre-fix; not independently re-verified post-fix against the
    user's live setup (their tracked roots/state weren't reproduced here),
    but the exact mechanism is covered end-to-end by the new integration
    test above.

- Phase 7 — `doctor`: the full check list (1a-1h from CLAUDE.md's kickoff
  decisions; 1i/MAL auth status excluded, Phase 8 doesn't exist yet), wired
  into `tsuzuki doctor [--fix] [--forget-orphans]`.
  - **`src/doctor.rs`** (new): every check is a pure function over
    already-resolved inputs (booleans, paths, file contents) — no check
    function itself shells out or touches real environment resolution
    (`dirs::config_dir()`, a real `flatpak` subprocess) directly, mirroring
    `launch.rs`'s own injectable-closure convention; that glue lives in
    `main.rs`'s `doctor_command` instead. `CheckResult { label, level: Ok |
    Warn | Fail, message }` is the common shape every check returns;
    `render()` formats one `[OK ]`/`[WARN]`/`[FAIL] label: message` line.
    - **1a mpv availability**: reuses `launch::native_mpv_on_path`/
      `flatpak_mpv_installed` (bumped `pub(crate)`, the same minimal-touch
      precedent Phase 6 already set for `mpv::is_under_a_tracked_root`) for
      detection, plus a new `launch::program_on_path` (the general form
      `native_mpv_on_path` now delegates to) and `is_executable_file`
      (also bumped `pub(crate)`) for validating a configured
      `command_override` actually resolves — `resolve_mpv_command` itself
      never validated the override's program exists, only split it, so
      doctor adds the missing check rather than reusing that function
      directly. Neither found → FAIL; exactly one → OK; both → WARN
      (ambiguous but not broken, since `resolve_mpv_command` always picks
      native); a broken override → FAIL even if native/Flatpak would
      otherwise resolve, since an override always takes priority at launch
      time too.
    - **1b mpv.conf**: `write_filename_flag_state` parses
      `write-filename-in-watch-later-config`'s last effective directive
      (bare flag / `=yes` / `no-` negation / commented-out-or-absent →
      `None`) — later directives override earlier ones, matching mpv's own
      config-parsing order. Checked only for install methods 1a actually
      detected (no native mpv → no native mpv.conf check) — an
      undetected-install's config file being "missing" isn't a real
      problem. Never writes to the file either way.
    - **1c watch_later reachability**: reuses `mpv::scan_watch_later`
      directly rather than re-implementing candidate resolution — its
      `ScanResult::warning` already carries exactly the
      "configured-override-doesn't-resolve" signal doctor needs. OS-default
      mode never FAILs (an absent candidate is normal); only a configured
      override that doesn't resolve does. `mpv::candidate_watch_later_dirs`
      bumped `pub(crate)` so doctor can report the OS-default candidate
      count without duplicating the table.
    - **1d Flatpak permission gap**: best-effort, `flatpak info
      --show-permissions io.mpv.Mpv` output (fetched by `main.rs`, parsed
      by a pure `check_flatpak_permissions`) scanned for
      `filesystem(s)=host`/`home`. Deliberately **never FAILs** under any
      input — sandbox permissions are the user's own security decision, and
      this check's own confidence (a substring scan, not real per-path
      verification) can't justify more than WARN regardless of what it
      finds. No `flatpak` CLI, no Flatpak mpv, or a failed subprocess call
      all degrade to a plain WARN saying so, never silently omitted.
    - **1e tsuzuki.lua**: `BUNDLED_SCRIPT` is `include_str!`'d from
      `contrib/mpv/tsuzuki.lua` at compile time, so `--fix` works from a
      distributed binary with no `contrib/` directory on disk. Comparison
      is a **direct byte-for-byte read**, not a hash — flagged as a
      judgment call below. `install_script` writes atomically (temp file in
      the same directory, then rename), matching `state.rs`'s own
      discipline exactly. Checked/fixed only for install methods 1a
      detected, same reasoning as 1b.
    - **1f mpv-log reachability**: every `script_log::mpv_log_dirs()`
      candidate checked — missing is `Ok` (indistinguishable from, and as
      normal as, "script just installed, mpv hasn't quit anything yet");
      existing-but-unreadable (a real permissions problem) is `Fail`.
    - **1g tracked-root validity**: plain existence/directory check per
      `config.roots.paths` entry.
    - **1h orphaned records**: `find_orphans` walks `series_in_root` under
      every root 1g found *valid* only — an unreachable root is skipped
      entirely for this check, never treated as "every series under it is
      gone" (CLAUDE.md's explicit false-positive concern for an unmounted
      drive). `forget_orphans` is a plain internal function (`state.series
      .retain(...)`) — no new public command or entry point, per decision
      2.
  - **Orphan-clearing command decision (locked in, not re-litigated):**
    `tsuzuki doctor --fix --forget-orphans` — both flags required together
    (`--forget-orphans` has `#[arg(long, requires = "fix")]` in `cli.rs`,
    enforced by clap itself, never a hand-rolled Tsuzuki check for this one
    dependency — matches the CLI bare-invocation contract's own precedent
    of leaving malformed-input-shaped cases to clap). Bulk-clears every
    *currently-confirmed* orphan in one pass, prints exactly what it's
    about to delete first. CLAUDE.md's "state.toml record whose series has
    been deleted" section updated in place (not appended) to reflect this
    exact command, replacing its old "not decided yet" placeholder.
  - **`main.rs`'s `doctor_command`**: runs `reconcile_state` first (the same
    "every state-touching command reconciles against mpv reality first"
    rule every other state-mutating command already follows — `--fix
    --forget-orphans` is a real `state.toml` mutation, so `doctor` counts),
    then runs every check in order, applying `--fix`'s script
    install/update inline (re-checking immediately after so the final
    report — and its exit code — reflect the post-fix reality, not the
    pre-fix one), then `--forget-orphans`'s deletion+write if requested.
    Every check line prints to **stdout**, not stderr — this report *is*
    doctor's real output/purpose, not an incidental warning about achieving
    some other goal (CLAUDE.md's stderr convention is for the latter).
    Exit 0 only if zero FAIL-level results remain after any mutation;
    `tsuzuki doctor && echo healthy` works as a script check. A trailing
    `MAL auth status: not checked — Phase 8 (MAL sync) doesn't exist yet.`
    line is always printed — not a check, not silently omitted either, per
    decision 1i.
  - **`cli.rs`**: `Doctor` grew from a bare stub variant to `Doctor { fix:
    bool, forget_orphans: bool }`.
  - 309 unit tests total (up from 258 at the end of Phase 6b — 47 new in
    `doctor.rs` covering every check function's full decision table in
    isolation, plus 4 new in `main.rs` covering `doctor_command`'s own
    glue: orphan-reported-but-not-cleared without `--forget-orphans`,
    `--fix` alone not clearing orphans, `--fix --forget-orphans` clearing
    exactly the confirmed orphan and leaving an unrelated still-tracked
    series completely untouched, and a no-orphans case leaving state.toml
    byte-for-byte unchanged) + 1 new integration test file
    (`tests/doctor_integration.rs`, 2 tests: the required real-binary/
    real-XDG-path test for this phase, spawning the compiled binary against
    a genuine scratch `$HOME` with a deliberately broken setup — an
    unreachable configured `watch_later_dir` override and one orphaned
    record — confirming the full report content and non-zero exit code,
    then confirming `--fix --forget-orphans` actually clears the orphan
    from the real on-disk `state.toml`; plus a second test confirming
    clap itself — not a hand-rolled check — rejects `--forget-orphans`
    without `--fix`) + the existing 6 (`script_log_integration.rs` ×2,
    `state_persistence.rs` ×2) = 315 tsuzuki-side tests total (up from
    262), all passing; `cargo clippy -p tsuzuki --no-deps --all-targets --
    -D warnings` clean; `cargo fmt -p tsuzuki --check` clean; `vendor/
    anitomy` confirmed untouched via `git diff --stat`.
  - Manually dogfooded against a real scratch `$HOME` on this machine's
    real, standing Flatpak mpv install (no native mpv on this machine):
    fresh install → `doctor` correctly reports mpv.conf missing and
    tsuzuki.lua not installed (both FAIL) alongside OK results for
    watch_later/mpv-log/tracked-root/orphans → `doctor --fix` installs
    tsuzuki.lua, confirmed byte-identical to `contrib/mpv/tsuzuki.lua` via
    `diff`, and a re-run shows it OK → fabricated a real bookmark, ran
    `resume` to create a genuine `in_progress` record, deleted that
    series' folder out from under its still-tracked root → `doctor`
    correctly reports it as the one orphan (and only that one — a sibling
    series whose folder still exists is never flagged) with the exact
    `tsuzuki doctor --fix --forget-orphans` invocation → running that
    exact command clears it, confirmed via a direct read of the real
    `state.toml` (`series = []`) afterward.
  - **Judgment calls, flagged per the task brief's own request:**
    1. **1e's "hash compare" was implemented as a direct byte-for-byte
       comparison instead.** The file is tiny (a few KB), so a real
       comparison costs nothing extra over hashing and carries zero
       collision risk — hashing's only actual advantage (comparing short
       digests instead of full content) doesn't matter at this size. No
       new dependency needed either way (a hash would have used
       `std::collections::hash_map::DefaultHasher`, stdlib-only, but the
       direct comparison needs even less code).
    2. **1d (Flatpak permission gap) is capped at WARN, never FAIL, by
       design** — not explicitly specified as a severity level in the
       kickoff decisions, only as "diagnostic only, no --fix." Chose WARN
       as the ceiling since the check's own confidence is inherently
       limited (a substring scan over `flatpak info --show-permissions`
       output, not real path-by-path verification) and the underlying
       fact — if genuinely narrow — is a deliberate user security choice,
       not a bug Tsuzuki should ever assert is definitely broken.
    3. **This development/test environment could not verify 1d's happy
       path against a real narrow-permission Flatpak install** — only
       against fabricated `flatpak info --show-permissions` output text
       (unit-tested directly) and the real environment's actual broad
       default permissions (dogfooded, produced the expected WARN). The
       `flatpak info --show-permissions` output format itself was not
       independently verified against multiple real flatpak versions;
       if a future flatpak release changes that output's shape, this
       check's substring scan may need updating — it already degrades to
       an honest WARN rather than a wrong OK/FAIL if the format drifts
       enough to just not match either pattern, so this isn't considered
       blocking, but it's worth naming as the check's real limitation.
    4. **mpv.conf/tsuzuki.lua checks are gated on 1a's install-method
       detection** (not checked at all for an install method with no mpv
       found) — not explicitly specified either way in the kickoff
       decisions. Chose this over checking both candidate locations
       unconditionally, since an install method with no mpv at all has no
       real mpv.conf/scripts directory to be right or wrong about yet;
       the alternative would produce a spurious FAIL for, say, the
       Flatpak mpv.conf on a machine that only has native mpv installed.

- Phase 7a — Correction: found by dogfooding the real release binary
  against a scratch `$HOME` (`/run catch-missing-tests`, not any automated
  test) — a real gap none of the 310 fabricated-entry tests happened to
  exercise. A present-but-unreadable `mpv.conf` (permission denied on the
  file itself) was reported identically to a genuinely missing one —
  `"...mpv.conf does not exist. Add this line to it..."` — which would send
  a user chasing "create the file" for what's actually a permissions
  problem. This is exactly the missing-vs-unreadable distinction
  `check_mpv_log_dirs` (1f) already makes correctly; 1b's `check_mpv_conf`
  simply never made the same distinction.
  - Also dogfooded and confirmed correct, no further gaps found: composable
    roots sharing a series name (`~/Anime/Monster` vs.
    `~/Anime/Rewatches/Monster`, both `in_progress`) — deleting only one
    copy's folder flagged exactly that one as orphaned, never the other;
    `--forget-orphans` with genuinely zero orphans present prints no
    spurious "forgetting 0 record(s)" message and leaves `state.toml`
    untouched; running `doctor --fix` twice in a row is fully idempotent
    (identical output, exit 0 both times) once every fixable problem is
    actually fixed; a pristine `$HOME` with zero tracked roots at all
    doesn't panic or print anything nonsensical for the tracked-root/orphan
    checks.
  - **Fix** (`src/doctor.rs`'s `check_mpv_conf`): matches on `fs::read_to_string`'s
    error `kind()` instead of collapsing every `Err` to "missing" —
    `NotFound` keeps the existing message; any other error (permission
    denied, and in principle anything else `read_to_string` can fail with)
    now reports `"<path> exists but is unreadable: <e>"`, mirroring 1f's
    own wording exactly. Never changes what's checked or whether the
    result is FAIL — a real problem either way — only makes the message
    honest about which real problem it is.
  - New regression test (`#[cfg(unix)]`, chmod `0o000` on a real file, same
    permission-manipulation pattern `check_mpv_log_dirs`'s own unreadable-
    directory test already uses) asserts the message says "unreadable" and
    explicitly asserts it does *not* say "does not exist" — confirmed
    failing pre-fix by re-running it against the reverted code path before
    landing the fix, not just written to pass. 310 unit tests total (up
    from 309), all passing; both clippy/fmt gates clean; `vendor/anitomy`
    untouched. Manually reproduced against the real release binary:
    `chmod 000` on a real `mpv.conf` now correctly reports "exists but is
    unreadable: Permission denied (os error 13)" instead of "does not
    exist."

- Phase 8a — MAL OAuth login, token storage/refresh, `doctor`'s MAL-auth
  check (`src/mal.rs`, new; `src/doctor.rs`'s `check_mal_auth`; `src/main.rs`'s
  `sync_login_command` and `doctor_command` wiring; `src/state.rs`'s `write`
  refactored to share a new `atomic_write` helper; `[mal] redirect_port`
  added to `config.rs`, default `8734`):
  - `tsuzuki sync login`: PKCE (RFC 7636, `plain` method — MAL doesn't
    support S256), a CSRF `state` value, a local `127.0.0.1`-only callback
    listener (never `0.0.0.0`) that accepts exactly one *valid* callback
    then shuts down, a 120s timeout with no hang, and code-for-token
    exchange. The listener's CSRF guard is real, not just asserted: a
    state-mismatched callback is rejected and listening continues; only a
    matching `state` resolves it.
  - Token storage: `~/.local/share/tsuzuki/mal_token.toml`, same XDG data
    dir as `state.toml`, written atomically via the same shared
    `state::atomic_write` helper (no second temp-file-then-rename path),
    `0600` permissions on Unix.
  - Automatic refresh: any check of token status looks at `expires_at` with
    a 60s safety margin first and only refreshes (a real network call) when
    actually needed — a valid, non-expiring token triggers zero network
    calls, confirmed by a unit test using a client that errors if ever
    invoked. A rejected refresh (dead `refresh_token`) is distinguished from
    a transport-level failure (offline, DNS, ...) — the former is `doctor`'s
    only path to FAIL on this check; the latter stays WARN, matching the
    existing Flatpak-permissions check's own "connectivity noise never
    fails the whole run" precedent.
  - `doctor`'s 1i (MAL auth status) is real: `crate::mal::token_status`
    resolves the real state (file read + conditional refresh), `doctor.rs`'s
    `check_mal_auth` does the pure OK/WARN/FAIL mapping over it — same
    "pure function over already-resolved inputs" shape every other check in
    that module already follows. Never-configured/never-logged-in → WARN
    (expected initial state, same category as "Flatpak mpv not detected").
    Rejected refresh → FAIL (confirmed, actionable). Network error → WARN
    (best-effort, never escalates on pure connectivity noise). No `--fix`
    action for this check — `sync login` is the only remedy, same
    diagnostic-only precedent as the Flatpak-permission check (1d).
  - HTTP client: `ureq` (blocking-only, no async runtime/tokio — matters
    directly against CLAUDE.md §5's "MAL calls must stay lazy/on-demand"),
    `native-tls` feature so it actually uses the pkg-config+openssl
    `shell.nix` already provisions, instead of pulling in rustls as a
    second, unused TLS stack. Every function above the HTTP layer (PKCE,
    URL building, the callback listener, token-status logic) goes through
    the injectable `MalHttpClient` trait — real network access is isolated
    to one `UreqClient` impl, never touched by the default test run.
  - Security: no real/"known" client_id or client_secret anywhere in source,
    comments, or tests (grepped, confirmed clean — see this task's own
    report). `client_id` only ever comes from the user's own `config.toml`
    (never committed). `mal_token.toml` lives outside the repo (XDG data
    dir), never touched by version control. `TokenRecord` hand-writes its
    own `Debug` impl to redact both token fields, so even an accidental
    `{:?}` can't leak a real credential — verified by a unit test. No code
    path prints or logs a raw token/code value; `doctor`/error output only
    ever shows redacted state (configured/not-configured/expiry seconds) or
    opaque error strings.
  - Judgment call, flagged per this task's own instructions rather than
    silently resolved: whether MAL's token endpoint genuinely requires no
    `client_secret` for this PKCE flow is *unverified* — this session has no
    real, registered `client_id` to test against. Implemented per
    design.md/CLAUDE.md's stated assumption (a public/native-client PKCE
    flow needs none) and left unconfirmed; the `#[ignore]`d integration test
    (`tests/mal_login_integration.rs`) is exactly where this would surface
    the first time someone runs it for real.
  - 346 tests passing (up from 310 — 36 new: 30 in `src/mal.rs`, 6 in
    `doctor.rs`'s new `check_mal_auth` suite), plus one `#[ignore]`d
    real-API integration test (documented setup steps at the top of its own
    file, genuinely never run this session — flagged explicitly, not
    claimed as verified). Both clippy (`-p tsuzuki --no-deps --all-targets
    -D warnings`) and fmt gates clean. `vendor/anitomy` untouched. One
    existing test (`tests/doctor_integration.rs`'s orphan-detection test)
    updated: it asserted on the old Phase-7 "MAL auth status: not checked"
    placeholder line, now asserts on the real `mal-auth: ... not configured`
    WARN output instead.

- Phase 8a-correction — reversed the original assumption that every user
  must register their own MAL API application before `sync login` could
  work. That assumption was wrong: MAL's Authorization Code + PKCE flow for
  a public/native-client app type does not treat `client_id` as
  confidential (only `client_secret` is, and Tsuzuki never has one) —
  confirmed by Taiga (erengy/taiga)'s own long-standing, public
  `client_id` constant in `src/sync/myanimelist.h`, the same pattern.
  - `src/mal.rs`: added `pub(crate) const DEFAULT_CLIENT_ID` (a real,
    already-registered client_id, committed deliberately — its doc comment
    explains why this is safe and warns against a future session
    "helpfully" removing it as a leaked credential) and
    `resolve_client_id(config)`, the single seam every caller goes through:
    empty `[mal] client_id` (the shipped default) → `DEFAULT_CLIENT_ID`;
    non-empty → the user's override, for anyone running their own fork
    against their own registration.
  - `src/main.rs`: `sync_login_command`'s old "no client_id configured, run
    `sync login` first" hard-fail is gone — both it and `doctor_command` now
    resolve through `mal::resolve_client_id` instead of reading
    `config.mal.client_id` directly. `tsuzuki sync login` now works against
    a completely untouched `config.toml` — no config edits required before
    first use.
  - `doctor.rs`'s `check_mal_auth` itself needed no changes — `TokenStatus`'s
    `NotConfigured` branch is still there (defensive only, for a direct
    `client_id: ""` call) but is no longer reachable through the real
    `doctor`/`sync login` call path, since both now always pass an
    already-resolved, non-empty client_id.
  - `tests/doctor_integration.rs`'s mal-auth assertion updated again: an
    empty `[mal] client_id` no longer produces "not configured" (that state
    isn't reachable from an empty config anymore) — a fresh `$HOME` with no
    `mal_token.toml` now correctly reports `mal-auth: ... not logged in`
    instead.
  - `tests/mal_login_integration.rs` rewritten to exercise the real
    zero-config default path by default (no env var needed at all anymore)
    — `TSUZUKI_TEST_MAL_CLIENT_ID` is now an optional way to test the
    override path specifically, not a requirement to run the test at all.
  - Verified: the real authorize URL through the actual zero-config default
    path (`Config::default()`, no `[mal] client_id` set) contains
    `client_id=ef3b1252d151e7a0ef248753dab34551` and
    `redirect_uri=http%3A%2F%2F127.0.0.1%3A8734%2Fcallback` exactly (printed
    via `cargo test print_real_default_authorize_url_from_zero_config --
    --nocapture`, asserted on directly in the test itself too). Grepped for
    `client_secret` (zero hits that are an actual field/value — only
    explanatory prose) and every 32-hex-char string in source (only
    `DEFAULT_CLIENT_ID` itself, its own test assertion, and the unrelated
    vendored-anitomy commit hash in CLAUDE.md).
  - 348 tests passing (up from 346 — 4 new: 2 `resolve_client_id` tests plus
    the rewritten default-URL print/assert test replacing the old
    fake-client-id one). Both clippy and fmt gates clean. `vendor/anitomy`
    untouched.
  - **Not verified by this session — flagged explicitly, same as Phase 8a's
    own report:** whether MAL's real token endpoint actually accepts this
    committed `client_id` for a genuine login/exchange has never been
    tested against the live API. The real `sync login` flow needs to be run
    by hand once this lands to confirm MAL accepts it end to end — this
    session did not and was not asked to attempt that.
  - Docs updated to match: design.md §7 (new "No per-user MAL app
    registration required" paragraph, replacing the old registration-first
    framing) and §8's example `client_id` comment; CLAUDE.md gained a new
    §3 decision section ("MAL `client_id` is a built-in, publicly committed
    credential — not a per-user registration requirement") stating the
    reversal and its reasoning explicitly, specifically so a future session
    doesn't "fix" this back the other way without the context.

**Next up:**
- Phase 8b: exact-match linking against the user's own MAL list, the
  two-way non-destructive episode-progress merge (never a status push —
  see design.md §7's scope-boundary line). Builds directly on Phase 8a's
  `MalHttpClient`/token-refresh plumbing.

**Open questions blocking further work:**
- cleared