//! Real end-to-end integration test against the *compiled binary*, driven
//! through a genuine XDG-style `$HOME` (per CLAUDE.md's standing "every
//! phase needs at least one real-binary, real-XDG-path integration test"
//! policy) — proving Phase 6's flagship claim: a script-log-only `eof`
//! reading, with zero `watch_later` trace at all, closes the documented
//! "clean binge leaves no trace" blind spot end-to-end, not just at the
//! unit-test level. `reconcile.rs`'s own unit tests exercise the same claim
//! via `reconcile_with_script_log_dir` against a fabricated directory
//! (the script-log directory is deliberately not a `config.toml` field, so
//! it can't be redirected the way `watch_later_dir` can be) — this test is
//! what proves the *real* `$XDG_DATA_HOME/tsuzuki/mpv-log` resolution
//! (`script_log::mpv_log_dir()`, via `dirs::data_dir()`) is wired correctly
//! end to end, spawning the real binary with an explicit `HOME` and both
//! `XDG_*` overrides cleared, matching a normal user's shell — mirroring
//! `tests/state_persistence.rs`'s own established pattern for this same
//! reason.

use std::fs;
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
fn script_only_eof_entry_with_zero_watch_later_trace_auto_drops_the_last_episode() {
    let home_dir = tempfile::tempdir().unwrap();
    let home = home_dir.path();

    let series_dir = home.join("Anime").join("Monster");
    fs::create_dir_all(&series_dir).unwrap();
    fs::File::create(series_dir.join("ep01.mkv")).unwrap();

    let add = run(home, &["add", "~/Anime"]);
    assert!(
        add.status.success(),
        "add failed: stdout={} stderr={}",
        String::from_utf8_lossy(&add.stdout),
        String::from_utf8_lossy(&add.stderr)
    );

    // Seed a real, pre-existing `in_progress` record directly at the real
    // XDG state path — the same shape `resume` itself would have written
    // after an earlier, ordinary real (>= resume_seconds) watch_later
    // bookmark. This stands in for "the user was already partway through
    // this series' one and only episode."
    let state_path = home.join(".local/share/tsuzuki/state.toml");
    fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    fs::write(
        &state_path,
        r#"[[series]]
root = "~/Anime"
name = "Monster"
status = "in_progress"
current_relative_path = "Monster/ep01.mkv"
position_seconds = 900.0
episode_number = 1
updated_at = 1700000000
"#,
    )
    .unwrap();

    // The script-log entry: a direct `eof` completion signal for the same
    // file, with NO corresponding watch_later entry anywhere (mpv's own
    // resume-playback would have deleted any prior bookmark on load,
    // regardless of what happens afterward — CLAUDE.md's documented blind
    // spot). Real, XDG-resolved path: $XDG_DATA_HOME/tsuzuki/mpv-log, i.e.
    // ~/.local/share/tsuzuki/mpv-log with no override set.
    let script_log_dir = home.join(".local/share/tsuzuki/mpv-log");
    fs::create_dir_all(&script_log_dir).unwrap();
    fs::write(
        script_log_dir.join("deadbeef"),
        format!(
            "# {}\nduration=1420.000000\nposition=1415.000000\nreason=eof\n",
            series_dir.join("ep01.mkv").display()
        ),
    )
    .unwrap();

    let list = run(home, &["list"]);
    assert!(
        list.status.success(),
        "list failed: stdout={} stderr={}",
        String::from_utf8_lossy(&list.stdout),
        String::from_utf8_lossy(&list.stderr)
    );
    let list_stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_stdout.contains("dropped"),
        "expected the script-only eof reading on the series' last episode to auto-Drop it, \
         got: {list_stdout}"
    );

    let state_contents = fs::read_to_string(&state_path).unwrap();
    assert!(
        state_contents.contains("\"dropped\"") || state_contents.contains("dropped"),
        "persisted state.toml contents:\n{state_contents}"
    );
    // Per CLAUDE.md's "pause/drop never zero the stored position" rule
    // (extended to auto-drop): the previously-real position/path survive
    // unchanged, not zeroed and not overwritten with the eof reading's own
    // value.
    assert!(
        state_contents.contains("900"),
        "the stored position must survive an auto-drop untouched:\n{state_contents}"
    );
}
