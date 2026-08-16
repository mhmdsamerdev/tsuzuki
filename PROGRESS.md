# PROGRESS.md — Tsuzuki build status

Status file, not a spec. Decisions and reasoning live in `CLAUDE.md` and
`docs/design.md` — this just tracks where the build currently stands.

**Current phase:** Phase 2 — Exclusions (done)

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

**Next up:**
- Phase 3 — Anitomy integration (design.md §11): episode-identity
  parsing (annotation only, never reordering — see CLAUDE.md's "Anitomy
  only annotates, never reorders"), and bare `tsuzuki exclude`'s
  suggested-candidates flow.

**Open questions blocking further work:**
- (none)
