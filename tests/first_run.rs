//! What a brand-new user sees: zero-state hints and setup guidance, against the real binary.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tsuzuki"))
        .args(args)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .output()
        .expect("failed to spawn the tsuzuki binary")
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Pins `[mpv]` to a custom `true` so `start`/`resume` never look for real mpv.
fn bypass_real_mpv(home: &Path) {
    assert!(run(home, &["config"]).status.success());
    let path = home.join(".config/tsuzuki/config.toml");
    let original = fs::read_to_string(&path).unwrap();
    let patched = original.replace(
        "install = \"auto\"\ncommand = \"\"",
        "install = \"custom\"\ncommand = \"true\"",
    );
    assert_ne!(patched, original);
    fs::write(&path, patched).unwrap();
}

fn make_root(home: &Path, series: &[&str]) -> std::path::PathBuf {
    let root = home.join("Anime");
    fs::create_dir_all(&root).unwrap();
    for s in series {
        fs::create_dir_all(root.join(s)).unwrap();
    }
    root
}

#[test]
fn start_with_no_tracked_roots_hints_at_add() {
    let h = tempfile::tempdir().unwrap();
    bypass_real_mpv(h.path());

    let o = run(h.path(), &["start"]);

    assert!(o.status.success());
    assert_eq!(out(&o), "Nothing to start.\n→ tsuzuki add <path>\n");
}

#[test]
fn start_with_an_empty_tracked_root_does_not_hint_at_add() {
    let h = tempfile::tempdir().unwrap();
    bypass_real_mpv(h.path());
    let root = make_root(h.path(), &[]);
    assert!(
        run(h.path(), &["add", root.to_str().unwrap()])
            .status
            .success()
    );

    let o = run(h.path(), &["start"]);

    assert!(o.status.success());
    assert_eq!(out(&o), "Nothing to start.\n");
}

#[test]
fn status_with_no_tracked_roots_hints_at_add_not_start() {
    let h = tempfile::tempdir().unwrap();

    let o = run(h.path(), &["status"]);

    assert!(o.status.success());
    assert_eq!(out(&o), "Nothing in progress.\n→ tsuzuki add <path>\n");
}

#[test]
fn list_with_no_tracked_roots_says_no_folders() {
    let h = tempfile::tempdir().unwrap();

    let o = run(h.path(), &["list"]);

    assert!(o.status.success());
    assert_eq!(out(&o), "No tracked folders.\n→ tsuzuki add <path>\n");
}

#[test]
fn list_with_an_empty_tracked_root_says_no_series_found() {
    let h = tempfile::tempdir().unwrap();
    let root = make_root(h.path(), &[]);
    assert!(
        run(h.path(), &["add", root.to_str().unwrap()])
            .status
            .success()
    );

    let o = run(h.path(), &["list"]);

    assert!(o.status.success());
    assert_eq!(out(&o), "No series found under your tracked folders.\n");
}

#[test]
fn add_reports_series_count_and_next_step() {
    let h = tempfile::tempdir().unwrap();
    let root = make_root(h.path(), &["Bleach", "Monster"]);
    let r = root.to_str().unwrap();

    let o = run(h.path(), &["add", r]);

    assert!(o.status.success());
    assert_eq!(out(&o), format!("✓ {r} — added (2 series)\n→ tsuzuki\n"));
}

#[test]
fn add_of_a_folder_with_no_subfolders_says_no_series_found() {
    let h = tempfile::tempdir().unwrap();
    let root = make_root(h.path(), &[]);
    let r = root.to_str().unwrap();

    let o = run(h.path(), &["add", r]);

    assert_eq!(
        out(&o),
        format!("✓ {r} — added (no series found)\n→ tsuzuki\n")
    );
}

#[test]
fn add_ani_cli_hints_at_doctor() {
    let h = tempfile::tempdir().unwrap();

    let o = run(h.path(), &["add", "ani-cli"]);

    assert!(o.status.success());
    assert_eq!(out(&o), "✓ ani-cli — added\n→ tsuzuki doctor\n");
}

#[test]
fn pause_unknown_series_points_at_list() {
    let h = tempfile::tempdir().unwrap();

    let o = run(h.path(), &["pause", "Nope"]);

    assert!(!o.status.success());
    assert_eq!(
        err(&o),
        "error: no tracked series named \"Nope\" — run `tsuzuki list` to see tracked series\n"
    );
}

#[test]
fn drop_never_started_series_names_the_series() {
    let h = tempfile::tempdir().unwrap();
    let root = make_root(h.path(), &["Bleach"]);
    assert!(
        run(h.path(), &["add", root.to_str().unwrap()])
            .status
            .success()
    );

    let o = run(h.path(), &["drop", "Bleach"]);

    assert!(!o.status.success());
    assert_eq!(
        err(&o),
        "error: \"Bleach\": no progress to act on — it was never started\n"
    );
}

#[test]
fn bare_exclude_with_no_roots_hints_at_add_without_jargon() {
    let h = tempfile::tempdir().unwrap();

    let o = run(h.path(), &["exclude"]);

    assert!(o.status.success());
    assert_eq!(
        out(&o),
        "No likely specials, OVAs or movies found under any tracked root.\n→ tsuzuki add <path>\n"
    );
}

#[test]
fn doctor_without_a_mal_login_does_not_count_it_as_an_issue() {
    let h = tempfile::tempdir().unwrap();
    bypass_real_mpv(h.path());
    let root = make_root(h.path(), &["Bleach"]);
    assert!(
        run(h.path(), &["add", root.to_str().unwrap()])
            .status
            .success()
    );

    let o = run(h.path(), &["doctor"]);

    let stdout = out(&o);
    assert!(
        stdout.contains("✓ MyAnimeList — not set up (optional)\n  → tsuzuki sync login"),
        "got: {stdout}"
    );
    // Only the custom mpv's unverifiable script remains.
    assert!(stdout.ends_with("1 issue found.\n"), "got: {stdout}");
}

#[test]
fn doctor_fix_without_mpv_says_nothing_could_be_fixed() {
    let h = tempfile::tempdir().unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_tsuzuki"))
        .args(["doctor", "--fix"])
        .env("HOME", h.path())
        .env("PATH", "")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .output()
        .unwrap();

    assert!(
        out(&o).contains("⚠ nothing to fix — mpv not found\n"),
        "got: {}",
        out(&o)
    );
}

#[test]
fn help_lists_the_setup_order() {
    let h = tempfile::tempdir().unwrap();

    let o = run(h.path(), &["--help"]);

    let stdout = out(&o);
    assert!(
        stdout.contains(
            "First run:\n  tsuzuki doctor --fix     install the mpv script\n  tsuzuki add ~/Anime      track the folder that holds your series\n  tsuzuki                  resume where you left off\n"
        ),
        "got: {stdout}"
    );
}
