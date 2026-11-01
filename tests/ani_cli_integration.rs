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
