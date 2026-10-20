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
use crate::mpv::{self, ChosenBy, Installed, Kind, ResolveError, Target};
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

// --- 1a. mpv target ---

/// Reports the mpv `mpv::resolve` picked and why. A custom target's program
/// must exist (`custom_resolves`). Other detected installs become one
/// `→` line saying how to pin them.
pub fn check_mpv_target(
    target: &Result<Target, ResolveError>,
    installed: Installed,
    custom_resolves: bool,
) -> CheckResult {
    let target = match target {
        Ok(target) => target,
        Err(ResolveError::NotFound) => {
            return CheckResult::fail("mpv", "not found").with_action(
                "install mpv, or set [mpv] install = \"custom\" and command in config.toml",
            );
        }
        Err(ResolveError::NotInstalled(kind)) => {
            return CheckResult::fail(
                "mpv",
                format!(
                    "[mpv] install is \"{}\" but it is not installed",
                    install_key(*kind)
                ),
            )
            .with_action("install it, or change [mpv] install in config.toml");
        }
        Err(ResolveError::NoCustomCommand) => {
            return CheckResult::fail("mpv", "[mpv] install is \"custom\" but command is empty")
                .with_action("set [mpv] command in config.toml");
        }
    };

    if target.kind == Kind::Custom && !custom_resolves {
        return CheckResult::fail("mpv", "custom command not found")
            .with_action("check [mpv] command in config.toml");
    }

    let chosen_by = match target.chosen_by {
        ChosenBy::Auto => "auto",
        ChosenBy::Pinned => "pinned",
    };
    let result = CheckResult::ok("mpv", format!("{} ({chosen_by})", target.kind.label()));

    let others: Vec<Kind> = [
        (Kind::Native, installed.native),
        (Kind::Flatpak, installed.flatpak),
    ]
    .into_iter()
    .filter(|&(kind, present)| present && kind != target.kind)
    .map(|(kind, _)| kind)
    .collect();
    match others.as_slice() {
        [] => result,
        [kind] => result.with_action(format!(
            "also found: {} mpv; to use it, set [mpv] install = \"{}\"",
            kind.label(),
            install_key(*kind)
        )),
        _ => result.with_action(
            "also found: native and Flatpak mpv; to use one, set [mpv] install = \"native\" or \"flatpak\"",
        ),
    }
}

/// The `[mpv] install` value that pins `kind`.
fn install_key(kind: Kind) -> &'static str {
    match kind {
        Kind::Native => "native",
        Kind::Flatpak => "flatpak",
        Kind::Custom => "custom",
    }
}

/// Whether a custom target's program is an executable path (if it contains
/// `/`) or a program on `$PATH`.
pub fn custom_command_resolves(program: &str) -> bool {
    if program.contains('/') {
        mpv::is_executable_file(Path::new(program))
    } else {
        mpv::program_on_path(program)
    }
}

// --- 1c. entry timestamps ---

/// Warns when any tracked-root script-log entry (`mtimes`) is dated after `now`;
/// `None` when none is. Report only: reconcile treats such an entry like any other, so a clock that
/// ran ahead can make a `drop` look undone until the clock catches up.
pub fn check_entry_timestamps(mtimes: &[SystemTime], now: SystemTime) -> Option<CheckResult> {
    let future = mtimes.iter().filter(|&&mtime| mtime > now).count();
    if future == 0 {
        return None;
    }
    let noun = if future == 1 { "entry" } else { "entries" };
    Some(
        CheckResult::warn(
            "mpv timestamps",
            format!("{future} {noun} dated in the future, which can reactivate dropped series"),
        )
        .with_action("check the system clock"),
    )
}

// --- 1d. Flatpak filesystem-permission gap ---

/// Whether Flatpak mpv can read each tracked root, from `flatpak info
/// --file-access=<root>` answers (`hidden`, `read-only`, `read-write`) paired
/// with the probed root; `None` is a failed probe. Never fails: sandbox
/// permissions are the user's call.
pub fn check_flatpak_access(
    flatpak_cli_present: bool,
    probes: &[(String, Option<String>)],
) -> CheckResult {
    if !flatpak_cli_present {
        return CheckResult::warn(
            "mpv permissions",
            "could not verify (flatpak CLI not found)",
        );
    }
    let mut hidden = Vec::new();
    for (root, answer) in probes {
        match answer.as_deref().map(str::trim) {
            Some("read-only" | "read-write") => {}
            Some("hidden") => hidden.push(root.as_str()),
            _ => {
                return CheckResult::warn("mpv permissions", "could not verify")
                    .with_action("flatpak info --file-access=<root> io.mpv.Mpv");
            }
        }
    }
    let Some(first) = hidden.first() else {
        return CheckResult::ok("mpv permissions", "tracked directories accessible");
    };
    CheckResult::warn(
        "mpv permissions",
        format!("mpv can't see: {}", hidden.join(", ")),
    )
    .with_action(format!(
        "flatpak override --user --filesystem={first}:ro io.mpv.Mpv"
    ))
}

// --- 1e. tsuzuki.lua mpv script (required) ---

pub struct ScriptCheckOutcome {
    pub result: CheckResult,
    /// The script file `--fix` should install, if any.
    pub needs_fix: Option<PathBuf>,
}

/// Compares the target's `tsuzuki.lua` byte-for-byte with the bundled copy; a
/// blank file counts as not installed. Only a `custom` target may lack a
/// script path (warned, not verified); native and Flatpak without one fail.
pub fn check_tsuzuki_lua(target: &Target) -> ScriptCheckOutcome {
    let label = target.kind.label();
    let Some(script_path) = &target.script_path else {
        let result = if target.kind == Kind::Custom {
            CheckResult::warn("mpv script", format!("not verified ({label})"))
                .with_action("set [mpv] script_path in config.toml")
        } else {
            CheckResult::fail("mpv script", format!("script directory unknown ({label})"))
                .with_action(
                    "set [mpv] install = \"custom\" with command and script_path in config.toml",
                )
        };
        return ScriptCheckOutcome {
            result,
            needs_fix: None,
        };
    };

    match fs::read(script_path) {
        Ok(bytes) if bytes == BUNDLED_SCRIPT.as_bytes() => ScriptCheckOutcome {
            result: CheckResult::ok("mpv script", format!("up to date ({label})")),
            needs_fix: None,
        },
        Ok(bytes) if !mpv::is_blank_script(&bytes) => ScriptCheckOutcome {
            result: CheckResult::fail("mpv script", format!("out of date ({label})"))
                .with_action("tsuzuki doctor --fix"),
            needs_fix: Some(script_path.clone()),
        },
        _ => ScriptCheckOutcome {
            result: CheckResult::fail("mpv script", format!("not installed ({label})"))
                .with_action("tsuzuki doctor --fix"),
            needs_fix: Some(script_path.clone()),
        },
    }
}

/// Installs or updates the script at `script_path` atomically (temp file in
/// the same directory, then rename). A symlinked script is written through, so
/// the user's link survives; a dangling link is an error.
pub fn install_script(script_path: &Path) -> io::Result<()> {
    let script_path = &match fs::symlink_metadata(script_path) {
        Ok(meta) if meta.file_type().is_symlink() => fs::canonicalize(script_path)?,
        _ => script_path.to_path_buf(),
    };
    let dir = script_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "script path has no parent directory",
        )
    })?;
    fs::create_dir_all(dir)?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    io::Write::write_all(&mut temp, BUNDLED_SCRIPT.as_bytes())?;
    temp.persist(script_path).map_err(|e| e.error)?;
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
        .with_action("flatpak override --user --filesystem=/mnt/a:ro io.mpv.Mpv");
        assert_eq!(
            warn.render(),
            "⚠ mpv permissions — tracked directories may not be accessible\n  → flatpak override --user --filesystem=/mnt/a:ro io.mpv.Mpv"
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

    // --- 1a: mpv target ---

    const NONE: Installed = Installed {
        native: false,
        flatpak: false,
    };
    const NATIVE: Installed = Installed {
        native: true,
        flatpak: false,
    };
    const FLATPAK: Installed = Installed {
        native: false,
        flatpak: true,
    };
    const BOTH: Installed = Installed {
        native: true,
        flatpak: true,
    };

    fn target(kind: Kind, chosen_by: ChosenBy, script_path: Option<PathBuf>) -> Target {
        Target {
            kind,
            chosen_by,
            program: "mpv".to_string(),
            base_args: Vec::new(),
            script_path,
        }
    }

    #[test]
    fn mpv_target_auto_native_alone_is_ok_with_no_also_found_line() {
        let t = Ok(target(Kind::Native, ChosenBy::Auto, None));
        assert_eq!(
            check_mpv_target(&t, NATIVE, false),
            CheckResult::ok("mpv", "native (auto)")
        );
    }

    #[test]
    fn mpv_target_auto_native_with_flatpak_present_names_how_to_pin_flatpak() {
        let t = Ok(target(Kind::Native, ChosenBy::Auto, None));
        assert_eq!(
            check_mpv_target(&t, BOTH, false),
            CheckResult::ok("mpv", "native (auto)")
                .with_action("also found: Flatpak mpv; to use it, set [mpv] install = \"flatpak\"")
        );
    }

    #[test]
    fn mpv_target_pinned_flatpak_with_native_present_names_how_to_pin_native() {
        let t = Ok(target(Kind::Flatpak, ChosenBy::Pinned, None));
        assert_eq!(
            check_mpv_target(&t, BOTH, false),
            CheckResult::ok("mpv", "Flatpak (pinned)")
                .with_action("also found: native mpv; to use it, set [mpv] install = \"native\"")
        );
        assert_eq!(
            check_mpv_target(&t, FLATPAK, false),
            CheckResult::ok("mpv", "Flatpak (pinned)")
        );
    }

    #[test]
    fn mpv_target_custom_resolving_lists_both_other_installs() {
        let t = Ok(target(Kind::Custom, ChosenBy::Pinned, None));
        assert_eq!(
            check_mpv_target(&t, BOTH, true),
            CheckResult::ok("mpv", "custom (pinned)").with_action(
                "also found: native and Flatpak mpv; to use one, set [mpv] install = \"native\" or \"flatpak\""
            )
        );
        assert_eq!(
            check_mpv_target(&t, NONE, true),
            CheckResult::ok("mpv", "custom (pinned)")
        );
    }

    #[test]
    fn mpv_target_custom_not_resolving_fails_whatever_is_installed() {
        let t = Ok(target(Kind::Custom, ChosenBy::Pinned, None));
        for installed in [NONE, NATIVE, FLATPAK, BOTH] {
            assert_eq!(
                check_mpv_target(&t, installed, false),
                CheckResult::fail("mpv", "custom command not found")
                    .with_action("check [mpv] command in config.toml")
            );
        }
    }

    #[test]
    fn mpv_target_resolve_errors_fail_with_the_next_step() {
        assert_eq!(
            check_mpv_target(&Err(ResolveError::NotFound), NONE, false),
            CheckResult::fail("mpv", "not found").with_action(
                "install mpv, or set [mpv] install = \"custom\" and command in config.toml"
            )
        );
        assert_eq!(
            check_mpv_target(
                &Err(ResolveError::NotInstalled(Kind::Flatpak)),
                NATIVE,
                false
            ),
            CheckResult::fail(
                "mpv",
                "[mpv] install is \"flatpak\" but it is not installed"
            )
            .with_action("install it, or change [mpv] install in config.toml")
        );
        assert_eq!(
            check_mpv_target(&Err(ResolveError::NoCustomCommand), BOTH, true),
            CheckResult::fail("mpv", "[mpv] install is \"custom\" but command is empty")
                .with_action("set [mpv] command in config.toml")
        );
    }

    #[test]
    fn custom_command_resolves_checks_a_bare_name_on_path() {
        assert!(custom_command_resolves("true"));
        assert!(!custom_command_resolves(
            "tsuzuki-definitely-not-a-real-binary"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn custom_command_resolves_checks_an_absolute_path_directly() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let fake = dir.path().join("fake-mpv");
        std_fs::write(&fake, b"#!/bin/sh\n").unwrap();
        std_fs::set_permissions(&fake, std_fs::Permissions::from_mode(0o755)).unwrap();

        assert!(custom_command_resolves(&fake.to_string_lossy()));
        assert!(!custom_command_resolves("/definitely/not/here/mpv"));
    }

    // --- 1d: Flatpak filesystem-permission gap ---

    fn probe(root: &str, answer: Option<&str>) -> (String, Option<String>) {
        (root.to_string(), answer.map(str::to_string))
    }

    #[test]
    fn flatpak_access_ok_when_every_root_is_readable() {
        let r = check_flatpak_access(
            true,
            &[
                probe("/mnt/a", Some("read-only")),
                probe("/mnt/b", Some("read-write\n")),
            ],
        );
        assert_eq!(
            r,
            CheckResult::ok("mpv permissions", "tracked directories accessible")
        );
    }

    #[test]
    fn flatpak_access_ok_with_no_roots() {
        let r = check_flatpak_access(true, &[]);
        assert_eq!(
            r,
            CheckResult::ok("mpv permissions", "tracked directories accessible")
        );
    }

    #[test]
    fn flatpak_access_names_only_the_hidden_roots() {
        let r = check_flatpak_access(
            true,
            &[
                probe("/mnt/a", Some("read-only")),
                probe("/mnt/b", Some("hidden")),
                probe("/mnt/c", Some("hidden")),
            ],
        );
        assert_eq!(
            r,
            CheckResult::warn("mpv permissions", "mpv can't see: /mnt/b, /mnt/c")
                .with_action("flatpak override --user --filesystem=/mnt/b:ro io.mpv.Mpv")
        );
    }

    #[test]
    fn flatpak_access_warns_could_not_verify_on_unknown_answer() {
        for answer in [Some(""), Some("garbage"), None] {
            let r = check_flatpak_access(
                true,
                &[probe("/mnt/a", Some("hidden")), probe("/mnt/b", answer)],
            );
            assert_eq!(
                r,
                CheckResult::warn("mpv permissions", "could not verify")
                    .with_action("flatpak info --file-access=<root> io.mpv.Mpv"),
                "answer {answer:?}"
            );
        }
    }

    #[test]
    fn flatpak_access_warns_when_cli_missing() {
        let r = check_flatpak_access(false, &[probe("/mnt/a", Some("read-only"))]);
        assert_eq!(
            r,
            CheckResult::warn(
                "mpv permissions",
                "could not verify (flatpak CLI not found)"
            )
        );
    }

    // --- 1e: tsuzuki.lua mpv script ---

    #[test]
    fn tsuzuki_lua_missing_fails_and_needs_fix_at_the_target_path() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("scripts").join("tsuzuki.lua");
        let outcome =
            check_tsuzuki_lua(&target(Kind::Native, ChosenBy::Auto, Some(script.clone())));
        assert_eq!(
            outcome.result,
            CheckResult::fail("mpv script", "not installed (native)")
                .with_action("tsuzuki doctor --fix")
        );
        assert_eq!(outcome.needs_fix, Some(script));
    }

    #[test]
    fn tsuzuki_lua_matching_bundled_is_ok_and_needs_no_fix() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("tsuzuki.lua");
        std_fs::write(&script, BUNDLED_SCRIPT).unwrap();

        let outcome = check_tsuzuki_lua(&target(Kind::Flatpak, ChosenBy::Auto, Some(script)));
        assert_eq!(
            outcome.result,
            CheckResult::ok("mpv script", "up to date (Flatpak)")
        );
        assert_eq!(outcome.needs_fix, None);
    }

    #[test]
    fn tsuzuki_lua_stale_content_fails_and_needs_fix() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("tsuzuki.lua");
        std_fs::write(&script, "-- an old version\n").unwrap();

        let outcome = check_tsuzuki_lua(&target(
            Kind::Custom,
            ChosenBy::Pinned,
            Some(script.clone()),
        ));
        assert_eq!(
            outcome.result,
            CheckResult::fail("mpv script", "out of date (custom)")
                .with_action("tsuzuki doctor --fix")
        );
        assert_eq!(outcome.needs_fix, Some(script));
    }

    #[test]
    fn tsuzuki_lua_custom_without_script_path_warns_not_verified_and_needs_no_fix() {
        let outcome = check_tsuzuki_lua(&target(Kind::Custom, ChosenBy::Pinned, None));
        assert_eq!(
            outcome.result,
            CheckResult::warn("mpv script", "not verified (custom)")
                .with_action("set [mpv] script_path in config.toml")
        );
        assert_eq!(outcome.needs_fix, None);
    }

    #[test]
    fn tsuzuki_lua_native_or_flatpak_with_unknown_path_fails_and_needs_no_fix() {
        for (kind, label) in [(Kind::Native, "native"), (Kind::Flatpak, "Flatpak")] {
            let outcome = check_tsuzuki_lua(&target(kind, ChosenBy::Auto, None));
            assert_eq!(
                outcome.result,
                CheckResult::fail("mpv script", format!("script directory unknown ({label})"))
                    .with_action(
                        "set [mpv] install = \"custom\" with command and script_path in config.toml"
                    )
            );
            assert_eq!(outcome.needs_fix, None);
        }
    }

    #[test]
    fn tsuzuki_lua_blank_content_is_not_installed_and_needs_fix() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("tsuzuki.lua");
        for contents in ["", " \n\t\n"] {
            std_fs::write(&script, contents).unwrap();

            let outcome =
                check_tsuzuki_lua(&target(Kind::Native, ChosenBy::Auto, Some(script.clone())));

            assert_eq!(
                outcome.result,
                CheckResult::fail("mpv script", "not installed (native)")
                    .with_action("tsuzuki doctor --fix"),
                "{contents:?}"
            );
            assert_eq!(outcome.needs_fix, Some(script.clone()), "{contents:?}");
        }
    }

    #[test]
    fn install_script_writes_the_bundled_content_and_creates_the_directory() {
        let dir = tempdir().unwrap();
        let script = dir
            .path()
            .join("does")
            .join("not")
            .join("exist")
            .join("tsuzuki.lua");

        install_script(&script).unwrap();

        assert_eq!(std_fs::read_to_string(&script).unwrap(), BUNDLED_SCRIPT);
        let outcome = check_tsuzuki_lua(&target(Kind::Native, ChosenBy::Auto, Some(script)));
        assert_eq!(
            outcome.result,
            CheckResult::ok("mpv script", "up to date (native)")
        );
    }

    #[test]
    fn install_script_overwrites_stale_content() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("tsuzuki.lua");
        std_fs::write(&script, "-- stale\n").unwrap();

        install_script(&script).unwrap();

        assert_eq!(std_fs::read_to_string(&script).unwrap(), BUNDLED_SCRIPT);
    }

    #[cfg(unix)]
    #[test]
    fn install_script_through_a_symlink_updates_the_linked_file_and_keeps_the_link() {
        let dir = tempdir().unwrap();
        let real = dir.path().join("dotfiles.lua");
        let link = dir.path().join("tsuzuki.lua");
        std_fs::write(&real, "-- stale\n").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        install_script(&link).unwrap();

        assert_eq!(std_fs::read_link(&link).unwrap(), real);
        assert_eq!(std_fs::read_to_string(&real).unwrap(), BUNDLED_SCRIPT);
    }

    #[cfg(unix)]
    #[test]
    fn install_script_through_a_dangling_symlink_errors_and_leaves_the_link() {
        let dir = tempdir().unwrap();
        let gone = dir.path().join("gone.lua");
        let link = dir.path().join("tsuzuki.lua");
        std::os::unix::fs::symlink(&gone, &link).unwrap();

        assert!(install_script(&link).is_err());

        assert_eq!(std_fs::read_link(&link).unwrap(), gone);
        assert!(!gone.exists());
    }

    #[cfg(unix)]
    #[test]
    fn install_script_leaves_no_stray_temp_files() {
        let dir = tempdir().unwrap();
        let scripts_dir = dir.path().join("scripts");
        install_script(&scripts_dir.join("tsuzuki.lua")).unwrap();

        let names: Vec<_> = std_fs::read_dir(&scripts_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("tsuzuki.lua")]);
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
    fn entry_timestamps_with_no_entries_is_silent() {
        let now = SystemTime::now();
        assert_eq!(check_entry_timestamps(&[], now), None);
    }

    #[test]
    fn entry_timestamps_at_or_before_now_is_silent() {
        let now = SystemTime::now();
        let past = now - Duration::from_secs(1);
        assert_eq!(check_entry_timestamps(&[past, now], now), None);
    }

    #[test]
    fn entry_timestamps_one_second_after_now_warns_with_the_clock_action() {
        let now = SystemTime::now();
        let r = check_entry_timestamps(&[now + Duration::from_secs(1)], now);
        assert_eq!(
            r,
            Some(
                CheckResult::warn(
                    "mpv timestamps",
                    "1 entry dated in the future, which can reactivate dropped series"
                )
                .with_action("check the system clock")
            )
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
            Some(
                CheckResult::warn(
                    "mpv timestamps",
                    "2 entries dated in the future, which can reactivate dropped series"
                )
                .with_action("check the system clock")
            )
        );
    }
}
