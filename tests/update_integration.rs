//! Real end-to-end integration test for `tsuzuki update`, against the
//! compiled binary — same `env!("CARGO_BIN_EXE_tsuzuki")` shape as
//! `tests/doctor_integration.rs` (CLAUDE.md's "Every phase needs at least
//! one real-binary, real-XDG-path integration test").
//!
//! `update`'s only real-network test below never actually finds a newer
//! release against this checked-out source (it's always at or ahead of
//! whatever's published), so it never exercises the actual install spawn
//! — there is no way to sandbox a real `curl -LsSf ... | sh` the way a
//! temp `$HOME` sandboxes MAL's token file, so that path is deliberately
//! left untested by any automated test, ignored or not.

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
fn update_help_describes_the_command() {
    // clap short-circuits --help before any dispatch/network code runs —
    // zero network, confirms the cli.rs wiring end to end.
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let output = run(home, &["update", "--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("newer published GitHub release"),
        "expected update --help to describe the command, got:\n{stdout}"
    );
}

#[test]
#[ignore = "hits the real GitHub API"]
fn update_check_against_real_github() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let output = run(home, &["update"]);
    assert!(
        output.status.success(),
        "expected a successful check against the real GitHub API: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "expected the current version to appear in the output, got:\n{stdout}"
    );
    assert!(
        stdout.contains("up to date")
            || stdout.contains("update available")
            || stdout.contains("ahead of"),
        "expected one of the three known status phrases, got:\n{stdout}"
    );
}
