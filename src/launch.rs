//! Launching mpv to resume playback.
//!
//! The launch is a fire-and-forget `spawn`. Native vs. Flatpak mpv is
//! resolved by a cheap existence check (PATH lookup, install-directory
//! check), never by trying a spawn and catching the failure.

use std::fmt;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

/// The program to run plus leading arguments that precede the file arguments
/// (e.g. `["run", "io.mpv.Mpv"]` for Flatpak, or a `command_override` split
/// on whitespace).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMpv {
    pub program: String,
    pub base_args: Vec<String>,
}

#[derive(Debug)]
pub enum LaunchError {
    /// No `command_override`, native mpv, or Flatpak install was found.
    NotResolvable,
    /// Resolution succeeded but `spawn` failed (e.g. permissions).
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

/// Resolves what to run: `command_override` first (detection is skipped),
/// then native mpv, then Flatpak. The existence checks are injected so
/// resolution is testable without the real `PATH` or filesystem.
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

/// True if an executable named `mpv` exists on `$PATH`. Shared with `doctor.rs`.
pub(crate) fn native_mpv_on_path() -> bool {
    program_on_path("mpv")
}

/// True if an executable named `name` exists on `$PATH`. Shared with `doctor.rs`.
pub(crate) fn program_on_path(name: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path_var).any(|dir| is_executable_file(&dir.join(name)))
}

/// True for a regular file with an execute bit set, so a non-executable file
/// that shares the name is rejected at resolution time. One `stat`, no spawn.
#[cfg(unix)]
pub(crate) fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
pub(crate) fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

/// True if `io.mpv.Mpv` is installed system-wide or under `home`, checked by
/// directory existence. Shared with `doctor.rs`.
pub(crate) fn flatpak_mpv_installed(home: Option<&Path>) -> bool {
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

/// Spawns mpv on `file` and returns without waiting. stdio is nulled so mpv's
/// console output doesn't interleave with the shell.
///
/// `--start` is wrapped in `--{ --}` so it applies to `file` only; a bare
/// `--start` is session-wide and the next playlist entry would inherit it.
/// No shell is involved, so the braces need no escaping.
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

/// Resolves mpv against the real `PATH`/filesystem and launches it on `file`
/// at `start_seconds`.
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
        // A `command_override` skips detection, so this is machine-independent
        // (the `NotResolvable` path would depend on the host's PATH and Flatpak).
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
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let fake_mpv = dir.path().join("mpv");
        fs::write(&fake_mpv, b"not actually a binary").unwrap();
        let mut perms = fs::metadata(&fake_mpv).unwrap().permissions();
        perms.set_mode(0o644); // readable/writable, deliberately not executable
        fs::set_permissions(&fake_mpv, perms).unwrap();

        assert!(!is_executable_file(&fake_mpv));

        // Setting the execute bit makes it count.
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
        // A fake "mpv" script dumps its argv, since the arguments can't be
        // inspected without spawning.
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let argv_out = dir.path().join("argv.txt");
        let fake_mpv = dir.path().join("mpv");
        fs::write(
            &fake_mpv,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n",
                argv_out.display()
            ),
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

        // Poll with a deadline until the child writes its output.
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
