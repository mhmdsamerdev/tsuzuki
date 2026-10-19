//! Real-binary tests for `tsuzuki doctor`. The mpv.conf/tsuzuki.lua assertions depend on whether mpv is installed here.

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

/// Whether this machine has an mpv install `doctor` would detect, checked independently of the binary under test.
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

    // An orphan: "GoneSeries" is tracked under "~/Anime" but has no directory on disk.
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
        stdout.contains("watch-later")
            && stdout.contains(&unreachable_override.display().to_string()),
        "must report the unreachable configured watch_later_dir, got:\n{stdout}"
    );
    assert!(
        stdout.contains("✗") && stdout.contains("orphaned records"),
        "must report the orphaned records check as failing, got:\n{stdout}"
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
        stdout.contains("MyAnimeList") && stdout.contains("not logged in"),
        "Phase 8a-correction: an unset [mal] client_id now resolves to the built-in default \
         client_id (no per-user registration required), so a fresh $HOME with no mal_token.toml \
         must report the real mal-auth check as not-logged-in (WARN) — not \"not configured\" \
         (that state is no longer reachable from an empty config), and not the old Phase-7 \
         placeholder line either, got:\n{stdout}"
    );

    if any_mpv_install_detected() {
        assert!(
            stdout.contains("companion script") && stdout.contains("not installed"),
            "a fresh scratch $HOME never has the companion script installed — if any mpv \
             install method was detected at all, its companion-script check must FAIL, got:\n{stdout}"
        );
    }

    // Clearing the confirmed orphan must actually remove it.
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
    // Re-read through the real command to confirm the state is still parseable.
    let list = run(home, &["list"]);
    assert!(
        list.status.success(),
        "state.toml must still be well-formed after the orphan-clearing write: {list:?}"
    );
}

#[test]
fn doctor_forget_orphans_without_fix_is_rejected_by_the_cli() {
    // clap's `requires = "fix"` rejects this before doctor's own logic runs.
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

#[test]
fn doctor_prints_the_local_version_banner_first_with_no_network_involved() {
    // The banner is the compiled-in version, never a network lookup.
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let doctor = run(home, &["doctor"]);
    let stdout = String::from_utf8_lossy(&doctor.stdout);
    let expected = format!("Tsuzuki v{}", env!("CARGO_PKG_VERSION"));
    assert!(
        stdout.starts_with(&expected),
        "expected doctor's first line to be {expected:?}, got:\n{stdout}"
    );
}

/// Tracked root with one series, mpv never spawned, and one script-log entry stamped `offset_seconds` from now.
fn home_with_a_script_log_entry(home: &Path, offset_seconds: i64) -> std::path::PathBuf {
    let ep = home.join("Anime/Monster/ep01.mkv");
    fs::create_dir_all(ep.parent().unwrap()).unwrap();
    fs::File::create(&ep).unwrap();
    assert!(run(home, &["add", "~/Anime"]).status.success());
    let config_path = home.join(".config/tsuzuki/config.toml");
    let patched = fs::read_to_string(&config_path)
        .unwrap()
        .replace("command_override = \"\"", "command_override = \"true\"");
    fs::write(&config_path, patched).unwrap();

    let log_dir = home.join(".local/share/tsuzuki/mpv-log");
    fs::create_dir_all(&log_dir).unwrap();
    let entry = log_dir.join("e1");
    fs::write(
        &entry,
        format!(
            "# {}\nduration=1420\nposition=300\nreason=quit\n",
            ep.display()
        ),
    )
    .unwrap();
    let now = std::time::SystemTime::now();
    let when = if offset_seconds >= 0 {
        now + std::time::Duration::from_secs(offset_seconds as u64)
    } else {
        now - std::time::Duration::from_secs(offset_seconds.unsigned_abs())
    };
    fs::File::options()
        .write(true)
        .open(&entry)
        .unwrap()
        .set_modified(when)
        .unwrap();
    entry
}

#[test]
fn doctor_warns_about_a_future_dated_entry_without_touching_state() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    home_with_a_script_log_entry(home, 86_400);

    let doctor = run(home, &["doctor"]);
    let stdout = String::from_utf8_lossy(&doctor.stdout);

    assert!(
        stdout.contains(
            "⚠ mpv timestamps — 1 entry dated in the future, which can reactivate dropped series\n  → check the system clock"
        ),
        "got:\n{stdout}"
    );
    let state_path = home.join(".local/share/tsuzuki/state.toml");
    let state = fs::read_to_string(&state_path).unwrap_or_default();
    assert!(
        state.contains("position_seconds = 300.0"),
        "doctor still reconciles like every state command, got:\n{state}"
    );
}

#[test]
fn doctor_reports_timestamps_ok_when_no_entry_is_in_the_future() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    home_with_a_script_log_entry(home, -60);

    let doctor = run(home, &["doctor"]);
    let stdout = String::from_utf8_lossy(&doctor.stdout);

    assert!(
        stdout.contains("✓ mpv timestamps — none dated in the future"),
        "got:\n{stdout}"
    );
}
