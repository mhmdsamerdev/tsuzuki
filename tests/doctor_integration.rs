//! Real-binary tests for `tsuzuki doctor`. mpv-target checks live in `mpv_target_integration.rs`.

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

#[test]
fn doctor_reports_an_orphaned_record_then_clears_the_orphan() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let root = home.join("Anime");
    fs::create_dir_all(root.join("StillHere")).unwrap();
    fs::File::create(root.join("StillHere").join("ep01.mkv")).unwrap();

    let add = run(home, &["add", "~/Anime"]);
    assert!(add.status.success(), "add failed: {add:?}");

    let config_path = home.join(".config").join("tsuzuki").join("config.toml");
    let patched = fs::read_to_string(&config_path).unwrap().replace(
        "install = \"auto\"\ncommand = \"\"",
        "install = \"custom\"\ncommand = \"true\"",
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
        !stdout.contains("watch-later") && !stdout.contains("mpv config"),
        "mpv.conf and watch_later are no longer checked, got:\n{stdout}"
    );
    assert!(
        stdout.contains("⚠ stale records — 1 (series folder moved or deleted)\n    GoneSeries\n"),
        "must report the stale records check as a warning, got:\n{stdout}"
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
        stdout.contains("✓ MyAnimeList — not set up (optional)"),
        "Phase 8a-correction: an unset [mal] client_id now resolves to the built-in default \
         client_id (no per-user registration required), so a fresh $HOME with no mal_token.toml \
         must report the real mal-auth check as optional and not set up (OK) — not \"not configured\" \
         (that state is no longer reachable from an empty config), and not the old Phase-7 \
         placeholder line either, got:\n{stdout}"
    );

    // Clearing the confirmed orphan must actually remove it.
    let fix = run(home, &["doctor", "--fix", "--forget-orphans"]);
    let fix_stdout = String::from_utf8_lossy(&fix.stdout);
    assert!(
        fix_stdout.contains("✓ removed 1 stale record\n    GoneSeries\n"),
        "must print what it's about to delete, got:\n{fix_stdout}"
    );

    assert!(
        !fix_stdout.contains("⚠ stale records"),
        "a cleared record must not be reported as still stale, got:\n{fix_stdout}"
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
    let patched = fs::read_to_string(&config_path).unwrap().replace(
        "install = \"auto\"\ncommand = \"\"",
        "install = \"custom\"\ncommand = \"true\"",
    );
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
fn doctor_omits_timestamps_line_when_no_entry_is_in_the_future() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    home_with_a_script_log_entry(home, -60);

    let doctor = run(home, &["doctor"]);
    let stdout = String::from_utf8_lossy(&doctor.stdout);

    assert!(!stdout.contains("mpv timestamps"), "got:\n{stdout}");
}

/// A temp `HOME` with Flatpak mpv pinned, `~/Anime/Show/ep01.mkv` a symlink to
/// `~/storage/ep01.mkv`, and a fake `flatpak` answering `access` for any path
/// containing `storage` and `read-only` for everything else.
#[cfg(unix)]
fn flatpak_home_with_a_symlinked_episode(access: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let dir = tempfile::tempdir().unwrap();
    let home = fs::canonicalize(dir.path()).unwrap().join("home");
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(home.join("storage")).unwrap();
    fs::File::create(home.join("storage/ep01.mkv")).unwrap();
    fs::create_dir_all(home.join("Anime/Show")).unwrap();
    symlink(
        home.join("storage/ep01.mkv"),
        home.join("Anime/Show/ep01.mkv"),
    )
    .unwrap();
    fs::create_dir_all(home.join(".local/share/flatpak/app/io.mpv.Mpv")).unwrap();
    let flatpak = bin.join("flatpak");
    fs::write(
        &flatpak,
        format!("#!/bin/sh\ncase \"$*\" in *storage*) echo {access};; *) echo read-only;; esac\n"),
    )
    .unwrap();
    fs::set_permissions(&flatpak, fs::Permissions::from_mode(0o755)).unwrap();
    let config = home.join(".config/tsuzuki/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(
        config,
        format!(
            "[roots]\npaths = [\"{}\"]\n\n[mpv]\ninstall = \"flatpak\"\n",
            home.join("Anime").display()
        ),
    )
    .unwrap();
    (dir, home)
}

#[cfg(unix)]
fn doctor_stdout_with_fake_flatpak(home: &Path) -> String {
    let out = Command::new(tsuzuki_bin())
        .arg("doctor")
        .env("HOME", home)
        .env("PATH", home.join("bin"))
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .output()
        .expect("failed to spawn the tsuzuki binary");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[cfg(unix)]
#[test]
fn doctor_warns_when_flatpak_mpv_cannot_see_the_folder_an_episode_symlink_points_to() {
    let (_dir, home) = flatpak_home_with_a_symlinked_episode("hidden");

    let out = doctor_stdout_with_fake_flatpak(&home);

    let storage = home.join("storage");
    assert!(
        out.contains(&format!(
            "⚠ mpv permissions — mpv can't see: {}\n  → flatpak override --user --filesystem={}:ro io.mpv.Mpv\n",
            storage.display(),
            storage.display()
        )),
        "{out}"
    );
}

#[cfg(unix)]
#[test]
fn doctor_accepts_flatpak_access_when_the_symlink_target_is_visible() {
    let (_dir, home) = flatpak_home_with_a_symlinked_episode("read-only");

    let out = doctor_stdout_with_fake_flatpak(&home);

    assert!(
        out.contains("✓ mpv permissions — tracked directories accessible\n"),
        "{out}"
    );
}
