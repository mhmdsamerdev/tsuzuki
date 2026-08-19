//! Real, manual, end-to-end test of MAL linking + one merge direction
//! against the actual MyAnimeList API. **`#[ignore]`d — never runs in a
//! normal `cargo test`.** Everything else in Phase 8b (title normalization,
//! exact-match linking, similarity ranking, the merge decision table,
//! partial-failure continuation, the non-destructive invariant) is covered
//! by real, network-free unit tests in `src/sync.rs`, all exercised through
//! the injectable `MalHttpClient` trait's fake. This is the only place in
//! the whole test suite that exercises `sync link` + `sync` against the
//! live MAL API, and — same as Phase 8a's own real-API test — it has
//! genuinely never been run. Run it yourself, by hand, before trusting this
//! path against production.
//!
//! Fully isolated: uses a fresh temp `$HOME`, never touches your real
//! `~/.local/share/tsuzuki`. It performs its own real login (same
//! interactive browser flow as `tests/mal_login_integration.rs`) into that
//! temp `$HOME`, so it never depends on — or risks corrupting — any
//! already-authenticated session or real tracked-series state you have.
//!
//! ## How to run this for real
//!
//! 1. Pick a real anime that's genuinely on your own MyAnimeList list, and
//!    note its numeric MAL id (visible in its MAL URL, e.g.
//!    `myanimelist.net/anime/21/One_Piece` → id `21`).
//! 2. Run:
//!
//!    ```sh
//!    TSUZUKI_TEST_MAL_ANIME_ID=<that id> \
//!      cargo test --test mal_sync_integration -- --ignored --nocapture
//!    ```
//!
//! 3. The test prints an authorize URL and opens your browser — log in and
//!    approve, same as the Phase 8a login test. It then tracks a fabricated,
//!    deliberately-unmatchable local series folder (so exact-match
//!    auto-linking can never accidentally succeed), links it to your given
//!    MAL id via `tsuzuki sync link --mal-id`, and runs `tsuzuki sync`.
//! 4. Without `TSUZUKI_TEST_MAL_ANIME_ID` set, this test skips itself with a
//!    clear message rather than failing.
//!
//! **What this specifically has not yet verified, since it has never been
//! run:** the exact real JSON shapes this session only verified against
//! third-party documentation/source (Taiga, the unofficial MAL API spec) —
//! particularly the single-anime endpoint's `my_list_status` presence/
//! absence behavior and the list endpoint's pagination (`paging.next`)
//! shape on a real account; and whether `num_watched_episodes` (the field
//! name used in the real PATCH request body — see `sync.rs`'s
//! `push_episode_count` doc comment) is genuinely correct against a live
//! request, not just cross-referenced documentation.
//!
//! This test cannot assert a specific merge direction ahead of time — it
//! doesn't know your real MAL episode count for the id you give it — so it
//! asserts only that linking and syncing both complete successfully and
//! that *some* consistent result was persisted, printing the actual outcome
//! for you to eyeball.

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

    // A fabricated, deliberately unmatchable series name — exact-match
    // auto-linking must never succeed against it, so the manual `sync
    // link --mal-id` path is what's actually under test here.
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

    // Fabricate real local progress: episode 1, a genuine watch_later-style
    // record, so the subsequent `sync` has something real to compare.
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
