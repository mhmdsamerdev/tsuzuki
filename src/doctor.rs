//! `tsuzuki doctor [--fix] [--forget-orphans]` — proactive environment
//! health checks (CLAUDE.md's Phase 7 decisions, design.md §9's `doctor`
//! row).
//!
//! Every check here is a pure function taking already-resolved inputs
//! (booleans, paths, file contents) rather than reaching into real system
//! state itself — the one exception is the tiny amount of real detection
//! glue (`dirs::config_dir()`, the real `flatpak` subprocess call) that
//! `main.rs`'s `doctor_command` performs and passes in. This mirrors
//! `launch.rs`'s own injectable-closure convention and is what makes every
//! check here testable against a fabricated environment without ever
//! touching the real system's mpv.conf or scripts directory.
//!
//! MAL auth status is deliberately excluded from this phase's check list —
//! Phase 8 (MAL sync) doesn't exist yet. `main.rs`'s `doctor_command` prints
//! a one-line note saying so, rather than silently omitting it without a
//! trace.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::mpv;
use crate::roots;
use crate::series;
use crate::state::State;

/// The bundled companion script, embedded at compile time — so `--fix` can
/// install it even from a distributed binary with no `contrib/` directory on
/// disk (CLAUDE.md's distribution goals, §10). This is the single source of
/// truth both for installing a missing script and for detecting whether an
/// already-installed one has drifted from it.
pub const BUNDLED_SCRIPT: &str = include_str!("../contrib/mpv/tsuzuki.lua");

/// A doctor check's severity — CLAUDE.md's "each check reports OK / WARN /
/// FAIL. WARN is informational... doesn't affect exit code. FAIL is
/// actionable."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CheckResult {
    pub label: String,
    pub level: Level,
    pub message: String,
}

impl CheckResult {
    pub fn ok(label: impl Into<String>, message: impl Into<String>) -> Self {
        CheckResult {
            label: label.into(),
            level: Level::Ok,
            message: message.into(),
        }
    }
    pub fn warn(label: impl Into<String>, message: impl Into<String>) -> Self {
        CheckResult {
            label: label.into(),
            level: Level::Warn,
            message: message.into(),
        }
    }
    pub fn fail(label: impl Into<String>, message: impl Into<String>) -> Self {
        CheckResult {
            label: label.into(),
            level: Level::Fail,
            message: message.into(),
        }
    }

    /// `[OK]  label: message`, one line (message may itself contain
    /// embedded newlines for a multi-line detail, e.g. the orphan list).
    pub fn render(&self) -> String {
        let tag = match self.level {
            Level::Ok => "OK  ",
            Level::Warn => "WARN",
            Level::Fail => "FAIL",
        };
        format!("[{tag}] {}: {}", self.label, self.message)
    }
}

// --- 1a. mpv availability ---

/// Whether native mpv and/or Flatpak `io.mpv.Mpv` were each independently
/// detected — computed once by the caller (`launch::native_mpv_on_path()`,
/// `launch::flatpak_mpv_installed(home)`) and passed to every check that
/// needs it, so this module never repeats real detection work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MpvPresence {
    pub native: bool,
    pub flatpak: bool,
}

/// Reports mpv availability. When `command_override` is set, detection is
/// bypassed entirely (matching `launch::resolve_mpv_command`'s own
/// priority) — but unlike that function, doctor actually validates the
/// override resolves to a real, existing program (`override_resolves`,
/// computed by the caller) rather than trusting it blindly, since an
/// unresolvable override would otherwise silently break every future
/// `resume`/`start` launch with no proactive warning. With no override,
/// both native and Flatpak present is a WARN (ambiguous, but not broken —
/// `resolve_mpv_command` always picks native), neither present is a FAIL.
pub fn check_mpv_availability(
    command_override: &str,
    presence: MpvPresence,
    override_resolves: bool,
) -> CheckResult {
    if !command_override.is_empty() {
        return if override_resolves {
            CheckResult::ok(
                "mpv",
                format!("configured [mpv] command_override resolves: \"{command_override}\""),
            )
        } else {
            CheckResult::fail(
                "mpv",
                format!(
                    "configured [mpv] command_override \"{command_override}\" does not resolve \
                     to an existing, executable program"
                ),
            )
        };
    }

    match (presence.native, presence.flatpak) {
        (false, false) => CheckResult::fail(
            "mpv",
            "mpv not found — no native mpv on PATH, no Flatpak io.mpv.Mpv install found. \
             Install mpv, or set [mpv] command_override in config.toml",
        ),
        (true, false) => CheckResult::ok("mpv", "found native mpv on PATH"),
        (false, true) => CheckResult::ok("mpv", "found Flatpak mpv (io.mpv.Mpv)"),
        (true, true) => CheckResult::warn(
            "mpv",
            "both native and Flatpak mpv found — consider pinning [mpv] command_override \
             in config.toml to avoid ambiguity",
        ),
    }
}

/// Whether `command_override`'s first whitespace-separated token resolves to
/// a real, executable program — a path containing `/` is checked directly,
/// otherwise it's looked up on `$PATH` (mirrors `launch::resolve_mpv_command`
/// splitting the override the same way, then `launch.rs`'s own
/// `is_executable_file`/`program_on_path` checks).
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

// --- 1b. write-filename-in-watch-later-config=yes ---

/// One mpv install's own config directory (contains `mpv.conf` and
/// `scripts/`) — native resolves via the real `dirs::config_dir()` (exactly
/// like Tsuzuki's own config resolution, just mpv's subdirectory instead of
/// `tsuzuki`'s); Flatpak resolves via `home`, mirroring the exact same
/// `XDG_CONFIG_HOME` sandbox remap `script_log.rs` already verified for
/// `XDG_DATA_HOME`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MpvInstall {
    pub label: &'static str,
    pub config_dir: PathBuf,
}

/// Every mpv install actually detected (per `presence`) — an install method
/// with no mpv found at all isn't worth checking its config file or scripts
/// directory for, since there's nothing there to be wrong yet.
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

/// Parses `write-filename-in-watch-later-config`'s final effective value out
/// of an mpv.conf's contents — `None` covers both "the key never appears"
/// and "only appears in a comment," since neither is distinguishable from
/// the other in the message doctor prints (both need the same fix: add the
/// line for real). Later directives override earlier ones, matching mpv's
/// own config-file parsing order — an mpv.conf that sets it, then later
/// unsets it, ends up unset.
fn write_filename_flag_state(contents: &str) -> Option<bool> {
    let mut state = None;
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "write-filename-in-watch-later-config" {
            state = Some(true);
        } else if line == "no-write-filename-in-watch-later-config" {
            state = Some(false);
        } else if let Some(rest) = line.strip_prefix("write-filename-in-watch-later-config=") {
            state = Some(matches!(rest.trim(), "yes" | "1" | "true"));
        }
    }
    state
}

/// Checks one install's `mpv.conf` for the required flag. Never writes to
/// the file, regardless of the result — doctor doesn't own this file
/// (CLAUDE.md: "diagnostic only... doctor does not edit a file it doesn't
/// own").
pub fn check_mpv_conf(install: &MpvInstall) -> CheckResult {
    let path = install.config_dir.join("mpv.conf");
    let label = format!("mpv.conf ({})", install.label);

    let contents = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => {
            return CheckResult::fail(
                label,
                format!(
                    "write-filename-in-watch-later-config=yes is missing — {} does not exist. \
                     Add this line to it:\n      write-filename-in-watch-later-config=yes",
                    path.display()
                ),
            );
        }
    };

    match write_filename_flag_state(&contents) {
        Some(true) => CheckResult::ok(
            label,
            format!(
                "write-filename-in-watch-later-config=yes is set in {}",
                path.display()
            ),
        ),
        _ => CheckResult::fail(
            label,
            format!(
                "write-filename-in-watch-later-config=yes is not set in {}. Add this line to it:\n      write-filename-in-watch-later-config=yes",
                path.display()
            ),
        ),
    }
}

// --- 1c. watch_later reachability ---

/// Reachability of mpv's `watch_later` state — reuses `mpv::scan_watch_later`
/// directly rather than re-implementing candidate resolution: its
/// `ScanResult::warning` already carries exactly the "configured override
/// doesn't resolve" signal doctor needs (CLAUDE.md's "An absent OS-default
/// candidate directory stays silent — an explicit `watch_later_dir` override
/// that doesn't resolve must be surfaced"). In OS-default mode, an absent
/// candidate directory is normal (nothing has ever been quit mid-playback
/// yet) and never fails this check — only a configured override that
/// doesn't resolve does.
pub fn check_watch_later(config: &Config, home: Option<&Path>) -> CheckResult {
    let scan = mpv::scan_watch_later(config, home);
    if let Some(mpv::ScanWarning::WatchLaterDirNotFound(p)) = &scan.warning {
        return CheckResult::fail(
            "watch_later",
            format!(
                "configured [mpv] watch_later_dir \"{}\" does not exist",
                p.display()
            ),
        );
    }

    if !config.mpv.watch_later_dir.is_empty() {
        return CheckResult::ok(
            "watch_later",
            format!(
                "configured [mpv] watch_later_dir resolves: \"{}\"",
                config.mpv.watch_later_dir
            ),
        );
    }

    let candidates = home
        .map(mpv::candidate_watch_later_dirs)
        .unwrap_or_default();
    let existing = candidates.iter().filter(|d| d.exists()).count();
    CheckResult::ok(
        "watch_later",
        format!(
            "using the OS-default candidate list: {existing}/{} candidate director{} currently \
             exist (a candidate being absent is normal until mpv writes to it for the first time)",
            candidates.len(),
            if candidates.len() == 1 { "y" } else { "ies" }
        ),
    )
}

// --- 1d. Flatpak filesystem-permission gap ---

/// Best-effort check of whether sandboxed Flatpak mpv likely has filesystem
/// access to the user's tracked roots. Never FAILs — sandbox permissions are
/// a security decision the user makes deliberately (CLAUDE.md), and this
/// check's own confidence is inherently limited (a plain substring scan over
/// `flatpak info --show-permissions` output, not a real path-by-path
/// verification), so WARN is the ceiling regardless of what's found. Every
/// input here is already resolved by the caller — no subprocess is spawned
/// in this module, keeping the check itself pure and fabricated-environment
/// testable; `main.rs` is the one place that actually runs `flatpak`.
pub fn check_flatpak_permissions(
    flatpak_present: bool,
    flatpak_cli_present: bool,
    permissions_output: Option<&str>,
) -> CheckResult {
    if !flatpak_present {
        return CheckResult::warn("flatpak-permissions", "skipped — Flatpak mpv not detected");
    }
    if !flatpak_cli_present {
        return CheckResult::warn(
            "flatpak-permissions",
            "the `flatpak` CLI was not found — cannot verify sandboxed mpv's filesystem access; \
             check manually with `flatpak info --show-permissions io.mpv.Mpv`",
        );
    }
    let Some(output) = permissions_output else {
        return CheckResult::warn(
            "flatpak-permissions",
            "could not run `flatpak info --show-permissions io.mpv.Mpv` — cannot verify \
             sandboxed mpv's filesystem access",
        );
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
        CheckResult::ok(
            "flatpak-permissions",
            "Flatpak mpv has broad host/home filesystem access — tracked roots are likely reachable",
        )
    } else {
        CheckResult::warn(
            "flatpak-permissions",
            "Flatpak mpv's filesystem permissions may not cover your tracked roots — verify \
             manually with `flatpak info --show-permissions io.mpv.Mpv` (grant broader access \
             with e.g. `flatpak override --filesystem=host io.mpv.Mpv` if needed)",
        )
    }
}

// --- 1e. tsuzuki.lua companion script ---

pub struct ScriptCheckOutcome {
    pub result: CheckResult,
    /// `Some(scripts_dir)` when installing/updating would fix this —
    /// `--fix` copies `BUNDLED_SCRIPT` there.
    pub needs_fix: Option<PathBuf>,
}

/// Checks one install's `scripts/tsuzuki.lua` against the bundled copy — a
/// direct byte-for-byte comparison, not a hash: the file is tiny (a handful
/// of KB), so a real comparison costs nothing extra and carries zero
/// collision risk, unlike a hash's only advantage (comparing digests instead
/// of content) which doesn't matter at this size.
pub fn check_tsuzuki_lua(scripts_dir: &Path, label: &str) -> ScriptCheckOutcome {
    let script_path = scripts_dir.join("tsuzuki.lua");
    let full_label = format!("tsuzuki.lua ({label})");

    match fs::read(&script_path) {
        Ok(bytes) if bytes == BUNDLED_SCRIPT.as_bytes() => ScriptCheckOutcome {
            result: CheckResult::ok(
                full_label,
                format!("up to date at {}", script_path.display()),
            ),
            needs_fix: None,
        },
        Ok(_) => ScriptCheckOutcome {
            result: CheckResult::fail(
                full_label,
                format!(
                    "installed but out of date (differs from the bundled contrib/mpv/tsuzuki.lua) \
                     at {} — run `tsuzuki doctor --fix` to update it",
                    script_path.display()
                ),
            ),
            needs_fix: Some(scripts_dir.to_path_buf()),
        },
        Err(_) => ScriptCheckOutcome {
            result: CheckResult::fail(
                full_label,
                format!(
                    "not installed at {} — run `tsuzuki doctor --fix` to install it",
                    script_path.display()
                ),
            ),
            needs_fix: Some(scripts_dir.to_path_buf()),
        },
    }
}

/// Installs/updates `tsuzuki.lua` in `scripts_dir`, atomically (temp file in
/// the same directory, then rename over it) — the same discipline
/// `state.rs`'s own atomic write uses, applied here since this is the one
/// file-writing path `--fix` has.
pub fn install_script(scripts_dir: &Path) -> io::Result<()> {
    fs::create_dir_all(scripts_dir)?;
    let target = scripts_dir.join("tsuzuki.lua");
    let mut temp = tempfile::NamedTempFile::new_in(scripts_dir)?;
    io::Write::write_all(&mut temp, BUNDLED_SCRIPT.as_bytes())?;
    temp.persist(&target).map_err(|e| e.error)?;
    Ok(())
}

// --- 1f. mpv-log directory reachability ---

/// Checks every candidate script-log directory
/// (`script_log::mpv_log_dirs`'s own list). A missing directory is `Ok` —
/// indistinguishable from (and just as normal as) "the script was only just
/// installed and mpv hasn't quit a file yet" — but an existing, unreadable
/// one (permissions) is a real, actionable problem.
pub fn check_mpv_log_dirs(dirs: &[PathBuf]) -> Vec<CheckResult> {
    dirs.iter()
        .map(|d| {
            if !d.exists() {
                CheckResult::ok(
                    "mpv-log",
                    format!(
                        "not created yet at {} (fine — the script may not be installed, or mpv \
                         hasn't quit a file while it's running, yet)",
                        d.display()
                    ),
                )
            } else {
                match fs::read_dir(d) {
                    Ok(_) => CheckResult::ok("mpv-log", format!("reachable at {}", d.display())),
                    Err(e) => CheckResult::fail(
                        "mpv-log",
                        format!("exists but is unreadable at {}: {e}", d.display()),
                    ),
                }
            }
        })
        .collect()
}

// --- 1g. tracked-root validity ---

/// One tracked root's own current validity, alongside its `CheckResult` —
/// callers need the plain `bool` too, to decide which roots are safe to run
/// per-series orphan detection under (1h must skip an unreachable root
/// entirely, never treat its absence as proof any series under it is gone).
pub struct RootCheck {
    pub root: String,
    pub valid: bool,
    pub result: CheckResult,
}

pub fn check_tracked_roots(config: &Config, home: Option<&Path>) -> Vec<RootCheck> {
    config
        .roots
        .paths
        .iter()
        .map(|root| {
            let expanded = roots::expand_tilde(Path::new(root), home);
            let valid = expanded.is_dir();
            let result = if valid {
                CheckResult::ok("tracked-root", format!("reachable: {root}"))
            } else {
                CheckResult::fail(
                    "tracked-root",
                    format!("does not exist or is not a directory: {root}"),
                )
            };
            RootCheck {
                root: root.clone(),
                valid,
                result,
            }
        })
        .collect()
}

// --- 1h. orphaned per-series state.toml records ---

#[derive(Debug, Clone, PartialEq)]
pub struct Orphan {
    pub root: String,
    pub name: String,
}

/// The exact command decision 2 hands back for clearing confirmed orphans —
/// kept as one named constant so the report, the doctor-without-`--fix`
/// hint, and CLAUDE.md's own reference to it can never drift apart.
pub const FORGET_ORPHANS_COMMAND: &str = "tsuzuki doctor --fix --forget-orphans";

/// Finds every `state.toml` record whose series folder is gone from under
/// its own root — but only for roots `valid_roots` actually lists as
/// currently reachable (per [`check_tracked_roots`]). A root that's
/// unreachable itself (unmounted drive, permission denied) is skipped
/// entirely for this check, never treated as "every series under it is
/// gone" — that would be a false-positive orphan flood the moment a drive
/// is simply unplugged, not a real per-series problem.
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
        return CheckResult::ok("orphaned-records", "no orphaned state.toml records found");
    }

    let mut message = String::from(
        "orphaned state.toml records found (series deleted, moved, or renamed out from under a \
         still-tracked root):\n",
    );
    for o in orphans {
        message.push_str(&format!("      \"{}\" (under {})\n", o.name, o.root));
    }
    message.push_str(&format!("    run `{FORGET_ORPHANS_COMMAND}` to clear them"));
    CheckResult::fail("orphaned-records", message)
}

/// Permanently removes every record in `orphans` from `state.series` — the
/// plain internal function `--fix --forget-orphans` calls; there is no
/// public command surface beyond `doctor` itself for this (CLAUDE.md's "no
/// standalone cleanup command"). Caller is responsible for printing what's
/// about to be deleted *before* calling this, and for persisting `state`
/// afterward.
pub fn forget_orphans(state: &mut State, orphans: &[Orphan]) {
    state
        .series
        .retain(|r| !orphans.iter().any(|o| o.root == r.root && o.name == r.name));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{SeriesRecord, Status};
    use std::fs as std_fs;
    use tempfile::tempdir;

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

    // --- 1b: write-filename-in-watch-later-config ---

    #[test]
    fn flag_state_present_as_yes() {
        assert_eq!(
            write_filename_flag_state("write-filename-in-watch-later-config=yes\n"),
            Some(true)
        );
    }

    #[test]
    fn flag_state_absent_entirely() {
        assert_eq!(write_filename_flag_state("volume=80\n"), None);
    }

    #[test]
    fn flag_state_commented_out_is_treated_as_absent() {
        assert_eq!(
            write_filename_flag_state("# write-filename-in-watch-later-config=yes\n"),
            None
        );
    }

    #[test]
    fn flag_state_bare_flag_form_implies_yes() {
        assert_eq!(
            write_filename_flag_state("write-filename-in-watch-later-config\n"),
            Some(true)
        );
    }

    #[test]
    fn flag_state_explicit_negation() {
        assert_eq!(
            write_filename_flag_state("no-write-filename-in-watch-later-config\n"),
            Some(false)
        );
    }

    #[test]
    fn flag_state_last_directive_wins() {
        assert_eq!(
            write_filename_flag_state(
                "write-filename-in-watch-later-config=yes\nno-write-filename-in-watch-later-config\n"
            ),
            Some(false)
        );
    }

    #[test]
    fn check_mpv_conf_missing_file_fails() {
        let dir = tempdir().unwrap();
        let install = MpvInstall {
            label: "native",
            config_dir: dir.path().join("mpv"),
        };
        let r = check_mpv_conf(&install);
        assert_eq!(r.level, Level::Fail);
        assert!(r.message.contains("does not exist"));
    }

    #[test]
    fn check_mpv_conf_present_and_set_is_ok() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join("mpv");
        std_fs::create_dir_all(&config_dir).unwrap();
        std_fs::write(
            config_dir.join("mpv.conf"),
            "write-filename-in-watch-later-config=yes\n",
        )
        .unwrap();
        let install = MpvInstall {
            label: "native",
            config_dir,
        };
        assert_eq!(check_mpv_conf(&install).level, Level::Ok);
    }

    #[test]
    fn check_mpv_conf_present_but_commented_out_fails() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join("mpv");
        std_fs::create_dir_all(&config_dir).unwrap();
        std_fs::write(
            config_dir.join("mpv.conf"),
            "# write-filename-in-watch-later-config=yes\nvolume=80\n",
        )
        .unwrap();
        let install = MpvInstall {
            label: "native",
            config_dir,
        };
        let r = check_mpv_conf(&install);
        assert_eq!(r.level, Level::Fail);
        // The file itself must be untouched — doctor never writes to it.
        let contents = std_fs::read_to_string(dir.path().join("mpv").join("mpv.conf")).unwrap();
        assert_eq!(
            contents,
            "# write-filename-in-watch-later-config=yes\nvolume=80\n"
        );
    }

    // --- 1c: watch_later reachability ---

    #[test]
    fn watch_later_os_default_mode_never_fails_when_nothing_exists() {
        let home = tempdir().unwrap();
        let config = Config::default();
        let r = check_watch_later(&config, Some(home.path()));
        assert_ne!(r.level, Level::Fail);
    }

    #[test]
    fn watch_later_override_resolving_is_ok() {
        let home = tempdir().unwrap();
        let override_dir = home.path().join("custom_watch_later");
        std_fs::create_dir_all(&override_dir).unwrap();
        let mut config = Config::default();
        config.mpv.watch_later_dir = override_dir.to_string_lossy().into_owned();
        let r = check_watch_later(&config, Some(home.path()));
        assert_eq!(r.level, Level::Ok);
    }

    #[test]
    fn watch_later_override_not_resolving_fails() {
        let home = tempdir().unwrap();
        let mut config = Config::default();
        config.mpv.watch_later_dir = home
            .path()
            .join("never_created")
            .to_string_lossy()
            .into_owned();
        let r = check_watch_later(&config, Some(home.path()));
        assert_eq!(r.level, Level::Fail);
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
        // Diagnostic-only, best-effort — CLAUDE.md's "sandbox permissions
        // are a security decision the user makes" means this check must
        // never escalate to FAIL under any input.
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

    // --- 1e: tsuzuki.lua companion script ---

    #[test]
    fn tsuzuki_lua_missing_needs_fix() {
        let dir = tempdir().unwrap();
        let outcome = check_tsuzuki_lua(&dir.path().join("scripts"), "native");
        assert_eq!(outcome.result.level, Level::Fail);
        assert!(outcome.needs_fix.is_some());
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
        assert_eq!(checks[0].result.level, Level::Ok);
    }

    #[test]
    fn tracked_root_missing_is_fail() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("GoneAnime");
        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());

        let checks = check_tracked_roots(&config, None);
        assert!(!checks[0].valid);
        assert_eq!(checks[0].result.level, Level::Fail);
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
        // The unmounted-drive case: the root itself doesn't currently
        // resolve, so per-series orphan detection must be skipped entirely
        // under it — never treated as "every series under it is gone."
        let dir = tempdir().unwrap();
        let root = dir.path().join("UnmountedAnime"); // deliberately never created

        let mut config = Config::default();
        config.roots.paths.push(root.to_string_lossy().into_owned());
        let state = State {
            series: vec![orphan_record(&root, "Monster")],
        };

        // Caller passes an EMPTY valid_roots list, since check_tracked_roots
        // would have marked this root invalid.
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
        assert!(r.message.contains("tsuzuki doctor --fix --forget-orphans"));
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
}
