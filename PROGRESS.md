# PROGRESS.md — Tsuzuki build status

Status file, not a spec. Decisions and reasoning live in `CLAUDE.md` and
`docs/design.md` — this only tracks where the build stands. Session
narrative, bug-hunt traces, and dogfood step-by-steps are not kept here
once a fix lands; git history has that story, and any decision worth
protecting from regression has already been folded into `CLAUDE.md`.

**Current phase:** Dogfooding is done; Phase 9 is now UX polish. Fixed so far:
- `sync` was pushing the tracked cursor's raw episode number to MAL
  instead of the last actually-watched episode (off by one in the common
  case) — fixed in `sync::local_effective_episode`; see CLAUDE.md's "MAL
  push number is the last watched episode, not the tracked cursor."
- `sync` terminal output was noisy: every one of the top-5 fuzzy
  candidates was printed with a raw similarity percentage, even fully
  unrelated series. Redesigned to one concise, symbol-coded line per
  series (`✓`/`⚠`/`✗`), a single "did you mean...?" suggestion only when
  it clears a new `[mal] fuzzy_match_threshold` config value (default
  `0.8`), and a summary line that omits zero-count categories — see
  CLAUDE.md's "Sync output only ever suggests a confident fuzzy match,
  and stays one line per series."
- `doctor` terminal output was even noisier: one `[OK]`/`[WARN]`/`[FAIL]`
  row per internal check function (two mpv.conf rows, two tsuzuki.lua
  rows, up to three mpv-log rows), full of raw paths, candidate-directory
  counts, and token-expiration seconds. Redesigned to one `✓`/`⚠`/`✗` row
  per conceptual check — install-scoped checks folded across every
  detected mpv install via a new `doctor::combine` helper — an indented
  `→ <command>` line only when there's a concrete next step, and a
  closing "All checks passed." / "`N` issue(s) found." summary. Severity
  levels and exit-code logic are unchanged — display only. See CLAUDE.md's
  "Doctor output is a concise health report, not a narration of every
  internal check."
- `tsuzuki config get/set/list/edit` turned out to have never actually
  been implemented — a Phase-0 stub (`not implemented: config get`, etc.)
  never assigned to a real phase. Rather than just fill in that shape,
  restudied what `config` is actually for on this tool specifically (one
  person, their own machine, a file already designed to be hand-edited)
  and cut it down to two real actions: bare `tsuzuki config` shows the
  resolved config (real path, defaults filled in, a footer pointing at
  `edit`); `tsuzuki config edit` opens it in `$EDITOR`, blocks, and
  re-validates on exit. `get`/`set`/`list` are gone — `roots.paths`/
  `exclusions` already have owning commands (`add`/`exclude`), and a
  dotted-key settings-store CLI would duplicate what hand-editing already
  does better for a file that's meant to be versioned as a whole. See
  CLAUDE.md's "`tsuzuki config` is show/edit, not a settings-store CLI."
- Same concise `✓`/`⚠`/`✗` output treatment extended to the remaining
  commands: `add`, `exclude`, `list`, `status`, `pause`, `drop`,
  `sync link`, plus the shared error/warning message helpers. Every
  command's output is now consistent with the visual vocabulary `sync`/
  `doctor` established — Phase 9 UX polish is done.

**Test suite:** 399 passing + 4 `#[ignore]`d real-API tests (excluded
from the default run). Both clippy gates (`-p tsuzuki --no-deps
--all-targets -D warnings`) and `cargo fmt -p tsuzuki --check` clean.
`vendor/anitomy` untouched throughout.

---

## Done

- **Phase 0 — Scaffolding & data model.** Cargo binary crate, full clap
  CLI skeleton, config.toml/state.toml read/write (atomic for state).

- **Phase 1 — Tracked roots & episode ordering.** `tsuzuki add`
  (`src/roots.rs`), idempotent, persists roots exactly as typed.
  Hierarchical natural-sort ordering engine (`src/series.rs`), symlink-
  aware (stat semantics, cycle-protected).

- **Phase 2 — Exclusions.** `tsuzuki exclude <path> [--root]`
  (`src/exclude.rs`): full ambiguity/idempotency/validation decision
  table from CLAUDE.md §3. Exclusion filtering wired into
  `series.rs`'s tree walk pre-sort.

- **Phase 3 — Anitomy integration.** Vendored `tsuzuki-anitomy` wired as
  a workspace member (fixed a rename leftover in its own `Cargo.toml`).
  `src/anitomy.rs`'s `episode_identity()` annotates ordered files with a
  parsed or ordinal episode number. Bare `tsuzuki exclude` candidate
  suggestions (OVA/Special/Movie/etc.) via `exclude.rs`.

- **Phase 4 — mpv `watch_later` reading.** `src/duration.rs` (Symphonia
  content-probe + AVI fallback, ~212µs/file release). `src/mpv.rs`:
  candidate-path table, redirect-entry filtering, `classify()`
  (Resume/StartFromZero/Completed). Real ffmpeg-generated fixture files
  added for probe testing. Not yet wired to any command.

- **Phase 5 — State machine & core commands** (5a planning, 5b build, 5c
  gap audit, 5d–5g corrections, 5.5 dogfood, 5.6 planning). The payoff
  phase: `state.rs` rekeyed to `(root, name)`; `reconcile.rs` (the shared
  mpv-reality resolution step); `launch.rs` (mpv spawn resolution);
  `pause_drop.rs`; `picker.rs`; `overview.rs` (Start/Resume/Rewatch?
  display words). `tsuzuki`/`resume`/`status`/`list`/`start`/`pause`/
  `drop` all wired in; `current` cut. Dogfooded end-to-end against a
  fabricated `$HOME`, which is where most of the real design gaps
  surfaced and got fixed:
  - config.toml forward-compatibility (missing-field defaults, 5d).
  - resume message missing episode/timestamp (5e).
  - reconcile freezing once a position crossed into the completion
    window — `Completed` needs to still land somewhere (5f).
  - cursor auto-advance / auto-`Dropped` when the current tracked
    episode itself completes (5g).
  - three real bugs from dogfooding against **real mpv** (5.5): a bare
    `--start=` leaking to the next playlist file (fixed via mpv's
    per-file `--{ --start=… --}` braces); a root nested inside another
    tracked root double-counting as a series; and the big one — absence
    of a `watch_later` entry being wrongly treated as proof of
    completion, silently discarding real progress (removed the
    absence-based branch entirely; see CLAUDE.md's "Completion detection
    has a real blind spot").
  - 5.6: studied and designed the mpv companion-script approach that
    closes the absence blind spot (see Phase 6).

- **Phase 6 — mpv companion script wiring** (+6a, 6b corrections).
  `src/script_log.rs` reads `contrib/mpv/tsuzuki.lua`'s log directory;
  `reconcile.rs` unions it with `watch_later` per path
  (`merge_candidates`, mtime-based winner). Corrections:
  - 6a: a never-before-tracked series whose only activity was a
    `Completed`/`eof` reading produced no record at all — fixed by
    giving it the same index-0 placeholder baseline dropped/never-started
    records already use.
  - 6b: Flatpak remaps `XDG_DATA_HOME` per-app, so the script's real log
    directory was invisible to Tsuzuki's single fixed path — fixed with
    a candidate-list merge (native + verified Flatpak remap), mirroring
    `watch_later`'s own install-method table.

- **Phase 7 — `doctor`** (+7a correction). `src/doctor.rs`: mpv
  resolvability, mpv.conf flag, watch_later reachability, Flatpak
  permission gap (WARN-only, never FAIL), tsuzuki.lua install/update
  (`--fix`), mpv-log reachability, tracked-root validity, orphaned
  `state.toml` records. `tsuzuki doctor --fix --forget-orphans` clears
  confirmed orphans in one pass. 7a: a present-but-unreadable `mpv.conf`
  was misreported as "missing" — fixed to distinguish `NotFound` from
  other read errors.

- **Phase 8a — MAL OAuth login** (+correction, +2 fixes). `src/mal.rs`:
  PKCE + CSRF-guarded local callback listener, token storage
  (`mal_token.toml`, atomic, `0600`, redacted `Debug`), automatic
  refresh. `doctor`'s MAL-auth check wired in. Corrections, found by
  live dogfooding against a real MAL account:
  - reversed the original "every user must register their own MAL app"
    assumption — PKCE public-client apps don't need a `client_secret`,
    so a built-in committed `client_id` is safe (see CLAUDE.md §3).
  - a real TLS bug: `ureq`'s bare shortcut functions never picked up the
    `native-tls` feature — fixed by building an explicit `AgentBuilder`
    with a `TlsConnector`.
  - a real MAL-side bug: the first registered `client_id` turned out to
    be a "web" app type requiring a secret despite PKCE — swapped to a
    verified secret-less registration. Full login flow (browser →
    approve → token exchange → `mal_token.toml` written) confirmed
    working end-to-end against the live API.

- **Phase 8b — MAL linking + two-way merge.** `src/sync.rs`: exact-title
  matching against the user's own MAL list (never fuzzy — similarity
  only ranks *displayed* candidates), `sync link --mal-id` (verified
  against the list before persisting), `decide_merge`'s
  never-write-a-smaller-number invariant, `push_episode_count`. Caught
  before shipping: MAL's PATCH field name (`num_watched_episodes`) is
  swapped from the read field name (`num_episodes_watched`) — see
  CLAUDE.md §3. Link storage (`state.toml`, not `config.toml`) and the
  `mal_episode_awareness` field's separation from `episode_number` are
  also documented there.

---

## Next up

Phase 9 UX polish is done. What's left is the rest of Phase 9 — distribution
& end-to-end tests (design.md §12), none of it started yet:

- `cargo-dist` config (`cargo dist init` has never been run; no
  `[workspace.metadata.dist]` in `Cargo.toml` yet, though `shell.nix`
  already provisions the binary).
- Install script (`curl ... | sh`), generated off the cargo-dist config
  above.
- GitHub Release CI (no `.github/workflows/` exists yet).
- PyPI wheel via `maturin` (design.md §10's third distribution door).
- `README.md` — doesn't exist anywhere in the repo yet; needed before any
  of the above actually make the project installable by someone else.
- `Cargo.toml` publish metadata (`description`/`license`/`repository`/
  `readme` — currently only `name`/`version`/`edition`).
- A dedicated end-to-end CLI test suite (`tests/cli.rs`, referenced but
  not yet added per `COMMANDS.md`'s own "Distribution"/"End-to-end tests"
  sections) — six real-binary integration tests already exist per-phase
  (config/doctor/mal-login/mal-sync/script-log/state-persistence), so
  this is about a comprehensive top-to-bottom CLI walkthrough, not a
  correctness gap.

## Open questions blocking further work

None.
