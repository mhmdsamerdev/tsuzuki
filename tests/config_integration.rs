//! Real-binary tests for `tsuzuki config`; `edit` runs a non-interactive `$EDITOR`.

use std::fs;
use std::path::Path;
use std::process::Command;

fn tsuzuki_bin() -> &'static str {
    env!("CARGO_BIN_EXE_tsuzuki")
}

fn base_command(home: &Path) -> Command {
    let mut cmd = Command::new(tsuzuki_bin());
    cmd.env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME");
    cmd
}

#[test]
fn bare_config_shows_the_real_path_resolved_defaults_and_edit_hint() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let output = base_command(home).arg("config").output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    let config_path = home.join(".config").join("tsuzuki").join("config.toml");
    assert!(
        stdout.contains(&config_path.display().to_string()),
        "must show the real config path, got:\n{stdout}"
    );
    // Resolved defaults, not the raw file.
    assert!(stdout.contains("resume_seconds = 120"));
    assert!(stdout.contains("completion_remaining_seconds = 180"));
    assert!(stdout.contains("redirect_port = 8734"));
    assert!(stdout.contains("fuzzy_match_threshold = 0.8"));
    assert!(stdout.contains("mkv"));
    assert!(
        stdout.contains("tsuzuki config edit"),
        "must hint at the one other action, got:\n{stdout}"
    );

    // The bare invocation writes the defaults to disk.
    assert!(config_path.is_file());
}

#[test]
fn edit_without_editor_set_fails_clearly() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let output = base_command(home)
        .args(["config", "edit"])
        .env_remove("EDITOR")
        .output()
        .unwrap();

    assert!(!output.status.success(), "must fail with no $EDITOR");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("EDITOR"),
        "error must name $EDITOR specifically, got:\n{stderr}"
    );
}

#[test]
fn edit_with_a_noop_editor_saves_and_persists_across_separate_invocations() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    // `true` is a no-op editor that leaves the file untouched.
    let edit = base_command(home)
        .args(["config", "edit"])
        .env("EDITOR", "true")
        .output()
        .unwrap();
    assert!(
        edit.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&edit.stderr)
    );
    assert!(String::from_utf8_lossy(&edit.stdout).contains("config saved"));

    // A separate process sees the config persisted on disk.
    let show = base_command(home).arg("config").output().unwrap();
    assert!(show.status.success());
    let stdout = String::from_utf8_lossy(&show.stdout);
    assert!(stdout.contains("resume_seconds = 120"));
}

#[test]
fn edit_that_leaves_invalid_toml_behind_is_caught_immediately() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    // Create config.toml first so the editor has something to overwrite.
    let show = base_command(home).arg("config").output().unwrap();
    assert!(show.status.success());

    let script_path = home_dir.path().join("corrupt-editor.sh");
    fs::write(
        &script_path,
        "#!/bin/sh\nprintf 'not valid toml {{{' > \"$1\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).unwrap();
    }

    let edit = base_command(home)
        .args(["config", "edit"])
        .env("EDITOR", &script_path)
        .output()
        .unwrap();

    assert!(
        !edit.status.success(),
        "a broken hand-edit must be a real failure, not silently accepted"
    );
    let stderr = String::from_utf8_lossy(&edit.stderr);
    assert!(
        stderr.contains("syntax error"),
        "must surface the parse error, got:\n{stderr}"
    );
}

fn config_text(home: &Path) -> String {
    fs::read_to_string(home.join(".config").join("tsuzuki").join("config.toml")).unwrap()
}

#[test]
fn add_ani_cli_enables_the_source_without_tracking_a_root() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let output = base_command(home)
        .args(["add", "ani-cli"])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "✓ ani-cli — added\n"
    );
    let config = config_text(home);
    assert!(config.contains("[ani_cli]\nenabled = true\n"), "{config}");
    assert!(config.contains("paths = []"), "{config}");
}

#[test]
fn add_ani_cli_twice_reports_already_tracked_and_leaves_config_unchanged() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    base_command(home)
        .args(["add", "ani-cli"])
        .output()
        .unwrap();
    let before = config_text(home);

    let output = base_command(home)
        .args(["add", "ani-cli"])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "✓ ani-cli — already tracked\n"
    );
    assert_eq!(config_text(home), before);
}

#[test]
fn add_dot_slash_ani_cli_tracks_a_folder_of_that_name_not_the_source() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    let cwd_dir = tempfile::tempdir().unwrap();
    fs::create_dir(cwd_dir.path().join("ani-cli")).unwrap();

    let output = base_command(home)
        .current_dir(cwd_dir.path())
        .args(["add", "./ani-cli"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let config = config_text(home);
    assert!(config.contains("./ani-cli"), "{config}");
    assert!(config.contains("enabled = false"), "{config}");
}
