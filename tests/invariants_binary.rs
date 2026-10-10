//! Real-binary checks for `watch_later` skipping, the `resume_seconds` boundary, `pause`/`drop`/`exclude` errors and a corrupt `state.toml`.

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

fn watch_later_dir(home: &Path) -> std::path::PathBuf {
    let dir = home.join(".local/state/mpv/watch_later");
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn list_skips_redirect_and_malformed_watch_later_entries_and_still_reads_the_valid_one() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let root = library(home);
    let dir = watch_later_dir(home);
    fs::write(dir.join("a-redirect"), "# redirect entry\n").unwrap();
    fs::write(dir.join("b-malformed"), b"\xff\xfe not an entry\n").unwrap();
    fs::write(
        dir.join("c-valid"),
        format!(
            "# {}\nstart=300.000000\n",
            root.join("Monster/ep02.mkv").display()
        ),
    )
    .unwrap();

    let out = run(home, &["list"]);

    assert!(out.status.success(), "stderr: {}", stderr_of(&out));
    assert_eq!(
        stdout_of(&out),
        "In progress:\n  Monster — ep. 2 (2 files)\n"
    );
}

#[test]
fn list_with_position_exactly_resume_seconds_records_it_and_just_below_does_not() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let root = library(home);
    let dir = watch_later_dir(home);
    let ep1 = root.join("Monster/ep01.mkv");

    fs::write(
        dir.join("e"),
        format!("# {}\nstart=119.900000\n", ep1.display()),
    )
    .unwrap();
    assert!(run(home, &["list"]).status.success());
    assert!(
        !state_file(home).exists()
            || !fs::read_to_string(state_file(home))
                .unwrap()
                .contains("[[series]]"),
        "119.9s is below the default 120s threshold and must not create a record"
    );

    fs::write(
        dir.join("e"),
        format!("# {}\nstart=120.000000\n", ep1.display()),
    )
    .unwrap();
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
