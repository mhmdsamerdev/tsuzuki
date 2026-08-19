//! Real end-to-end integration test against the *compiled binary*, driven
//! through a genuine XDG-style `$HOME` — not an in-process function call
//! with an explicit tempdir path handed straight to `state::write`/
//! `config::write`, which is what every one of the 213 unit tests in
//! `src/` does instead.
//!
//! That distinction is the actual gap this closes. Every existing test
//! constructs its own `state_path`/`config_path` via `tempdir().join(...)`
//! and passes it directly as an argument — none of them ever calls
//! `state::state_path()` or `config::config_path()` themselves, so none of
//! them exercise `dirs::data_dir()`/`dirs::config_dir()` real environment-
//! variable-driven resolution, and none of them invoke `main()`/`dispatch()`
//! at all. A bug in that specific wiring (wrong `dirs` function used, a
//! path resolution that silently diverges between config and state, an
//! env var not being read the way it's expected to) could pass all 213
//! unit tests and still be broken for a real user — which is exactly the
//! shape of bug this was written to investigate. Spawns the real compiled
//! binary via `env!("CARGO_BIN_EXE_tsuzuki")` with an explicit `HOME` and
//! both `XDG_*` overrides cleared, matching a normal user's shell.

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

    // The real config.toml path, resolved the same way `dirs::config_dir()`
    // resolves it (~/.config on Linux with no XDG_CONFIG_HOME override) —
    // not a path this test invents itself.
    let config_path = home.join(".config").join("tsuzuki").join("config.toml");
    assert!(
        config_path.exists(),
        "add should have created the real config.toml at {}",
        config_path.display()
    );

    // Bypass real mpv detection entirely so this test doesn't depend on
    // mpv being installed on the machine running it — `true` is a
    // standard, always-present, instantly-exiting program.
    let patched = fs::read_to_string(&config_path)
        .unwrap()
        .replace("command_override = \"\"", "command_override = \"true\"");
    fs::write(&config_path, patched).unwrap();

    // Fabricate a real (>= the default 120s resume_seconds) mpv
    // watch_later bookmark, exactly as mpv itself would leave one behind
    // after being quit mid-episode.
    let watch_later_dir = home.join(".local/state/mpv/watch_later");
    fs::create_dir_all(&watch_later_dir).unwrap();
    fs::write(
        watch_later_dir.join("fake-entry"),
        format!(
            "# {}\nstart=560.250000\n",
            series_dir.join("ep01.mkv").display()
        ),
    )
    .unwrap();

    // The real state.toml path — ~/.local/share on Linux, per
    // `dirs::data_dir()` — computed here independently of the source
    // under test, the same way a real user's shell would resolve it.
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
    assert!(
        state_contents.contains("in_progress"),
        "persisted state.toml contents:\n{state_contents}"
    );
    assert!(
        state_contents.contains("560.25"),
        "persisted state.toml contents:\n{state_contents}"
    );
    assert!(
        state_contents.contains("Monster"),
        "persisted state.toml contents:\n{state_contents}"
    );
}

#[test]
fn list_and_status_read_back_the_same_real_persisted_state() {
    // A second, independent invocation of the real binary, reading back
    // what the first one (via `resume`) actually wrote to disk — proving
    // persistence survives across process invocations, not just within
    // the one process that wrote it.
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let series_dir = home.join("Anime").join("Monster");
    fs::create_dir_all(&series_dir).unwrap();
    fs::File::create(series_dir.join("ep01.mkv")).unwrap();

    run(home, &["add", "~/Anime"]);
    let config_path = home.join(".config").join("tsuzuki").join("config.toml");
    let patched = fs::read_to_string(&config_path)
        .unwrap()
        .replace("command_override = \"\"", "command_override = \"true\"");
    fs::write(&config_path, patched).unwrap();

    let watch_later_dir = home.join(".local/state/mpv/watch_later");
    fs::create_dir_all(&watch_later_dir).unwrap();
    fs::write(
        watch_later_dir.join("fake-entry"),
        format!(
            "# {}\nstart=400.000000\n",
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
