//! Real-binary tests of `dirs`-based `config.toml`/`state.toml` path resolution, which unit tests bypass with explicit paths.

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
fn resume_persists_state_toml_to_the_real_xdg_data_path_with_episode_and_timestamp_shown() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let series_dir = home.join("Anime").join("Monster");
    fs::create_dir_all(&series_dir).unwrap();
    fs::File::create(series_dir.join("ep01.mkv")).unwrap();

    let add = run(home, &["add", "~/Anime"]);
    assert!(
        add.status.success(),
        "add failed: stdout={} stderr={}",
        String::from_utf8_lossy(&add.stdout),
        String::from_utf8_lossy(&add.stderr)
    );

    // config.toml under ~/.config, as `dirs::config_dir()` resolves it.
    let config_path = home.join(".config").join("tsuzuki").join("config.toml");
    assert!(
        config_path.exists(),
        "add should have created the real config.toml at {}",
        config_path.display()
    );

    // `true` stands in for mpv so the test doesn't need it installed.
    let patched = fs::read_to_string(&config_path).unwrap().replace(
        "install = \"auto\"\ncommand = \"\"",
        "install = \"custom\"\ncommand = \"true\"",
    );
    fs::write(&config_path, patched).unwrap();

    // A quit reading above the default 120s `resume_seconds`, as the mpv script logs after a mid-episode quit.
    let log_dir = home.join(".local/share/tsuzuki/mpv-log");
    fs::create_dir_all(&log_dir).unwrap();
    fs::write(
        log_dir.join("fake-entry"),
        format!(
            "# {}\nduration=\nposition=560.250000\nreason=quit\n",
            series_dir.join("ep01.mkv").display()
        ),
    )
    .unwrap();

    // state.toml under ~/.local/share, as `dirs::data_dir()` resolves it.
    let state_path = home.join(".local/share/tsuzuki/state.toml");
    assert!(
        !state_path.exists(),
        "state.toml must not exist before resume ever runs"
    );

    let resume = run(home, &["resume", "Monster"]);
    assert!(
        resume.status.success(),
        "resume failed: stdout={} stderr={}",
        String::from_utf8_lossy(&resume.stdout),
        String::from_utf8_lossy(&resume.stderr)
    );

    let stdout = String::from_utf8_lossy(&resume.stdout);
    assert!(
        stdout.contains("episode 1"),
        "resume message must show the episode number, got: {stdout}"
    );
    assert!(
        stdout.contains("09:20"),
        "resume message must show the mm:ss timestamp (560.25s = 09:20), got: {stdout}"
    );

    assert!(
        state_path.exists(),
        "state.toml must exist at the real XDG data path ({}) after a resume that picked up \
         genuine real activity — reconcile should have created an in_progress record for it",
        state_path.display()
    );
    let state_contents = fs::read_to_string(&state_path).unwrap();
    let mut state: toml::Table = toml::from_str(&state_contents).unwrap();
    let series = state["series"].as_array_mut().unwrap();
    series[0].as_table_mut().unwrap().remove("updated_at");
    let expected: toml::Table = toml::from_str(
        r#"
        [[series]]
        root = "~/Anime"
        name = "Monster"
        status = "in_progress"
        current_relative_path = "Monster/ep01.mkv"
        position_seconds = 560.25
        episode_number = 1
        "#,
    )
    .unwrap();
    assert_eq!(
        state["series"], expected["series"],
        "state.toml:\n{state_contents}"
    );
}

#[test]
fn list_and_status_read_back_the_same_real_persisted_state() {
    // A second process reads back what `resume` wrote.
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let series_dir = home.join("Anime").join("Monster");
    fs::create_dir_all(&series_dir).unwrap();
    fs::File::create(series_dir.join("ep01.mkv")).unwrap();

    run(home, &["add", "~/Anime"]);
    let config_path = home.join(".config").join("tsuzuki").join("config.toml");
    let patched = fs::read_to_string(&config_path).unwrap().replace(
        "install = \"auto\"\ncommand = \"\"",
        "install = \"custom\"\ncommand = \"true\"",
    );
    fs::write(&config_path, patched).unwrap();

    let log_dir = home.join(".local/share/tsuzuki/mpv-log");
    fs::create_dir_all(&log_dir).unwrap();
    fs::write(
        log_dir.join("fake-entry"),
        format!(
            "# {}\nduration=\nposition=400.000000\nreason=quit\n",
            series_dir.join("ep01.mkv").display()
        ),
    )
    .unwrap();

    let resume = run(home, &["resume", "Monster"]);
    assert!(resume.status.success());

    let status = run(home, &["status"]);
    assert!(status.status.success());
    let status_stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        status_stdout.contains("Monster"),
        "a fresh `status` invocation must see the state the earlier `resume` process persisted, \
         got: {status_stdout}"
    );

    let list = run(home, &["list"]);
    assert!(list.status.success());
    let list_stdout = String::from_utf8_lossy(&list.stdout);
    assert!(list_stdout.contains("In progress"), "got: {list_stdout}");
}

#[test]
fn resume_after_the_saved_episode_moved_warns_and_resets_the_record_to_the_first_episode() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let series_dir = home.join("Anime").join("Ajin");
    fs::create_dir_all(&series_dir).unwrap();
    fs::File::create(series_dir.join("ep01.mkv")).unwrap();
    fs::File::create(series_dir.join("ep02.mkv")).unwrap();
    assert!(run(home, &["add", "~/Anime"]).status.success());

    let config_path = home.join(".config/tsuzuki/config.toml");
    let patched = fs::read_to_string(&config_path).unwrap().replace(
        "install = \"auto\"\ncommand = \"\"",
        "install = \"custom\"\ncommand = \"true\"",
    );
    fs::write(&config_path, patched).unwrap();

    // Build the record through the real command: ep02 at 09:20.
    let log_dir = home.join(".local/share/tsuzuki/mpv-log");
    fs::create_dir_all(&log_dir).unwrap();
    fs::write(
        log_dir.join("fake-entry"),
        format!(
            "# {}\nduration=\nposition=560.250000\nreason=quit\n",
            series_dir.join("ep02.mkv").display()
        ),
    )
    .unwrap();
    assert!(run(home, &["resume", "Ajin"]).status.success());

    // The user reorganises the folder into a season subfolder.
    let season_dir = series_dir.join("S01");
    fs::create_dir_all(&season_dir).unwrap();
    fs::rename(series_dir.join("ep01.mkv"), season_dir.join("ep01.mkv")).unwrap();
    fs::rename(series_dir.join("ep02.mkv"), season_dir.join("ep02.mkv")).unwrap();

    let resume = run(home, &["resume", "Ajin"]);
    assert!(resume.status.success());
    let stdout = String::from_utf8_lossy(&resume.stdout);
    let stderr = String::from_utf8_lossy(&resume.stderr);
    assert!(
        stdout.contains("→ Starting \"Ajin\" from the first episode — episode 1 (00:00)"),
        "stdout: {stdout}"
    );
    assert!(
        stderr.contains("⚠ \"Ajin\": saved episode Ajin/ep02.mkv is gone (moved or renamed?)"),
        "stderr: {stderr}"
    );

    let state_text = fs::read_to_string(home.join(".local/share/tsuzuki/state.toml")).unwrap();
    let mut state: toml::Table = toml::from_str(&state_text).unwrap();
    state["series"].as_array_mut().unwrap()[0]
        .as_table_mut()
        .unwrap()
        .remove("updated_at");
    let expected: toml::Table = toml::from_str(
        r#"
        [[series]]
        root = "~/Anime"
        name = "Ajin"
        status = "in_progress"
        current_relative_path = "Ajin/S01/ep01.mkv"
        position_seconds = 0.0
        episode_number = 1
        "#,
    )
    .unwrap();
    assert_eq!(
        state["series"], expected["series"],
        "state.toml:\n{state_text}"
    );

    let status = run(home, &["status"]);
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("Ajin — ep. 1"),
        "status: {}",
        String::from_utf8_lossy(&status.stdout)
    );
}
