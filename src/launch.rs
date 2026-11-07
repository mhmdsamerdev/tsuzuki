//! Launching mpv to resume playback.
//!
//! The launch is a fire-and-forget `spawn` of the target `mpv::resolve`
//! picked; this module never detects mpv itself. mpv's own output goes to a
//! log file so a launch that dies at once can still be diagnosed.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use crate::config::AniCli;
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

/// The ani-cli run that plays `episode` of `title` through the mpv in
/// `launch`: its search path (`-e`, exact title, first result), never `-c`.
/// A `start_seconds` above 0 reaches mpv as `--start` through
/// `ANI_CLI_PLAYER_FLAGS`, after any flags already in `existing_flags`.
/// `--exit-after-play` makes ani-cli run the player in the foreground and
/// exit with it. `ANI_CLI_MENU=true` gives its "next/replay/…" menu an empty
/// answer, so no episode after this one is ever played.
pub fn ani_cli_command(
    launch: &Launch,
    config: &AniCli,
    title: &str,
    episode: u32,
    start_seconds: f64,
    existing_flags: Option<&str>,
) -> Command {
    let player = std::iter::once(launch.target.program.as_str())
        .chain(launch.target.base_args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    let mut command = Command::new(&config.command);
    command
        .args(&config.args)
        .args(["-S", "1", "-e"])
        .arg(episode.to_string())
        .arg("--exit-after-play")
        .arg(title)
        .env("ANI_CLI_EXACT", "1")
        .env("ANI_CLI_MENU", "true")
        .env("ANI_CLI_PLAYER", player);
    if start_seconds > 0.0 {
        let start = format!("--start={start_seconds}");
        let flags = match existing_flags.filter(|f| !f.trim().is_empty()) {
            Some(existing) => format!("{start} {existing}"),
            None => start,
        };
        command.env("ANI_CLI_PLAYER_FLAGS", flags);
    }
    command
}

/// What ani-cli prints when the requested episode isn't in the episode list
/// it just fetched.
const INVALID_EPISODE: &str = "Invalid episode!";

/// What ani-cli prints once it has the links, after the episode check: past
/// this point `Invalid episode!` can no longer come.
const LINKS_FETCHED: &str = "links fetched";

/// How long `start_ani_cli` follows ani-cli's start before leaving it.
pub const ANI_CLI_START_WAIT: Duration = Duration::from_secs(30);

const LOG_POLL: Duration = Duration::from_millis(50);

/// The ani-cli launch log, next to the mpv launch log.
pub fn ani_cli_log_path(launch: &Launch) -> PathBuf {
    launch.log_path.with_file_name("ani-cli-launch.log")
}

/// How following ani-cli's start ended.
#[derive(Debug, PartialEq, Eq)]
pub enum AniCliRun {
    /// ani-cli said the episode isn't in its list.
    InvalidEpisode,
    /// ani-cli exited unsuccessfully before playing.
    Failed(ExitStatus),
    /// ani-cli exited successfully inside the wait.
    Finished,
    /// ani-cli got its links and is still running; nothing more to learn.
    Playing,
    /// The wait ran out with ani-cli still starting.
    TimedOut,
}

#[derive(Debug, PartialEq, Eq)]
enum Marker {
    InvalidEpisode,
    LinksFetched,
}

/// The decisive marker in ani-cli's output so far. `Invalid episode!` wins:
/// ani-cli can't print both.
fn classify_log(text: &str) -> Option<Marker> {
    if text.contains(INVALID_EPISODE) {
        Some(Marker::InvalidEpisode)
    } else if text.contains(LINKS_FETCHED) {
        Some(Marker::LinksFetched)
    } else {
        None
    }
}

/// Starts `command` (ani-cli) with stdin nulled and stdout and stderr in the
/// log at `log_path`, then follows the log, echoing it to stderr, until
/// ani-cli exits, reports `Invalid episode!`, prints `links fetched`, or
/// `wait` runs out. ani-cli is never waited on past that: it keeps playing
/// after this returns.
pub fn start_ani_cli(
    mut command: Command,
    log_path: &Path,
    wait: Duration,
) -> io::Result<AniCliRun> {
    command.stdin(Stdio::null());
    let (stdout, stderr) = open_log(log_path, &command)
        .ok_or_else(|| io::Error::other(format!("could not open {}", log_path.display())))?;
    command.stdout(stdout).stderr(stderr);
    let mut reader = File::open(log_path)?;
    reader.seek(SeekFrom::End(0))?;
    let mut child = command.spawn()?;
    let deadline = Instant::now() + wait;
    let mut seen = Vec::new();
    loop {
        let exited = child.try_wait()?;
        follow(&mut reader, &mut seen)?;
        match (classify_log(&String::from_utf8_lossy(&seen)), exited) {
            (Some(Marker::InvalidEpisode), _) => return Ok(AniCliRun::InvalidEpisode),
            (Some(Marker::LinksFetched), None) => return Ok(AniCliRun::Playing),
            (_, Some(status)) if status.success() => return Ok(AniCliRun::Finished),
            (_, Some(status)) => return Ok(AniCliRun::Failed(status)),
            (_, None) if Instant::now() >= deadline => return Ok(AniCliRun::TimedOut),
            (_, None) => std::thread::sleep(LOG_POLL),
        }
    }
}

/// Reads what ani-cli has logged since the last call into `seen` and echoes
/// it to stderr.
fn follow(reader: &mut File, seen: &mut Vec<u8>) -> io::Result<()> {
    let from = seen.len();
    reader.read_to_end(seen)?;
    let mut out = io::stderr();
    let _ = out.write_all(&seen[from..]);
    let _ = out.flush();
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

    #[cfg(unix)]
    fn shell_command(dir: &Path, body: &str) -> Command {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("fake-ani-cli");
        fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        Command::new(script)
    }

    fn run_fake(dir: &Path, body: &str, wait: Duration) -> (io::Result<AniCliRun>, String) {
        let log = dir.join("ani-cli-launch.log");
        let run = start_ani_cli(shell_command(dir, body), &log, wait);
        (run, fs::read_to_string(&log).unwrap_or_default())
    }

    const WAIT: Duration = Duration::from_secs(10);

    #[test]
    fn classify_log_finds_the_decisive_marker_and_ignores_other_output() {
        assert_eq!(
            classify_log("\u{1b}[1;31mInvalid episode!\u{1b}[0m\n"),
            Some(Marker::InvalidEpisode)
        );
        assert_eq!(
            classify_log("\u{1b}[2K\r\u{1b}[1;34mhianime.at links fetched\u{1b}[0m\n"),
            Some(Marker::LinksFetched)
        );
        assert_eq!(classify_log("Episode not released!\n"), None);
        assert_eq!(classify_log(""), None);
    }

    #[test]
    fn classify_log_prefers_invalid_episode_when_both_appear() {
        assert_eq!(
            classify_log("hianime.at links fetched\nInvalid episode!\n"),
            Some(Marker::InvalidEpisode)
        );
    }

    #[cfg(unix)]
    #[test]
    fn start_ani_cli_reports_an_invalid_episode_message() {
        let dir = tempfile::tempdir().unwrap();

        let (run, log) = run_fake(
            dir.path(),
            "printf '\\033[1;31mInvalid episode!\\033[0m\\n' >&2; exit 1",
            WAIT,
        );

        assert_eq!(run.unwrap(), AniCliRun::InvalidEpisode);
        assert!(
            log.ends_with("\u{1b}[1;31mInvalid episode!\u{1b}[0m\n"),
            "{log:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn start_ani_cli_reports_a_failed_exit_without_a_marker() {
        let dir = tempfile::tempdir().unwrap();

        let (run, _) = run_fake(dir.path(), "echo 'Episode not released!' >&2; exit 1", WAIT);

        match run.unwrap() {
            AniCliRun::Failed(status) => assert_eq!(status.code(), Some(1)),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn start_ani_cli_reports_a_clean_exit_as_finished() {
        let dir = tempfile::tempdir().unwrap();

        let (run, _) = run_fake(dir.path(), "exit 0", WAIT);

        assert_eq!(run.unwrap(), AniCliRun::Finished);
    }

    #[cfg(unix)]
    #[test]
    fn start_ani_cli_returns_playing_while_ani_cli_is_still_running() {
        let dir = tempfile::tempdir().unwrap();
        let release = dir.path().join("release");
        let done = dir.path().join("done");
        let body = format!(
            "echo 'hianime.at links fetched'\ni=0\nwhile [ ! -e {} ] && [ $i -lt 200 ]; do sleep 0.05; i=$((i+1)); done\ntouch {}",
            release.display(),
            done.display()
        );

        let (run, log) = run_fake(dir.path(), &body, WAIT);

        assert_eq!(run.unwrap(), AniCliRun::Playing);
        assert!(!done.exists());
        assert!(log.ends_with("hianime.at links fetched\n"), "{log:?}");
        fs::write(&release, "").unwrap();
        let deadline = Instant::now() + WAIT;
        while !done.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(done.exists());
    }

    #[cfg(unix)]
    #[test]
    fn start_ani_cli_times_out_on_a_silent_ani_cli_that_keeps_running() {
        let dir = tempfile::tempdir().unwrap();

        let (run, _) = run_fake(dir.path(), "sleep 2", Duration::from_millis(100));

        assert_eq!(run.unwrap(), AniCliRun::TimedOut);
    }

    #[cfg(unix)]
    #[test]
    fn start_ani_cli_logs_the_command_line_then_ani_clis_output() {
        let dir = tempfile::tempdir().unwrap();

        let (_, log) = run_fake(dir.path(), "echo out; echo err >&2", WAIT);

        assert_eq!(
            log,
            format!("# {}/fake-ani-cli\nout\nerr\n", dir.path().display())
        );
    }

    #[cfg(unix)]
    #[test]
    fn start_ani_cli_gives_ani_cli_no_stdin() {
        let dir = tempfile::tempdir().unwrap();

        let (run, log) = run_fake(
            dir.path(),
            "if read line; then echo got-input; else echo no-input; fi",
            WAIT,
        );

        assert_eq!(run.unwrap(), AniCliRun::Finished);
        assert!(log.ends_with("no-input\n"), "{log:?}");
    }

    #[test]
    fn start_ani_cli_of_a_nonexistent_program_returns_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();

        let err = start_ani_cli(
            Command::new("/definitely/does/not/exist/tsuzuki-fake-ani-cli"),
            &dir.path().join("ani-cli-launch.log"),
            WAIT,
        )
        .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn start_ani_cli_with_an_unopenable_log_returns_an_error_before_running_it() {
        let dir = tempfile::tempdir().unwrap();
        let blocking_file = dir.path().join("file");
        fs::write(&blocking_file, b"x").unwrap();

        let err = start_ani_cli(
            Command::new("/definitely/does/not/exist/tsuzuki-fake-ani-cli"),
            &blocking_file.join("ani-cli-launch.log"),
            WAIT,
        )
        .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::Other);
    }

    #[test]
    fn ani_cli_log_path_sits_next_to_the_mpv_launch_log() {
        let launch = launch_of(
            custom_target("mpv", &[]),
            PathBuf::from("/data/tsuzuki/mpv-launch.log"),
        );

        assert_eq!(
            ani_cli_log_path(&launch),
            PathBuf::from("/data/tsuzuki/ani-cli-launch.log")
        );
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

    // --- ani-cli ---

    fn ani_config(args: &[&str]) -> AniCli {
        AniCli {
            enabled: true,
            history_path: String::new(),
            command: "ani-cli".to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        }
    }

    type Env = Vec<(String, Option<String>)>;

    fn command_parts(command: &Command) -> (String, Vec<String>, Env) {
        let mut envs: Env = command
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        envs.sort();
        (
            command.get_program().to_string_lossy().into_owned(),
            command
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect(),
            envs,
        )
    }

    fn env(key: &str, value: &str) -> (String, Option<String>) {
        (key.to_string(), Some(value.to_string()))
    }

    #[test]
    fn ani_cli_command_replays_an_episode_from_a_position_through_the_search_path() {
        let launch = launch_of(custom_target("mpv", &[]), PathBuf::from("log"));

        let command = ani_cli_command(&launch, &ani_config(&[]), "Blue Lock", 9, 312.5, None);

        assert_eq!(
            command_parts(&command),
            (
                "ani-cli".to_string(),
                ["-S", "1", "-e", "9", "--exit-after-play", "Blue Lock"]
                    .map(String::from)
                    .to_vec(),
                vec![
                    env("ANI_CLI_EXACT", "1"),
                    env("ANI_CLI_MENU", "true"),
                    env("ANI_CLI_PLAYER", "mpv"),
                    env("ANI_CLI_PLAYER_FLAGS", "--start=312.5"),
                ],
            )
        );
    }

    #[test]
    fn ani_cli_command_from_the_start_sets_no_player_flags_so_the_users_stay_untouched() {
        let launch = launch_of(custom_target("mpv", &[]), PathBuf::from("log"));

        let command = ani_cli_command(
            &launch,
            &ani_config(&[]),
            "Blue Lock",
            10,
            0.0,
            Some("--fs"),
        );

        let (_, _, envs) = command_parts(&command);
        assert_eq!(
            envs,
            vec![
                env("ANI_CLI_EXACT", "1"),
                env("ANI_CLI_MENU", "true"),
                env("ANI_CLI_PLAYER", "mpv")
            ]
        );
    }

    #[test]
    fn ani_cli_command_puts_start_before_the_users_existing_player_flags() {
        let launch = launch_of(custom_target("mpv", &[]), PathBuf::from("log"));

        let with_flags = ani_cli_command(
            &launch,
            &ani_config(&[]),
            "X",
            1,
            60.0,
            Some("--fs --volume=50"),
        );
        let blank_flags = ani_cli_command(&launch, &ani_config(&[]), "X", 1, 60.0, Some("  "));

        let flags = |c: &Command| {
            command_parts(c)
                .2
                .into_iter()
                .find(|(k, _)| k == "ANI_CLI_PLAYER_FLAGS")
                .and_then(|(_, v)| v)
        };
        assert_eq!(
            flags(&with_flags).as_deref(),
            Some("--start=60 --fs --volume=50")
        );
        assert_eq!(flags(&blank_flags).as_deref(), Some("--start=60"));
    }

    #[test]
    fn ani_cli_command_names_the_resolved_mpv_with_its_base_args_as_the_player() {
        let launch = launch_of(
            custom_target("flatpak", &["run", "io.mpv.Mpv"]),
            PathBuf::from("log"),
        );

        let command = ani_cli_command(&launch, &ani_config(&[]), "X", 1, 0.0, None);

        assert_eq!(
            command_parts(&command).2,
            vec![
                env("ANI_CLI_EXACT", "1"),
                env("ANI_CLI_MENU", "true"),
                env("ANI_CLI_PLAYER", "flatpak run io.mpv.Mpv"),
            ]
        );
    }

    #[test]
    fn ani_cli_command_puts_configured_args_first_and_uses_the_configured_program() {
        let launch = launch_of(custom_target("mpv", &[]), PathBuf::from("log"));
        let mut config = ani_config(&["--dub", "-q", "720"]);
        config.command = "/opt/ani-cli".to_string();

        let command = ani_cli_command(&launch, &config, "X", 3, 0.0, None);

        let (program, args, _) = command_parts(&command);
        assert_eq!(program, "/opt/ani-cli");
        assert_eq!(
            args,
            [
                "--dub",
                "-q",
                "720",
                "-S",
                "1",
                "-e",
                "3",
                "--exit-after-play",
                "X"
            ]
            .map(String::from)
        );
    }
}
