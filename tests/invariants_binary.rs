//! Real-binary checks for script-log skipping, the `resume_seconds` boundary, `pause`/`drop`/`exclude` errors and a corrupt `state.toml`.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tsuzuki"))
        .args(args)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .output()
        .expect("failed to spawn the tsuzuki binary")
}

fn stdout_of(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr_of(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn state_file(home: &Path) -> std::path::PathBuf {
    home.join(".local/share/tsuzuki/state.toml")
}

fn library(home: &Path) -> std::path::PathBuf {
    let root = home.join("Anime");
    fs::create_dir_all(root.join("Monster")).unwrap();
    fs::File::create(root.join("Monster/ep01.mkv")).unwrap();
    fs::File::create(root.join("Monster/ep02.mkv")).unwrap();
    assert!(run(home, &["add", root.to_str().unwrap()]).status.success());
    root
}

fn mpv_log_dir(home: &Path) -> std::path::PathBuf {
    let dir = home.join(".local/share/tsuzuki/mpv-log");
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Writes a script-log entry and stamps its mtime `offset_seconds` from now.
fn write_log_entry(home: &Path, name: &str, source: &Path, body: &str, offset_seconds: i64) {
    let path = mpv_log_dir(home).join(name);
    fs::write(&path, format!("# {}\n{body}", source.display())).unwrap();
    let now = std::time::SystemTime::now();
    let when = if offset_seconds >= 0 {
        now + std::time::Duration::from_secs(offset_seconds as u64)
    } else {
        now - std::time::Duration::from_secs(offset_seconds.unsigned_abs())
    };
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

#[test]
fn list_skips_redirect_and_malformed_script_log_entries_and_still_reads_the_valid_one() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let root = library(home);
    let ep02 = root.join("Monster/ep02.mkv");
    write_log_entry(
        home,
        "a-redirect",
        &ep02,
        "duration=\nposition=\nreason=redirect\n",
        0,
    );
    fs::write(
        mpv_log_dir(home).join("b-malformed"),
        b"\xff\xfe not an entry\n",
    )
    .unwrap();
    write_log_entry(
        home,
        "c-valid",
        &ep02,
        "duration=\nposition=300.000000\nreason=quit\n",
        0,
    );

    let out = run(home, &["list"]);

    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert_eq!(
        stdout_of(&out),
        "In progress:\n  Monster — ep. 2 (2 files)\n"
    );
}

#[test]
fn list_ignores_a_leftover_mpv_watch_later_entry_and_creates_no_state() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let root = library(home);
    let dir = home.join(".local/state/mpv/watch_later");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("abc"),
        format!(
            "# {}\nstart=300.000000\n",
            root.join("Monster/ep02.mkv").display()
        ),
    )
    .unwrap();

    let out = run(home, &["list"]);

    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert!(
        !state_file(home).exists() || !state_text(home).contains("[[series]]"),
        "a watch_later file must not create a record"
    );
}

#[test]
fn list_loads_a_config_that_still_has_the_removed_watch_later_dir_key() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let root = library(home);
    let config = home.join(".config/tsuzuki/config.toml");
    let patched = fs::read_to_string(&config)
        .unwrap()
        .replace("[mpv]\n", "[mpv]\nwatch_later_dir = \"/nonexistent\"\n");
    assert!(
        patched.contains("watch_later_dir"),
        "the [mpv] table must exist"
    );
    fs::write(&config, patched).unwrap();
    write_log_entry(
        home,
        "e",
        &root.join("Monster/ep02.mkv"),
        "duration=\nposition=300.000000\nreason=quit\n",
        0,
    );

    let out = run(home, &["list"]);

    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert_eq!(
        stdout_of(&out),
        "In progress:\n  Monster — ep. 2 (2 files)\n"
    );
    assert_eq!(stderr_of(&out), "");
}

#[test]
fn list_with_position_exactly_resume_seconds_records_it_and_just_below_does_not() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let root = library(home);
    let ep1 = root.join("Monster/ep01.mkv");

    write_log_entry(
        home,
        "e",
        &ep1,
        "duration=\nposition=119.900000\nreason=quit\n",
        0,
    );
    assert!(run(home, &["list"]).status.success());
    assert!(
        !state_file(home).exists()
            || !fs::read_to_string(state_file(home))
                .unwrap()
                .contains("[[series]]"),
        "119.9s is below the default 120s threshold and must not create a record"
    );

    write_log_entry(
        home,
        "e",
        &ep1,
        "duration=\nposition=120.000000\nreason=quit\n",
        0,
    );
    assert!(run(home, &["list"]).status.success());
    let state = fs::read_to_string(state_file(home)).unwrap();
    assert!(
        state.contains("position_seconds = 120.0"),
        "exactly 120s must be recorded, got:\n{state}"
    );
}

#[test]
fn pause_and_drop_without_a_record_exit_nonzero_with_a_plain_message() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    library(home);

    for cmd in ["pause", "drop"] {
        let out = run(home, &[cmd, "Monster"]);
        assert_eq!(out.status.code(), Some(1), "{cmd}");
        assert_eq!(
            stderr_of(&out),
            "error: no progress to act on for this series — it was never started\n",
            "{cmd}"
        );
    }
}

#[test]
fn exclude_with_an_absolute_path_is_rejected_and_leaves_config_untouched() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    library(home);
    let config = home.join(".config/tsuzuki/config.toml");
    let before = fs::read_to_string(&config).unwrap();

    let out = run(home, &["exclude", "/abs/path"]);

    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stderr_of(&out),
        "error: path must be relative to a tracked root, not absolute: /abs/path\n"
    );
    assert_eq!(fs::read_to_string(&config).unwrap(), before);
}

#[test]
fn corrupt_state_toml_errors_without_panic_and_is_not_overwritten() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    library(home);
    fs::create_dir_all(state_file(home).parent().unwrap()).unwrap();
    fs::write(state_file(home), "this is = not [toml\n").unwrap();

    let out = run(home, &["list"]);

    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr_of(&out).starts_with("error: could not read state at "),
        "got: {}",
        stderr_of(&out)
    );
    assert_eq!(
        fs::read_to_string(state_file(home)).unwrap(),
        "this is = not [toml\n"
    );
}

fn state_text(home: &Path) -> String {
    fs::read_to_string(state_file(home)).unwrap()
}

#[test]
fn dropped_series_stays_dropped_on_a_leftover_reading_and_resumes_on_a_fresh_one() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let root = library(home);
    let ep01 = root.join("Monster/ep01.mkv");

    write_log_entry(
        home,
        "abc",
        &ep01,
        "duration=\nposition=300\nreason=quit\n",
        0,
    );
    assert!(run(home, &["list"]).status.success());
    assert!(run(home, &["drop", "Monster"]).status.success());
    assert!(state_text(home).contains("status = \"dropped\""));

    // Quit 97s before the end of EP01, but written before the drop.
    let quit = "duration=1551.551000\nposition=1454.474000\nreason=quit\n";
    write_log_entry(home, "e1", &ep01, quit, -3600);
    assert!(run(home, &["list"]).status.success());
    let state = state_text(home);
    assert!(state.contains("status = \"dropped\""), "{state}");
    assert!(state.contains("position_seconds = 300.0"), "{state}");

    // The same reading written after the drop is new watching.
    write_log_entry(home, "e1", &ep01, quit, 3600);
    assert!(run(home, &["list"]).status.success());
    let state = state_text(home);
    assert!(state.contains("status = \"in_progress\""), "{state}");
    assert!(
        state.contains("current_relative_path = \"Monster/ep02.mkv\""),
        "{state}"
    );
    assert!(state.contains("position_seconds = 0.0"), "{state}");
}

#[test]
fn completed_series_rewatched_mid_episode_is_in_progress_at_that_position() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let root = library(home);
    let ep01 = root.join("Monster/ep01.mkv");
    let ep02 = root.join("Monster/ep02.mkv");

    write_log_entry(
        home,
        "abc",
        &ep01,
        "duration=\nposition=300\nreason=quit\n",
        0,
    );
    assert!(run(home, &["list"]).status.success());
    // The log entry for EP01 is replaced by the finish of EP02.
    fs::remove_file(mpv_log_dir(home).join("abc")).unwrap();
    write_log_entry(
        home,
        "last",
        &ep02,
        "duration=1420\nposition=1415\nreason=eof\n",
        0,
    );
    assert!(run(home, &["list"]).status.success());
    assert!(state_text(home).contains("status = \"completed\""));

    // Months later: stop EP01 ten minutes in.
    write_log_entry(
        home,
        "again",
        &ep01,
        "duration=1420\nposition=600\nreason=quit\n",
        3600,
    );
    assert!(run(home, &["list"]).status.success());
    let state = state_text(home);
    assert!(state.contains("status = \"in_progress\""), "{state}");
    assert!(
        state.contains("current_relative_path = \"Monster/ep01.mkv\""),
        "{state}"
    );
    assert!(state.contains("position_seconds = 600.0"), "{state}");
}
