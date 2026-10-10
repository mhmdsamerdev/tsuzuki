//! Multi-command user sessions against the real binary, with hand-made mpv files and no network.

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

/// Runs `tsuzuki <args>`, asserts success, and returns stdout.
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

/// Points `[mpv] command_override` at `true` so `resume`/`start` never spawn a real mpv.
fn bypass_real_mpv(home: &Path) {
    let path = config_path(home);
    let patched = fs::read_to_string(&path)
        .unwrap()
        .replace("command_override = \"\"", "command_override = \"true\"");
    fs::write(&path, patched).unwrap();
}

/// Fabricates one mpv `watch_later` entry (`# <path>` then `start=<seconds>`) in the first OS-default candidate directory.
/// `entry_name` only needs to be unique within the directory.
fn write_watch_later(home: &Path, entry_name: &str, source_file: &Path, position_seconds: f64) {
    let dir = home.join(".local/state/mpv/watch_later");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(entry_name),
        format!("# {}\nstart={position_seconds:.6}\n", source_file.display()),
    )
    .unwrap();
}

/// Fabricates one `tsuzuki.lua` log entry at `~/.local/share/tsuzuki/mpv-log`.
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

/// Rewrites the single `state.toml` record's `updated_at` to `seconds_ago` in the past.
/// Only valid for a single-record `state.toml`.
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

/// Coarse filesystem mtime can tie two writes, so ordering-dependent tests pause between them.
fn ensure_next_write_is_strictly_fresher() {
    sleep(Duration::from_millis(1100));
}

// First-time onboarding: add, non-TTY `start` lists choices, explicit `start <series>` plays episode 1.

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

    // Nothing watched yet: both series show "Never started" and no record exists.
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

    // Non-interactive bare `start` lists 1-based choices and exits non-zero instead of hanging.
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

    // An explicit series argument bypasses the picker and plays episode 1 at position 0.
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

// Natural sort alone orders episodes; Anitomy only annotates the displayed number. Includes a full binge to completion.
// ---------------------------------------------------------------------

#[test]
fn natural_sort_orders_seasons_anitomy_only_annotates_and_a_binge_chain_resolves_and_completes() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let anime_root = home.join("Anime");
    let series_dir = anime_root.join("Test Show");
    // Parsed episode numbers contradict natural-sort position: Season 2's file parses as 5, Season 1's as 3.
    // Ordering must follow the directory names.
    let season1_file = series_dir
        .join("Season 1")
        .join("[Group] Test Show - 03 [1080p].mkv");
    let season2_file = series_dir
        .join("Season 2")
        .join("[Group] Test Show - 05 [1080p].mkv");
    touch(&season1_file);
    touch(&season2_file);

    add_root(home, &anime_root);

    // A bookmark on the Season 1 file, which natural sort places first.
    write_watch_later(home, "entry-1", &season1_file, 560.0);

    let list1 = run_ok(home, &["list"]);
    assert!(list1.contains("In progress:"), "got: {list1}");
    // Position is on the first natural-sort file while the displayed number is Anitomy's "03".
    assert!(
        list1.contains("Test Show — ep. 3"),
        "natural sort must place Season 1 first; Anitomy must only annotate its number, got: {list1}"
    );

    // The `eof` signal completes the current episode, so the cursor advances to the next at position 0.
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

    // The last file completing moves the series to Completed; there is no next episode.
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
    assert!(!list3.contains("Dropped:"), "got: {list3}");
    assert!(!list3.contains("ep."), "got: {list3}");
    assert!(
        list3.starts_with("Completed:\n  Test Show ("),
        "got: {list3}"
    );
    let state_contents = fs::read_to_string(state_path(home)).unwrap();
    assert!(
        state_contents.contains("status = \"completed\""),
        "a finished series must persist as completed, not dropped:\n{state_contents}"
    );

    // A completed series shows "Rewatch?", never "Start".
    let start_listing = run(home, &["start"]);
    let start_stdout = stdout_of(&start_listing);
    assert!(
        start_stdout.contains("Test Show — Rewatch?"),
        "got: {start_stdout}"
    );
}

// Bare `exclude` suggests an OVA candidate; excluding it removes it from the ordered tree, and repeating is idempotent.

#[test]
fn exclusion_workflow_suggest_confirm_removes_from_order_and_is_idempotent() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let anime_root = home.join("Anime");
    let series_dir = anime_root.join("One Piece");
    touch(&series_dir.join("[Group] One Piece - 01 [1080p].mkv"));
    touch(&series_dir.join("[Group] One Piece - 02 [1080p].mkv"));
    // OVA filename pattern that Anitomy flags.
    let ova_file = series_dir.join("[Group] One Piece - OVA 1 [1080p].mkv");
    touch(&ova_file);

    add_root(home, &anime_root);

    // Before excluding, the OVA counts toward the file count.
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

    // Bare `exclude` no longer suggests it.
    let suggested_after = run_ok(home, &["exclude"]);
    assert!(
        !suggested_after.contains("OVA"),
        "an already-excluded candidate must not be suggested again, got: {suggested_after}"
    );

    // Exclusion applies before ordering, so the file count drops to 2.
    let list_after = run_ok(home, &["list"]);
    assert!(
        list_after.contains("One Piece") && list_after.contains("(2 files)"),
        "got: {list_after}"
    );

    // Re-excluding the same path exits 0 as a no-op.
    let reexcluded = run(home, &["exclude", relative_path]);
    assert!(reexcluded.status.success());
    assert!(
        stdout_of(&reexcluded).contains("already excluded"),
        "got: {}",
        stdout_of(&reexcluded)
    );
}

// Pause/drop lifecycle: pause, list grouping, drop, an invalid transition leaves state untouched, "Rewatch?".

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

    // Re-pausing is idempotent.
    let pause2 = run_ok(home, &["pause", "Monster"]);
    assert!(pause2.contains("already paused"), "got: {pause2}");

    // Paused -> Dropped is legal.
    let drop1 = run_ok(home, &["drop", "Monster"]);
    assert!(drop1.contains("\"Monster\" — dropped"), "got: {drop1}");

    let list_dropped = run_ok(home, &["list"]);
    assert_eq!(
        list_dropped, "Dropped:\n  Monster — ep. 1 (1 files)\n",
        "a dropped series keeps its episode number"
    );

    // Dropped -> Paused is illegal: non-zero exit, state untouched.
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

    // `start` shows the dropped display word.
    let start_listing = stdout_of(&run(home, &["start"]));
    assert!(
        start_listing.contains("Monster — Rewatch?"),
        "got: {start_listing}"
    );

    // Re-dropping is idempotent.
    let drop2 = run_ok(home, &["drop", "Monster"]);
    assert!(drop2.contains("already dropped"), "got: {drop2}");
}

// The same series name under two roots is ambiguous without `--root`; the error prints the exact re-invocation and mutates nothing.

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

    // The rejected ambiguous call touched neither series.
    let list_untouched = run_ok(home, &["list"]);
    assert_eq!(
        list_untouched.matches("In progress:").count(),
        1,
        "both Overlap records must still be in_progress, got: {list_untouched}"
    );

    // `--root` pauses only the named one.
    let paused = run_ok(
        home,
        &["pause", "Overlap", "--root", root_a.to_str().unwrap()],
    );
    assert!(paused.contains("paused"), "got: {paused}");

    let list_after = run_ok(home, &["list"]);
    assert!(list_after.contains("In progress:"), "got: {list_after}");
    assert!(list_after.contains("Paused:"), "got: {list_after}");
}

// `doctor` detects an orphaned record and `--fix --forget-orphans` clears exactly it.

#[test]
fn doctor_detects_and_forgets_a_real_orphaned_record() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let anime_root = home.join("Anime");
    let gone_series_dir = anime_root.join("GoneShow");
    let file = gone_series_dir.join("ep01.mkv");
    touch(&file);
    // A second series keeps the root non-empty.
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

    // The series folder disappears; moved, renamed and deleted look the same.
    fs::remove_dir_all(&gone_series_dir).unwrap();

    let doctor1 = run(home, &["doctor"]);
    let doctor1_stdout = stdout_of(&doctor1);
    assert!(
        doctor1_stdout.contains("GoneShow"),
        "doctor must name the specific orphaned series, got: {doctor1_stdout}"
    );

    // A plain `doctor` never removes it; only `--fix --forget-orphans` does.
    let state_before_fix = fs::read_to_string(state_path(home)).unwrap();
    assert!(state_before_fix.contains("GoneShow"));

    // Not `run_ok`: on a real dev machine with a real mpv install, other
    // Not `run_ok`: other doctor checks (e.g. mpv.conf) can fail on a dev machine with mpv installed.
    // Only the orphan removal is under test.
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

// `config` show/edit round trip, including a broken-TOML edit.

#[test]
fn config_show_and_edit_round_trip_including_a_broken_edit() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    // Any config-touching command creates config.toml with defaults.
    run_ok(home, &["list"]);

    let shown = run_ok(home, &["config"]);
    assert!(
        shown.starts_with(&format!("# {}", config_path(home).display())),
        "got: {shown}"
    );
    assert!(shown.contains("resume_seconds = 120"), "got: {shown}");
    assert!(shown.contains("→ tsuzuki config edit"), "got: {shown}");

    // A non-interactive "$EDITOR" script that bumps resume_seconds.
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

    // A broken-TOML edit is reported immediately with a non-zero exit.
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

// The script log wins only when fresher, and an unchanged log entry isn't re-read as fresh activity.

#[test]
fn companion_script_wins_on_freshness_and_a_stale_entry_never_reappears_as_new_activity() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let anime_root = home.join("Anime");
    let file = anime_root.join("Monster").join("ep01.mkv");
    touch(&file);
    add_root(home, &anime_root);

    // An older watch_later bookmark...
    write_watch_later(home, "wl-entry", &file, 300.0);
    ensure_next_write_is_strictly_fresher();
    // ...then a fresher script-log reading for the same file at a different position.
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

    // Nothing changed on disk; the lingering script-log entry must not count as fresh activity.
    let list2 = run_ok(home, &["list"]);
    assert!(list2.contains("In progress:"), "got: {list2}");
    let state2 = fs::read_to_string(state_path(home)).unwrap();
    assert_eq!(
        state1, state2,
        "an unchanged script-log entry must never look like new activity on a later, unrelated run"
    );
}

// A sub-`resume_seconds` reading never reaches state.toml, even when fresher than a saved position.

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

    // A 45s reading (below the default 120s) with a later mtime.
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

// Forward scan only: a bookmark on an earlier episode never reactivates or moves a Paused series.

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

    // Progress on episode 2, then paused.
    write_watch_later(home, "entry-2", &ep2, 500.0);
    run_ok(home, &["list"]);
    run_ok(home, &["pause", "Monster"]);
    let state_paused = fs::read_to_string(state_path(home)).unwrap();
    assert!(state_paused.contains("ep02.mkv"), "got:\n{state_paused}");

    // A bookmark on episode 1 with a fresher mtime than anything recorded.
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

// Symlinks are followed: a symlinked series folder is tracked like a real one, and a watch_later entry via the symlink path attributes correctly.

#[test]
#[cfg(unix)]
fn a_symlinked_series_folder_is_tracked_exactly_like_a_real_one() {
    use std::os::unix::fs::symlink;

    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let anime_root = home.join("Anime");
    touch(&anime_root.join("RealShow").join("ep01.mkv"));

    // The real storage is outside the tracked root; "LinkedShow" under the root is a symlink to it.
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

    // mpv reports the symlinked path it was opened through.
    let symlinked_path = anime_root.join("LinkedShow").join("ep01.mkv");
    write_watch_later(home, "entry", &symlinked_path, 400.0);

    let list_after = run_ok(home, &["list"]);
    assert!(
        list_after.contains("In progress:") && list_after.contains("LinkedShow"),
        "got: {list_after}"
    );
}

// Non-interactive bare `resume` with several series in progress lists choices and exits non-zero.

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

    // The listing mutated nothing.
    let state_untouched = fs::read_to_string(state_path(home)).unwrap();
    assert!(state_untouched.contains("Monster") && state_untouched.contains("Naruto"));
}

// `sync` with progress but no MAL login fails cleanly without touching the network.

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

// A paused series is excluded from sync before any MAL auth or network access.

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

// A dropped series is excluded from sync; only a finished series is acted on.

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

    // Two minutes into ep1 of two, then a manual drop.
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

    // A one-episode series watched to its end is a sync target and fails at authentication.
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

// `pause`/`drop` reject a finished series.

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

// A deleted series folder leaves its record inert: sync ignores it and only `doctor` cleans it up.

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

// `auto_pause_days` is off by default; opted in, an idle series auto-pauses (never drops) and keeps its position.

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

    // Backdating with the default (0 = never) pauses nothing. Re-running `list` doesn't bump `updated_at`, so
    // backdating is the only way to simulate elapsed time.
    backdate_only_updated_at(home, 365 * 86_400);
    let list_default = run_ok(home, &["list"]);
    assert!(
        list_default.contains("In progress:"),
        "auto_pause_days defaults to 0 (never auto-pause); got: {list_default}"
    );

    // Opt in to a 1-day threshold.
    let cfg = config_path(home);
    let patched = fs::read_to_string(&cfg)
        .unwrap()
        .replace("auto_pause_days = 0", "auto_pause_days = 1");
    fs::write(&cfg, patched).unwrap();

    // Fresh activity refreshes `updated_at`, so the series stays in_progress.
    ensure_next_write_is_strictly_fresher();
    write_watch_later(home, "entry", &file, 600.0);
    let list_still_fresh = run_ok(home, &["list"]);
    assert!(
        list_still_fresh.contains("In progress:"),
        "got: {list_still_fresh}"
    );

    // Idle for 2 days against the 1-day threshold, with no new activity.
    backdate_only_updated_at(home, 2 * 86_400);

    let list_idle = run_ok(home, &["list"]);
    assert!(
        list_idle.contains("Paused:") && !list_idle.contains("In progress:"),
        "a series idle past auto_pause_days must be auto-paused, got: {list_idle}"
    );

    // The saved position and episode survive, as with a manual pause.
    let state_after = fs::read_to_string(state_path(home)).unwrap();
    assert!(state_after.contains("600"), "got:\n{state_after}");
    assert!(state_after.contains("ep01.mkv"), "got:\n{state_after}");
}

// Multi-argument `add`/`exclude`/`pause`/`drop`: each is handled independently; exit is non-zero only if one failed.

/// Tracks `Anime/{names}` with one episode each and persists an `in_progress` record per series via `list`.
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

// With several tracked roots, `list`, `start` and `resume` name each series' root; a single root keeps plain output.

/// Two tracked roots, `Anime` (added first) and `Shows`, as their stored strings.
fn two_roots_with_series(home: &Path, anime: &[&str], shows: &[&str]) -> (String, String) {
    let anime_root = home.join("Anime");
    let shows_root = home.join("Shows");
    for name in anime {
        touch(&anime_root.join(name).join("ep01.mkv"));
    }
    for name in shows {
        touch(&shows_root.join(name).join("ep01.mkv"));
    }
    add_root(home, &anime_root);
    add_root(home, &shows_root);
    (
        anime_root.to_str().unwrap().to_string(),
        shows_root.to_str().unwrap().to_string(),
    )
}

#[test]
fn list_with_two_routes_tags_every_row_with_its_route() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    two_roots_with_series(home, &["Monster"], &["Frieren", "Monster"]);

    let list = run_ok(home, &["list"]);

    assert_eq!(
        list,
        "Never started:\n  Frieren (1 files)  [Shows]\n  Monster (1 files)  [Anime]\n  Monster (1 files)  [Shows]\n"
    );
}

#[test]
fn list_with_one_route_has_no_route_tags() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let root = home.join("Anime");
    touch(&root.join("Monster").join("ep01.mkv"));
    add_root(home, &root);

    let list = run_ok(home, &["list"]);

    assert_eq!(list, "Never started:\n  Monster (1 files)\n");
}

#[test]
fn non_tty_start_with_two_routes_groups_choices_under_route_headers() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    two_roots_with_series(home, &["Monster"], &["Frieren", "Monster"]);

    let start = run(home, &["start"]);

    assert!(!start.status.success());
    assert_eq!(
        stdout_of(&start),
        "Available to start:\n\nAnime\n  1. Monster — Start (1 files)\n\nShows\n  2. Frieren — Start (1 files)\n  3. Monster — Start (1 files)\n\nRun `tsuzuki start <series>` to pick one.\n"
    );
}

#[test]
fn non_tty_resume_with_two_routes_groups_in_progress_series_under_route_headers() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let (anime, shows) = two_roots_with_series(home, &["Monster"], &["Frieren"]);
    write_watch_later(
        home,
        "entry-a",
        &Path::new(&anime).join("Monster").join("ep01.mkv"),
        300.0,
    );
    write_watch_later(
        home,
        "entry-b",
        &Path::new(&shows).join("Frieren").join("ep01.mkv"),
        400.0,
    );
    run_ok(home, &["list"]); // persists both as real in_progress records

    let resume = run(home, &["resume"]);

    assert!(!resume.status.success());
    assert_eq!(
        stdout_of(&resume),
        "Anime\n  1. Monster (ep. 1)\n\nShows\n  2. Frieren (ep. 1)\n\nRun `tsuzuki resume <series>` to pick one.\n"
    );
}

// `start`/`resume` scoped to one root by folder name or `--root`.

/// Adds one root per `(folder, series)` pair under `home`, each series holding one episode; returns the stored roots in order.
fn library(home: &Path, roots: &[(&str, &[&str])]) -> Vec<String> {
    roots
        .iter()
        .map(|(folder, series)| {
            let root = home.join(folder);
            fs::create_dir_all(&root).unwrap();
            for name in *series {
                touch(&root.join(name).join("ep01.mkv"));
            }
            add_root(home, &root);
            root.to_str().unwrap().to_string()
        })
        .collect()
}

/// Marks `<root>/<series>/ep01.mkv` in progress at `position` seconds via a `list` run.
fn watch(home: &Path, entry: &str, root: &str, series: &str, position: f64) {
    write_watch_later(
        home,
        entry,
        &Path::new(root).join(series).join("ep01.mkv"),
        position,
    );
    run_ok(home, &["list"]);
}

fn snapshot(home: &Path) -> (String, String) {
    (
        fs::read_to_string(config_path(home)).unwrap(),
        fs::read_to_string(state_path(home)).unwrap_or_default(),
    )
}

const START_TAIL: &str = "\nRun `tsuzuki start <series>` to pick one.\n";
const RESUME_TAIL: &str = "\nRun `tsuzuki resume <series>` to pick one.\n";

#[test]
fn start_with_root_folder_name_lists_only_that_root_without_route_tags() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    library(
        home,
        &[
            ("anime", &["Monster", "Naruto"]),
            ("re", &["Bleach"]),
            ("tv-series", &["Dark"]),
        ],
    );

    let out = run(home, &["start", "re"]);

    assert!(!out.status.success());
    assert_eq!(
        stdout_of(&out),
        format!("Available to start:\n\n  1. Bleach — Start (1 files)\n{START_TAIL}")
    );
    assert_eq!(stderr_of(&out), "");
}

#[test]
fn start_without_argument_still_lists_every_root_under_headers() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    library(
        home,
        &[
            ("anime", &["Monster"]),
            ("re", &["Bleach"]),
            ("tv-series", &["Dark"]),
        ],
    );

    let out = run(home, &["start"]);

    assert!(!out.status.success());
    assert_eq!(
        stdout_of(&out),
        format!(
            "Available to start:\n\nanime\n  1. Monster — Start (1 files)\n\nre\n  2. Bleach — Start (1 files)\n\ntv-series\n  3. Dark — Start (1 files)\n{}",
            START_TAIL
        )
    );
}

#[test]
fn start_root_flag_without_series_lists_only_that_root() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let roots = library(home, &[("anime", &["Monster"]), ("re", &["Bleach"])]);
    let expected = format!("Available to start:\n\n  1. Bleach — Start (1 files)\n{START_TAIL}");

    let by_path = run(home, &["start", "--root", &roots[1]]);
    let by_tilde = run(home, &["start", "--root", "~/re"]);

    assert_eq!(stdout_of(&by_path), expected);
    assert_eq!(stdout_of(&by_tilde), expected);
}

#[test]
fn start_root_flag_that_is_not_tracked_errors_and_writes_nothing() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    library(home, &[("anime", &["Monster"])]);
    let before = snapshot(home);

    let out = run(home, &["start", "--root", "/nowhere"]);

    assert!(!out.status.success());
    assert_eq!(stdout_of(&out), "");
    assert_eq!(stderr_of(&out), "error: not a tracked root: /nowhere\n");
    assert_eq!(snapshot(home), before);
}

#[test]
fn start_with_root_folder_name_matches_a_root_stored_with_a_trailing_slash() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    touch(&home.join("re").join("Bleach").join("ep01.mkv"));
    run_ok(home, &["add", &format!("{}/", home.join("re").display())]);

    let out = run(home, &["start", "re"]);

    assert_eq!(
        stdout_of(&out),
        format!("Available to start:\n\n  1. Bleach — Start (1 files)\n{START_TAIL}")
    );
}

#[test]
fn start_with_root_folder_name_is_case_sensitive() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    library(home, &[("re", &["Bleach"])]);

    let out = run(home, &["start", "RE"]);

    assert!(!out.status.success());
    assert_eq!(stdout_of(&out), "");
    assert_eq!(
        stderr_of(&out),
        "error: no tracked series or root named \"RE\"\n"
    );
}

#[test]
fn start_with_unknown_name_errors_without_listing_or_writing() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    library(home, &[("anime", &["Monster"]), ("re", &["Bleach"])]);
    let before = snapshot(home);

    let start = run(home, &["start", "nope"]);
    let resume = run(home, &["resume", "nope"]);

    for out in [&start, &resume] {
        assert!(!out.status.success());
        assert_eq!(stdout_of(out), "");
        assert_eq!(
            stderr_of(out),
            "error: no tracked series or root named \"nope\"\n"
        );
    }
    assert_eq!(snapshot(home), before);
}

#[test]
fn start_with_root_whose_every_series_is_in_progress_errors_nonzero() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let roots = library(home, &[("anime", &["Monster"]), ("re", &["Bleach"])]);
    watch(home, "e1", &roots[1], "Bleach", 300.0);
    let before = snapshot(home);

    let out = run(home, &["start", "re"]);

    assert!(!out.status.success());
    assert_eq!(stdout_of(&out), "");
    assert_eq!(stderr_of(&out), "error: nothing to start in re\n");
    assert_eq!(snapshot(home), before);
}

#[test]
fn start_with_root_that_has_no_series_errors_nonzero() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    library(home, &[("anime", &["Monster"]), ("empty", &[])]);

    let out = run(home, &["start", "empty"]);

    assert!(!out.status.success());
    assert_eq!(stdout_of(&out), "");
    assert_eq!(stderr_of(&out), "error: nothing to start in empty\n");
}

#[test]
fn start_without_argument_and_nothing_to_start_still_exits_zero() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    library(home, &[("empty", &[])]);

    let out = run(home, &["start"]);

    assert!(out.status.success());
    assert_eq!(stdout_of(&out), "Nothing to start.\n");
}

#[test]
fn start_with_series_name_and_no_root_of_that_name_still_plays_the_series() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let roots = library(home, &[("anime", &["Monster"]), ("re", &["Bleach"])]);
    bypass_real_mpv(home);

    let out = run(home, &["start", "Bleach"]);

    assert!(out.status.success());
    assert_eq!(
        stdout_of(&out),
        format!(
            "✓ Starting \"Bleach\" — episode 1 (00:00)\n  → {}/Bleach/ep01.mkv\n",
            roots[1]
        )
    );
}

#[test]
fn start_with_series_and_root_flag_behaves_as_before() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let roots = library(home, &[("a", &["Monster"]), ("b", &["Monster"])]);
    bypass_real_mpv(home);

    let out = run(home, &["start", "Monster", "--root", &roots[1]]);

    assert!(out.status.success());
    assert_eq!(
        stdout_of(&out),
        format!(
            "✓ Starting \"Monster\" — episode 1 (00:00)\n  → {}/Monster/ep01.mkv\n",
            roots[1]
        )
    );
}

#[test]
fn start_with_name_that_is_both_a_series_and_a_root_errors_and_runs_nothing() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let roots = library(home, &[("anime", &["re", "Monster"]), ("re", &["Bleach"])]);
    bypass_real_mpv(home);
    let before = snapshot(home);

    let out = run(home, &["start", "re"]);

    assert!(!out.status.success());
    assert_eq!(stdout_of(&out), "");
    assert_eq!(
        stderr_of(&out),
        format!(
            "✗ \"re\" is both a series and a root folder name\n  → tsuzuki start --root {}\n  → tsuzuki start \"re\" --root {}\n",
            roots[1], roots[0]
        )
    );
    assert_eq!(snapshot(home), before);

    // Each printed re-invocation does what it says.
    let scoped = run(home, &["start", "--root", &roots[1]]);
    assert_eq!(
        stdout_of(&scoped),
        format!("Available to start:\n\n  1. Bleach — Start (1 files)\n{START_TAIL}")
    );
    let series = run(home, &["start", "re", "--root", &roots[0]]);
    assert!(series.status.success());
    assert_eq!(
        stdout_of(&series),
        format!(
            "✓ Starting \"re\" — episode 1 (00:00)\n  → {}/re/ep01.mkv\n",
            roots[0]
        )
    );
}

#[test]
fn resume_with_name_that_is_both_a_series_and_a_root_errors_with_resume_hints() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let roots = library(home, &[("anime", &["re"]), ("re", &["Bleach"])]);
    let before = snapshot(home);

    let out = run(home, &["resume", "re"]);

    assert!(!out.status.success());
    assert_eq!(stdout_of(&out), "");
    assert_eq!(
        stderr_of(&out),
        format!(
            "✗ \"re\" is both a series and a root folder name\n  → tsuzuki resume --root {}\n  → tsuzuki resume \"re\" --root {}\n",
            roots[1], roots[0]
        )
    );
    assert_eq!(snapshot(home), before);
}

#[test]
fn start_with_folder_name_shared_by_two_roots_is_ambiguous_and_flag_resolves_it() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let roots = library(home, &[("a/anime", &["Monster"]), ("b/anime", &["Naruto"])]);
    let before = snapshot(home);

    let out = run(home, &["start", "anime"]);

    assert!(!out.status.success());
    assert_eq!(stdout_of(&out), "");
    assert_eq!(
        stderr_of(&out),
        format!(
            "✗ \"anime\" is ambiguous — 2 tracked roots share that folder name\n  → tsuzuki start --root {}\n  → tsuzuki start --root {}\n",
            roots[0], roots[1]
        )
    );
    assert_eq!(snapshot(home), before);

    let resolved = run(home, &["start", "--root", &roots[1]]);
    assert_eq!(
        stdout_of(&resolved),
        format!("Available to start:\n\n  1. Naruto — Start (1 files)\n{START_TAIL}")
    );
}

#[test]
fn resume_with_root_folder_name_lists_only_that_roots_in_progress_series() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let roots = library(
        home,
        &[
            ("anime", &["Monster", "Naruto"]),
            ("re", &["Bleach"]),
            ("tv-series", &["Dark"]),
        ],
    );
    watch(home, "e1", &roots[0], "Monster", 300.0);
    watch(home, "e2", &roots[0], "Naruto", 310.0);
    watch(home, "e3", &roots[1], "Bleach", 320.0);
    watch(home, "e4", &roots[2], "Dark", 330.0);

    let out = run(home, &["resume", "anime"]);

    assert!(!out.status.success());
    assert_eq!(
        stdout_of(&out),
        format!("  1. Monster (ep. 1)\n  2. Naruto (ep. 1)\n{RESUME_TAIL}")
    );
}

#[test]
fn resume_with_root_having_exactly_one_in_progress_series_plays_it_without_a_prompt() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let roots = library(
        home,
        &[("anime", &["Monster", "Naruto"]), ("re", &["Bleach"])],
    );
    watch(home, "e1", &roots[0], "Monster", 300.0);
    watch(home, "e2", &roots[0], "Naruto", 310.0);
    watch(home, "e3", &roots[1], "Bleach", 320.0);
    bypass_real_mpv(home);

    let out = run(home, &["resume", "re"]);

    assert!(out.status.success());
    assert_eq!(
        stdout_of(&out),
        format!(
            "✓ Resuming \"Bleach\" — episode 1 (05:20)\n  → {}/Bleach/ep01.mkv\n",
            roots[1]
        )
    );
}

#[test]
fn resume_root_flag_without_series_scopes_to_that_root() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let roots = library(home, &[("anime", &["Monster"]), ("re", &["Bleach"])]);
    watch(home, "e1", &roots[0], "Monster", 300.0);
    watch(home, "e2", &roots[1], "Bleach", 320.0);
    bypass_real_mpv(home);

    let out = run(home, &["resume", "--root", &roots[1]]);

    assert!(out.status.success());
    assert_eq!(
        stdout_of(&out),
        format!(
            "✓ Resuming \"Bleach\" — episode 1 (05:20)\n  → {}/Bleach/ep01.mkv\n",
            roots[1]
        )
    );
}

#[test]
fn resume_with_root_having_nothing_in_progress_errors_and_never_falls_through_to_start() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let roots = library(home, &[("anime", &["Monster"]), ("re", &["Bleach"])]);
    watch(home, "e1", &roots[0], "Monster", 300.0);
    let before = snapshot(home);

    let out = run(home, &["resume", "re"]);

    assert!(!out.status.success());
    assert_eq!(stdout_of(&out), "");
    assert_eq!(stderr_of(&out), "error: nothing in progress in re\n");
    assert_eq!(snapshot(home), before);
}

#[test]
fn resume_without_argument_and_nothing_in_progress_still_falls_through_to_start() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    library(home, &[("anime", &["Monster"])]);

    let out = run(home, &["resume"]);

    assert!(!out.status.success());
    assert_eq!(
        stdout_of(&out),
        format!("Available to start:\n\n  1. Monster — Start (1 files)\n{START_TAIL}")
    );
}
