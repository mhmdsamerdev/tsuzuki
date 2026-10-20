//! Launching mpv to resume playback.
//!
//! The launch is a fire-and-forget `spawn` of the target `mpv::resolve`
//! picked; this module never detects mpv itself.

use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::mpv::Target;

/// Spawns mpv on `file` and returns without waiting. stdio is nulled so mpv's
/// console output doesn't interleave with the shell.
///
/// `--start` is wrapped in `--{ --}` so it applies to `file` only; a bare
/// `--start` is session-wide and the next playlist entry would inherit it.
/// No shell is involved, so the braces need no escaping.
pub fn resume_playback(target: &Target, file: &Path, start_seconds: f64) -> io::Result<()> {
    Command::new(&target.program)
        .args(&target.base_args)
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

    #[test]
    fn resume_playback_of_a_nonexistent_program_returns_an_error_not_a_panic() {
        let target = custom_target("/definitely/does/not/exist/tsuzuki-fake-mpv", &[]);
        let err = resume_playback(&target, Path::new("/x.mkv"), 0.0).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
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
        resume_playback(&target, Path::new("/x.mkv"), 42.5).unwrap();

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
