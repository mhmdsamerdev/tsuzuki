//! The dedicated end-to-end CLI test suite (`docs/design.md` §12's Phase 9
//! "exercise the whole CLI surface end-to-end" line item, `PROGRESS.md`'s
//! distribution checklist).
//!
//! Every other integration test file under `tests/` proves one specific
//! phase's own flagship claim in isolation. This file is different on
//! purpose: each test here is a realistic, multi-command *user session* —
//! `add` a real-shaped library, fabricate the mpv/`tsuzuki.lua` activity a
//! real evening of watching would leave behind, then walk the same
//! sequence of commands a real person actually types (`list`, `start`,
//! `pause`, `doctor`, ...) — checking that the commands agree with each
//! other about what happened, across real process invocations, the way an
//! actual user's shell history would. All of it against the real compiled
//! binary (`env!("CARGO_BIN_EXE_tsuzuki")`) and a real temp `$HOME`, never
//! an in-process function call — same standing rule as every other file in
//! this directory.
//!
//! No real mpv install or real MAL account is used or required — mpv's own
//! `watch_later`/companion-script log files are fabricated by hand, exactly
//! shaped the way real mpv/`tsuzuki.lua` activity would leave them (proven
//! formats, cross-checked against `src/mpv.rs`/`src/script_log.rs`), and
//! `[mpv] command_override` is pointed at `true` wherever a command would
//! otherwise try to spawn a real mpv. `tests/mal_login_integration.rs`/
//! `tests/mal_sync_integration.rs` already own the real-network-required
//! paths (both `#[ignore]`d) — nothing here makes a network call.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread::sleep;
use std::time::Duration;

fn tsuzuki_bin() -> &'static str {
    env!("CARGO_BIN_EXE_tsuzuki")
}

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(tsuzuki_bin())
        .args(args)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .output()
        .expect("failed to spawn the tsuzuki binary")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Runs `tsuzuki <args>`, asserting success, and returns stdout — the
/// common case for every command used purely to advance a scenario.
fn run_ok(home: &Path, args: &[&str]) -> String {
    let output = run(home, args);
    assert!(
        output.status.success(),
        "`tsuzuki {}` should have succeeded\nstdout: {}\nstderr: {}",
        args.join(" "),
        stdout_of(&output),
        stderr_of(&output),
    );
    stdout_of(&output)
}

fn config_path(home: &Path) -> PathBuf {
    home.join(".config").join("tsuzuki").join("config.toml")
}

fn state_path(home: &Path) -> PathBuf {
    home.join(".local")
        .join("share")
        .join("tsuzuki")
        .join("state.toml")
}

fn touch(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::File::create(path).unwrap();
}

/// Registers `root` (an absolute path under `home`) as a tracked root.
fn add_root(home: &Path, root: &Path) {
    run_ok(home, &["add", root.to_str().unwrap()]);
}

/// Points `[mpv] command_override` at `true` — a real, always-present,
/// instantly-exiting program — so a command that launches mpv (`resume`/
/// `start`) never depends on a real mpv install being on this machine.
/// Mirrors `tests/state_persistence.rs`'s own established trick.
fn bypass_real_mpv(home: &Path) {
    let path = config_path(home);
    let patched = fs::read_to_string(&path)
        .unwrap()
        .replace("command_override = \"\"", "command_override = \"true\"");
    fs::write(&path, patched).unwrap();
}

/// Fabricates one real mpv `watch_later` entry for `source_file`, exactly
/// the shape `src/mpv.rs::parse_entry_file` expects (`# <path>` then
/// `start=<seconds>`), in the first OS-default candidate directory
/// (`~/.local/state/mpv/watch_later`, matching every other integration
/// test's precedent — never the config override, so no `config.toml`
/// change is needed just to exercise this path). `entry_name` only needs
/// to be unique within the directory — mpv itself names these by an MD5
/// hash of the source path, but nothing in Tsuzuki's parser cares what the
/// filename actually is.
fn write_watch_later(home: &Path, entry_name: &str, source_file: &Path, position_seconds: f64) {
    let dir = home.join(".local/state/mpv/watch_later");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(entry_name),
        format!("# {}\nstart={position_seconds:.6}\n", source_file.display()),
    )
    .unwrap();
}

/// Fabricates one `tsuzuki.lua` companion-script log entry, exactly the
/// shape `src/script_log.rs` expects, at the real native XDG path
/// (`~/.local/share/tsuzuki/mpv-log`, `XDG_DATA_HOME` unset).
fn write_script_log(
    home: &Path,
    entry_name: &str,
    source_file: &Path,
    duration_seconds: Option<f64>,
    position_seconds: Option<f64>,
    reason: &str,
) {
    let dir = home.join(".local/share/tsuzuki/mpv-log");
    fs::create_dir_all(&dir).unwrap();
    let duration = duration_seconds
        .map(|d| format!("{d:.6}"))
        .unwrap_or_default();
    let position = position_seconds
        .map(|p| format!("{p:.6}"))
        .unwrap_or_default();
    fs::write(
        dir.join(entry_name),
        format!(
            "# {}\nduration={duration}\nposition={position}\nreason={reason}\n",
            source_file.display()
        ),
    )
    .unwrap();
}

/// Rewrites the single existing `state.toml` record's `updated_at` field
/// to `seconds_ago` in the past — the only way to exercise `auto_pause_days`
/// idleness from outside the crate (this test binary can't reach
/// `state::now_unix()`'s callers directly, and there's no CLI surface for
/// backdating a record — genuine idleness only ever comes from real time
/// passing). A plain text find-and-replace, exactly like `bypass_real_mpv`'s
/// own `command_override` patch above; only valid for a single-record
/// `state.toml`, which is all this suite ever needs it for.
fn backdate_only_updated_at(home: &Path, seconds_ago: u64) {
    let path = state_path(home);
    let contents = fs::read_to_string(&path).unwrap();
    let marker = "updated_at = ";
    let start = contents
        .find(marker)
        .expect("state.toml must contain updated_at")
        + marker.len();
    let rest = &contents[start..];
    let digits_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let old_value: u64 = rest[..digits_end].parse().unwrap();
    let new_value = old_value.saturating_sub(seconds_ago);
    let patched = format!("{}{new_value}{}", &contents[..start], &rest[digits_end..]);
    fs::write(&path, patched).unwrap();
}

/// mtime resolution on some filesystems is coarse enough that two writes
/// within the same instant can tie — every freshest-file-wins scenario in
/// this suite needs a genuinely later mtime, not just a later wall-clock
/// call, so tests that depend on ordering pause briefly between writes.
fn ensure_next_write_is_strictly_fresher() {
    sleep(Duration::from_millis(1100));
}

// ---------------------------------------------------------------------
// 1. First-time onboarding: add -> nothing in progress yet -> non-TTY
//    `start` lists real choices -> an explicit `start <series>` opens
//    real episode 1.
// ---------------------------------------------------------------------

#[test]
fn first_time_onboarding_add_list_and_start_a_series() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let anime_root = home.join("Anime");
    touch(&anime_root.join("Monster").join("ep01.mkv"));
    touch(&anime_root.join("Monster").join("ep02.mkv"));
    touch(&anime_root.join("Naruto").join("ep01.mkv"));

    add_root(home, &anime_root);
    bypass_real_mpv(home);

    // Nothing has been watched yet: `list` shows both series, both
    // "Never started" — no `state.toml` record exists for either.
    let list = run_ok(home, &["list"]);
    assert!(list.contains("Never started:"), "got: {list}");
    assert!(
        list.contains("Monster") && list.contains("(2 files)"),
        "got: {list}"
    );
    assert!(
        list.contains("Naruto") && list.contains("(1 files)"),
        "got: {list}"
    );
    assert!(
        !state_path(home).exists(),
        "no record should exist before anything is opened"
    );

    // `status` has nothing to report yet.
    let status = run_ok(home, &["status"]);
    assert!(status.contains("Nothing in progress."), "got: {status}");

    // Bare `start`, run non-interactively (this test's subprocess has no
    // real TTY on stdin) — must list real choices with a 1-based index
    // and exit non-zero, never hang waiting for arrow-key input
    // (CLAUDE.md's "Series selection: interactive by default, explicit
    // when scripted").
    let start_listing = run(home, &["start"]);
    assert!(
        !start_listing.status.success(),
        "a non-TTY picker must exit non-zero — nothing was actually started"
    );
    let start_stdout = stdout_of(&start_listing);
    assert!(
        start_stdout.contains("Available to start:"),
        "got: {start_stdout}"
    );
    assert!(
        start_stdout.contains("Monster — Start (2 files)"),
        "got: {start_stdout}"
    );
    assert!(
        start_stdout.contains("Naruto — Start (1 files)"),
        "got: {start_stdout}"
    );
    assert!(
        start_stdout.contains("1.") && start_stdout.contains("2."),
        "must show 1-based indexes so a script can act on the listing, got: {start_stdout}"
    );

    // An explicit series argument always bypasses the picker and actually
    // plays episode 1 at position 0.
    let started = run_ok(home, &["start", "Monster"]);
    assert!(
        started.contains("Starting \"Monster\" — episode 1 (00:00)"),
        "got: {started}"
    );
    assert!(
        started.contains(
            &anime_root
                .join("Monster")
                .join("ep01.mkv")
                .display()
                .to_string()
        ),
        "must print exactly which file it opened, got: {started}"
    );
}

// ---------------------------------------------------------------------
// 2. Natural sort is the sole ordering authority (season folders decide
//    position; Anitomy only annotates the displayed episode number) +
//    a full binge chain resolved in single invocations, including
//    completion when the last episode finishes.
// ---------------------------------------------------------------------

#[test]
fn natural_sort_orders_seasons_anitomy_only_annotates_and_a_binge_chain_resolves_and_completes() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let anime_root = home.join("Anime");
    let series_dir = anime_root.join("Test Show");
    // Anitomy-parseable filenames (confirmed pattern, see src/anitomy.rs's
    // own unit tests) whose *parsed episode numbers* deliberately
    // contradict natural-sort position: if Anitomy's number ever
    // (wrongly) decided order, "Season 2"'s file (Anitomy episode 5)
    // would sort before "Season 1"'s (Anitomy episode 3). The directory
    // names, not the filenames, are what natural sort actually orders on
    // here — Anitomy is never even given a directory name as input
    // (CLAUDE.md's "Anitomy must be called with a bare filename").
    let season1_file = series_dir
        .join("Season 1")
        .join("[Group] Test Show - 03 [1080p].mkv");
    let season2_file = series_dir
        .join("Season 2")
        .join("[Group] Test Show - 05 [1080p].mkv");
    touch(&season1_file);
    touch(&season2_file);

    add_root(home, &anime_root);

    // A real (>= resume_seconds) bookmark on the Season 1 file — natural
    // sort must have placed it first for this to become the tracked
    // episode 1 position, regardless of its Anitomy-parsed number.
    write_watch_later(home, "entry-1", &season1_file, 560.0);

    let list1 = run_ok(home, &["list"]);
    assert!(list1.contains("In progress:"), "got: {list1}");
    // Position is real progress on the *first natural-sort file*, but the
    // displayed episode number is Anitomy's parsed "03" — proving
    // Anitomy annotated identity without moving which file is "first".
    assert!(
        list1.contains("Test Show — ep. 3"),
        "natural sort must place Season 1 first; Anitomy must only annotate its number, got: {list1}"
    );

    // The Season 1 episode now completes (the companion script's own
    // unconditional `eof` signal — no duration probing needed). This is
    // the *current* tracked episode completing, so reconcile must advance
    // the cursor straight to the next natural-sort episode, at position 0,
    // in a single invocation (CLAUDE.md's "When the CURRENT tracked
    // episode itself completes: advance to the next one").
    ensure_next_write_is_strictly_fresher();
    write_script_log(
        home,
        "log-1",
        &season1_file,
        Some(1420.0),
        Some(1415.0),
        "eof",
    );

    let list2 = run_ok(home, &["list"]);
    assert!(list2.contains("In progress:"), "got: {list2}");
    assert!(
        list2.contains("Test Show — ep. 5"),
        "must have auto-advanced to Season 2's file (Anitomy ep. 5), got: {list2}"
    );
    assert!(
        !list2.contains("ep. 3"),
        "the completed episode must no longer be the tracked position, got: {list2}"
    );

    // Season 2's episode is the *last* file in the whole series. Its own
    // completion must transition the series straight to Completed — no
    // next episode exists to advance to.
    ensure_next_write_is_strictly_fresher();
    write_script_log(
        home,
        "log-2",
        &season2_file,
        Some(1380.0),
        Some(1375.0),
        "eof",
    );

    let list3 = run_ok(home, &["list"]);
    assert!(
        !list3.contains("In progress:"),
        "the series must have left the in_progress group, got: {list3}"
    );
    assert!(list3.contains("Dropped:"), "got: {list3}");
    assert!(list3.contains("Test Show"), "got: {list3}");
    let state_contents = fs::read_to_string(state_path(home)).unwrap();
    assert!(
        state_contents.contains("status = \"completed\""),
        "a finished series must persist as completed, not dropped:\n{state_contents}"
    );

    // `start`'s display word for a completed series is "Rewatch?", never
    // "Start".
    let start_listing = run(home, &["start"]);
    let start_stdout = stdout_of(&start_listing);
    assert!(
        start_stdout.contains("Test Show — Rewatch?"),
        "got: {start_stdout}"
    );
}

// ---------------------------------------------------------------------
// 3. Exclusion workflow: bare `exclude` surfaces a real Anitomy-suggested
//    OVA candidate, confirming it removes it from the natural-sort tree
//    (never a post-filter), and re-confirming is idempotent.
// ---------------------------------------------------------------------

#[test]
fn exclusion_workflow_suggest_confirm_removes_from_order_and_is_idempotent() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let anime_root = home.join("Anime");
    let series_dir = anime_root.join("One Piece");
    touch(&series_dir.join("[Group] One Piece - 01 [1080p].mkv"));
    touch(&series_dir.join("[Group] One Piece - 02 [1080p].mkv"));
    // Confirmed-by-direct-testing OVA pattern (src/exclude.rs's own unit
    // tests use this exact filename shape).
    let ova_file = series_dir.join("[Group] One Piece - OVA 1 [1080p].mkv");
    touch(&ova_file);

    add_root(home, &anime_root);

    // Before excluding anything, the OVA file counts toward the series'
    // file count.
    let list_before = run_ok(home, &["list"]);
    assert!(
        list_before.contains("One Piece") && list_before.contains("(3 files)"),
        "got: {list_before}"
    );

    let suggested = run_ok(home, &["exclude"]);
    assert!(
        suggested.contains("Anitomy-suggested candidates (not yet excluded):"),
        "got: {suggested}"
    );
    assert!(
        suggested.contains("One Piece - OVA 1 [1080p].mkv"),
        "got: {suggested}"
    );
    assert!(
        suggested.contains("tsuzuki exclude \""),
        "must print the exact ready-to-run command, got: {suggested}"
    );

    let relative_path = "One Piece/[Group] One Piece - OVA 1 [1080p].mkv";
    let excluded = run_ok(home, &["exclude", relative_path]);
    assert!(excluded.contains("excluded"), "got: {excluded}");

    // Re-running bare `exclude` no longer suggests it.
    let suggested_after = run_ok(home, &["exclude"]);
    assert!(
        !suggested_after.contains("OVA"),
        "an already-excluded candidate must not be suggested again, got: {suggested_after}"
    );

    // Exclusions run before ordering, never as a post-filter: the file
    // count drops to 2.
    let list_after = run_ok(home, &["list"]);
    assert!(
        list_after.contains("One Piece") && list_after.contains("(2 files)"),
        "got: {list_after}"
    );

    // Re-excluding the same path is idempotent — exit 0, no-op.
    let reexcluded = run(home, &["exclude", relative_path]);
    assert!(reexcluded.status.success());
    assert!(
        stdout_of(&reexcluded).contains("already excluded"),
        "got: {}",
        stdout_of(&reexcluded)
    );
}

// ---------------------------------------------------------------------
// 4. Pause/drop lifecycle end to end: real progress -> pause -> list
//    grouping -> drop -> invalid-transition error leaves state
//    untouched -> `start`'s "Rewatch?" word.
// ---------------------------------------------------------------------

#[test]
fn pause_drop_lifecycle_list_grouping_and_invalid_transition_is_rejected() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let anime_root = home.join("Anime");
    let file = anime_root.join("Monster").join("ep01.mkv");
    touch(&file);
    add_root(home, &anime_root);

    write_watch_later(home, "entry", &file, 300.0);
    run_ok(home, &["list"]); // persists the real in_progress record

    let pause1 = run_ok(home, &["pause", "Monster"]);
    assert!(pause1.contains("\"Monster\" — paused"), "got: {pause1}");

    let list_paused = run_ok(home, &["list"]);
    assert!(list_paused.contains("Paused:"), "got: {list_paused}");
    assert!(!list_paused.contains("In progress:"), "got: {list_paused}");

    // Idempotent re-pause.
    let pause2 = run_ok(home, &["pause", "Monster"]);
    assert!(pause2.contains("already paused"), "got: {pause2}");

    // Paused -> Dropped is an allowed transition.
    let drop1 = run_ok(home, &["drop", "Monster"]);
    assert!(drop1.contains("\"Monster\" — dropped"), "got: {drop1}");

    let list_dropped = run_ok(home, &["list"]);
    assert!(list_dropped.contains("Dropped:"), "got: {list_dropped}");

    // Dropped -> Paused does not exist (CLAUDE.md's allowed-transitions
    // table) — must be a real error, non-zero exit, state left untouched.
    let invalid_pause = run(home, &["pause", "Monster"]);
    assert!(
        !invalid_pause.status.success(),
        "pausing an already-dropped series must be rejected, not silently accepted"
    );
    assert!(
        stderr_of(&invalid_pause).contains("currently dropped"),
        "got: {}",
        stderr_of(&invalid_pause)
    );
    let list_still_dropped = run_ok(home, &["list"]);
    assert!(
        list_still_dropped.contains("Dropped:") && !list_still_dropped.contains("Paused:"),
        "the rejected transition must not have mutated state, got: {list_still_dropped}"
    );

    // `start`'s dropped display word.
    let start_listing = stdout_of(&run(home, &["start"]));
    assert!(
        start_listing.contains("Monster — Rewatch?"),
        "got: {start_listing}"
    );

    // Idempotent re-drop.
    let drop2 = run_ok(home, &["drop", "Monster"]);
    assert!(drop2.contains("already dropped"), "got: {drop2}");
}

// ---------------------------------------------------------------------
// 5. Composable roots: the same series name under two different tracked
//    roots is genuinely ambiguous without `--root`, and the ambiguity
//    error must be scriptable (prints the exact re-invocation), never an
//    interactive prompt — and must never mutate either series.
// ---------------------------------------------------------------------

#[test]
fn multi_root_disambiguation_for_pause_is_explicit_and_never_mutates_on_ambiguity() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let root_a = home.join("Anime");
    let root_b = home.join("MoreAnime");
    let file_a = root_a.join("Overlap").join("ep01.mkv");
    let file_b = root_b.join("Overlap").join("ep01.mkv");
    touch(&file_a);
    touch(&file_b);

    add_root(home, &root_a);
    add_root(home, &root_b);

    write_watch_later(home, "entry-a", &file_a, 300.0);
    write_watch_later(home, "entry-b", &file_b, 400.0);
    run_ok(home, &["list"]); // persists both real in_progress records

    let ambiguous = run(home, &["pause", "Overlap"]);
    assert!(!ambiguous.status.success());
    let stderr = stderr_of(&ambiguous);
    assert!(stderr.contains("is ambiguous"), "got: {stderr}");
    assert!(
        stderr.contains(&format!("--root {}", root_a.display()))
            || stderr.contains("--root ~/Anime"),
        "must print the exact re-invocation for root A, got: {stderr}"
    );
    assert!(
        stderr.contains(&format!("--root {}", root_b.display()))
            || stderr.contains("--root ~/MoreAnime"),
        "must print the exact re-invocation for root B, got: {stderr}"
    );

    // Neither series was touched by the rejected ambiguous call.
    let list_untouched = run_ok(home, &["list"]);
    assert_eq!(
        list_untouched.matches("In progress:").count(),
        1,
        "both Overlap records must still be in_progress, got: {list_untouched}"
    );

    // Disambiguating with --root pauses only that one.
    let paused = run_ok(
        home,
        &["pause", "Overlap", "--root", root_a.to_str().unwrap()],
    );
    assert!(paused.contains("paused"), "got: {paused}");

    let list_after = run_ok(home, &["list"]);
    assert!(list_after.contains("In progress:"), "got: {list_after}");
    assert!(list_after.contains("Paused:"), "got: {list_after}");
}

// ---------------------------------------------------------------------
// 6. `doctor` detects a real orphaned record (series folder deleted out
//    from under a still-valid root) and `--fix --forget-orphans` clears
//    exactly it, permanently.
// ---------------------------------------------------------------------

#[test]
fn doctor_detects_and_forgets_a_real_orphaned_record() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let anime_root = home.join("Anime");
    let gone_series_dir = anime_root.join("GoneShow");
    let file = gone_series_dir.join("ep01.mkv");
    touch(&file);
    // A second, untouched series so the root itself stays non-empty and
    // obviously still valid after the first is deleted.
    touch(&anime_root.join("StillHere").join("ep01.mkv"));

    add_root(home, &anime_root);
    write_watch_later(home, "entry", &file, 300.0);
    run_ok(home, &["list"]); // persists a real in_progress record for GoneShow

    assert!(
        state_path(home).exists()
            && fs::read_to_string(state_path(home))
                .unwrap()
                .contains("GoneShow")
    );

    // The series folder disappears (moved, deleted, renamed — all
    // indistinguishable, per CLAUDE.md's "A moved/renamed series folder
    // is indistinguishable from deletion, on purpose").
    fs::remove_dir_all(&gone_series_dir).unwrap();

    let doctor1 = run(home, &["doctor"]);
    let doctor1_stdout = stdout_of(&doctor1);
    assert!(
        doctor1_stdout.contains("GoneShow"),
        "doctor must name the specific orphaned series, got: {doctor1_stdout}"
    );

    // Never auto-removed by a plain `doctor` — requires --fix
    // --forget-orphans, and must still be present until then.
    let state_before_fix = fs::read_to_string(state_path(home)).unwrap();
    assert!(state_before_fix.contains("GoneShow"));

    // Not `run_ok`: on a real dev machine with a real mpv install, other
    // doctor checks (e.g. mpv.conf's own `write-filename-in-watch-later-
    // config` setting) can legitimately FAIL and drive the overall exit
    // code independently of the orphan fix — the same real-machine
    // dependency `tests/doctor_integration.rs` already documents. Only
    // the orphan-forgetting behavior itself is under test here.
    let fixed = stdout_of(&run(home, &["doctor", "--fix", "--forget-orphans"]));
    assert!(
        fixed.contains("removed") && fixed.contains("GoneShow"),
        "got: {fixed}"
    );

    let state_after_fix = fs::read_to_string(state_path(home)).unwrap();
    assert!(
        !state_after_fix.contains("GoneShow"),
        "the orphaned record must be permanently gone, got:\n{state_after_fix}"
    );

    // Re-running doctor no longer reports it.
    let doctor2_stdout = stdout_of(&run(home, &["doctor"]));
    assert!(
        !doctor2_stdout.contains("GoneShow"),
        "got: {doctor2_stdout}"
    );
}

// ---------------------------------------------------------------------
// 7. `config` show/edit round trip, including a real broken-TOML edit.
// ---------------------------------------------------------------------

#[test]
fn config_show_and_edit_round_trip_including_a_broken_edit() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    // Any command that touches config.toml is enough to create it with
    // real, filled-in defaults.
    run_ok(home, &["list"]);

    let shown = run_ok(home, &["config"]);
    assert!(
        shown.starts_with(&format!("# {}", config_path(home).display())),
        "got: {shown}"
    );
    assert!(shown.contains("resume_seconds = 120"), "got: {shown}");
    assert!(shown.contains("→ tsuzuki config edit"), "got: {shown}");

    // A real, non-interactive "$EDITOR" that bumps resume_seconds — a
    // plain shell script, never an actual interactive editor (matching
    // tests/config_integration.rs's own established approach).
    let good_editor = home.join("good-editor.sh");
    fs::write(
        &good_editor,
        "#!/bin/sh\nsed -i 's/resume_seconds = 120/resume_seconds = 121/' \"$1\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&good_editor, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let edited = Command::new(tsuzuki_bin())
        .args(["config", "edit"])
        .env("HOME", home)
        .env("EDITOR", &good_editor)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .output()
        .unwrap();
    assert!(edited.status.success(), "stderr: {}", stderr_of(&edited));
    assert!(
        stdout_of(&edited).contains("config saved"),
        "got: {}",
        stdout_of(&edited)
    );

    let shown_after = run_ok(home, &["config"]);
    assert!(
        shown_after.contains("resume_seconds = 121"),
        "got: {shown_after}"
    );

    // A real broken-TOML edit is surfaced immediately, non-zero exit, not
    // silently left for the next unrelated command to trip over.
    let bad_editor = home.join("bad-editor.sh");
    fs::write(
        &bad_editor,
        "#!/bin/sh\necho 'this is not valid toml [[[' >> \"$1\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bad_editor, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let broken = Command::new(tsuzuki_bin())
        .args(["config", "edit"])
        .env("HOME", home)
        .env("EDITOR", &bad_editor)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .output()
        .unwrap();
    assert!(
        !broken.status.success(),
        "a broken config.toml must be a real error"
    );
    assert!(
        stderr_of(&broken).contains("syntax error"),
        "got: {}",
        stderr_of(&broken)
    );
}

// ---------------------------------------------------------------------
// 8. The companion script wins on real freshness (not "script always
//    wins"), and — since mpv never deletes a script-log entry — a
//    second, unrelated invocation with nothing new must not manufacture
//    fresh activity out of the same still-sitting-there entry.
// ---------------------------------------------------------------------

#[test]
fn companion_script_wins_on_freshness_and_a_stale_entry_never_reappears_as_new_activity() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let anime_root = home.join("Anime");
    let file = anime_root.join("Monster").join("ep01.mkv");
    touch(&file);
    add_root(home, &anime_root);

    // An older, real watch_later bookmark...
    write_watch_later(home, "wl-entry", &file, 300.0);
    ensure_next_write_is_strictly_fresher();
    // ...then a genuinely fresher script-log reading for the very same
    // file, at a distinguishably different real position.
    write_script_log(
        home,
        "script-entry",
        &file,
        Some(2000.0),
        Some(900.0),
        "quit",
    );

    let list1 = run_ok(home, &["list"]);
    assert!(list1.contains("In progress:"), "got: {list1}");
    let state1 = fs::read_to_string(state_path(home)).unwrap();
    assert!(
        state1.contains("900"),
        "the fresher script-log position must win over the older watch_later one, got:\n{state1}"
    );
    assert!(!state1.contains("300.0"), "got:\n{state1}");

    // Second, unrelated invocation: nothing on disk changed. The
    // script-log entry is still sitting there (mpv/the script never
    // delete it) — it must not be re-read as fresh activity.
    let list2 = run_ok(home, &["list"]);
    assert!(list2.contains("In progress:"), "got: {list2}");
    let state2 = fs::read_to_string(state_path(home)).unwrap();
    assert_eq!(
        state1, state2,
        "an unchanged script-log entry must never look like new activity on a later, unrelated run"
    );
}

// ---------------------------------------------------------------------
// 9. A sub-resume_seconds reading is invisible to state.toml, even when
//    it arrives *after* — and with a fresher mtime than — a real saved
//    position. mpv has no concept of Tsuzuki's threshold.
// ---------------------------------------------------------------------

#[test]
fn a_noise_level_reading_never_overwrites_a_real_saved_position() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let anime_root = home.join("Anime");
    let file = anime_root.join("Monster").join("ep01.mkv");
    touch(&file);
    add_root(home, &anime_root);

    write_watch_later(home, "entry", &file, 560.0);
    run_ok(home, &["list"]);
    let state1 = fs::read_to_string(state_path(home)).unwrap();
    assert!(state1.contains("560"), "got:\n{state1}");

    // The user briefly reopened the same file and quit almost
    // immediately (45s < the default 120s resume_seconds) — mpv's own
    // watch_later file now genuinely says 45s, with a real, later mtime.
    ensure_next_write_is_strictly_fresher();
    write_watch_later(home, "entry", &file, 45.0);

    let list2 = run_ok(home, &["list"]);
    assert!(list2.contains("In progress:"), "got: {list2}");
    let state2 = fs::read_to_string(state_path(home)).unwrap();
    assert!(
        state2.contains("560"),
        "a sub-threshold reading must never overwrite real saved progress, got:\n{state2}"
    );
    assert!(!state2.contains("45.0"), "got:\n{state2}");
}

// ---------------------------------------------------------------------
// 10. Forward-scan is strictly forward: a real bookmark on an *earlier*
//     episode must never reactivate or move a Paused series.
// ---------------------------------------------------------------------

#[test]
fn forward_scan_never_reactivates_a_paused_series_from_an_earlier_bookmark() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let anime_root = home.join("Anime");
    let series_dir = anime_root.join("Monster");
    let ep1 = series_dir.join("ep01.mkv");
    let ep2 = series_dir.join("ep02.mkv");
    touch(&ep1);
    touch(&ep2);
    add_root(home, &anime_root);

    // Real progress on episode 2, then paused there.
    write_watch_later(home, "entry-2", &ep2, 500.0);
    run_ok(home, &["list"]);
    run_ok(home, &["pause", "Monster"]);
    let state_paused = fs::read_to_string(state_path(home)).unwrap();
    assert!(state_paused.contains("ep02.mkv"), "got:\n{state_paused}");

    // A nostalgic rewatch peek at episode 1 — a real (>= resume_seconds)
    // bookmark, with a fresher mtime than anything recorded so far.
    ensure_next_write_is_strictly_fresher();
    write_watch_later(home, "entry-1", &ep1, 200.0);

    let list_after = run_ok(home, &["list"]);
    assert!(
        list_after.contains("Paused:"),
        "an earlier bookmark must never reactivate a paused series, got: {list_after}"
    );
    let state_after = fs::read_to_string(state_path(home)).unwrap();
    assert!(
        state_after.contains("ep02.mkv"),
        "the paused position must stay on episode 2, got:\n{state_after}"
    );
}

// ---------------------------------------------------------------------
// 11. Symlinks are followed everywhere: a symlinked series folder is
//     tracked exactly like a real one, and a watch_later entry reached
//     only through the symlinked path still attributes correctly.
// ---------------------------------------------------------------------

#[test]
#[cfg(unix)]
fn a_symlinked_series_folder_is_tracked_exactly_like_a_real_one() {
    use std::os::unix::fs::symlink;

    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let anime_root = home.join("Anime");
    touch(&anime_root.join("RealShow").join("ep01.mkv"));

    // The actual storage location lives entirely outside the tracked
    // root; only a symlink named "LinkedShow" under the root points at
    // it, exactly the "series folder is itself a symlink" case CLAUDE.md
    // calls out.
    let real_storage = home.join("ActualStorage").join("LinkedShowReal");
    let linked_file = real_storage.join("ep01.mkv");
    touch(&linked_file);
    fs::create_dir_all(&anime_root).unwrap();
    symlink(&real_storage, anime_root.join("LinkedShow")).unwrap();

    add_root(home, &anime_root);

    let list_before = run_ok(home, &["list"]);
    assert!(list_before.contains("RealShow"), "got: {list_before}");
    assert!(
        list_before.contains("LinkedShow"),
        "a symlinked series folder must be tracked exactly like a real one, got: {list_before}"
    );

    // mpv reports the path it was actually opened through — the
    // symlinked path under the tracked root, not the real storage path.
    let symlinked_path = anime_root.join("LinkedShow").join("ep01.mkv");
    write_watch_later(home, "entry", &symlinked_path, 400.0);

    let list_after = run_ok(home, &["list"]);
    assert!(
        list_after.contains("In progress:") && list_after.contains("LinkedShow"),
        "got: {list_after}"
    );
}

// ---------------------------------------------------------------------
// 12. Bare `resume`, non-interactively, with more than one series
//     genuinely in progress: must list real choices and exit non-zero —
//     never hang, never guess which one to play.
// ---------------------------------------------------------------------

#[test]
fn non_tty_bare_resume_with_multiple_in_progress_series_lists_and_never_hangs() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let anime_root = home.join("Anime");
    let file_a = anime_root.join("Monster").join("ep01.mkv");
    let file_b = anime_root.join("Naruto").join("ep01.mkv");
    touch(&file_a);
    touch(&file_b);
    add_root(home, &anime_root);

    write_watch_later(home, "entry-a", &file_a, 300.0);
    write_watch_later(home, "entry-b", &file_b, 400.0);
    run_ok(home, &["list"]); // persists both as real in_progress records

    let resume = run(home, &["resume"]);
    assert!(
        !resume.status.success(),
        "a non-TTY picker with more than one choice must exit non-zero, not silently pick one"
    );
    let stdout = stdout_of(&resume);
    assert!(stdout.contains("Monster"), "got: {stdout}");
    assert!(stdout.contains("Naruto"), "got: {stdout}");
    assert!(
        stdout.contains("Run `tsuzuki resume <series>`"),
        "got: {stdout}"
    );

    // Nothing was mutated by the failed, non-acting listing.
    let state_untouched = fs::read_to_string(state_path(home)).unwrap();
    assert!(state_untouched.contains("Monster") && state_untouched.contains("Naruto"));
}

// ---------------------------------------------------------------------
// 13. `sync` with real tracked progress but no MAL login fails cleanly —
//     no network access is attempted, no crash, a real actionable error.
// ---------------------------------------------------------------------

#[test]
fn sync_without_a_prior_login_fails_cleanly_with_no_network_access() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let anime_root = home.join("Anime");
    let file = anime_root.join("Monster").join("ep01.mkv");
    touch(&file);
    add_root(home, &anime_root);

    write_watch_later(home, "entry", &file, 300.0);
    run_ok(home, &["list"]); // real progress to sync exists now

    let sync = run(home, &["sync"]);
    assert!(
        !sync.status.success(),
        "syncing with no stored MAL token must be a real error, not a hang or a crash"
    );
    assert!(
        stderr_of(&sync).contains("not logged in to MyAnimeList"),
        "got: {}",
        stderr_of(&sync)
    );
    assert!(
        stderr_of(&sync).contains("tsuzuki sync login"),
        "the error must point at the actual next step, got: {}",
        stderr_of(&sync)
    );
}

// ---------------------------------------------------------------------
// 14. A paused series is excluded before MAL authentication or network
//     access, preserving its local state as an explicit sync opt-out.
// ---------------------------------------------------------------------

#[test]
fn paused_series_are_not_synced_and_need_no_mal_login() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let anime_root = home.join("Anime");
    let file = anime_root.join("Monster").join("ep01.mkv");
    touch(&file);
    add_root(home, &anime_root);

    write_watch_later(home, "entry", &file, 300.0);
    run_ok(home, &["list"]);
    run_ok(home, &["pause", "Monster"]);
    let before = fs::read_to_string(state_path(home)).unwrap();

    let stdout = run_ok(home, &["sync"]);
    assert_eq!(stdout.trim(), "Nothing eligible to sync.");
    assert_eq!(
        fs::read_to_string(state_path(home)).unwrap(),
        before,
        "sync must leave a paused record byte-for-byte unchanged"
    );
}

// ---------------------------------------------------------------------
// 14b. A manually dropped series is abandoned, not finished: sync excludes
//      it before MAL access, however far in it was dropped. A series that
//      really finished is the only one sync will act on.
// ---------------------------------------------------------------------

#[test]
fn dropped_series_are_not_synced_but_completed_series_are() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let anime_root = home.join("Anime");
    let abandoned = anime_root.join("Abandoned").join("ep01.mkv");
    touch(&abandoned);
    touch(&anime_root.join("Abandoned").join("ep02.mkv"));
    let finished = anime_root.join("Finished").join("ep01.mkv");
    touch(&finished);
    add_root(home, &anime_root);

    // Two minutes into ep1 of a two-episode series, then a manual drop.
    write_watch_later(home, "entry", &abandoned, 120.0);
    run_ok(home, &["list"]);
    run_ok(home, &["drop", "Abandoned"]);
    let before = fs::read_to_string(state_path(home)).unwrap();

    let stdout = run_ok(home, &["sync"]);
    assert_eq!(stdout.trim(), "Nothing eligible to sync.");
    assert_eq!(
        fs::read_to_string(state_path(home)).unwrap(),
        before,
        "sync must leave a dropped record byte-for-byte unchanged"
    );

    // A one-episode series watched to its end becomes a sync target: with
    // no MAL login the run now reaches authentication and fails there.
    write_script_log(
        home,
        "log-finished",
        &finished,
        Some(1420.0),
        Some(1415.0),
        "eof",
    );
    let sync = run(home, &["sync"]);
    assert!(!sync.status.success());
    assert!(
        stderr_of(&sync).contains("not logged in to MyAnimeList"),
        "got: {}",
        stderr_of(&sync)
    );
}

#[test]
fn dropped_at_every_depth_is_never_synced() {
    // Dropped mid ep1; right after ep1 finished (cursor on ep2); mid last episode.
    let names = ["MidFirst", "AfterFirst", "MidLast"];
    for name in names {
        let home_dir = tempfile::tempdir().unwrap();
        let home = home_dir.path();
        let anime_root = home.join("Anime");
        let ep1 = anime_root.join(name).join("ep01.mkv");
        let ep2 = anime_root.join(name).join("ep02.mkv");
        touch(&ep1);
        touch(&ep2);
        add_root(home, &anime_root);

        match name {
            "MidFirst" => write_watch_later(home, "e1", &ep1, 120.0),
            "AfterFirst" => {
                write_script_log(home, "l1", &ep1, Some(1420.0), Some(1415.0), "eof");
            }
            _ => {
                write_script_log(home, "l1", &ep1, Some(1420.0), Some(1415.0), "eof");
                run_ok(home, &["list"]);
                ensure_next_write_is_strictly_fresher();
                write_watch_later(home, "e2", &ep2, 700.0);
            }
        }
        run_ok(home, &["list"]);
        run_ok(home, &["drop", name]);
        let before = fs::read_to_string(state_path(home)).unwrap();

        assert_eq!(
            run_ok(home, &["sync"]).trim(),
            "Nothing eligible to sync.",
            "{name}"
        );
        assert_eq!(
            fs::read_to_string(state_path(home)).unwrap(),
            before,
            "{name}"
        );
    }
}

// ---------------------------------------------------------------------
// 14c. `pause`/`drop` never act on a finished series: Completed is reached
//      only by watching the final episode.
// ---------------------------------------------------------------------

#[test]
fn pause_and_drop_reject_a_completed_series_and_leave_state_unchanged() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let anime_root = home.join("Anime");
    let file = anime_root.join("Monster").join("ep01.mkv");
    touch(&file);
    add_root(home, &anime_root);

    write_script_log(home, "log-1", &file, Some(1420.0), Some(1415.0), "eof");
    run_ok(home, &["list"]);
    let before = fs::read_to_string(state_path(home)).unwrap();
    assert!(before.contains("status = \"completed\""), "got: {before}");

    for action in ["pause", "drop"] {
        let out = run(home, &[action, "Monster"]);
        assert!(!out.status.success(), "{action} must be rejected");
        assert!(
            stderr_of(&out).contains("currently completed"),
            "got: {}",
            stderr_of(&out)
        );
    }
    assert_eq!(fs::read_to_string(state_path(home)).unwrap(), before);
}

// ---------------------------------------------------------------------
// 15. A deleted series folder leaves its state record inert: sync neither
//     lists it nor requires MAL access, and doctor remains the only cleanup.
// ---------------------------------------------------------------------

#[test]
fn orphaned_series_are_not_synced_and_their_state_is_preserved() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let anime_root = home.join("Anime");
    let series_dir = anime_root.join("Shokei Shoujo no Virgin Road");
    let file = series_dir.join("ep07.mkv");
    touch(&file);
    add_root(home, &anime_root);

    write_watch_later(home, "entry", &file, 300.0);
    run_ok(home, &["list"]);
    fs::remove_dir_all(&series_dir).unwrap();
    let before = fs::read_to_string(state_path(home)).unwrap();

    let stdout = run_ok(home, &["sync"]);
    assert_eq!(stdout.trim(), "Nothing eligible to sync.");
    assert!(!stdout.contains("Shokei Shoujo no Virgin Road"));
    assert_eq!(
        fs::read_to_string(state_path(home)).unwrap(),
        before,
        "sync must leave an orphaned record for doctor to handle explicitly"
    );
}

// ---------------------------------------------------------------------
// 16. `auto_pause_days`: off by default (a genuinely idle series stays
//     in_progress forever), and — once explicitly opted in — a real,
//     end-to-end auto-pause of a genuinely idle series, never a drop,
//     never touching its saved position.
// ---------------------------------------------------------------------

#[test]
fn auto_pause_days_is_off_by_default_and_pauses_a_genuinely_idle_series_once_opted_in() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let anime_root = home.join("Anime");
    let file = anime_root.join("Monster").join("ep01.mkv");
    touch(&file);
    add_root(home, &anime_root);

    write_watch_later(home, "entry", &file, 300.0);
    run_ok(home, &["list"]); // persists a real in_progress record

    // Backdating alone, with the documented default (0 = never auto-pause)
    // still in effect, must never pause anything — no matter how old.
    // Merely re-running `list` never bumps `updated_at` on its own (the
    // watch_later entry is unchanged, so reconcile finds no genuine
    // change) — direct backdating is the only way to simulate real
    // elapsed time here.
    backdate_only_updated_at(home, 365 * 86_400);
    let list_default = run_ok(home, &["list"]);
    assert!(
        list_default.contains("In progress:"),
        "auto_pause_days defaults to 0 (never auto-pause); got: {list_default}"
    );

    // Opt in to a real 1-day threshold.
    let cfg = config_path(home);
    let patched = fs::read_to_string(&cfg)
        .unwrap()
        .replace("auto_pause_days = 0", "auto_pause_days = 1");
    fs::write(&cfg, patched).unwrap();

    // Genuine fresh activity (a distinguishably different real position)
    // refreshes `updated_at` to now — a 1-day-old bar hasn't been cleared
    // yet, so this must stay in_progress.
    ensure_next_write_is_strictly_fresher();
    write_watch_later(home, "entry", &file, 600.0);
    let list_still_fresh = run_ok(home, &["list"]);
    assert!(
        list_still_fresh.contains("In progress:"),
        "got: {list_still_fresh}"
    );

    // Genuinely idle for 2 days against the 1-day threshold, no further
    // activity accompanying it.
    backdate_only_updated_at(home, 2 * 86_400);

    let list_idle = run_ok(home, &["list"]);
    assert!(
        list_idle.contains("Paused:") && !list_idle.contains("In progress:"),
        "a series idle past auto_pause_days must be auto-paused, got: {list_idle}"
    );

    // design.md §8: "never auto-dropped" — the saved position/episode must
    // survive the transition untouched, exactly like a manual pause.
    let state_after = fs::read_to_string(state_path(home)).unwrap();
    assert!(state_after.contains("600"), "got:\n{state_after}");
    assert!(state_after.contains("ep01.mkv"), "got:\n{state_after}");
}

// ---------------------------------------------------------------------
// 16. `add`, `exclude`, `pause` and `drop` take several arguments: each is
//     handled independently, a failing one never blocks the rest, and the
//     exit code is non-zero only if something failed.
// ---------------------------------------------------------------------

/// Tracks `Anime/{names}` with one episode each and persists an
/// `in_progress` record per series through the real `list` reconcile.
fn start_series(home: &Path, names: &[&str]) -> PathBuf {
    let anime_root = home.join("Anime");
    for (i, name) in names.iter().enumerate() {
        let file = anime_root.join(name).join("ep01.mkv");
        touch(&file);
        if i == 0 {
            add_root(home, &anime_root);
        }
        write_watch_later(home, &format!("entry-{i}"), &file, 300.0);
    }
    run_ok(home, &["list"]);
    anime_root
}

fn count_status(home: &Path, status: &str) -> usize {
    fs::read_to_string(state_path(home))
        .unwrap()
        .matches(&format!("status = \"{status}\""))
        .count()
}

#[test]
fn pause_multiple_series_pauses_each_and_persists_both() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    start_series(home, &["Alpha", "Beta"]);

    let out = run_ok(home, &["pause", "Alpha", "Beta"]);
    assert_eq!(out, "✓ \"Alpha\" — paused\n✓ \"Beta\" — paused\n");
    assert_eq!(count_status(home, "paused"), 2);
    assert_eq!(count_status(home, "in_progress"), 0);

    let again = run_ok(home, &["pause", "Alpha", "Beta"]);
    assert_eq!(
        again,
        "✓ \"Alpha\" — already paused\n✓ \"Beta\" — already paused\n"
    );
}

#[test]
fn drop_multiple_series_drops_each_and_persists_both() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    start_series(home, &["Alpha", "Beta"]);

    let out = run_ok(home, &["drop", "Alpha", "Beta"]);
    assert_eq!(out, "✓ \"Alpha\" — dropped\n✓ \"Beta\" — dropped\n");
    assert_eq!(count_status(home, "dropped"), 2);
}

#[test]
fn pause_multiple_series_with_unknown_name_pauses_the_rest_and_exits_nonzero() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    start_series(home, &["Alpha", "Beta"]);

    let out = run(home, &["pause", "Alpha", "Nope", "Beta"]);
    assert!(!out.status.success());
    assert_eq!(
        stdout_of(&out),
        "✓ \"Alpha\" — paused\n✓ \"Beta\" — paused\n"
    );
    assert_eq!(stderr_of(&out), "error: no tracked series named \"Nope\"\n");
    assert_eq!(count_status(home, "paused"), 2);
}

#[test]
fn drop_multiple_series_with_never_started_one_drops_the_rest_and_exits_nonzero() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let anime_root = start_series(home, &["Alpha", "Beta"]);
    touch(&anime_root.join("Fresh").join("ep01.mkv"));

    let out = run(home, &["drop", "Alpha", "Fresh", "Beta"]);
    assert!(!out.status.success());
    assert_eq!(
        stdout_of(&out),
        "✓ \"Alpha\" — dropped\n✓ \"Beta\" — dropped\n"
    );
    assert_eq!(
        stderr_of(&out),
        "error: no progress to act on for this series — it was never started\n"
    );
    assert_eq!(count_status(home, "dropped"), 2);
}

#[test]
fn pause_multiple_series_with_dropped_one_rejects_only_that_one_and_keeps_its_position() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    start_series(home, &["Alpha", "Beta"]);
    run_ok(home, &["drop", "Alpha"]);
    let position_before = fs::read_to_string(state_path(home))
        .unwrap()
        .matches("300")
        .count();

    let out = run(home, &["pause", "Alpha", "Beta"]);
    assert!(!out.status.success());
    assert_eq!(stdout_of(&out), "✓ \"Beta\" — paused\n");
    assert_eq!(
        stderr_of(&out),
        "error: cannot pause a series that's currently dropped\n"
    );
    assert_eq!(count_status(home, "dropped"), 1);
    assert_eq!(count_status(home, "paused"), 1);
    let position_after = fs::read_to_string(state_path(home))
        .unwrap()
        .matches("300")
        .count();
    assert_eq!(position_before, position_after);
}

#[test]
fn pause_multiple_series_ambiguous_name_is_not_mutated_while_others_proceed() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let root_a = home.join("Anime");
    let root_b = home.join("MoreAnime");
    let overlap_a = root_a.join("Overlap").join("ep01.mkv");
    let overlap_b = root_b.join("Overlap").join("ep01.mkv");
    let solo = root_a.join("Solo").join("ep01.mkv");
    for f in [&overlap_a, &overlap_b, &solo] {
        touch(f);
    }
    add_root(home, &root_a);
    add_root(home, &root_b);
    write_watch_later(home, "entry-a", &overlap_a, 300.0);
    write_watch_later(home, "entry-b", &overlap_b, 400.0);
    write_watch_later(home, "entry-c", &solo, 500.0);
    run_ok(home, &["list"]);

    let out = run(home, &["pause", "Overlap", "Solo"]);
    assert!(!out.status.success());
    assert_eq!(stdout_of(&out), "✓ \"Solo\" — paused\n");
    assert!(
        stderr_of(&out).starts_with("✗ \"Overlap\" is ambiguous — tracked under 2 roots\n"),
        "got: {}",
        stderr_of(&out)
    );
    assert_eq!(count_status(home, "paused"), 1);
    assert_eq!(count_status(home, "in_progress"), 2);
}

#[test]
fn pause_without_series_is_a_usage_error() {
    let home_dir = tempfile::tempdir().unwrap();
    let out = run(home_dir.path(), &["pause"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn add_multiple_paths_tracks_each_as_typed_and_repeat_is_already_tracked() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let a = home.join("A");
    let b = home.join("B");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    let (a_s, b_s) = (a.to_str().unwrap(), b.to_str().unwrap());

    let out = run_ok(home, &["add", a_s, b_s]);
    assert_eq!(out, format!("✓ {a_s} — added\n✓ {b_s} — added\n"));
    let config = fs::read_to_string(config_path(home)).unwrap();
    assert!(
        config.contains(a_s) && config.contains(b_s),
        "got: {config}"
    );

    let again = run_ok(home, &["add", a_s, b_s]);
    assert_eq!(
        again,
        format!("✓ {a_s} — already tracked\n✓ {b_s} — already tracked\n")
    );
}

#[test]
fn add_multiple_paths_with_missing_one_adds_the_rest_and_exits_nonzero() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let a = home.join("A");
    let b = home.join("B");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    let missing = home.join("Missing");
    let (a_s, b_s) = (a.to_str().unwrap(), b.to_str().unwrap());

    let out = run(home, &["add", a_s, missing.to_str().unwrap(), b_s]);
    assert!(!out.status.success());
    assert_eq!(
        stdout_of(&out),
        format!("✓ {a_s} — added\n✓ {b_s} — added\n")
    );
    assert!(
        stderr_of(&out).starts_with("error: "),
        "got: {}",
        stderr_of(&out)
    );
    let config = fs::read_to_string(config_path(home)).unwrap();
    assert!(
        config.contains(a_s) && config.contains(b_s),
        "got: {config}"
    );
    assert!(!config.contains("Missing"), "got: {config}");
}

#[test]
fn exclude_multiple_paths_excludes_each_and_rejects_only_the_absolute_one() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let anime_root = home.join("Anime");
    for rel in ["Show/ova1.mkv", "Show/ova2.mkv", "Show/ep01.mkv"] {
        touch(&anime_root.join(rel));
    }
    add_root(home, &anime_root);
    let root_s = anime_root.to_str().unwrap();

    let out = run(
        home,
        &["exclude", "Show/ova1.mkv", "/abs/path.mkv", "Show/ova2.mkv"],
    );
    assert!(!out.status.success());
    assert_eq!(
        stdout_of(&out),
        format!(
            "✓ \"Show/ova1.mkv\" — excluded (under {root_s})\n✓ \"Show/ova2.mkv\" — excluded (under {root_s})\n"
        )
    );
    assert!(
        stderr_of(&out).starts_with("error: "),
        "got: {}",
        stderr_of(&out)
    );
    let config = fs::read_to_string(config_path(home)).unwrap();
    assert!(config.contains("Show/ova1.mkv") && config.contains("Show/ova2.mkv"));
    assert!(!config.contains("/abs/path.mkv"), "got: {config}");

    let again = run_ok(home, &["exclude", "Show/ova1.mkv", "Show/ova2.mkv"]);
    assert_eq!(
        again,
        format!(
            "✓ \"Show/ova1.mkv\" — already excluded (under {root_s})\n✓ \"Show/ova2.mkv\" — already excluded (under {root_s})\n"
        )
    );
}

#[test]
fn exclude_without_paths_still_lists_candidates_and_never_mutates_config() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let anime_root = home.join("Anime");
    touch(&anime_root.join("Show").join("ep01.mkv"));
    add_root(home, &anime_root);
    let before = fs::read_to_string(config_path(home)).unwrap();

    let out = run_ok(home, &["exclude"]);
    assert_eq!(
        out,
        "No Anitomy-suggested candidates found under any tracked root.\n"
    );
    assert_eq!(fs::read_to_string(config_path(home)).unwrap(), before);
}
