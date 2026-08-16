# PROGRESS.md — Tsuzuki build status

Status file, not a spec. Decisions and reasoning live in `CLAUDE.md` and
`docs/design.md` — this just tracks where the build currently stands.

**Current phase:** Phase 3 — Anitomy integration (done)

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

**Next up:**
- Phase 4 — mpv `watch_later` reading (design.md §11/§4) — not yet started.

**Open questions blocking further work:**
- (none)
