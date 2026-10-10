//! Manual `#[ignore]`d test of `sync link` + `sync` against the live MAL API, in a temp `$HOME` with its own login.
//! It links a fabricated series to your MAL anime id and asserts linking and syncing succeed.
//! Without `TSUZUKI_TEST_MAL_ANIME_ID` it skips itself.
//!
//! ```sh
//! TSUZUKI_TEST_MAL_ANIME_ID=<id from myanimelist.net/anime/<id>/...> \
//!   cargo test --test mal_sync_integration -- --ignored --nocapture
//! ```

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

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
#[ignore = "requires manual browser interaction against the live MAL API and a real MAL anime id you own — see module docs"]
fn real_link_and_one_merge_direction_against_the_live_mal_api() {
    let Ok(mal_id) = std::env::var("TSUZUKI_TEST_MAL_ANIME_ID") else {
        eprintln!(
            "skipping: set TSUZUKI_TEST_MAL_ANIME_ID to a real MAL anime id genuinely on your \
             own list to actually run this test (see module docs at the top of this file)"
        );
        return;
    };

    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    // An unmatchable name, so exact-match auto-linking can't succeed and `sync link --mal-id` is what's tested.
    let series_name = "Definitely Not A Real MAL Title Zzyzx 9000";
    let root = home.join("Anime");
    fs::create_dir_all(root.join(series_name)).unwrap();
    fs::write(
        root.join(series_name).join("ep01.mkv"),
        b"fake video content",
    )
    .unwrap();

    let add = run(home, &["add", &root.to_string_lossy()]);
    assert!(add.status.success(), "add failed: {add:?}");

    println!(
        "Starting `tsuzuki sync login` — a browser window should open. Log in to MyAnimeList \
         and approve the app within 2 minutes."
    );
    let mut login = Command::new(tsuzuki_bin())
        .args(["sync", "login"])
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn tsuzuki sync login");
    let login_status = login.wait().unwrap();
    assert!(
        login_status.success(),
        "real login failed — see output above"
    );

    // Local progress at episode 1, so `sync` has something to compare.
    let state_path = home.join(".local/share/tsuzuki/state.toml");
    fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    fs::write(
        &state_path,
        format!(
            r#"[[series]]
root = "{root}"
name = "{series_name}"
status = "in_progress"
current_relative_path = "{series_name}/ep01.mkv"
position_seconds = 300.0
episode_number = 1
updated_at = 1
"#,
            root = root.display(),
        ),
    )
    .unwrap();

    let link = run(home, &["sync", "link", series_name, "--mal-id", &mal_id]);
    let link_stdout = String::from_utf8_lossy(&link.stdout);
    let link_stderr = String::from_utf8_lossy(&link.stderr);
    assert!(
        link.status.success(),
        "sync link failed — is {mal_id} genuinely on your MAL list? stdout:\n{link_stdout}\nstderr:\n{link_stderr}"
    );
    println!("sync link output:\n{link_stdout}");

    let sync = run(home, &["sync"]);
    let sync_stdout = String::from_utf8_lossy(&sync.stdout);
    println!("sync output:\n{sync_stdout}");
    assert!(
        sync.status.success(),
        "sync failed — stdout:\n{sync_stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&sync.stderr)
    );
    assert!(
        sync_stdout.contains(series_name),
        "expected the series to appear in sync output, got:\n{sync_stdout}"
    );

    let final_state = fs::read_to_string(&state_path).unwrap();
    assert!(
        final_state.contains(&mal_id),
        "expected the linked mal_id to persist in state.toml, got:\n{final_state}"
    );

    println!(
        "Real link + sync succeeded end to end. Inspect the printed sync output above to confirm \
         the merge direction (push/pull/no-op) matches what you expect for this anime."
    );
}
