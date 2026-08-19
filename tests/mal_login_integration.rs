//! Real, manual, end-to-end test of `tsuzuki sync login` against the actual
//! MyAnimeList API. **`#[ignore]`d — never runs in a normal `cargo test`.**
//! Everything else in Phase 8a (PKCE generation, the local callback
//! listener's state-mismatch/timeout/denied handling, token-expiry-triggers-
//! refresh logic, atomic `mal_token.toml` writes, client_id resolution) is
//! covered by real, network-free unit tests in `src/mal.rs`. This one test
//! is the only place in the whole test suite that talks to the real MAL
//! API, and it has genuinely never been run — this codebase's own report
//! says so explicitly. Run it yourself, by hand, before trusting this auth
//! path against production, and especially before trusting the now-committed
//! `mal::DEFAULT_CLIENT_ID` actually works end to end.
//!
//! ## How to run this for real
//!
//! ```sh
//! cargo test --test mal_login_integration -- --ignored --nocapture
//! ```
//!
//! No config setup is required — this exercises the real, zero-config
//! default path (an untouched, freshly-generated `config.toml`, no `[mal]
//! client_id` override), the same path a brand-new install goes through.
//! The test prints the authorize URL and opens your browser. Log in to MAL
//! and approve the app within 2 minutes. It then reads back the resulting
//! `mal_token.toml` and runs `tsuzuki doctor`, checking the `mal-auth` line
//! reports `OK`.
//!
//! To instead test the `[mal] client_id` *override* path (someone running
//! their own fork against their own registered app), set
//! `TSUZUKI_TEST_MAL_CLIENT_ID` to that app's client_id and register
//! `http://127.0.0.1:8734/callback` as its redirect URL first:
//!
//! ```sh
//! TSUZUKI_TEST_MAL_CLIENT_ID=<your own registered client id> \
//!   cargo test --test mal_login_integration -- --ignored --nocapture
//! ```
//!
//! **What this specifically has not yet verified, since it has never been
//! run:** whether MAL's token endpoint truly requires no `client_secret` for
//! this PKCE flow (design.md/CLAUDE.md's assumption, matching Taiga's own
//! long-standing public use of the same pattern, but unconfirmed against a
//! live exchange by this codebase); whether MAL actually accepts the
//! now-committed `mal::DEFAULT_CLIENT_ID` for a real token exchange; and the
//! exact shape of a real token/refresh response body match what
//! `TokenResponse` in `src/mal.rs` expects.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

fn tsuzuki_bin() -> &'static str {
    env!("CARGO_BIN_EXE_tsuzuki")
}

#[test]
#[ignore = "requires manual browser interaction against the live MAL API — see module docs"]
fn real_login_flow_against_the_live_mal_api() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    // An optional override, for testing the [mal] client_id override path
    // specifically. Left unset, this test exercises the real zero-config
    // default (mal::DEFAULT_CLIENT_ID) — no config.toml is written at all,
    // matching exactly what a brand-new install's first run looks like.
    if let Ok(client_id) = std::env::var("TSUZUKI_TEST_MAL_CLIENT_ID") {
        println!("testing the [mal] client_id override path with a custom client_id");
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
    } else {
        println!(
            "testing the real zero-config default path (mal::DEFAULT_CLIENT_ID) — no \
             config.toml pre-seeded"
        );
    }

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
