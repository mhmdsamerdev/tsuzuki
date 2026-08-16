# PROGRESS.md — Tsuzuki build status

Status file, not a spec. Decisions and reasoning live in `CLAUDE.md` and
`docs/design.md` — this just tracks where the build currently stands.

**Current phase:** Phase 1 — Tracked roots & episode ordering (done)

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

**Next up:**
- Phase 2 — Exclusions (design.md §11): `tsuzuki exclude`, path
  resolution against tracked roots, the multi-root ambiguity case,
  wiring exclusions to run before the Phase 1 sort.

**Open questions blocking further work:**
- (none)
