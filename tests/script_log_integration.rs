//! Real-binary tests that a script-log-only `eof` completes the series, using the real log-directory resolution.
//! Includes the Flatpak `io.mpv.Mpv` case, where `XDG_DATA_HOME` is remapped to `~/.var/app/io.mpv.Mpv/data`.

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
fn script_only_eof_entry_with_zero_watch_later_trace_completes_the_last_episode() {
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

    // An existing `in_progress` record, as an earlier watch_later bookmark would have left it.
    let state_path = home.join(".local/share/tsuzuki/state.toml");
    fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    fs::write(
        &state_path,
        r#"[[series]]
root = "~/Anime"
name = "Monster"
status = "in_progress"
current_relative_path = "Monster/ep01.mkv"
position_seconds = 900.0
episode_number = 1
updated_at = 1700000000
"#,
    )
    .unwrap();

    // An `eof` entry for the same file with no watch_later entry, at $XDG_DATA_HOME/tsuzuki/mpv-log.
    let script_log_dir = home.join(".local/share/tsuzuki/mpv-log");
    fs::create_dir_all(&script_log_dir).unwrap();
    fs::write(
        script_log_dir.join("deadbeef"),
        format!(
            "# {}\nduration=1420.000000\nposition=1415.000000\nreason=eof\n",
            series_dir.join("ep01.mkv").display()
        ),
    )
    .unwrap();

    let list = run(home, &["list"]);
    assert!(
        list.status.success(),
        "list failed: stdout={} stderr={}",
        String::from_utf8_lossy(&list.stdout),
        String::from_utf8_lossy(&list.stderr)
    );
    let list_stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_stdout.starts_with("Completed:\n"),
        "expected the script-only eof reading on the series' last episode to complete it, \
         got: {list_stdout}"
    );

    let state_contents = fs::read_to_string(&state_path).unwrap();
    assert!(
        state_contents.contains("status = \"completed\""),
        "persisted state.toml contents:\n{state_contents}"
    );
    // Completion keeps the stored position and path.
    assert!(
        state_contents.contains("position_seconds = 900.0"),
        "the stored position must survive an auto-drop untouched:\n{state_contents}"
    );
}

#[test]
fn an_entry_only_at_the_flatpak_remapped_path_is_still_found() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let series_dir = home.join("Anime").join("Monster");
    fs::create_dir_all(&series_dir).unwrap();
    fs::File::create(series_dir.join("ep01.mkv")).unwrap();
    fs::File::create(series_dir.join("ep02.mkv")).unwrap();

    let add = run(home, &["add", "~/Anime"]);
    assert!(add.status.success());

    // The entry exists only at the Flatpak-remapped path, not at the native one.
    let flatpak_log_dir = home.join(".var/app/io.mpv.Mpv/data/tsuzuki/mpv-log");
    fs::create_dir_all(&flatpak_log_dir).unwrap();
    fs::write(
        flatpak_log_dir.join("deadbeef"),
        format!(
            "# {}\nduration=\nposition=400.000000\nreason=quit\n",
            series_dir.join("ep01.mkv").display()
        ),
    )
    .unwrap();

    assert!(
        !home.join(".local/share/tsuzuki/mpv-log").exists(),
        "sanity check: the native mpv-log directory must not exist for this test to be valid"
    );

    let list = run(home, &["list"]);
    assert!(
        list.status.success(),
        "list failed: stdout={} stderr={}",
        String::from_utf8_lossy(&list.stdout),
        String::from_utf8_lossy(&list.stderr)
    );
    let list_stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_stdout.contains("In progress"),
        "an entry only reachable via the Flatpak-remapped candidate must still be found, \
         got: {list_stdout}"
    );

    let state_path = home.join(".local/share/tsuzuki/state.toml");
    let state_contents = fs::read_to_string(&state_path).unwrap();
    assert!(
        state_contents.contains("400"),
        "persisted state.toml contents:\n{state_contents}"
    );
}
