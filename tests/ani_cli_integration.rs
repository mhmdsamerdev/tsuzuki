//! Real-binary tests for the ani-cli source: config, history, mpv-log and
//! state.toml all live under a temp `HOME`.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tsuzuki"))
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_STATE_HOME")
        .env_remove("ANI_CLI_HIST_DIR")
        .args(args)
        .output()
        .unwrap()
}

fn write_history(home: &Path, contents: &str) {
    let dir = home.join(".local/state/ani-cli");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("ani-hsts"), contents).unwrap();
}

fn write_stream(home: &Path, name: &str, title: &str, position: f64, reason: &str) {
    let dir = home.join(".local/share/tsuzuki/mpv-log");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(name),
        format!(
            "# https://cdn.example/x/index.m3u8?t=abc\ntitle={title}\nduration=1400\nposition={position}\nreason={reason}\n"
        ),
    )
    .unwrap();
}

fn state_text(home: &Path) -> String {
    fs::read_to_string(home.join(".local/share/tsuzuki/state.toml")).unwrap_or_default()
}

fn state_field(state: &str, key: &str) -> Option<String> {
    state
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{key} = ")))
        .map(str::to_string)
}

#[test]
fn a_logged_ani_cli_stream_becomes_a_record_once_the_source_is_added() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    assert!(run(home, &["add", "ani-cli"]).status.success());
    write_history(home, "9\tblue-lock-964\tBlue Lock\n");
    write_stream(home, "a1", "Blue Lock Episode 9", 300.0, "quit");

    assert!(run(home, &["status"]).status.success());

    let state = state_text(home);
    assert_eq!(
        state_field(&state, "root").as_deref(),
        Some("\"ani-cli://\"")
    );
    assert_eq!(
        state_field(&state, "name").as_deref(),
        Some("\"Blue Lock\"")
    );
    assert_eq!(
        state_field(&state, "status").as_deref(),
        Some("\"in_progress\"")
    );
    assert_eq!(state_field(&state, "episode_number").as_deref(), Some("9"));
    assert_eq!(
        state_field(&state, "position_seconds").as_deref(),
        Some("300.0")
    );
}

#[test]
fn a_logged_ani_cli_stream_is_ignored_before_the_source_is_added() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    write_history(home, "9\tblue-lock-964\tBlue Lock\n");
    write_stream(home, "a1", "Blue Lock Episode 9", 300.0, "quit");

    assert!(run(home, &["status"]).status.success());

    assert_eq!(state_text(home), "");
}

#[test]
fn a_stream_whose_title_is_not_in_history_is_ignored_even_when_enabled() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    assert!(run(home, &["add", "ani-cli"]).status.success());
    write_history(home, "9\tblue-lock-964\tBlue Lock\n");
    write_stream(home, "a1", "Some Video Episode 3", 300.0, "quit");

    assert!(run(home, &["status"]).status.success());

    assert_eq!(state_text(home), "");
}

#[test]
fn a_finished_ani_cli_episode_moves_the_record_to_the_next_episode() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    assert!(run(home, &["add", "ani-cli"]).status.success());
    write_history(home, "9\tblue-lock-964\tBlue Lock\n");
    write_stream(home, "a1", "Blue Lock Episode 9", 1399.0, "eof");

    assert!(run(home, &["status"]).status.success());

    let state = state_text(home);
    assert_eq!(state_field(&state, "episode_number").as_deref(), Some("10"));
    assert_eq!(
        state_field(&state, "position_seconds").as_deref(),
        Some("0.0")
    );
}

#[test]
fn ani_cli_history_path_follows_ani_cli_hist_dir() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let hist = tempfile::tempdir().unwrap();
    assert!(run(home, &["add", "ani-cli"]).status.success());
    fs::write(
        hist.path().join("ani-hsts"),
        "9\tblue-lock-964\tBlue Lock\n",
    )
    .unwrap();
    write_stream(home, "a1", "Blue Lock Episode 9", 300.0, "quit");

    let output = Command::new(env!("CARGO_BIN_EXE_tsuzuki"))
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_STATE_HOME")
        .env("ANI_CLI_HIST_DIR", hist.path())
        .arg("status")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        state_field(&state_text(home), "name").as_deref(),
        Some("\"Blue Lock\"")
    );
}

fn watched_blue_lock(home: &Path) {
    assert!(run(home, &["add", "ani-cli"]).status.success());
    write_history(home, "9\tblue-lock-964\tBlue Lock\n");
    write_stream(home, "a1", "Blue Lock Episode 9", 300.0, "quit");
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn list_and_status_show_an_ani_cli_show_with_its_episode_and_position() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    watched_blue_lock(home);

    assert_eq!(
        stdout_of(&run(home, &["status"])),
        "Blue Lock — ep. 9 (05:00)\n"
    );
    assert_eq!(
        stdout_of(&run(home, &["list"])),
        "In progress:\n  Blue Lock — ep. 9 (ani-cli)\n"
    );
}

#[test]
fn start_never_offers_an_ani_cli_show_even_once_paused() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    watched_blue_lock(home);
    assert!(run(home, &["pause", "Blue Lock"]).status.success());

    let output = run(home, &["start"]);

    assert!(!stdout_of(&output).contains("Blue Lock"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("Blue Lock"));
}

#[test]
fn pause_and_drop_an_ani_cli_show_keep_its_stored_position() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    watched_blue_lock(home);

    let paused = run(home, &["pause", "Blue Lock"]);
    assert!(paused.status.success());
    assert_eq!(stdout_of(&paused), "✓ \"Blue Lock\" — paused\n");
    let state = state_text(home);
    assert_eq!(state_field(&state, "status").as_deref(), Some("\"paused\""));
    assert_eq!(
        state_field(&state, "position_seconds").as_deref(),
        Some("300.0")
    );

    let dropped = run(home, &["drop", "Blue Lock", "--root", "ani-cli"]);
    assert!(dropped.status.success());
    let state = state_text(home);
    assert_eq!(
        state_field(&state, "status").as_deref(),
        Some("\"dropped\"")
    );
    assert_eq!(
        state_field(&state, "position_seconds").as_deref(),
        Some("300.0")
    );
}

#[test]
fn pausing_an_unknown_ani_cli_show_fails_without_writing_state() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    watched_blue_lock(home);
    assert!(run(home, &["status"]).status.success());
    let before = state_text(home);

    let output = run(home, &["pause", "Nope", "--root", "ani-cli"]);

    assert!(!output.status.success());
    assert_eq!(state_text(home), before);
}
