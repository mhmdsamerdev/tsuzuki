# PROGRESS.md — Tsuzuki build status

Status file, not a spec. Decisions and reasoning live in `CLAUDE.md` and
`docs/design.md` — this just tracks where the build currently stands.

**Current phase:** Phase 4 — mpv `watch_later` reading (done)

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
- Phase 5 — Planning & design (chat only, no code yet):
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

**Next up:**
- Phase 5 — State machine & core commands (design.md §11). Design work is
  done — every open question from Phase 4 is resolved, plus a full round
  of additional Phase 5-specific decisions (state.toml's schema, the
  shared reconcile-before-acting rule, pause/drop semantics and allowed
  transitions, mpv launch resolution, the `current` command being cut,
  start's three display words) are all written into CLAUDE.md. Kickoff
  prompt is written and ready to hand off. Code not started yet. Dogfood
  checkpoint per CLAUDE.md §6: pause and confirm with the user after this
  phase before continuing to Phase 6.

**Open questions blocking further work:**
- cleared