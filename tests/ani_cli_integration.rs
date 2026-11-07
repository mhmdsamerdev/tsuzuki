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

// --- resume: a fake ani-cli records how it was run ---

#[cfg(unix)]
mod resume {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A config that runs the fake ani-cli and a custom "mpv" with no script
    /// to check, so nothing real is spawned.
    fn write_config(home: &Path, fake: &Path, extra_ani: &str, script_path: &str) {
        let dir = home.join(".config/tsuzuki");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("config.toml"),
            format!(
                "[mpv]\ninstall = \"custom\"\ncommand = \"fake-mpv --profile=anime\"\nscript_path = \"{script_path}\"\n\n[ani_cli]\nenabled = true\ncommand = \"{}\"\n{extra_ani}\n",
                fake.display()
            ),
        )
        .unwrap();
    }

    /// Writes a fake ani-cli that dumps argv and the env it was given to
    /// `<home>/ran.txt`, then runs `after` (shell) and exits with `code`.
    fn fake_ani_cli(home: &Path, code: i32, after: &str) -> std::path::PathBuf {
        let fake = home.join("fake-ani-cli");
        let ran = home.join("ran.txt");
        fs::write(
            &fake,
            format!(
                "#!/bin/sh\n{{\nprintf 'ARG %s\\n' \"$@\"\nprintf 'EXACT=%s\\n' \"$ANI_CLI_EXACT\"\nprintf 'MENU=%s\\n' \"$ANI_CLI_MENU\"\nprintf 'PLAYER=%s\\n' \"$ANI_CLI_PLAYER\"\nprintf 'FLAGS=%s\\n' \"$ANI_CLI_PLAYER_FLAGS\"\n}} > {}\n{after}\nexit {code}\n",
                ran.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        fake
    }

    fn ran(home: &Path) -> Option<String> {
        fs::read_to_string(home.join("ran.txt")).ok()
    }

    fn watch(home: &Path, reason: &str, position: f64) {
        write_history(home, "9\tblue-lock-964\tBlue Lock\n");
        write_stream(home, "a1", "Blue Lock Episode 9", position, reason);
    }

    fn resume(home: &Path) -> Output {
        run(home, &["resume"])
    }

    fn resume_with_flags(home: &Path, flags: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tsuzuki"))
            .env("HOME", home)
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .env_remove("XDG_STATE_HOME")
            .env_remove("ANI_CLI_HIST_DIR")
            .env("ANI_CLI_PLAYER_FLAGS", flags)
            .arg("resume")
            .output()
            .unwrap()
    }

    #[test]
    fn start_offers_a_paused_ani_cli_show_with_its_episode() {
        let home_dir = tempfile::tempdir().unwrap();
        let home = home_dir.path();
        let fake = fake_ani_cli(home, 0, "");
        write_config(home, &fake, "", "");
        watch(home, "quit", 312.5);
        assert!(run(home, &["pause", "Blue Lock"]).status.success());

        let output = run(home, &["start"]);

        assert_eq!(
            stdout_of(&output),
            "Available to start:\n\n  1. Blue Lock — Resume (ep. 9, ani-cli)\n\nRun `tsuzuki start <series>` to pick one.\n"
        );
    }

    #[test]
    fn start_never_offers_a_dropped_ani_cli_show() {
        let home_dir = tempfile::tempdir().unwrap();
        let home = home_dir.path();
        let fake = fake_ani_cli(home, 0, "");
        write_config(home, &fake, "", "");
        watch(home, "quit", 312.5);
        assert!(run(home, &["drop", "Blue Lock"]).status.success());

        let output = run(home, &["start"]);

        assert_eq!(stdout_of(&output), "Nothing to start.\n");
    }

    #[test]
    fn resuming_an_unfinished_episode_replays_it_from_the_saved_second() {
        let home_dir = tempfile::tempdir().unwrap();
        let home = home_dir.path();
        let fake = fake_ani_cli(home, 0, "");
        write_config(home, &fake, "args = [\"--dub\"]", "");
        watch(home, "quit", 312.5);

        let output = resume(home);

        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            stdout_of(&output),
            "→ Resuming \"Blue Lock\" — episode 9 (05:13)\n"
        );
        assert_eq!(
            ran(home).unwrap(),
            "ARG --dub\nARG -S\nARG 1\nARG -e\nARG 9\nARG --exit-after-play\nARG Blue Lock\nEXACT=1\nMENU=true\nPLAYER=fake-mpv --profile=anime\nFLAGS=--start=312.5\n"
        );
    }

    #[test]
    fn resume_returns_once_ani_cli_is_past_its_early_failures_and_leaves_it_playing() {
        let home_dir = tempfile::tempdir().unwrap();
        let home = home_dir.path();
        let release = home.join("release");
        let done = home.join("done");
        let fake = fake_ani_cli(
            home,
            0,
            &format!(
                "printf 'hianime.at links fetched\\n'\ni=0\nwhile [ ! -e {} ] && [ $i -lt 200 ]; do sleep 0.05; i=$((i+1)); done\ntouch {}",
                release.display(),
                done.display()
            ),
        );
        write_config(home, &fake, "", "");
        watch(home, "quit", 300.0);

        let output = resume(home);

        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!done.exists(), "tsuzuki waited for ani-cli to finish");
        assert_eq!(
            stdout_of(&output),
            format!(
                "→ Resuming \"Blue Lock\" — episode 9 (05:00)\n  → ani-cli keeps playing; output: {}\n",
                home.join(".local/share/tsuzuki/ani-cli-launch.log")
                    .display()
            )
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("hianime.at links fetched"));
        let state = state_text(home);
        assert_eq!(
            state_field(&state, "status").as_deref(),
            Some("\"in_progress\"")
        );
        assert_eq!(
            state_field(&state, "position_seconds").as_deref(),
            Some("300.0")
        );

        fs::write(&release, "").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !done.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(done.exists(), "ani-cli never finished after release");
    }

    #[test]
    fn resume_gives_ani_clis_menu_an_empty_answer_so_no_second_episode_plays() {
        let home_dir = tempfile::tempdir().unwrap();
        let home = home_dir.path();
        let plays = home.join("plays.txt");
        let fake = fake_ani_cli(
            home,
            0,
            &format!(
                "echo play >> {p}\nanswer=$(${{ANI_CLI_MENU:-echo}} 'Playing episode 9...')\n[ -n \"$answer\" ] && echo play >> {p}",
                p = plays.display()
            ),
        );
        write_config(home, &fake, "", "");
        watch(home, "quit", 300.0);

        let output = resume(home);

        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read_to_string(&plays).unwrap(), "play\n");
    }

    #[test]
    fn resume_writes_ani_clis_output_to_a_launch_log_next_to_the_state() {
        let home_dir = tempfile::tempdir().unwrap();
        let home = home_dir.path();
        let fake = fake_ani_cli(home, 0, "echo hello-from-ani-cli");
        write_config(home, &fake, "", "");
        watch(home, "quit", 300.0);

        assert!(resume(home).status.success());

        assert_eq!(
            fs::read_to_string(home.join(".local/share/tsuzuki/ani-cli-launch.log")).unwrap(),
            format!(
                "# {} -S 1 -e 9 --exit-after-play Blue Lock\nhello-from-ani-cli\n",
                fake.display()
            )
        );
    }

    #[test]
    fn resuming_a_finished_episode_plays_the_next_one_with_no_start_flag() {
        let home_dir = tempfile::tempdir().unwrap();
        let home = home_dir.path();
        let fake = fake_ani_cli(home, 0, "");
        write_config(home, &fake, "", "");
        watch(home, "eof", 1399.0);

        let output = resume(home);

        assert!(output.status.success());
        assert_eq!(
            ran(home).unwrap(),
            "ARG -S\nARG 1\nARG -e\nARG 10\nARG --exit-after-play\nARG Blue Lock\nEXACT=1\nMENU=true\nPLAYER=fake-mpv --profile=anime\nFLAGS=\n"
        );
    }

    #[test]
    fn resuming_keeps_the_users_own_player_flags_after_start() {
        let home_dir = tempfile::tempdir().unwrap();
        let home = home_dir.path();
        let fake = fake_ani_cli(home, 0, "");
        write_config(home, &fake, "", "");
        watch(home, "quit", 300.0);

        assert!(resume_with_flags(home, "--fs").status.success());

        assert!(ran(home).unwrap().ends_with("FLAGS=--start=300 --fs\n"));
    }

    #[test]
    fn resume_refuses_without_running_ani_cli_when_history_is_missing_or_ambiguous() {
        for (history, expected) in [
            (None, "could not read ani-cli history"),
            (
                Some("1\ta-1\tOther\n"),
                "\"Blue Lock\" is no longer in ani-cli's history",
            ),
            (
                Some("9\ta-1\tBlue Lock\n1\tb-2\tBlue Lock\n"),
                "\"Blue Lock\" is in ani-cli's history 2 times",
            ),
        ] {
            let home_dir = tempfile::tempdir().unwrap();
            let home = home_dir.path();
            let fake = fake_ani_cli(home, 0, "");
            write_config(home, &fake, "", "");
            // Record the show while history is still fine, then break history.
            watch(home, "quit", 300.0);
            assert!(run(home, &["status"]).status.success());
            let hist = home.join(".local/state/ani-cli/ani-hsts");
            match history {
                Some(text) => fs::write(&hist, text).unwrap(),
                None => fs::remove_file(&hist).unwrap(),
            }

            let output = resume(home);

            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(expected),
                "{expected}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(ran(home), None, "ani-cli must not run: {expected}");
        }
    }

    #[test]
    fn resume_is_blocked_by_a_missing_mpv_script_before_ani_cli_runs() {
        let home_dir = tempfile::tempdir().unwrap();
        let home = home_dir.path();
        let fake = fake_ani_cli(home, 0, "");
        let missing = home.join("no-such-dir/tsuzuki.lua");
        write_config(home, &fake, "", &missing.display().to_string());
        watch(home, "quit", 300.0);

        let output = resume(home);

        assert!(!output.status.success());
        assert_eq!(ran(home), None);
    }

    #[test]
    fn a_failing_ani_cli_is_reported_as_a_warning_with_a_failure_exit() {
        let home_dir = tempfile::tempdir().unwrap();
        let home = home_dir.path();
        let fake = fake_ani_cli(home, 1, "");
        write_config(home, &fake, "", "");
        watch(home, "eof", 1399.0);

        let output = resume(home);

        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("⚠ ani-cli failed to play episode 10")
        );
        let state = state_text(home);
        assert_eq!(
            state_field(&state, "status").as_deref(),
            Some("\"in_progress\"")
        );
        assert_eq!(state_field(&state, "episode_number").as_deref(), Some("10"));
    }

    #[test]
    fn ani_cli_reporting_an_invalid_episode_after_a_finished_one_completes_the_show() {
        let home_dir = tempfile::tempdir().unwrap();
        let home = home_dir.path();
        let fake = fake_ani_cli(home, 1, "echo 'Invalid episode!' >&2");
        write_config(home, &fake, "", "");
        watch(home, "eof", 1399.0);

        let output = resume(home);

        assert!(output.status.success());
        assert_eq!(
            stdout_of(&output),
            "→ Resuming \"Blue Lock\" — episode 10 (00:00)\n✓ \"Blue Lock\" — ani-cli has no episode 10; marked completed\n"
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("Invalid episode!"));
        let state = state_text(home);
        assert_eq!(
            state_field(&state, "status").as_deref(),
            Some("\"completed\"")
        );
        assert_eq!(state_field(&state, "episode_number").as_deref(), Some("9"));
        assert_eq!(
            stdout_of(&run(home, &["list"])),
            "Completed:\n  Blue Lock (ani-cli)\n"
        );
        assert_eq!(stdout_of(&resume(home)), "Nothing to start.\n");
    }

    #[test]
    fn ani_cli_reporting_an_invalid_episode_mid_episode_leaves_the_show_alone() {
        let home_dir = tempfile::tempdir().unwrap();
        let home = home_dir.path();
        let fake = fake_ani_cli(home, 1, "echo 'Invalid episode!' >&2");
        write_config(home, &fake, "", "");
        watch(home, "quit", 300.0);

        let output = resume(home);

        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("⚠ ani-cli failed to play episode 9")
        );
        let state = state_text(home);
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
    fn a_different_show_taking_the_title_is_warned_about_after_the_run() {
        let home_dir = tempfile::tempdir().unwrap();
        let home = home_dir.path();
        let hist = home.join(".local/state/ani-cli/ani-hsts");
        let fake = fake_ani_cli(
            home,
            0,
            &format!(
                "printf '1\\tblue-lock-2\\tBlue Lock\\n' >> {}",
                hist.display()
            ),
        );
        write_config(home, &fake, "", "");
        watch(home, "quit", 300.0);

        let output = resume(home);

        assert!(output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("⚠ ani-cli may have played a different show titled \"Blue Lock\"")
        );
    }

    #[test]
    fn resume_leaves_ani_cli_history_untouched() {
        let home_dir = tempfile::tempdir().unwrap();
        let home = home_dir.path();
        let fake = fake_ani_cli(home, 0, "");
        write_config(home, &fake, "", "");
        watch(home, "quit", 300.0);
        let hist = home.join(".local/state/ani-cli/ani-hsts");
        let before = fs::read_to_string(&hist).unwrap();

        assert!(resume(home).status.success());

        assert_eq!(fs::read_to_string(&hist).unwrap(), before);
    }
}

#[test]
fn a_saved_mal_total_completes_an_ani_cli_show_once_its_last_episode_is_finished() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    assert!(run(home, &["add", "ani-cli"]).status.success());
    write_history(home, "12\tblue-lock-964\tBlue Lock\n");
    write_stream(home, "a1", "Blue Lock Episode 12", 1399.0, "eof");
    assert!(run(home, &["status"]).status.success());
    assert_eq!(
        state_field(&state_text(home), "episode_number").as_deref(),
        Some("13")
    );

    // What `sync` saves for a linked show.
    let linked = format!(
        "{}\n[[mal_links]]\nroot = \"ani-cli://\"\nname = \"Blue Lock\"\nmal_id = 7\ntotal_episodes = 12\n",
        state_text(home).replace("mal_links = []\n", "")
    );
    fs::write(home.join(".local/share/tsuzuki/state.toml"), linked).unwrap();

    assert_eq!(
        stdout_of(&run(home, &["list"])),
        "Completed:\n  Blue Lock (ani-cli)\n"
    );
    let state = state_text(home);
    assert_eq!(
        state_field(&state, "status").as_deref(),
        Some("\"completed\"")
    );
    assert_eq!(state_field(&state, "episode_number").as_deref(), Some("12"));
}

#[test]
fn pause_and_drop_reject_a_completed_ani_cli_show_and_leave_state_untouched() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    assert!(run(home, &["add", "ani-cli"]).status.success());
    write_history(home, "12\tblue-lock-964\tBlue Lock\n");
    write_stream(home, "a1", "Blue Lock Episode 12", 1399.0, "eof");
    assert!(run(home, &["status"]).status.success());
    let linked = format!(
        "{}\n[[mal_links]]\nroot = \"ani-cli://\"\nname = \"Blue Lock\"\nmal_id = 7\ntotal_episodes = 12\n",
        state_text(home).replace("mal_links = []\n", "")
    );
    fs::write(home.join(".local/share/tsuzuki/state.toml"), linked).unwrap();
    assert_eq!(
        stdout_of(&run(home, &["list"])),
        "Completed:\n  Blue Lock (ani-cli)\n"
    );
    let before = state_text(home);

    for command in ["pause", "drop"] {
        let output = run(home, &[command, "Blue Lock", "--root", "ani-cli"]);
        assert!(!output.status.success(), "{command}");
        assert_eq!(state_text(home), before, "{command}");
    }
}

#[test]
fn pausing_an_already_paused_ani_cli_show_succeeds_and_keeps_it_paused() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    watched_blue_lock(home);
    assert!(run(home, &["pause", "Blue Lock"]).status.success());

    let again = run(home, &["pause", "Blue Lock"]);

    assert!(again.status.success());
    let state = state_text(home);
    assert_eq!(state_field(&state, "status").as_deref(), Some("\"paused\""));
    assert_eq!(
        state_field(&state, "position_seconds").as_deref(),
        Some("300.0")
    );
}

fn config_text(home: &Path) -> String {
    fs::read_to_string(home.join(".config/tsuzuki/config.toml")).unwrap()
}

#[test]
fn sync_exclude_names_an_ani_cli_show_or_the_whole_source() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    watched_blue_lock(home);
    assert!(run(home, &["status"]).status.success());

    let show = run(home, &["sync", "exclude", "Blue Lock"]);
    assert!(show.status.success());
    assert_eq!(stdout_of(&show), "✓ \"Blue Lock\" — excluded from sync\n");
    let source = run(home, &["sync", "exclude", "--root", "ani-cli"]);
    assert!(source.status.success());
    assert_eq!(
        stdout_of(&source),
        "✓ ani-cli (root) — excluded from sync\n"
    );

    let config = config_text(home);
    assert!(
        config.contains("root = \"ani-cli://\"\nseries = \"Blue Lock\""),
        "{config}"
    );
    assert_eq!(config.matches("[[sync_exclusions]]").count(), 2, "{config}");
}

#[test]
fn sync_exclude_of_an_unknown_ani_cli_show_fails_and_leaves_config_untouched() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    watched_blue_lock(home);
    assert!(run(home, &["status"]).status.success());
    let before = config_text(home);

    let output = run(home, &["sync", "exclude", "Nope", "--root", "ani-cli"]);

    assert!(!output.status.success());
    assert_eq!(config_text(home), before);
}

#[test]
fn sync_with_every_ani_cli_show_excluded_has_nothing_to_do_and_needs_no_network() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    watched_blue_lock(home);
    assert!(
        run(home, &["sync", "exclude", "--root", "ani-cli"])
            .status
            .success()
    );

    let output = run(home, &["sync"]);

    assert!(output.status.success());
    assert_eq!(stdout_of(&output), "Nothing eligible to sync.\n");
}

#[cfg(unix)]
mod doctor {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A `bin` dir holding fake `fzf` and `ani-cli`, used as the whole PATH so
    /// the real machine's programs never decide the result.
    fn bin_with(home: &Path, programs: &[&str]) -> std::path::PathBuf {
        let bin = home.join("bin");
        fs::create_dir_all(&bin).unwrap();
        for name in programs {
            let path = bin.join(name);
            fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        bin
    }

    fn doctor(home: &Path, bin: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tsuzuki"))
            .env("HOME", home)
            .env("PATH", bin)
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .env_remove("XDG_STATE_HOME")
            .env_remove("ANI_CLI_HIST_DIR")
            .env_remove("ANI_CLI_MENU")
            .env_remove("ANI_CLI_PLAYER")
            .arg("doctor")
            .args(args)
            .output()
            .unwrap()
    }

    fn doctor_with_menu(home: &Path, bin: &Path, menu: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tsuzuki"))
            .env("HOME", home)
            .env("PATH", bin)
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .env_remove("XDG_STATE_HOME")
            .env_remove("ANI_CLI_HIST_DIR")
            .env("ANI_CLI_MENU", menu)
            .arg("doctor")
            .output()
            .unwrap()
    }

    fn lines_with(output: &Output, needle: &str) -> Vec<String> {
        stdout_of(output)
            .lines()
            .filter(|l| l.contains(needle))
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn doctor_reports_the_ani_cli_rows_only_once_the_source_is_added() {
        let home_dir = tempfile::tempdir().unwrap();
        let home = home_dir.path();
        let bin = bin_with(home, &["ani-cli", "fzf"]);
        write_history(home, "9\tblue-lock-964\tBlue Lock\n");

        assert_eq!(
            lines_with(&doctor(home, &bin, &[]), "ani-cli"),
            Vec::<String>::new()
        );

        assert!(run(home, &["add", "ani-cli"]).status.success());
        let output = doctor(home, &bin, &[]);
        assert_eq!(
            lines_with(&output, "ani-cli"),
            vec![
                "✓ ani-cli — found (ani-cli)",
                "✓ ani-cli menu — fzf found",
                "✓ ani-cli history — 1 show",
            ]
        );
    }

    #[test]
    fn doctor_fails_when_ani_cli_or_its_menu_is_missing() {
        let home_dir = tempfile::tempdir().unwrap();
        let home = home_dir.path();
        let bin = bin_with(home, &[]);
        assert!(run(home, &["add", "ani-cli"]).status.success());
        write_history(home, "9\tblue-lock-964\tBlue Lock\n");

        let output = doctor(home, &bin, &[]);

        assert!(!output.status.success());
        let rows = lines_with(&output, "✗ ani-cli");
        assert_eq!(
            rows,
            vec![
                "✗ ani-cli — `ani-cli` not found",
                "✗ ani-cli menu — `fzf` not found"
            ]
        );
    }

    #[test]
    fn doctor_flags_a_show_that_left_the_history_and_forget_orphans_removes_only_it() {
        let home_dir = tempfile::tempdir().unwrap();
        let home = home_dir.path();
        let bin = bin_with(home, &["ani-cli", "fzf"]);
        assert!(run(home, &["add", "ani-cli"]).status.success());
        write_history(
            home,
            "9\tblue-lock-964\tBlue Lock\n1\tyasuke-3298\tYasuke\n",
        );
        write_stream(home, "a1", "Blue Lock Episode 9", 300.0, "quit");
        write_stream(home, "a2", "Yasuke Episode 1", 400.0, "quit");
        assert!(run(home, &["status"]).status.success());
        write_history(home, "1\tyasuke-3298\tYasuke\n");

        let report = doctor(home, &bin, &[]);
        assert_eq!(
            lines_with(&report, "ani-cli stale records"),
            vec!["⚠ ani-cli stale records — 1 (no longer in ani-cli's history)"]
        );
        assert!(stdout_of(&report).contains("    Blue Lock"));

        let fixed = doctor(home, &bin, &["--fix", "--forget-orphans"]);
        assert!(stdout_of(&fixed).contains("✓ removed 1 stale record\n    Blue Lock\n"));
        // A run that removes records reports that, not a "stale records" row.
        assert_eq!(lines_with(&fixed, "stale records"), Vec::<String>::new());
        let state = state_text(home);
        assert!(!state.contains("Blue Lock"), "{state}");
        assert!(state.contains("Yasuke"), "{state}");
    }

    #[test]
    fn doctor_warns_when_ani_cli_history_is_missing_and_never_calls_records_stale() {
        let home_dir = tempfile::tempdir().unwrap();
        let home = home_dir.path();
        let bin = bin_with(home, &["ani-cli", "fzf"]);
        assert!(run(home, &["add", "ani-cli"]).status.success());
        write_history(home, "9\tblue-lock-964\tBlue Lock\n");
        write_stream(home, "a1", "Blue Lock Episode 9", 300.0, "quit");
        assert!(run(home, &["status"]).status.success());
        fs::remove_file(home.join(".local/state/ani-cli/ani-hsts")).unwrap();

        let output = doctor(home, &bin, &[]);

        assert_eq!(
            lines_with(&output, "stale records").len(),
            1,
            "only the folder row"
        );
        assert!(lines_with(&output, "stale records")[0].starts_with("✓ stale records"));
        assert_eq!(lines_with(&output, "ani-cli history").len(), 1);
        assert!(
            lines_with(&output, "ani-cli history")[0]
                .starts_with("⚠ ani-cli history — not readable at")
        );
    }

    #[test]
    fn doctor_checks_the_menu_named_by_ani_cli_menu_and_falls_back_to_fzf_when_it_is_empty() {
        let home_dir = tempfile::tempdir().unwrap();
        let home = home_dir.path();
        let bin = bin_with(home, &["ani-cli", "rofi"]);
        assert!(run(home, &["add", "ani-cli"]).status.success());
        write_history(home, "9\tblue-lock-964\tBlue Lock\n");

        assert_eq!(
            lines_with(&doctor_with_menu(home, &bin, "rofi"), "ani-cli menu"),
            vec!["✓ ani-cli menu — rofi found"]
        );
        assert_eq!(
            lines_with(&doctor_with_menu(home, &bin, ""), "ani-cli menu"),
            vec!["✗ ani-cli menu — `fzf` not found"]
        );
    }
}
