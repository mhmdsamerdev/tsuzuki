//! Opt-in speed benchmark: everyday commands against a large library and a
//! large mpv-log, timed on the real release binary.
//!
//! Run: `cargo test --release --test perf -- --ignored --nocapture`

use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

/// Upper bound for every command's median.
const BUDGET: Duration = Duration::from_millis(1);

const TIMED_RUNS: usize = 9;

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

/// Two tracked roots (`Anime` and the nested `Anime/[RE]`), 171 series of 24
/// files (one symlinked in from outside), and 5 000 mpv-log entries, 3 500 of
/// them for files outside every root. 66 series end up in progress.
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
    quit_entry(
        home,
        "linked",
        &anime.join("Linked Show/ep03.mkv"),
        300.0,
    );
    for i in 1..=5 {
        let file = rewatches.join(format!("Rewatch {i:02}/ep02.mkv"));
        quit_entry(home, &format!("r{i:02}"), &file, 300.0);
    }
    for n in 0..3_494 {
        let file = home.join(format!("Downloads/clip{n:04}.mkv"));
        quit_entry(home, &format!("u{n:04}"), &file, 300.0);
    }
}

/// Median wall time of `TIMED_RUNS` runs after one warm-up.
fn median_time(home: &Path, args: &[&str], code: i32) -> Duration {
    run_expecting(home, args, code);
    let mut times: Vec<Duration> = (0..TIMED_RUNS)
        .map(|_| {
            let start = Instant::now();
            run_expecting(home, args, code);
            start.elapsed()
        })
        .collect();
    times.sort();
    times[TIMED_RUNS / 2]
}

#[test]
#[ignore = "timing; run in release: cargo test --release --test perf -- --ignored --nocapture"]
fn perf_large_library_every_command_stays_under_budget() {
    assert!(
        !cfg!(debug_assertions),
        "debug timings are meaningless; run with --release"
    );
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();
    large_library(home);

    let mut expected = String::from("Linked Show — ep. 3\n");
    for i in 1..=60 {
        expected.push_str(&format!("Series {i:03} — ep. 10\n"));
    }
    for i in 1..=5 {
        expected.push_str(&format!("Rewatch {i:02} — ep. 2\n"));
    }
    expected.push_str("\nRunning `tsuzuki` bare will prompt among these.\n");
    assert_eq!(stdout_of(&run_expecting(home, &["status"], 0)), expected);

    let commands: [(&[&str], i32); 7] = [
        (&["list"], 0),
        (&["status"], 0),
        // Several in progress and no TTY: lists them and exits non-zero.
        (&[], 1),
        (&["resume", "Series 003"], 0),
        (&["start", "Series 100"], 0),
        (&["pause", "Series 001"], 0),
        (&["drop", "Series 002"], 0),
    ];

    let mut over = Vec::new();
    for (args, code) in commands {
        let median = median_time(home, args, code);
        let label = if args.is_empty() {
            "tsuzuki".to_string()
        } else {
            args.join(" ")
        };
        println!(
            "{label:<20} {:>7.1} ms  (budget {} ms)",
            median.as_secs_f64() * 1000.0,
            BUDGET.as_millis()
        );
        if median > BUDGET {
            over.push(format!("{label}: {median:?}"));
        }
    }
    assert!(over.is_empty(), "over the {BUDGET:?} budget: {over:?}");
}
