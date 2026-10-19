//! `tsuzuki doctor [--fix] [--forget-orphans]`: environment health checks.
//!
//! Every check is a pure function over already-resolved inputs (booleans,
//! paths, file contents); the real detection (`dirs::config_dir()`, the
//! `flatpak` subprocess, MAL token refresh) happens in the caller, so checks
//! are testable against a fabricated environment.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::config::Config;
use crate::roots;
use crate::series;
use crate::state::State;

/// The mpv script embedded at compile time, so `--fix` can install it
/// from a distributed binary and detect drift against it.
pub const BUNDLED_SCRIPT: &str = include_str!("../contrib/mpv/tsuzuki.lua");

/// A check's severity. `Warn` is informational; `Fail` is actionable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

/// One report line: a short label and result, with an optional `action`
/// (the single next step that resolves the check). No implementation detail
/// such as paths or counts.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckResult {
    pub label: String,
    pub level: Level,
    pub message: String,
    pub action: Option<String>,
}

impl CheckResult {
    pub fn ok(label: impl Into<String>, message: impl Into<String>) -> Self {
        CheckResult {
            label: label.into(),
            level: Level::Ok,
            message: message.into(),
            action: None,
        }
    }
    pub fn warn(label: impl Into<String>, message: impl Into<String>) -> Self {
        CheckResult {
            label: label.into(),
            level: Level::Warn,
            message: message.into(),
            action: None,
        }
    }
    pub fn fail(label: impl Into<String>, message: impl Into<String>) -> Self {
        CheckResult {
            label: label.into(),
            level: Level::Fail,
            message: message.into(),
            action: None,
        }
    }

    /// Attaches the one next step that resolves this check.
    pub fn with_action(mut self, action: impl Into<String>) -> Self {
        self.action = Some(action.into());
        self
    }

    /// `✓ label — message`, plus an indented `→ action` line when set.
    pub fn render(&self) -> String {
        let symbol = match self.level {
            Level::Ok => "✓",
            Level::Warn => "⚠",
            Level::Fail => "✗",
        };
        match &self.action {
            Some(action) => format!("{symbol} {} — {}\n  → {action}", self.label, self.message),
            None => format!("{symbol} {} — {}", self.label, self.message),
        }
    }
}

/// Folds same-category checks into one row. The worst level wins
/// (`Fail` > `Warn` > `Ok`) using the first offender's message and action; an
/// all-`Ok` group uses `ok_message`.
pub fn combine(label: impl Into<String>, ok_message: &str, results: &[CheckResult]) -> CheckResult {
    let label = label.into();
    if let Some(worst) = results.iter().find(|r| r.level == Level::Fail) {
        return CheckResult {
            label,
            level: Level::Fail,
            message: worst.message.clone(),
            action: worst.action.clone(),
        };
    }
    if let Some(worst) = results.iter().find(|r| r.level == Level::Warn) {
        return CheckResult {
            label,
            level: Level::Warn,
            message: worst.message.clone(),
            action: worst.action.clone(),
        };
    }
    CheckResult::ok(label, ok_message)
}

// --- 1a. mpv availability ---

/// Which mpv installs were detected, computed once by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MpvPresence {
    pub native: bool,
    pub flatpak: bool,
}

/// Reports mpv availability. A `command_override` bypasses detection but must
/// resolve (`override_resolves`). Native and Flatpak both present warns
/// (native wins); neither present fails.
pub fn check_mpv_availability(
    command_override: &str,
    presence: MpvPresence,
    override_resolves: bool,
) -> CheckResult {
    if !command_override.is_empty() {
        return if override_resolves {
            CheckResult::ok("mpv", "custom command found")
        } else {
            CheckResult::fail("mpv", "configured command not found")
                .with_action("check [mpv] command_override in config.toml")
        };
    }

    match (presence.native, presence.flatpak) {
        (false, false) => CheckResult::fail("mpv", "not found")
            .with_action("install mpv, or set [mpv] command_override in config.toml"),
        (true, false) => CheckResult::ok("mpv", "native installation found"),
        (false, true) => CheckResult::ok("mpv", "Flatpak installation found"),
        (true, true) => CheckResult::warn("mpv", "both native and Flatpak found, using native")
            .with_action("pin [mpv] command_override in config.toml to avoid ambiguity"),
    }
}

/// Whether the override's first whitespace-separated token is an executable
/// path (if it contains `/`) or a program on `$PATH`.
pub fn command_override_resolves(command_override: &str) -> bool {
    let Some(program) = command_override.split_whitespace().next() else {
        return false;
    };
    if program.contains('/') {
        crate::launch::is_executable_file(Path::new(program))
    } else {
        crate::launch::program_on_path(program)
    }
}

// --- 1b. mpv installs ---

/// One mpv install's config directory (holds `mpv.conf` and `scripts/`).
/// Flatpak's lives under `home`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MpvInstall {
    pub label: &'static str,
    pub config_dir: PathBuf,
}

/// The installs detected per `presence`; undetected ones have nothing to check.
pub fn detected_mpv_installs(presence: MpvPresence, home: Option<&Path>) -> Vec<MpvInstall> {
    let mut installs = Vec::new();
    if presence.native
        && let Some(base) = dirs::config_dir()
    {
        installs.push(MpvInstall {
            label: "native",
            config_dir: base.join("mpv"),
        });
    }
    if presence.flatpak
        && let Some(home) = home
    {
        installs.push(MpvInstall {
            label: "flatpak",
            config_dir: home.join(".var/app/io.mpv.Mpv/config/mpv"),
        });
    }
    installs
}

// --- 1c. entry timestamps ---

/// Warns when any tracked-root script-log entry (`mtimes`) is dated after `now`.
/// Report only: reconcile treats such an entry like any other, so a clock that
/// ran ahead can make a `drop` look undone until the clock catches up.
pub fn check_entry_timestamps(mtimes: &[SystemTime], now: SystemTime) -> CheckResult {
    let future = mtimes.iter().filter(|&&mtime| mtime > now).count();
    if future == 0 {
        return CheckResult::ok("mpv timestamps", "none dated in the future");
    }
    let noun = if future == 1 { "entry" } else { "entries" };
    CheckResult::warn(
        "mpv timestamps",
        format!("{future} {noun} dated in the future, which can reactivate dropped series"),
    )
    .with_action("check the system clock")
}

// --- 1d. Flatpak filesystem-permission gap ---

/// Best-effort check that Flatpak mpv can reach the tracked roots, from a
/// substring scan of `flatpak info --show-permissions`. Never fails: sandbox
/// permissions are the user's call and the scan is not authoritative.
pub fn check_flatpak_permissions(
    flatpak_present: bool,
    flatpak_cli_present: bool,
    permissions_output: Option<&str>,
) -> CheckResult {
    const VERIFY_ACTION: &str = "flatpak info --show-permissions io.mpv.Mpv";

    if !flatpak_present {
        return CheckResult::warn("mpv permissions", "skipped — Flatpak mpv not detected");
    }
    if !flatpak_cli_present {
        return CheckResult::warn(
            "mpv permissions",
            "could not verify (flatpak CLI not found)",
        );
    }
    let Some(output) = permissions_output else {
        return CheckResult::warn("mpv permissions", "could not verify").with_action(VERIFY_ACTION);
    };
    let broad_access = [
        "filesystem=host",
        "filesystems=host",
        "filesystem=home",
        "filesystems=home",
    ]
    .iter()
    .any(|needle| output.contains(needle));
    if broad_access {
        CheckResult::ok("mpv permissions", "tracked directories accessible")
    } else {
        CheckResult::warn(
            "mpv permissions",
            "tracked directories may not be accessible",
        )
        .with_action(VERIFY_ACTION)
    }
}

// --- 1e. tsuzuki.lua mpv script (required) ---

pub struct ScriptCheckOutcome {
    pub result: CheckResult,
    /// The scripts directory `--fix` should install into, if any.
    pub needs_fix: Option<PathBuf>,
}

/// Compares one install's `scripts/tsuzuki.lua` byte-for-byte with the
/// bundled copy.
pub fn check_tsuzuki_lua(scripts_dir: &Path, label: &str) -> ScriptCheckOutcome {
    let script_path = scripts_dir.join("tsuzuki.lua");
    let full_label = format!("mpv script ({label})");

    match fs::read(&script_path) {
        Ok(bytes) if bytes == BUNDLED_SCRIPT.as_bytes() => ScriptCheckOutcome {
            result: CheckResult::ok(full_label, "up to date"),
            needs_fix: None,
        },
        Ok(_) => ScriptCheckOutcome {
            result: CheckResult::fail(full_label, format!("out of date ({label})"))
                .with_action("tsuzuki doctor --fix"),
            needs_fix: Some(scripts_dir.to_path_buf()),
        },
        Err(_) => ScriptCheckOutcome {
            result: CheckResult::fail(full_label, format!("not installed ({label})"))
                .with_action("tsuzuki doctor --fix"),
            needs_fix: Some(scripts_dir.to_path_buf()),
        },
    }
}

/// Installs or updates `tsuzuki.lua` atomically (temp file, then rename).
pub fn install_script(scripts_dir: &Path) -> io::Result<()> {
    fs::create_dir_all(scripts_dir)?;
    let target = scripts_dir.join("tsuzuki.lua");
    let mut temp = tempfile::NamedTempFile::new_in(scripts_dir)?;
    io::Write::write_all(&mut temp, BUNDLED_SCRIPT.as_bytes())?;
    temp.persist(&target).map_err(|e| e.error)?;
    Ok(())
}

// --- 1f. mpv-log directory reachability ---

/// Checks every candidate script-log directory. A missing one is `Ok` (mpv
/// hasn't logged yet); an existing but unreadable one fails.
pub fn check_mpv_log_dirs(dirs: &[PathBuf]) -> Vec<CheckResult> {
    dirs.iter()
        .map(|d| {
            if !d.exists() {
                CheckResult::ok("mpv script", "activity log not created yet")
            } else {
                match fs::read_dir(d) {
                    Ok(_) => CheckResult::ok("mpv script", "activity log reachable"),
                    Err(_) => CheckResult::fail("mpv script", "activity log unreadable")
                        .with_action(format!("check permissions on {}", d.display())),
                }
            }
        })
        .collect()
}

// --- 1g. tracked-root validity ---

/// One tracked root's reachability. Orphan detection (1h) must skip invalid
/// roots rather than read their absence as missing series.
pub struct RootCheck {
    pub root: String,
    pub valid: bool,
}

pub fn check_tracked_roots(config: &Config, home: Option<&Path>) -> Vec<RootCheck> {
    config
        .roots
        .paths
        .iter()
        .map(|root| {
            let expanded = roots::expand_tilde(Path::new(root), home);
            RootCheck {
                root: root.clone(),
                valid: expanded.is_dir(),
            }
        })
        .collect()
}

/// Folds root reachability into one "library" row, naming every unreachable
/// root. No tracked roots is `Ok`.
pub fn summarize_tracked_roots(checks: &[RootCheck]) -> CheckResult {
    if checks.is_empty() {
        return CheckResult::ok("library", "no tracked roots yet")
            .with_action("tsuzuki add <path>");
    }
    let unreachable: Vec<&str> = checks
        .iter()
        .filter(|c| !c.valid)
        .map(|c| c.root.as_str())
        .collect();
    if unreachable.is_empty() {
        let n = checks.len();
        let noun = if n == 1 { "root" } else { "roots" };
        CheckResult::ok("library", format!("{n} tracked {noun} reachable"))
    } else {
        CheckResult::fail(
            "library",
            format!("not reachable: {}", unreachable.join(", ")),
        )
    }
}

// --- 1h. orphaned per-series state.toml records ---

#[derive(Debug, Clone, PartialEq)]
pub struct Orphan {
    pub root: String,
    pub name: String,
}

/// The command that clears confirmed orphans, shared by the report and hints.
pub const FORGET_ORPHANS_COMMAND: &str = "tsuzuki doctor --fix --forget-orphans";

/// Finds `state.toml` records whose series folder is gone from its root,
/// only under roots in `valid_roots`. An unreachable root (e.g. unplugged
/// drive) is skipped, never read as all its series being gone.
pub fn find_orphans(
    config: &Config,
    state: &State,
    valid_roots: &[String],
    home: Option<&Path>,
) -> Vec<Orphan> {
    let tracked_root_real_paths: Vec<_> = config
        .roots
        .paths
        .iter()
        .map(|r| roots::normalize(Path::new(r), home))
        .collect();

    let mut orphans = Vec::new();
    for root in valid_roots {
        let expanded = roots::expand_tilde(Path::new(root), home);
        let Ok(live) = series::series_in_root(&expanded, &tracked_root_real_paths) else {
            continue;
        };
        let live_names: HashSet<String> = live.into_iter().map(|s| s.name).collect();

        for record in &state.series {
            if &record.root == root && !live_names.contains(&record.name) {
                orphans.push(Orphan {
                    root: record.root.clone(),
                    name: record.name.clone(),
                });
            }
        }
    }
    orphans
}

pub fn orphan_check_result(orphans: &[Orphan]) -> CheckResult {
    if orphans.is_empty() {
        return CheckResult::ok("orphaned records", "none found");
    }

    let names: Vec<&str> = orphans.iter().map(|o| o.name.as_str()).collect();
    CheckResult::fail(
        "orphaned records",
        format!("{} found: {}", orphans.len(), names.join(", ")),
    )
    .with_action(FORGET_ORPHANS_COMMAND)
}

/// Removes every record in `orphans` from `state.series`. The caller prints
/// what is deleted first and persists `state` afterward.
pub fn forget_orphans(state: &mut State, orphans: &[Orphan]) {
    state
        .series
        .retain(|r| !orphans.iter().any(|o| o.root == r.root && o.name == r.name));
}

// --- 1i. MAL auth status ---

/// Maps a resolved [`crate::mal::TokenStatus`] to a `CheckResult`. No
/// `--fix`: `sync login` opens a browser, so the user runs it.
///
/// - Not configured or not logged in warns: MAL sync is optional.
/// - A rejected refresh fails: the stored token is dead.
/// - A network error warns: offline says nothing about the token.
pub fn check_mal_auth(status: &crate::mal::TokenStatus) -> CheckResult {
    use crate::mal::TokenStatus;
    match status {
        TokenStatus::NotConfigured => CheckResult::warn("MyAnimeList", "not configured")
            .with_action("set [mal] client_id in config.toml, then run `tsuzuki sync login`"),
        TokenStatus::NotLoggedIn => {
            CheckResult::warn("MyAnimeList", "not logged in").with_action("tsuzuki sync login")
        }
        TokenStatus::Valid { .. } => CheckResult::ok("MyAnimeList", "signed in"),
        TokenStatus::Refreshed { .. } => CheckResult::ok("MyAnimeList", "signed in"),
        TokenStatus::RefreshRejected(_) => {
            CheckResult::fail("MyAnimeList", "sign-in expired").with_action("tsuzuki sync login")
        }
        TokenStatus::NetworkError(_) => {
            CheckResult::warn("MyAnimeList", "could not verify sign-in (network error)")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{SeriesRecord, Status};
    use std::fs as std_fs;
    use std::time::Duration;
    use tempfile::tempdir;

    // --- CheckResult::render, combine ---

    #[test]
    fn render_uses_symbols_and_indents_the_action_line() {
        let ok = CheckResult::ok("mpv", "native installation found");
        assert_eq!(ok.render(), "✓ mpv — native installation found");

        let warn = CheckResult::warn(
            "mpv permissions",
            "tracked directories may not be accessible",
        )
        .with_action("flatpak info --show-permissions io.mpv.Mpv");
        assert_eq!(
            warn.render(),
            "⚠ mpv permissions — tracked directories may not be accessible\n  → flatpak info --show-permissions io.mpv.Mpv"
        );

        let fail = CheckResult::fail("mpv", "not found");
        assert_eq!(fail.render(), "✗ mpv — not found");
    }

    #[test]
    fn combine_is_ok_only_when_every_result_is_ok() {
        let all_ok = [CheckResult::ok("x", "a"), CheckResult::ok("x", "b")];
        let r = combine("group", "all good", &all_ok);
        assert_eq!(r.level, Level::Ok);
        assert_eq!(r.message, "all good");
    }

    #[test]
    fn combine_fail_wins_over_warn_and_ok() {
        let mixed = [
            CheckResult::ok("x", "fine"),
            CheckResult::warn("x", "hmm"),
            CheckResult::fail("x", "broken").with_action("fix it"),
        ];
        let r = combine("group", "all good", &mixed);
        assert_eq!(r.level, Level::Fail);
        assert_eq!(r.message, "broken");
        assert_eq!(r.action.as_deref(), Some("fix it"));
    }

    #[test]
    fn combine_warn_wins_over_ok_when_no_fail_present() {
        let mixed = [CheckResult::ok("x", "fine"), CheckResult::warn("x", "hmm")];
        let r = combine("group", "all good", &mixed);
        assert_eq!(r.level, Level::Warn);
        assert_eq!(r.message, "hmm");
    }

    // --- 1a: mpv availability ---

    #[test]
    fn native_only_is_ok() {
        let r = check_mpv_availability(
            "",
            MpvPresence {
                native: true,
                flatpak: false,
            },
            false,
        );
        assert_eq!(r.level, Level::Ok);
        assert!(r.message.contains("native"));
    }

    #[test]
    fn flatpak_only_is_ok() {
        let r = check_mpv_availability(
            "",
            MpvPresence {
                native: false,
                flatpak: true,
            },
            false,
        );
        assert_eq!(r.level, Level::Ok);
        assert!(r.message.contains("Flatpak"));
    }

    #[test]
    fn neither_present_is_fail() {
        let r = check_mpv_availability(
            "",
            MpvPresence {
                native: false,
                flatpak: false,
            },
            false,
        );
        assert_eq!(r.level, Level::Fail);
    }

    #[test]
    fn both_present_is_warn_not_fail() {
        let r = check_mpv_availability(
            "",
            MpvPresence {
                native: true,
                flatpak: true,
            },
            false,
        );
        assert_eq!(r.level, Level::Warn);
    }

    #[test]
    fn command_override_set_and_resolving_is_ok_regardless_of_presence() {
        let r = check_mpv_availability(
            "flatpak run io.mpv.Mpv",
            MpvPresence {
                native: false,
                flatpak: false,
            },
            true,
        );
        assert_eq!(r.level, Level::Ok);
    }

    #[test]
    fn command_override_set_and_not_resolving_is_fail_even_if_native_present() {
        let r = check_mpv_availability(
            "/does/not/exist",
            MpvPresence {
                native: true,
                flatpak: false,
            },
            false,
        );
        assert_eq!(
            r.level,
            Level::Fail,
            "an override takes priority over detection — a broken override must fail even \
             though native mpv would otherwise resolve fine"
        );
    }

    #[test]
    fn command_override_resolves_checks_the_first_token_on_path() {
        assert!(command_override_resolves("true"));
        assert!(!command_override_resolves(
            "tsuzuki-definitely-not-a-real-binary"
        ));
        assert!(!command_override_resolves(""));
    }

    #[cfg(unix)]
    #[test]
    fn command_override_resolves_checks_an_absolute_path_directly() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let fake = dir.path().join("fake-mpv");
        std_fs::write(&fake, b"#!/bin/sh\n").unwrap();
        let mut perms = std_fs::metadata(&fake).unwrap().permissions();
        perms.set_mode(0o755);
        std_fs::set_permissions(&fake, perms).unwrap();

        assert!(command_override_resolves(&fake.to_string_lossy()));
        assert!(!command_override_resolves("/definitely/not/here/mpv"));
    }

    // --- 1d: Flatpak filesystem-permission gap ---

    #[test]
    fn flatpak_permissions_skipped_when_flatpak_not_present() {
        let r = check_flatpak_permissions(false, true, Some("filesystems=host;"));
        assert_eq!(r.level, Level::Warn);
        assert!(r.message.contains("skipped"));
    }

    #[test]
    fn flatpak_permissions_warns_when_cli_missing() {
        let r = check_flatpak_permissions(true, false, None);
        assert_eq!(r.level, Level::Warn);
    }

    #[test]
    fn flatpak_permissions_warns_when_output_unavailable() {
        let r = check_flatpak_permissions(true, true, None);
        assert_eq!(r.level, Level::Warn);
    }

    #[test]
    fn flatpak_permissions_ok_on_broad_host_access() {
        let r = check_flatpak_permissions(true, true, Some("[Context]\nfilesystems=host;\n"));
        assert_eq!(r.level, Level::Ok);
    }

    #[test]
    fn flatpak_permissions_warns_on_narrow_access() {
        let r =
            check_flatpak_permissions(true, true, Some("[Context]\nfilesystems=xdg-download;\n"));
        assert_eq!(r.level, Level::Warn);
    }

    #[test]
    fn flatpak_permissions_never_fails_regardless_of_input() {
        // Never escalates to FAIL.
        for output in [
            None,
            Some(""),
            Some("filesystems=xdg-download;"),
            Some("garbage"),
        ] {
            for cli in [true, false] {
                assert_ne!(
                    check_flatpak_permissions(true, cli, output).level,
                    Level::Fail
                );
            }
        }
    }

    // --- 1e: tsuzuki.lua mpv script ---

    #[test]
    fn tsuzuki_lua_missing_needs_fix() {
        let dir = tempdir().unwrap();
        let outcome = check_tsuzuki_lua(&dir.path().join("scripts"), "native");
        assert_eq!(outcome.result.level, Level::Fail);
        assert_eq!(outcome.needs_fix, Some(dir.path().join("scripts")));
    }

    #[test]
    fn tsuzuki_lua_matching_bundled_is_ok_and_needs_no_fix() {
        let dir = tempdir().unwrap();
        let scripts_dir = dir.path().join("scripts");
        std_fs::create_dir_all(&scripts_dir).unwrap();
        std_fs::write(scripts_dir.join("tsuzuki.lua"), BUNDLED_SCRIPT).unwrap();

        let outcome = check_tsuzuki_lua(&scripts_dir, "native");
        assert_eq!(outcome.result.level, Level::Ok);
        assert_eq!(outcome.needs_fix, None);
    }

    #[test]
    fn tsuzuki_lua_stale_content_needs_fix() {
        let dir = tempdir().unwrap();
        let scripts_dir = dir.path().join("scripts");
        std_fs::create_dir_all(&scripts_dir).unwrap();
        std_fs::write(scripts_dir.join("tsuzuki.lua"), "-- an old version\n").unwrap();

        let outcome = check_tsuzuki_lua(&scripts_dir, "native");
        assert_eq!(outcome.result.level, Level::Fail);
        assert_eq!(outcome.needs_fix, Some(scripts_dir));
    }

    #[test]
    fn install_script_writes_the_bundled_content_and_creates_the_directory() {
        let dir = tempdir().unwrap();
        let scripts_dir = dir
            .path()
            .join("does")
            .join("not")
            .join("exist")
            .join("scripts");

        install_script(&scripts_dir).unwrap();

        let installed = std_fs::read_to_string(scripts_dir.join("tsuzuki.lua")).unwrap();
        assert_eq!(installed, BUNDLED_SCRIPT);

        // Re-checking now reports OK with no further fix needed.
        let outcome = check_tsuzuki_lua(&scripts_dir, "native");
        assert_eq!(outcome.result.level, Level::Ok);
    }

    #[test]
    fn install_script_overwrites_stale_content() {
        let dir = tempdir().unwrap();
        let scripts_dir = dir.path().join("scripts");
        std_fs::create_dir_all(&scripts_dir).unwrap();
        std_fs::write(scripts_dir.join("tsuzuki.lua"), "-- stale\n").unwrap();

        install_script(&scripts_dir).unwrap();

        let installed = std_fs::read_to_string(scripts_dir.join("tsuzuki.lua")).unwrap();
        assert_eq!(installed, BUNDLED_SCRIPT);
    }

    #[cfg(unix)]
    #[test]
    fn install_script_leaves_no_stray_temp_files() {
        let dir = tempdir().unwrap();
        let scripts_dir = dir.path().join("scripts");
        install_script(&scripts_dir).unwrap();

        let entries: Vec<_> = std_fs::read_dir(&scripts_dir).unwrap().collect();
        assert_eq!(
            entries.len(),
            1,
            "exactly one file should remain after an atomic write"
        );
    }

    // --- 1f: mpv-log directory reachability ---

    #[test]
    fn mpv_log_missing_directory_is_ok() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("never-created");
        let results = check_mpv_log_dirs(&[missing]);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].level, Level::Ok);
    }

    #[test]
    fn mpv_log_existing_readable_directory_is_ok() {
        let dir = tempdir().unwrap();
        let existing = dir.path().join("mpv-log");
        std_fs::create_dir_all(&existing).unwrap();
        let results = check_mpv_log_dirs(&[existing]);
        assert_eq!(results[0].level, Level::Ok);
    }

    #[cfg(unix)]
    #[test]
    fn mpv_log_existing_unreadable_directory_fails() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let existing = dir.path().join("mpv-log");
        std_fs::create_dir_all(&existing).unwrap();
        std_fs::set_permissions(&existing, std_fs::Permissions::from_mode(0o000)).unwrap();

        let results = check_mpv_log_dirs(std::slice::from_ref(&existing));
        assert_eq!(results[0].level, Level::Fail);

        // Restore permissions so tempdir cleanup can actually remove it.
        std_fs::set_permissions(&existing, std_fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn mpv_log_missing_and_unreadable_are_distinguished_not_conflated() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("missing");
        let results = check_mpv_log_dirs(&[missing]);
        assert_eq!(results[0].level, Level::Ok, "missing must be OK, not FAIL");
    }

    // --- 1g: tracked-root validity ---

    #[test]
    fn tracked_root_reachable_is_ok() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        std_fs::create_dir_all(&root).unwrap();
        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());

        let checks = check_tracked_roots(&config, None);
        assert_eq!(checks.len(), 1);
        assert!(checks[0].valid);
    }

    #[test]
    fn tracked_root_missing_is_fail() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("GoneAnime");
        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());

        let checks = check_tracked_roots(&config, None);
        assert!(!checks[0].valid);
    }

    #[test]
    fn summarize_tracked_roots_with_none_configured_is_ok_not_absent() {
        let r = summarize_tracked_roots(&[]);
        assert_eq!(r.level, Level::Ok);
        assert_eq!(r.action, Some("tsuzuki add <path>".to_string()));
    }

    #[test]
    fn summarize_tracked_roots_names_every_unreachable_root_not_just_the_first() {
        let checks = [
            RootCheck {
                root: "~/Anime".to_string(),
                valid: false,
            },
            RootCheck {
                root: "~/Manga".to_string(),
                valid: false,
            },
        ];
        let r = summarize_tracked_roots(&checks);
        assert_eq!(r.level, Level::Fail);
        assert!(r.message.contains("~/Anime"));
        assert!(r.message.contains("~/Manga"));
    }

    // --- 1h: orphaned records ---

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            std_fs::create_dir_all(parent).unwrap();
        }
        std_fs::File::create(path).unwrap();
    }

    fn orphan_record(root: &Path, name: &str) -> SeriesRecord {
        SeriesRecord {
            root: root.to_string_lossy().into_owned(),
            name: name.to_string(),
            status: Status::InProgress,
            current_relative_path: format!("{name}/ep01.mkv"),
            position_seconds: 100.0,
            episode_number: Some(1),
            updated_at: 1,
            mal_episode_awareness: None,
            season_number: None,
        }
    }

    #[test]
    fn a_record_whose_series_folder_is_gone_is_an_orphan() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        std_fs::create_dir_all(&root).unwrap(); // root exists, but "Monster" under it doesn't

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());
        let state = State {
            series: vec![orphan_record(&root, "Monster")],

            mal_links: Vec::new(),
        };

        let orphans = find_orphans(
            &config,
            &state,
            &[root.to_string_lossy().into_owned()],
            None,
        );
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].name, "Monster");
    }

    #[test]
    fn a_record_whose_series_folder_still_exists_is_not_an_orphan() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        touch(&root.join("Monster").join("ep01.mkv"));

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());
        let state = State {
            series: vec![orphan_record(&root, "Monster")],

            mal_links: Vec::new(),
        };

        let orphans = find_orphans(
            &config,
            &state,
            &[root.to_string_lossy().into_owned()],
            None,
        );
        assert!(orphans.is_empty());
    }

    #[test]
    fn a_record_under_an_unreachable_root_is_never_flagged_as_an_orphan() {
        // An unreachable root is skipped, not read as all its series gone.
        let dir = tempdir().unwrap();
        let root = dir.path().join("UnmountedAnime"); // deliberately never created

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());
        let state = State {
            series: vec![orphan_record(&root, "Monster")],

            mal_links: Vec::new(),
        };

        // An invalid root is excluded from valid_roots.
        let orphans = find_orphans(&config, &state, &[], None);
        assert!(
            orphans.is_empty(),
            "a record under a currently-unreachable root must not be reported as an orphan"
        );
    }

    #[test]
    fn orphan_check_result_lists_every_orphan_and_the_exact_forget_command() {
        let orphans = vec![
            Orphan {
                root: "~/Anime".to_string(),
                name: "Monster".to_string(),
            },
            Orphan {
                root: "~/Anime".to_string(),
                name: "One Piece".to_string(),
            },
        ];
        let r = orphan_check_result(&orphans);
        assert_eq!(r.level, Level::Fail);
        assert!(r.message.contains("Monster"));
        assert!(r.message.contains("One Piece"));
        assert_eq!(
            r.action.as_deref(),
            Some("tsuzuki doctor --fix --forget-orphans")
        );
    }

    #[test]
    fn orphan_check_result_with_no_orphans_is_ok() {
        let r = orphan_check_result(&[]);
        assert_eq!(r.level, Level::Ok);
    }

    #[test]
    fn forget_orphans_removes_only_the_confirmed_orphans() {
        let mut state = State {
            series: vec![
                orphan_record(Path::new("~/Anime"), "Monster"),
                orphan_record(Path::new("~/Anime"), "One Piece"),
            ],

            mal_links: Vec::new(),
        };
        let orphans = vec![Orphan {
            root: "~/Anime".to_string(),
            name: "Monster".to_string(),
        }];

        forget_orphans(&mut state, &orphans);

        assert_eq!(state.series.len(), 1);
        assert_eq!(state.series[0].name, "One Piece");
    }

    // --- detected_mpv_installs: Flatpak branch (home-injectable) ---

    #[test]
    fn detected_installs_includes_flatpak_when_present_and_home_known() {
        let home = PathBuf::from("/home/user");
        let installs = detected_mpv_installs(
            MpvPresence {
                native: false,
                flatpak: true,
            },
            Some(&home),
        );
        assert!(installs.iter().any(|i| i.label == "flatpak"));
    }

    #[test]
    fn detected_installs_excludes_flatpak_when_not_present() {
        let home = PathBuf::from("/home/user");
        let installs = detected_mpv_installs(
            MpvPresence {
                native: false,
                flatpak: false,
            },
            Some(&home),
        );
        assert!(installs.is_empty());
    }

    #[test]
    fn detected_installs_excludes_flatpak_with_no_home() {
        let installs = detected_mpv_installs(
            MpvPresence {
                native: false,
                flatpak: true,
            },
            None,
        );
        assert!(installs.is_empty());
    }

    // --- 1i: MAL auth status ---

    use crate::mal::TokenStatus;

    #[test]
    fn mal_auth_not_configured_is_warn() {
        let r = check_mal_auth(&TokenStatus::NotConfigured);
        assert_eq!(r.level, Level::Warn);
    }

    #[test]
    fn mal_auth_not_logged_in_is_warn() {
        let r = check_mal_auth(&TokenStatus::NotLoggedIn);
        assert_eq!(r.level, Level::Warn);
    }

    #[test]
    fn mal_auth_valid_token_is_ok() {
        let r = check_mal_auth(&TokenStatus::Valid {
            expires_in_secs: 1800,
        });
        assert_eq!(r.level, Level::Ok);
    }

    #[test]
    fn mal_auth_refreshed_token_is_ok() {
        let r = check_mal_auth(&TokenStatus::Refreshed {
            expires_in_secs: 3600,
        });
        assert_eq!(r.level, Level::Ok);
    }

    #[test]
    fn mal_auth_refresh_rejected_is_fail() {
        let r = check_mal_auth(&TokenStatus::RefreshRejected("invalid_grant".to_string()));
        assert_eq!(r.level, Level::Fail);
        assert_eq!(r.action.as_deref(), Some("tsuzuki sync login"));
    }

    #[test]
    fn mal_auth_network_error_is_warn_not_fail() {
        // Connectivity noise must not fail the run.
        let r = check_mal_auth(&TokenStatus::NetworkError("connection refused".to_string()));
        assert_eq!(r.level, Level::Warn);
    }

    #[test]
    fn a_record_for_a_nested_root_as_its_parents_series_is_an_orphan() {
        let dir = tempdir().unwrap();
        let anime = dir.path().join("Anime");
        let rewatch = anime.join("Rewatch");
        std_fs::create_dir_all(rewatch.join("Bleach")).unwrap();
        std_fs::create_dir_all(anime.join("Monster")).unwrap();

        let mut config = Config::default();
        config
            .roots
            .paths
            .push(anime.to_string_lossy().into_owned());
        config
            .roots
            .paths
            .push(rewatch.to_string_lossy().into_owned());
        let state = State {
            series: vec![
                orphan_record(&anime, "Rewatch"),
                orphan_record(&anime, "Monster"),
                orphan_record(&rewatch, "Bleach"),
            ],
            mal_links: Vec::new(),
        };
        let valid = [
            anime.to_string_lossy().into_owned(),
            rewatch.to_string_lossy().into_owned(),
        ];

        let orphans = find_orphans(&config, &state, &valid, None);
        assert_eq!(
            orphans,
            vec![Orphan {
                root: anime.to_string_lossy().into_owned(),
                name: "Rewatch".to_string(),
            }]
        );
    }

    // --- 1c-bis: entry timestamps ---

    #[test]
    fn entry_timestamps_with_no_entries_is_ok() {
        let now = SystemTime::now();
        let r = check_entry_timestamps(&[], now);
        assert_eq!(
            r,
            CheckResult::ok("mpv timestamps", "none dated in the future")
        );
    }

    #[test]
    fn entry_timestamps_at_or_before_now_is_ok() {
        let now = SystemTime::now();
        let past = now - Duration::from_secs(1);
        let r = check_entry_timestamps(&[past, now], now);
        assert_eq!(
            r,
            CheckResult::ok("mpv timestamps", "none dated in the future")
        );
    }

    #[test]
    fn entry_timestamps_one_second_after_now_warns_with_the_clock_action() {
        let now = SystemTime::now();
        let r = check_entry_timestamps(&[now + Duration::from_secs(1)], now);
        assert_eq!(
            r,
            CheckResult::warn(
                "mpv timestamps",
                "1 entry dated in the future, which can reactivate dropped series"
            )
            .with_action("check the system clock")
        );
    }

    #[test]
    fn entry_timestamps_counts_only_the_future_ones_among_mixed_entries() {
        let now = SystemTime::now();
        let mtimes = [
            now - Duration::from_secs(60),
            now + Duration::from_secs(60),
            now + Duration::from_secs(86_400),
        ];
        let r = check_entry_timestamps(&mtimes, now);
        assert_eq!(
            r,
            CheckResult::warn(
                "mpv timestamps",
                "2 entries dated in the future, which can reactivate dropped series"
            )
            .with_action("check the system clock")
        );
    }
}
