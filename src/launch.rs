//! Launching mpv to resume playback (CLAUDE.md's "Launching mpv:
//! fire-and-forget, cheap existence check before spawn, install-method
//! detection").
//!
//! Resuming is a real, core feature — unlike `watch_later` reading
//! (`mpv.rs`), which never launches anything — but the launch itself is a
//! plain fire-and-forget `spawn`, never awaited, never followed by an
//! immediate re-scan. Native vs. Flatpak mpv is resolved by a cheap
//! existence check (PATH lookup / known install-directory check), never by
//! attempting a spawn and catching its failure — a failed spawn attempt is
//! real, wasted process-launch overhead against this project's own
//! startup-latency rationale.

use std::fmt;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

/// What was resolved to actually run: a program name/path plus any leading
/// arguments that must precede the file/`--start=` arguments (e.g.
/// `["run", "io.mpv.Mpv"]` for Flatpak, or a user's own `command_override`
/// split on whitespace).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMpv {
    pub program: String,
    pub base_args: Vec<String>,
}

#[derive(Debug)]
pub enum LaunchError {
    /// Neither a `command_override`, nor native mpv on `PATH`, nor a
    /// Flatpak install could be resolved.
    NotResolvable,
    /// Resolution succeeded but the actual `spawn` call itself failed
    /// (missing binary despite passing the existence check, permissions,
    /// ...). Never a panic.
    Spawn(io::Error),
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaunchError::NotResolvable => write!(
                f,
                "could not find mpv — install it, or set [mpv] command_override in config.toml"
            ),
            LaunchError::Spawn(e) => write!(f, "failed to launch mpv: {e}"),
        }
    }
}

/// Resolves what to run, in priority order: an explicit `command_override`
/// bypasses detection entirely (its closures are never called — proven by a
/// test passing panicking closures); otherwise a cheap native-mpv-on-PATH
/// check; otherwise a cheap Flatpak-install-directory check. `native_exists`/
/// `flatpak_exists` are injected so this resolution logic is testable
/// without depending on the real `PATH` or filesystem.
pub fn resolve_mpv_command(
    command_override: &str,
    native_exists: impl FnOnce() -> bool,
    flatpak_exists: impl FnOnce() -> bool,
) -> Option<ResolvedMpv> {
    if !command_override.is_empty() {
        let mut parts = command_override.split_whitespace();
        let program = parts.next()?.to_string();
        let base_args = parts.map(|s| s.to_string()).collect();
        return Some(ResolvedMpv { program, base_args });
    }

    if native_exists() {
        return Some(ResolvedMpv {
            program: "mpv".to_string(),
            base_args: Vec::new(),
        });
    }

    if flatpak_exists() {
        return Some(ResolvedMpv {
            program: "flatpak".to_string(),
            base_args: vec!["run".to_string(), "io.mpv.Mpv".to_string()],
        });
    }

    None
}

/// True if an executable named `mpv` exists somewhere on `$PATH` — a plain
/// filesystem check, not a spawn. Checks the executable permission bit, not
/// just file presence: `is_file()` alone would report success for a
/// non-executable file that merely happens to be named `mpv`, which would
/// then only fail — still cleanly, as a typed `Spawn` error, never a panic —
/// at actual launch time instead of being correctly rejected during
/// resolution. The permission check is still a single `stat` call, not a
/// spawn, so it stays exactly as cheap as CLAUDE.md's "cheap existence
/// check" requires.
fn native_mpv_on_path() -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path_var).any(|dir| is_executable_file(&dir.join("mpv")))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

/// Known Flatpak app-install locations for `io.mpv.Mpv` — a plain directory
/// existence check, not a `flatpak` invocation.
fn flatpak_mpv_installed(home: Option<&Path>) -> bool {
    const SYSTEM_INSTALL: &str = "/var/lib/flatpak/app/io.mpv.Mpv";
    if Path::new(SYSTEM_INSTALL).is_dir() {
        return true;
    }
    if let Some(home) = home
        && home.join(".local/share/flatpak/app/io.mpv.Mpv").is_dir()
    {
        return true;
    }
    false
}

/// Spawns `resolved`'s program with `--start=<start_seconds>` scoped to
/// `file` via mpv's per-file option braces (`--{ --start=… file --}`), and
/// immediately lets go — no `wait()`, no re-scan. A `Child` handle dropped
/// without `wait()` does not terminate the process; this is exactly the
/// fire-and-forget behavior CLAUDE.md specifies. `stdout`/`stderr`/`stdin`
/// are set to `null` so mpv's own console UI doesn't interleave with
/// Tsuzuki's, since Tsuzuki has already returned control to the shell by the
/// time mpv would print anything.
///
/// The braces matter: a bare trailing `--start=` is a command-line option,
/// which mpv treats as a session-wide default rather than a property of
/// just this file — so once mpv advances past `file` on its own (EOF
/// auto-advance, an autoload script, `playlist-next`), the *next* file
/// would silently inherit the same start offset. Scoping `--start` inside
/// `--{ --}` attaches it only to the enclosed playlist entry; anything
/// loaded afterward falls back to mpv's normal default (0). No shell sits
/// between this and `execve`, so `--{`/`--}` need no escaping here despite
/// the mpv manual's warning about that for interactive shell use.
fn spawn(resolved: &ResolvedMpv, file: &Path, start_seconds: f64) -> io::Result<()> {
    Command::new(&resolved.program)
        .args(&resolved.base_args)
        .arg("--{")
        .arg(format!("--start={start_seconds}"))
        .arg(file)
        .arg("--}")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

/// Resolves mpv (via `command_override`, then native, then Flatpak, using
/// the real `PATH`/filesystem checks) and launches it on `file` at
/// `start_seconds`. The one entry point production code (`main.rs`) calls;
/// `resolve_mpv_command`/`spawn` stay separately unit-testable above.
pub fn resume_playback(
    command_override: &str,
    home: Option<&Path>,
    file: &Path,
    start_seconds: f64,
) -> Result<(), LaunchError> {
    let resolved = resolve_mpv_command(command_override, native_mpv_on_path, || {
        flatpak_mpv_installed(home)
    })
    .ok_or(LaunchError::NotResolvable)?;
    spawn(&resolved, file, start_seconds).map_err(LaunchError::Spawn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_present_picks_native_without_attempting_flatpak_first() {
        let resolved = resolve_mpv_command(
            "",
            || true,
            || panic!("flatpak_exists must not be called when native mpv is present"),
        );
        assert_eq!(
            resolved,
            Some(ResolvedMpv {
                program: "mpv".to_string(),
                base_args: Vec::new(),
            })
        );
    }

    #[test]
    fn falls_back_to_flatpak_when_native_is_absent() {
        let resolved = resolve_mpv_command("", || false, || true);
        assert_eq!(
            resolved,
            Some(ResolvedMpv {
                program: "flatpak".to_string(),
                base_args: vec!["run".to_string(), "io.mpv.Mpv".to_string()],
            })
        );
    }

    #[test]
    fn command_override_bypasses_detection_entirely() {
        let resolved = resolve_mpv_command(
            "flatpak run io.mpv.Mpv",
            || panic!("native_exists must not be called when command_override is set"),
            || panic!("flatpak_exists must not be called when command_override is set"),
        );
        assert_eq!(
            resolved,
            Some(ResolvedMpv {
                program: "flatpak".to_string(),
                base_args: vec!["run".to_string(), "io.mpv.Mpv".to_string()],
            })
        );
    }

    #[test]
    fn command_override_with_a_single_word_has_no_base_args() {
        let resolved = resolve_mpv_command("mpv", || panic!("unused"), || panic!("unused"));
        assert_eq!(
            resolved,
            Some(ResolvedMpv {
                program: "mpv".to_string(),
                base_args: Vec::new(),
            })
        );
    }

    #[test]
    fn neither_native_nor_flatpak_available_returns_none() {
        let resolved = resolve_mpv_command("", || false, || false);
        assert_eq!(resolved, None);
    }

    #[test]
    fn resume_playback_returns_a_typed_spawn_error_never_panics_when_resolution_fails_to_launch() {
        // Exercises `resume_playback`'s real entry point (not the injectable
        // `resolve_mpv_command` directly) end-to-end. Forcing the "neither
        // native nor Flatpak resolves" branch would require mutating the
        // process-global `PATH` env var and depend on the test machine
        // having no Flatpak mpv install (this one genuinely does, at
        // `/var/lib/flatpak/app/io.mpv.Mpv` — a real, not injectable,
        // constant in `flatpak_mpv_installed`) — both properties this test
        // shouldn't depend on. `command_override` bypasses detection
        // entirely and deterministically, on any machine, points at a
        // program that can't actually be spawned, proving the same "typed
        // error, never a panic" property via the `Spawn` variant instead.
        let result = resume_playback(
            "/definitely/does/not/exist/tsuzuki-fake-mpv",
            None,
            Path::new("/x.mkv"),
            0.0,
        );
        assert!(matches!(result, Err(LaunchError::Spawn(_))));
    }

    #[cfg(unix)]
    #[test]
    fn a_non_executable_file_named_mpv_on_path_is_not_resolved_as_native() {
        // Sweep finding: `native_mpv_on_path` originally used `is_file()`
        // alone, which can't distinguish "exists" from "is actually
        // runnable" — a stray non-executable file literally named `mpv`
        // sitting on PATH would have falsely resolved as native, only
        // failing later at spawn time. This proves the fix at the real
        // `is_executable_file` level: a file with no execute bit set must
        // not be reported as present.
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let fake_mpv = dir.path().join("mpv");
        fs::write(&fake_mpv, b"not actually a binary").unwrap();
        let mut perms = fs::metadata(&fake_mpv).unwrap().permissions();
        perms.set_mode(0o644); // readable/writable, deliberately not executable
        fs::set_permissions(&fake_mpv, perms).unwrap();

        assert!(!is_executable_file(&fake_mpv));

        // And the inverse: marking it executable makes it resolve.
        let mut perms = fs::metadata(&fake_mpv).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake_mpv, perms).unwrap();
        assert!(is_executable_file(&fake_mpv));
    }

    #[test]
    fn spawn_failure_on_a_nonexistent_program_returns_a_typed_error_not_a_panic() {
        let resolved = ResolvedMpv {
            program: "/definitely/does/not/exist/tsuzuki-fake-mpv".to_string(),
            base_args: Vec::new(),
        };
        let result = spawn(&resolved, Path::new("/x.mkv"), 0.0);
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn start_is_scoped_to_the_file_with_per_file_option_braces() {
        // Regression test for the dogfood-found bug: a bare trailing
        // `--start=` is a session-wide default in mpv, so it silently
        // leaked onto whatever file mpv played next (autoload, EOF
        // auto-advance, playlist-next). Wrapping it in `--{ --}` scopes it
        // to just the one file passed here. Uses a fake "mpv" shell script
        // that dumps its own argv, since this is the one property that
        // can't be checked by inspecting `Command` without actually
        // spawning something.
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let argv_out = dir.path().join("argv.txt");
        let fake_mpv = dir.path().join("mpv");
        fs::write(
            &fake_mpv,
            format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n", argv_out.display()),
        )
        .unwrap();
        let mut perms = fs::metadata(&fake_mpv).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake_mpv, perms).unwrap();

        let resolved = ResolvedMpv {
            program: fake_mpv.to_string_lossy().to_string(),
            base_args: Vec::new(),
        };
        spawn(&resolved, Path::new("/x.mkv"), 42.5).unwrap();

        // Give the fake script a moment to run and write its output.
        for _ in 0..100 {
            if argv_out.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let captured = fs::read_to_string(&argv_out).unwrap();
        let args: Vec<&str> = captured.lines().collect();
        assert_eq!(args, vec!["--{", "--start=42.5", "/x.mkv", "--}"]);
    }
}
