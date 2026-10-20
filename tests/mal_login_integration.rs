//! Manual `#[ignore]`d test of `tsuzuki sync login` against the live MAL API.
//! Approve the login in the browser within 2 minutes; it then checks `doctor` reports `mal-auth` `OK`.
//!
//! ```sh
//! cargo test --test mal_login_integration -- --ignored --nocapture
//! ```
//!
//! Set `TSUZUKI_TEST_MAL_CLIENT_ID` to test the `[mal] client_id` override instead of the default;
//! its app needs `http://127.0.0.1:8734/callback` as redirect URL.

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

    // Optional override; unset, no config.toml is written and the default client id is used.
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

    // `doctor` confirms the token end to end, not just that a file appeared.
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
