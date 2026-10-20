//! Launching mpv to resume playback.
//!
//! The launch is a fire-and-forget `spawn` of the target `mpv::resolve`
//! picked; this module never detects mpv itself. mpv's own output goes to a
//! log file so a launch that dies at once can still be diagnosed.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::mpv::Target;

/// The mpv to spawn and where its output goes.
pub struct Launch {
    pub target: Target,
    /// Overwritten on every launch.
    pub log_path: PathBuf,
}

/// The launch log, next to `state_path`.
pub fn log_path(state_path: &Path) -> PathBuf {
    state_path.with_file_name("mpv-launch.log")
}

/// Spawns mpv on `file` and returns without waiting. stdin is nulled; stdout
/// and stderr go to the launch log, which starts with the command line. If the
/// log can't be opened, they are nulled too.
///
/// `--start` is wrapped in `--{ --}` so it applies to `file` only; a bare
/// `--start` is session-wide and the next playlist entry would inherit it.
/// No shell is involved, so the braces need no escaping.
pub fn resume_playback(launch: &Launch, file: &Path, start_seconds: f64) -> io::Result<()> {
    let target = &launch.target;
    let mut command = Command::new(&target.program);
    command
        .args(&target.base_args)
        .arg("--{")
        .arg(format!("--start={start_seconds}"))
        .arg(file)
        .arg("--}")
        .stdin(Stdio::null());
    match open_log(&launch.log_path, &command) {
        Some((stdout, stderr)) => command.stdout(stdout).stderr(stderr),
        None => command.stdout(Stdio::null()).stderr(Stdio::null()),
    };
    command.spawn()?;
    Ok(())
}

/// Truncates the log and writes `command`'s line to it, then returns two
/// handles to it for the child's stdout and stderr.
fn open_log(path: &Path, command: &Command) -> Option<(Stdio, Stdio)> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok()?;
    }
    let mut log = File::create(path).ok()?;
    let line = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    writeln!(log, "# {line}").ok()?;
    let stderr = log.try_clone().ok()?;
    Some((log.into(), stderr.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mpv::{ChosenBy, Kind};

    fn custom_target(program: &str, base_args: &[&str]) -> Target {
        Target {
            kind: Kind::Custom,
            chosen_by: ChosenBy::Pinned,
            program: program.to_string(),
            base_args: base_args.iter().map(|s| s.to_string()).collect(),
            script_path: None,
        }
    }

    fn launch_of(target: Target, log_path: PathBuf) -> Launch {
        Launch { target, log_path }
    }

    #[test]
    fn resume_playback_of_a_nonexistent_program_returns_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let target = custom_target("/definitely/does/not/exist/tsuzuki-fake-mpv", &[]);
        let launch = launch_of(target, dir.path().join("mpv-launch.log"));
        let err = resume_playback(&launch, Path::new("/x.mkv"), 0.0).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn log_path_sits_next_to_the_state_file() {
        assert_eq!(
            log_path(Path::new("/data/tsuzuki/state.toml")),
            PathBuf::from("/data/tsuzuki/mpv-launch.log")
        );
    }

    #[test]
    fn open_log_with_an_unopenable_path_yields_no_handles() {
        let dir = tempfile::tempdir().unwrap();
        let blocking_file = dir.path().join("file");
        fs::write(&blocking_file, b"x").unwrap();
        let command = Command::new("mpv");
        assert!(open_log(&blocking_file.join("mpv-launch.log"), &command).is_none());
        assert!(open_log(dir.path(), &command).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn start_is_scoped_to_the_file_with_per_file_option_braces_after_base_args() {
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
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}.tmp && mv {0}.tmp {0}\n",
                argv_out.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&fake_mpv, fs::Permissions::from_mode(0o755)).unwrap();

        let target = custom_target(&fake_mpv.to_string_lossy(), &["run", "io.mpv.Mpv"]);
        let launch = launch_of(target, dir.path().join("log/mpv-launch.log"));
        resume_playback(&launch, Path::new("/x.mkv"), 42.5).unwrap();

        // Poll with a deadline until the child writes its output.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !argv_out.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let captured = fs::read_to_string(&argv_out).unwrap();
        let args: Vec<&str> = captured.lines().collect();
        assert_eq!(
            args,
            vec!["run", "io.mpv.Mpv", "--{", "--start=42.5", "/x.mkv", "--}"]
        );
    }
}
