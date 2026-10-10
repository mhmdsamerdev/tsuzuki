//! Real-binary tests for `tsuzuki update`; the install spawn (`curl ... | sh`) is untested.

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
    // clap short-circuits --help before any dispatch or network code runs.
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
