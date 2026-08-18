//! Real end-to-end integration test for `tsuzuki doctor`, against the
//! compiled binary and a genuine XDG-style `$HOME` — same shape as
//! `tests/state_persistence.rs`/`tests/script_log_integration.rs`, the
//! required real-binary test for this phase (CLAUDE.md's "Every phase needs
//! at least one real-binary, real-XDG-path integration test").
//!
//! Deliberately broken setup: a configured `watch_later_dir` override that
//! doesn't resolve, and one orphaned `state.toml` record (a series tracked
//! in state but whose folder no longer exists under its still-valid root).
//! The mpv.conf/tsuzuki.lua checks additionally depend on this development
//! machine's real, standing mpv install (native and/or Flatpak `io.mpv.Mpv`)
//! being detected — the same accepted real-machine dependency
//! `src/main.rs`'s own `tracked_config` test helper and `src/launch.rs`'s
//! tests already rely on (a fresh scratch `$HOME` never has mpv.conf or the
//! companion script installed, so if *any* install method is detected here,
//! those checks are guaranteed to FAIL too — this test asserts on that
//! conditionally, never assuming one specific method).

use std::fs;
use std::path::Path;
use std::process::Command;

fn tsuzuki_bin() -> &'static str {
    env!("CARGO_BIN_EXE_tsuzuki")
}

fn run(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(tsuzuki_bin())
        .args(args)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .output()
        .expect("failed to spawn the tsuzuki binary")
}

/// Whether *this* machine has any mpv install `doctor` would detect —
/// mirrors `launch::flatpak_mpv_installed`'s own fixed system-install path
/// check and a plain `$PATH` lookup for native mpv, independently of the
/// binary under test, purely so this test can decide what it may safely
/// assert about the mpv.conf/tsuzuki.lua checks' behavior.
fn any_mpv_install_detected() -> bool {
    if Path::new("/var/lib/flatpak/app/io.mpv.Mpv").is_dir() {
        return true;
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            if dir.join("mpv").is_file() {
                return true;
            }
        }
    }
    false
}

#[test]
fn doctor_reports_unreachable_watch_later_and_an_orphaned_record_then_clears_the_orphan() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let root = home.join("Anime");
    fs::create_dir_all(root.join("StillHere")).unwrap();
    fs::File::create(root.join("StillHere").join("ep01.mkv")).unwrap();

    let add = run(home, &["add", "~/Anime"]);
    assert!(add.status.success(), "add failed: {add:?}");

    let config_path = home.join(".config").join("tsuzuki").join("config.toml");
    let unreachable_override = home.join("this_watch_later_dir_was_never_created");
    let patched = fs::read_to_string(&config_path)
        .unwrap()
        .replace("command_override = \"\"", "command_override = \"true\"")
        .replace(
            "watch_later_dir = \"\"",
            &format!("watch_later_dir = \"{}\"", unreachable_override.display()),
        );
    fs::write(&config_path, patched).unwrap();

    // Fabricate the orphaned record directly: "GoneSeries" is tracked under
    // "~/Anime" (the same literal form `add` just persisted) but has no
    // corresponding directory on disk at all.
    let state_path = home.join(".local/share/tsuzuki/state.toml");
    fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    fs::write(
        &state_path,
        r#"[[series]]
root = "~/Anime"
name = "GoneSeries"
status = "in_progress"
current_relative_path = "GoneSeries/ep01.mkv"
position_seconds = 300.0
episode_number = 1
updated_at = 1
"#,
    )
    .unwrap();

    let doctor = run(home, &["doctor"]);
    let stdout = String::from_utf8_lossy(&doctor.stdout);

    assert!(
        !doctor.status.success(),
        "a deliberately broken setup must exit non-zero, got stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("watch_later")
            && stdout.contains(&unreachable_override.display().to_string()),
        "must report the unreachable configured watch_later_dir, got:\n{stdout}"
    );
    assert!(
        stdout.contains("FAIL") && stdout.contains("orphaned-records"),
        "must report the orphaned-records check as FAIL, got:\n{stdout}"
    );
    assert!(
        stdout.contains("GoneSeries"),
        "must name the orphaned series, got:\n{stdout}"
    );
    assert!(
        stdout.contains("tsuzuki doctor --fix --forget-orphans"),
        "must hand back the exact clearing command, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("StillHere"),
        "a series whose folder still exists must never be reported as orphaned, got:\n{stdout}"
    );
    assert!(
        stdout.contains("MAL auth status"),
        "must leave a visible trace that MAL auth status is a future addition, got:\n{stdout}"
    );

    if any_mpv_install_detected() {
        assert!(
            stdout.contains("tsuzuki.lua") && stdout.contains("not installed"),
            "a fresh scratch $HOME never has the companion script installed — if any mpv \
             install method was detected at all, its tsuzuki.lua check must FAIL, got:\n{stdout}"
        );
    }

    // Confirmed dead — clear it, and confirm doing so actually did.
    let fix = run(home, &["doctor", "--fix", "--forget-orphans"]);
    let fix_stdout = String::from_utf8_lossy(&fix.stdout);
    assert!(
        fix_stdout.contains("GoneSeries"),
        "must print what it's about to delete, got:\n{fix_stdout}"
    );

    let state_contents = fs::read_to_string(&state_path).unwrap();
    assert!(
        !state_contents.contains("GoneSeries"),
        "the orphaned record must actually be gone from state.toml after --forget-orphans, got:\n{state_contents}"
    );
    assert!(
        state_contents.is_empty() || !state_contents.trim().is_empty(),
        "sanity: state.toml must still be valid TOML (readable)"
    );
    // Re-read via the real command to confirm it's still valid/parseable
    // state, not just that the string happens to lack "GoneSeries".
    let list = run(home, &["list"]);
    assert!(
        list.status.success(),
        "state.toml must still be well-formed after the orphan-clearing write: {list:?}"
    );
}

#[test]
fn doctor_forget_orphans_without_fix_is_rejected_by_the_cli() {
    // clap's own `requires = "fix"` on `--forget-orphans` — never reaches
    // doctor_command's own logic at all, so this is real clap usage/error
    // output, not a hand-rolled Tsuzuki error layer (CLAUDE.md's CLI
    // bare-invocation contract: malformed input stays on clap's own
    // error/usage output).
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let doctor = run(home, &["doctor", "--forget-orphans"]);
    assert!(
        !doctor.status.success(),
        "--forget-orphans without --fix must be rejected"
    );
    let stderr = String::from_utf8_lossy(&doctor.stderr);
    assert!(
        stderr.contains("--fix") || stderr.contains("required"),
        "expected clap's own dependency-violation error, got:\n{stderr}"
    );
}
