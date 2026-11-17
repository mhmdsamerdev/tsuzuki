//! Opt-in speed benchmark: everyday commands against a large library and a
//! large mpv-log, timed on the real release binary. Warm and cold-cache runs
//! are separate tests with separate budgets, so a regression names which grew.
//!
//! Warm: `cargo test --release --test perf perf_warm -- --ignored --nocapture`
//! Cold: `sudo -v && cargo test --release --test perf perf_cold -- --ignored --nocapture`
//! (the cold test drops the OS caches through `sudo -n`).

use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

/// Upper bound for every warm command's median: about 2.5× the slowest
/// measured (~80 ms on an NVMe laptop).
const WARM_BUDGET: Duration = Duration::from_millis(200);

/// Upper bound for every cold-cache command's median, on the project's disk.
const COLD_BUDGET: Duration = Duration::from_millis(1000);

const WARM_RUNS: usize = 9;
const COLD_RUNS: usize = 3;

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tsuzuki"))
        .args(args)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .output()
        .expect("failed to spawn the tsuzuki binary")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Runs `tsuzuki <args>` and asserts its exit code.
fn run_expecting(home: &Path, args: &[&str], code: i32) -> Output {
    let output = run(home, args);
    assert_eq!(
        output.status.code(),
        Some(code),
        "`tsuzuki {}`\nstdout: {}\nstderr: {}",
        args.join(" "),
        stdout_of(&output),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

fn touch(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::File::create(path).unwrap();
}

/// A series folder of `ep01.mkv` .. `ep24.mkv`.
fn series(dir: &Path) {
    for ep in 1..=24 {
        touch(&dir.join(format!("ep{ep:02}.mkv")));
    }
}

/// Pins `[mpv]` to a custom `true` with no script to check, so `resume`/`start`
/// never spawn a real mpv.
fn bypass_real_mpv(home: &Path) {
    let path = home.join(".config/tsuzuki/config.toml");
    let original = fs::read_to_string(&path).unwrap();
    let patched = original.replace(
        "install = \"auto\"\ncommand = \"\"",
        "install = \"custom\"\ncommand = \"true\"",
    );
    assert_ne!(
        patched, original,
        "config.toml must hold the default [mpv] keys"
    );
    fs::write(&path, patched).unwrap();
}

/// One `tsuzuki.lua` quit entry (no duration) at `position_seconds`.
fn quit_entry(home: &Path, entry_name: &str, source_file: &Path, position_seconds: f64) {
    let dir = home.join(".local/share/tsuzuki/mpv-log");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(entry_name),
        format!(
            "# {}\nduration=\nposition={position_seconds:.6}\nreason=quit\n",
            source_file.display()
        ),
    )
    .unwrap();
}

/// Two tracked roots (`Anime` and the nested `Anime/[RE]`), 173 series of 24
/// files (one symlinked in from outside, one with symlinked season folders, one
/// with symlinked episode files), and 5 000 mpv-log entries, 3 500 of them for
/// files outside every root (the files exist, as in a real library). 68 series end up in progress.
fn large_library(home: &Path) {
    let anime = home.join("Anime");
    let rewatches = anime.join("[RE]");
    for i in 1..=150 {
        series(&anime.join(format!("Series {i:03}")));
    }
    for i in 1..=20 {
        series(&rewatches.join(format!("Rewatch {i:02}")));
    }
    let outside = home.join("Elsewhere/Linked Show");
    series(&outside);
    std::os::unix::fs::symlink(&outside, anime.join("Linked Show")).unwrap();

    // A real series folder holding a symlinked season folder, and one whose
    // episode files are symlinks, both into storage outside every root.
    let storage = home.join("Storage");
    series(&storage.join("Season Links S01"));
    fs::create_dir_all(anime.join("Season Links")).unwrap();
    std::os::unix::fs::symlink(
        storage.join("Season Links S01"),
        anime.join("Season Links/S01"),
    )
    .unwrap();
    series(&storage.join("Episode Links"));
    fs::create_dir_all(anime.join("Episode Links")).unwrap();
    for ep in 1..=24 {
        let name = format!("ep{ep:02}.mkv");
        std::os::unix::fs::symlink(
            storage.join("Episode Links").join(&name),
            anime.join("Episode Links").join(&name),
        )
        .unwrap();
    }

    run_expecting(home, &["add", anime.to_str().unwrap()], 0);
    run_expecting(home, &["add", rewatches.to_str().unwrap()], 0);
    bypass_real_mpv(home);

    // Series 001-060 are watched to ep. 10; 061-150 only have noise readings.
    for i in 1..=150 {
        let position = if i <= 60 { 300.0 } else { 30.0 };
        for ep in 1..=10 {
            let file = anime.join(format!("Series {i:03}/ep{ep:02}.mkv"));
            quit_entry(home, &format!("s{i:03}e{ep:02}"), &file, position);
        }
    }
    quit_entry(home, "linked", &anime.join("Linked Show/ep03.mkv"), 300.0);
    quit_entry(
        home,
        "season",
        &anime.join("Season Links/S01/ep04.mkv"),
        300.0,
    );
    quit_entry(
        home,
        "episode",
        &anime.join("Episode Links/ep05.mkv"),
        300.0,
    );
    for i in 1..=5 {
        let file = rewatches.join(format!("Rewatch {i:02}/ep02.mkv"));
        quit_entry(home, &format!("r{i:02}"), &file, 300.0);
    }
    for n in 0..3_494 {
        let file = home.join(format!("Downloads/clip{n:04}.mkv"));
        touch(&file);
        quit_entry(home, &format!("u{n:04}"), &file, 300.0);
    }
}

/// The measured commands and their exit codes.
const COMMANDS: [(&[&str], i32); 7] = [
    (&["list"], 0),
    (&["status"], 0),
    // Several in progress and no TTY: lists them and exits non-zero.
    (&[], 1),
    (&["resume", "Series 003"], 0),
    (&["start", "Series 100"], 0),
    (&["pause", "Series 001"], 0),
    (&["drop", "Series 002"], 0),
];

fn label(args: &[&str]) -> String {
    if args.is_empty() {
        "tsuzuki".to_string()
    } else {
        args.join(" ")
    }
}

fn median(mut times: Vec<Duration>) -> Duration {
    times.sort();
    times[times.len() / 2]
}

/// Median wall time of `WARM_RUNS` runs after one warm-up.
fn warm_median_time(home: &Path, args: &[&str], code: i32) -> Duration {
    run_expecting(home, args, code);
    median(
        (0..WARM_RUNS)
            .map(|_| {
                let start = Instant::now();
                run_expecting(home, args, code);
                start.elapsed()
            })
            .collect(),
    )
}

/// Drops the page, dentry and inode caches, so the next run starts cold.
fn drop_caches() {
    let status = Command::new("sudo")
        .args(["-n", "sh", "-c", "sync; echo 3 > /proc/sys/vm/drop_caches"])
        .status()
        .expect("failed to spawn sudo");
    assert!(
        status.success(),
        "dropping caches needs passwordless sudo; run `sudo -v` first"
    );
}

/// Median wall time of `COLD_RUNS` runs, each right after a cache drop.
fn cold_median_time(home: &Path, args: &[&str], code: i32) -> Duration {
    median(
        (0..COLD_RUNS)
            .map(|_| {
                drop_caches();
                let start = Instant::now();
                run_expecting(home, args, code);
                start.elapsed()
            })
            .collect(),
    )
}

/// The large library under the project's target dir: a real disk, unlike
/// `/tmp` (tmpfs), so dropped caches make the next run truly cold.
fn large_library_fixture() -> tempfile::TempDir {
    if cfg!(debug_assertions) {
        panic!("debug timings are meaningless; run with --release");
    }
    let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
    fs::create_dir_all(base).unwrap();
    let home_dir = tempfile::tempdir_in(base).unwrap();
    let home = home_dir.path();
    large_library(home);

    let mut expected = String::from(
        "Episode Links — ep. 5 (05:00)\nLinked Show — ep. 3 (05:00)\nSeason Links — ep. 4 (05:00)\n",
    );
    for i in 1..=60 {
        expected.push_str(&format!("Series {i:03} — ep. 10 (05:00)\n"));
    }
    for i in 1..=5 {
        expected.push_str(&format!("Rewatch {i:02} — ep. 2 (05:00)\n"));
    }
    expected.push_str("\nRunning `tsuzuki` bare will prompt among these.\n");
    assert_eq!(stdout_of(&run_expecting(home, &["status"], 0)), expected);
    home_dir
}

/// Prints one `kind` table and asserts every median is within `budget`.
fn assert_within_budget(
    kind: &str,
    home: &Path,
    budget: Duration,
    time: fn(&Path, &[&str], i32) -> Duration,
) {
    let mut over = Vec::new();
    for (args, code) in COMMANDS {
        let median = time(home, args, code);
        let label = label(args);
        println!(
            "{kind:<5} {label:<20} {:>8.1} ms  (budget {} ms)",
            median.as_secs_f64() * 1000.0,
            budget.as_millis()
        );
        if median > budget {
            over.push(format!("{label}: {median:?}"));
        }
    }
    assert!(
        over.is_empty(),
        "{kind} over the {budget:?} budget: {over:?}"
    );
}

#[test]
#[ignore = "timing; run in release: cargo test --release --test perf perf_warm -- --ignored --nocapture"]
fn perf_warm_large_library_every_command_stays_under_budget() {
    let home_dir = large_library_fixture();
    assert_within_budget("warm", home_dir.path(), WARM_BUDGET, warm_median_time);
}

#[test]
#[ignore = "timing, drops OS caches via sudo; run in release: sudo -v && cargo test --release --test perf perf_cold -- --ignored --nocapture"]
fn perf_cold_large_library_every_command_stays_under_budget() {
    let home_dir = large_library_fixture();
    assert_within_budget("cold", home_dir.path(), COLD_BUDGET, cold_median_time);
}
