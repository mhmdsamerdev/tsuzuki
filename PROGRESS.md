# PROGRESS.md — Tsuzuki build status

Status file, not a spec. Decisions and reasoning live in `CLAUDE.md` and
`docs/design.md` — this just tracks where the build currently stands.

**Current phase:** Phase 5 — State machine & core commands (implemented,
verified, dogfooded — paused at the dogfood checkpoint for user review
before Phase 6)

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
  - Also checked for a referenced "prior correction" to conditional
    `--start` launch logic the report asked to be re-verified — found no
    trace of it in `git log`, the current code (`launch.rs`'s `--start=`
    is unconditional, always appended), or any other running session.
    Flagged rather than fabricated.
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
  - **Flagged, not acted on:** CLAUDE.md documents a separate, already-
    written "bare spawn, no unconditional `--start`" correction for
    `launch.rs` as settled, but `launch.rs` still unconditionally passes
    `--start=<seconds>` on every launch — that fix has not actually landed
    in code yet. Out of scope here (explicitly excluded); noted for
    whoever picks up that correction next.

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

**Next up:**
- Phase 6's actual Rust-side wiring is unbuilt: `mpv.rs` needs to read
  `contrib/mpv/tsuzuki.lua`'s log directory, and `duration.rs`/
  `reconcile.rs` need the per-file preferred-source-with-fallback rule
  described in Phase 6's own entry. The script file and its docs exist;
  the Rust code consuming them does not yet.
- The `--start` conditional-launch correction CLAUDE.md describes is not
  yet implemented in `launch.rs` — flagged during Phase 5g, still not
  fixed (distinct from Phase 5.5 finding 1's per-file *scoping* fix, which
  addresses inheritance across files, not whether `--start` is passed at
  all).

**Open questions blocking further work:**
- cleared