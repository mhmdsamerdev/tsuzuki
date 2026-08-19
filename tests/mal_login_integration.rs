//! Real, manual, end-to-end test of `tsuzuki sync login` against the actual
//! MyAnimeList API. **`#[ignore]`d — never runs in a normal `cargo test`.**
//! Everything else in Phase 8a (PKCE generation, the local callback
//! listener's state-mismatch/timeout/denied handling, token-expiry-triggers-
//! refresh logic, atomic `mal_token.toml` writes) is covered by real,
//! network-free unit tests in `src/mal.rs`. This one test is the only place
//! in the whole test suite that talks to the real MAL API, and it has
//! genuinely never been run — Phase 8a's own report says so explicitly. Run
//! it yourself, by hand, before trusting this auth path against production.
//!
//! ## How to run this for real
//!
//! 1. Register an app at <https://myanimelist.net/apppages> (or your
//!    existing dev app). App type: "other". Note its **Client ID** — do
//!    **not** commit it anywhere in this repo.
//! 2. In that app's settings, add an **App Redirect URL** of exactly
//!    `http://127.0.0.1:8734/callback` (or whatever port you'll pass below —
//!    it must match byte-for-byte, including the port).
//! 3. Run:
//!
//!    ```sh
//!    TSUZUKI_TEST_MAL_CLIENT_ID=<your real client id> \
//!      cargo test --test mal_login_integration -- --ignored --nocapture
//!    ```
//!
//! 4. The test prints the authorize URL and opens your browser. Log in to
//!    MAL and approve the app. The test polls for up to 2 minutes for the
//!    resulting `mal_token.toml` to appear, then runs `tsuzuki doctor` and
//!    checks the `mal-auth` line reports `OK`.
//! 5. Without `TSUZUKI_TEST_MAL_CLIENT_ID` set, this test skips itself with
//!    a clear message rather than failing — so accidentally running
//!    `-- --ignored` without a real client_id doesn't produce a confusing
//!    failure.
//!
//! **What this specifically has not yet verified, since it has never been
//! run:** whether MAL's token endpoint truly requires no `client_secret` for
//! this PKCE flow (design.md/CLAUDE.md's assumption, per MAL's own public
//! OAuth docs, but unconfirmed against a live exchange by this codebase);
//! and the exact shape of a real token/refresh response body match what
//! `TokenResponse` in `src/mal.rs` expects.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

fn tsuzuki_bin() -> &'static str {
    env!("CARGO_BIN_EXE_tsuzuki")
}

#[test]
#[ignore = "requires a real, registered MAL client_id and manual browser interaction — see module docs"]
fn real_login_flow_against_the_live_mal_api() {
    let Ok(client_id) = std::env::var("TSUZUKI_TEST_MAL_CLIENT_ID") else {
        eprintln!(
            "skipping: set TSUZUKI_TEST_MAL_CLIENT_ID to a real, registered MAL client_id to \
             actually run this test (see module docs at the top of this file)"
        );
        return;
    };

    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    // Seed config.toml with the real client_id before `sync login` runs —
    // mirrors how a real user would set it via `tsuzuki config set` or by
    // hand-editing the file.
    let config_dir = home.join(".config").join("tsuzuki");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        format!(
            r#"[thresholds]
resume_seconds = 120
completion_remaining_seconds = 180
auto_pause_days = 0

[roots]
paths = []
video_extensions = ["mkv", "mp4", "avi", "webm", "mov"]

[mpv]
watch_later_dir = ""
command_override = ""

[mal]
client_id = "{client_id}"
redirect_port = 8734
"#
        ),
    )
    .unwrap();

    println!(
        "Starting `tsuzuki sync login` — a browser window should open. Log in to MyAnimeList \
         and approve the app within 2 minutes."
    );

    let mut child = Command::new(tsuzuki_bin())
        .args(["sync", "login"])
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("failed to spawn tsuzuki sync login");

    let status = child.wait().expect("failed to wait on tsuzuki sync login");
    assert!(
        status.success(),
        "`tsuzuki sync login` exited non-zero — see output above for the real error"
    );

    let token_path = home
        .join(".local")
        .join("share")
        .join("tsuzuki")
        .join("mal_token.toml");
    assert!(
        token_path.exists(),
        "expected mal_token.toml to exist at {} after a successful login",
        token_path.display()
    );

    let contents = fs::read_to_string(&token_path).unwrap();
    assert!(contents.contains("access_token"));
    assert!(contents.contains("refresh_token"));
    assert!(contents.contains("expires_at"));

    // Real doctor run against the freshly-written token — confirms the
    // whole path end to end, not just that a file happened to appear.
    let doctor = run(home, &["doctor"]);
    let stdout = String::from_utf8_lossy(&doctor.stdout);
    assert!(
        stdout.contains("mal-auth") && stdout.contains("OK") && stdout.contains("token valid"),
        "expected doctor to report a valid mal-auth token after a real login, got:\n{stdout}"
    );

    println!("Real login flow succeeded end to end.");
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
