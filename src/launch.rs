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
/// filesystem check, not a spawn.
fn native_mpv_on_path() -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path_var).any(|dir| dir.join("mpv").is_file())
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

/// Spawns `resolved`'s program with `file` and `--start=<start_seconds>`
/// appended, and immediately lets go — no `wait()`, no re-scan. A `Child`
/// handle dropped without `wait()` does not terminate the process; this is
/// exactly the fire-and-forget behavior CLAUDE.md specifies. `stdout`/
/// `stderr`/`stdin` are set to `null` so mpv's own console UI doesn't
/// interleave with Tsuzuki's, since Tsuzuki has already returned control to
/// the shell by the time mpv would print anything.
fn spawn(resolved: &ResolvedMpv, file: &Path, start_seconds: f64) -> io::Result<()> {
    Command::new(&resolved.program)
        .args(&resolved.base_args)
        .arg(file)
        .arg(format!("--start={start_seconds}"))
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

    #[test]
    fn spawn_failure_on_a_nonexistent_program_returns_a_typed_error_not_a_panic() {
        let resolved = ResolvedMpv {
            program: "/definitely/does/not/exist/tsuzuki-fake-mpv".to_string(),
            base_args: Vec::new(),
        };
        let result = spawn(&resolved, Path::new("/x.mkv"), 0.0);
        assert!(result.is_err());
    }
}
