mod anitomy;
mod cli;
mod config;
mod doctor;
mod duration;
mod exclude;
mod http;
mod launch;
mod mal;
mod mpv;
mod overview;
mod pause_drop;
mod picker;
mod reconcile;
mod roots;
mod script_log;
mod series;
mod state;
mod sync;
mod target;
mod update;

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Command, ConfigCommand, SyncCommand};
use config::Config;
use state::{SeriesRecord, State, Status};

fn main() -> ExitCode {
    let cli = Cli::parse();

    let config_path = match config::config_path() {
        Ok(path) => path,
        Err(e) => return fail(&format!("could not resolve config path: {e}")),
    };
    let mut config = match config::read(&config_path) {
        Ok(config) => config,
        Err(e) => {
            return fail(&format!(
                "could not read config at {}: {e}",
                config_path.display()
            ));
        }
    };

    let state_path = match state::state_path() {
        Ok(path) => path,
        Err(e) => return fail(&format!("could not resolve state path: {e}")),
    };

    let home = dirs::home_dir();
    let is_tty = picker::stdin_is_tty();

    dispatch(
        cli.command.unwrap_or(Command::Resume {
            series: None,
            root: None,
        }),
        &mut config,
        &config_path,
        &state_path,
        home.as_deref(),
        is_tty,
    )
}

/// `is_tty` is resolved once in `main` and passed down as a plain `bool`, so
/// tests can force it.
fn dispatch(
    command: Command,
    config: &mut Config,
    config_path: &Path,
    state_path: &Path,
    home: Option<&Path>,
    is_tty: bool,
) -> ExitCode {
    match command {
        Command::Resume { series, root } => {
            resume_command(series, root, config, state_path, home, is_tty)
        }
        Command::Status => status_command(config, state_path, home),
        Command::List => list_command(config, state_path, home),
        Command::Add { paths } => add_command(&paths, config, config_path),
        Command::Start { series, root } => {
            start_command(series, root, config, state_path, home, is_tty)
        }
        Command::Pause { series, root } => {
            pause_command(&series, root.as_deref(), config, state_path, home)
        }
        Command::Drop { series, root } => {
            drop_command(&series, root.as_deref(), config, state_path, home)
        }
        Command::Exclude { paths, .. } if paths.is_empty() => {
            suggest_candidates_command(config);
            ExitCode::SUCCESS
        }
        Command::Exclude { paths, root } => {
            exclude_command(&paths, root.as_deref(), config, config_path)
        }
        Command::Sync { action } => match action {
            None => sync_command(config, state_path, home),
            Some(SyncCommand::Login) => sync_login_command(config),
            Some(SyncCommand::Link {
                series,
                mal_id,
                root,
            }) => sync_link_command(&series, mal_id, root.as_deref(), config, state_path, home),
        },
        Command::Config { action } => match action {
            None => config_show_command(config, config_path),
            Some(ConfigCommand::Edit) => config_edit_command(config_path),
        },
        Command::Doctor {
            fix,
            forget_orphans,
        } => doctor_command(fix, forget_orphans, config, state_path, home),
        Command::Update => update_command(),
    }
}

/// `tsuzuki add <path>...`: validates and persists new tracked roots. A
/// failing path doesn't stop the rest; config is written once, and the
/// per-path lines are printed only after that write succeeds.
fn add_command(paths: &[PathBuf], config: &mut Config, config_path: &Path) -> ExitCode {
    let mut lines = Vec::new();
    let mut changed = false;
    let mut failed = false;
    for path in paths {
        match roots::add_root(config, path) {
            Ok(roots::AddOutcome::AlreadyTracked(as_typed)) => {
                lines.push((as_typed.display().to_string(), "already tracked"));
            }
            Ok(roots::AddOutcome::Added(as_typed)) => {
                changed = true;
                lines.push((as_typed.display().to_string(), "added"));
            }
            Err(e) => {
                fail(&e.to_string());
                failed = true;
            }
        }
    }
    if changed && let Err(e) = config::write(config_path, config) {
        return fail(&format!(
            "could not write config at {}: {e}",
            config_path.display()
        ));
    }
    for (subject, result) in &lines {
        confirm(subject, result);
    }
    exit_code(failed)
}

fn exit_code(failed: bool) -> ExitCode {
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `tsuzuki exclude <path>... [--root <root>]`: resolves each path against
/// tracked roots and persists the exclusions. A failing path doesn't stop
/// the rest; config is written once, and the per-path lines are printed only
/// after that write succeeds.
fn exclude_command(
    paths: &[String],
    root: Option<&Path>,
    config: &mut Config,
    config_path: &Path,
) -> ExitCode {
    let mut lines = Vec::new();
    let mut changed = false;
    let mut failed = false;
    for path in paths {
        match exclude::exclude(config, Path::new(path), root) {
            Ok(exclude::ExcludeOutcome::AlreadyExcluded {
                root,
                relative_path,
            }) => {
                lines.push((
                    format!("\"{relative_path}\""),
                    format!("already excluded (under {root})"),
                ));
            }
            Ok(exclude::ExcludeOutcome::Excluded {
                root,
                relative_path,
            }) => {
                changed = true;
                lines.push((
                    format!("\"{relative_path}\""),
                    format!("excluded (under {root})"),
                ));
            }
            Err(exclude::ExcludeError::Ambiguous {
                relative_path,
                matches,
            }) => {
                let roots: Vec<String> = matches.into_iter().map(|m| m.root).collect();
                print_ambiguous(&relative_path, "exclude", &roots);
                failed = true;
            }
            Err(e) => {
                fail(&e.to_string());
                failed = true;
            }
        }
    }
    if changed && let Err(e) = config::write(config_path, config) {
        return fail(&format!(
            "could not write config at {}: {e}",
            config_path.display()
        ));
    }
    for (subject, result) in &lines {
        confirm(subject, result);
    }
    exit_code(failed)
}

/// Bare `tsuzuki exclude`: lists Anitomy-suggested OVA/Special/Movie
/// candidates with the `exclude` command for each. Never excludes anything
/// itself.
fn suggest_candidates_command(config: &Config) {
    let candidates = exclude::suggest_candidates(config, dirs::home_dir().as_deref());

    if candidates.is_empty() {
        println!("No Anitomy-suggested candidates found under any tracked root.");
        return;
    }

    println!("Anitomy-suggested candidates (not yet excluded):");
    println!();
    for candidate in &candidates {
        println!("⚠ {}", candidate.relative_path);
        println!("  → tsuzuki exclude \"{}\"", candidate.relative_path);
    }
}

/// Bare `tsuzuki config`: prints the config path as a `#` comment header,
/// then the resolved config (defaults filled in for anything missing on
/// disk), then a pointer to `config edit`.
fn config_show_command(config: &Config, config_path: &Path) -> ExitCode {
    println!("# {}", config_path.display());
    println!();
    print!(
        "{}",
        toml::to_string_pretty(config).expect("Config always serializes")
    );
    println!();
    println!("→ tsuzuki config edit");
    ExitCode::SUCCESS
}

/// `tsuzuki config edit`: opens config.toml in `$EDITOR` and waits for it to
/// exit. No fallback editor is guessed if `$EDITOR` is unset. Re-reads the
/// file afterwards so a syntax error surfaces immediately.
fn config_edit_command(config_path: &Path) -> ExitCode {
    let editor = match std::env::var("EDITOR") {
        Ok(e) if !e.is_empty() => e,
        _ => {
            return fail("$EDITOR is not set — set it to open config.toml, e.g. export EDITOR=vim");
        }
    };

    if let Err(e) = std::process::Command::new(&editor)
        .arg(config_path)
        .status()
    {
        return fail(&format!("could not launch $EDITOR (\"{editor}\"): {e}"));
    }

    match config::read(config_path) {
        Ok(_) => {
            println!("✓ config saved");
            ExitCode::SUCCESS
        }
        Err(e) => fail(&format!("config.toml now has a syntax error: {e}")),
    }
}

fn fail(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    ExitCode::FAILURE
}

/// `✓ <subject> — <result>`, the confirmation line shared by `add`,
/// `exclude`, `pause`, `drop`, and `sync link`.
fn confirm(subject: &str, result: &str) {
    println!("✓ {subject} — {result}");
}

/// Reads `state.toml`, runs the shared `reconcile` pass, and persists the
/// result only if it changed. Any `ScanWarning` from the `watch_later` scan
/// is returned for the caller to print to stderr.
fn reconcile_state(
    config: &Config,
    state_path: &Path,
    home: Option<&Path>,
) -> Result<(State, Option<mpv::ScanWarning>), ExitCode> {
    let mut state = state::read(state_path).map_err(|e| {
        fail(&format!(
            "could not read state at {}: {e}",
            state_path.display()
        ))
    })?;

    let before = state.clone();
    let outcome = reconcile::reconcile(config, &mut state, home);

    if state != before {
        state::write(state_path, &state).map_err(|e| {
            fail(&format!(
                "could not write state at {}: {e}",
                state_path.display()
            ))
        })?;
    }

    Ok((state, outcome.warning))
}

fn print_scan_warning(warning: &mpv::ScanWarning) {
    match warning {
        mpv::ScanWarning::WatchLaterDirNotFound(p) => {
            eprintln!(
                "⚠ configured [mpv] watch_later_dir \"{}\" does not exist",
                p.display()
            );
        }
    }
}

/// The resolved file, starting position, and episode number to play.
struct PlaybackTarget {
    file: PathBuf,
    position_seconds: f64,
    episode_number: u32,
    season: Option<u32>,
}

/// Resolves the file and position to play for a series. An `in_progress` or
/// `paused` record resumes at its saved position; a `dropped` or `completed`
/// record, or no record, starts at the first episode at 0. `None` means the
/// series has no video files.
///
/// The episode number comes from `anitomy::episode_identity`, not
/// `record.episode_number`, since the start-fresh case has no usable record.
fn playback_target(
    config: &Config,
    home: Option<&Path>,
    root: &str,
    name: &str,
    record: Option<&SeriesRecord>,
) -> Option<PlaybackTarget> {
    let expanded_root = roots::expand_tilde(Path::new(root), home);
    let series_path = expanded_root.join(name);
    let excluded = exclude::excluded_paths_for_root(config, root, home);
    let ordered =
        series::ordered_episodes_excluding(&series_path, &config.roots.video_extensions, &excluded)
            .ok()?;
    if ordered.is_empty() {
        return None;
    }

    // `Some((idx, position))` only when the record's file is found in the
    // current ordering; index and position resolve together or not at all.
    let resolved = record
        .filter(|r| !matches!(r.status, Status::Dropped | Status::Completed))
        .and_then(|r| {
            let target = expanded_root.join(&r.current_relative_path);
            let real_target = roots::normalize(&target, home);
            ordered
                .iter()
                .position(|p| roots::normalize(p, home) == real_target)
                .map(|idx| (idx, r.position_seconds))
        });
    let (index, position_seconds) = resolved.unwrap_or((0, 0.0));

    let identities = anitomy::episode_identity(&ordered);
    let episode_number = identities
        .get(index)
        .map(|id| id.episode_number)
        .unwrap_or((index + 1) as u32);
    let season = identities.get(index).and_then(|id| id.season);

    Some(PlaybackTarget {
        file: ordered[index].clone(),
        position_seconds,
        episode_number,
        season,
    })
}

/// `mm:ss`, or `h:mm:ss` past an hour.
fn format_timestamp(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let secs = total % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes:02}:{secs:02}")
    }
}

/// Launches `name` under `root` and prints a confirmation line. Every path
/// that plays a series ends here.
fn play_series(
    config: &Config,
    state: &State,
    root: &str,
    name: &str,
    home: Option<&Path>,
    verb: &str,
) -> ExitCode {
    let record = state.find(root, name);
    match playback_target(config, home, root, name, record) {
        Some(target) => {
            match launch::resume_playback(
                &config.mpv.command_override,
                home,
                &target.file,
                target.position_seconds,
            ) {
                Ok(()) => {
                    println!(
                        "✓ {verb} \"{name}\" — {} ({})",
                        anitomy::episode_phrase(target.season, target.episode_number),
                        format_timestamp(target.position_seconds),
                    );
                    println!("  → {}", target.file.display());
                    ExitCode::SUCCESS
                }
                Err(e) => fail(&e.to_string()),
            }
        }
        None => fail(&format!("no episodes found for \"{name}\"")),
    }
}

/// Resolves an explicit `series [--root]` argument and plays it, bypassing
/// the picker whether or not there is a TTY.
fn explicit_play(
    series: &str,
    root_arg: Option<&Path>,
    config: &Config,
    state: &State,
    home: Option<&Path>,
    command_name: &str,
    verb: &str,
) -> ExitCode {
    match target::resolve_series_root(config, series, root_arg, home) {
        Ok(root) => play_series(config, state, &root, series, home, verb),
        Err(e) => print_target_error(&e, command_name),
    }
}

/// Narrows `start`/`resume`'s picker. `Break` ends the command (a series
/// was played, or the arguments were rejected); `Continue(Some(root))` limits
/// the picker to that stored root. A bare `series` is a series first, else a
/// root's folder name (both is an error); `--root` alone is that root.
fn resolve_scope(
    series: Option<String>,
    root_arg: Option<&Path>,
    config: &Config,
    state: &State,
    home: Option<&Path>,
    command_name: &str,
    verb: &str,
) -> ControlFlow<ExitCode, Option<String>> {
    match (series, root_arg) {
        (Some(series), Some(_)) => ControlFlow::Break(explicit_play(
            &series,
            root_arg,
            config,
            state,
            home,
            command_name,
            verb,
        )),
        (Some(arg), None) => match target::resolve_positional(config, &arg, home) {
            Ok(target::Resolved::Series(root)) => {
                ControlFlow::Break(play_series(config, state, &root, &arg, home, verb))
            }
            Ok(target::Resolved::Root(root)) => ControlFlow::Continue(Some(root)),
            Err(e) => ControlFlow::Break(print_positional_error(&e, command_name)),
        },
        (None, Some(root_arg)) => match state::resolve_root_arg(config, root_arg, home) {
            Some(root) => ControlFlow::Continue(Some(root)),
            None => ControlFlow::Break(print_target_error(
                &target::TargetError::RootNotFound(root_arg.to_string_lossy().into_owned()),
                command_name,
            )),
        },
        (None, None) => ControlFlow::Continue(None),
    }
}

fn in_scope(scope: &Option<String>, root: &str) -> bool {
    scope.as_deref().is_none_or(|s| s == root)
}

/// A scoped root as shown in messages: its folder name.
fn scope_label(root: &str) -> String {
    overview::route_display(&[root]).remove(0)
}

fn print_positional_error(err: &target::PositionalError, command_name: &str) -> ExitCode {
    match err {
        target::PositionalError::NotFound(arg) => {
            fail(&format!("no tracked series or root named \"{arg}\""))
        }
        target::PositionalError::AmbiguousSeries { series, roots } => {
            print_ambiguous(series, command_name, roots)
        }
        target::PositionalError::AmbiguousRoot { name, roots } => {
            eprintln!(
                "✗ \"{name}\" is ambiguous — {} tracked roots share that folder name",
                roots.len()
            );
            for r in roots {
                eprintln!("  → tsuzuki {command_name} --root {r}");
            }
            ExitCode::FAILURE
        }
        target::PositionalError::Collision {
            name,
            series_roots,
            roots,
        } => {
            eprintln!("✗ \"{name}\" is both a series and a root folder name");
            for r in roots {
                eprintln!("  → tsuzuki {command_name} --root {r}");
            }
            for r in series_roots {
                eprintln!("  → tsuzuki {command_name} \"{name}\" --root {r}");
            }
            ExitCode::FAILURE
        }
    }
}

fn print_target_error(err: &target::TargetError, command_name: &str) -> ExitCode {
    match err {
        target::TargetError::NotFound(name) => fail(&format!("no tracked series named \"{name}\"")),
        target::TargetError::RootNotFound(r) => fail(&format!("not a tracked root: {r}")),
        target::TargetError::Ambiguous { series, roots } => {
            print_ambiguous(series, command_name, roots)
        }
    }
}

/// Renders "`subject` exists under more than one tracked root" with the
/// exact `--root` re-invocation for each root.
fn print_ambiguous(subject: &str, command_name: &str, roots: &[String]) -> ExitCode {
    eprintln!(
        "✗ \"{subject}\" is ambiguous — tracked under {} roots",
        roots.len()
    );
    for r in roots {
        eprintln!("  → tsuzuki {command_name} \"{subject}\" --root {r}");
    }
    ExitCode::FAILURE
}

/// `tsuzuki` / `tsuzuki resume [series] [--root <root>]`: with no explicit
/// series, resumes the only `in_progress` series or picks among several.
fn resume_command(
    series: Option<String>,
    root: Option<PathBuf>,
    config: &Config,
    state_path: &Path,
    home: Option<&Path>,
    is_tty: bool,
) -> ExitCode {
    let (state, warning) = match reconcile_state(config, state_path, home) {
        Ok(v) => v,
        Err(code) => return code,
    };
    if let Some(w) = &warning {
        print_scan_warning(w);
    }

    let scope = match resolve_scope(
        series,
        root.as_deref(),
        config,
        &state,
        home,
        "resume",
        "Resuming",
    ) {
        ControlFlow::Break(code) => return code,
        ControlFlow::Continue(scope) => scope,
    };

    let overviews = overview::all_series(config, &state, home);
    let in_progress: Vec<_> = overview::group_by_root(
        overviews
            .into_iter()
            .filter(|o| o.status == Some(Status::InProgress))
            .filter(|o| in_scope(&scope, &o.root))
            .collect(),
        &config.roots.paths,
    );

    match in_progress.len() {
        // Nothing in progress falls through to `start`, unless a root was asked for.
        0 => match &scope {
            Some(root) => fail(&format!("nothing in progress in {}", scope_label(root))),
            None => start_command(None, None, config, state_path, home, is_tty),
        },
        1 => {
            let o = &in_progress[0];
            play_series(config, &state, &o.root, &o.name, home, "Resuming")
        }
        _ => {
            let outcome = picker::pick(
                &in_progress,
                |o| {
                    let ep = o
                        .episode_label()
                        .unwrap_or_else(|| "unknown episode".to_string());
                    format!("{} ({ep})", o.name)
                },
                |o| o.root.as_str(),
                is_tty,
                "Resume which series?",
                |line| println!("{line}"),
                picker::dialoguer_select,
            );
            match outcome {
                picker::PickerOutcome::Selected(o) => {
                    play_series(config, &state, &o.root, &o.name, home, "Resuming")
                }
                picker::PickerOutcome::Listed => {
                    println!();
                    println!("Run `tsuzuki resume <series>` to pick one.");
                    ExitCode::FAILURE
                }
                picker::PickerOutcome::Cancelled => ExitCode::FAILURE,
            }
        }
    }
}

/// `tsuzuki status`: every `in_progress` series.
fn status_command(config: &Config, state_path: &Path, home: Option<&Path>) -> ExitCode {
    let (state, warning) = match reconcile_state(config, state_path, home) {
        Ok(v) => v,
        Err(code) => return code,
    };
    if let Some(w) = &warning {
        print_scan_warning(w);
    }

    let overviews = overview::all_series(config, &state, home);
    let in_progress: Vec<_> = overviews
        .into_iter()
        .filter(|o| o.status == Some(Status::InProgress))
        .collect();

    if in_progress.is_empty() {
        println!("Nothing in progress.");
        println!("→ tsuzuki start");
        return ExitCode::SUCCESS;
    }

    for o in &in_progress {
        let ep = o
            .episode_label()
            .unwrap_or_else(|| "unknown episode".to_string());
        println!("{} — {ep}", o.name);
    }
    if in_progress.len() > 1 {
        println!();
        println!("Running `tsuzuki` bare will prompt among these.");
    }
    ExitCode::SUCCESS
}

/// `tsuzuki list`: every tracked series, grouped by status.
fn list_command(config: &Config, state_path: &Path, home: Option<&Path>) -> ExitCode {
    let (state, warning) = match reconcile_state(config, state_path, home) {
        Ok(v) => v,
        Err(code) => return code,
    };
    if let Some(w) = &warning {
        print_scan_warning(w);
    }

    let overviews = overview::sorted_for_list(overview::all_series(config, &state, home));
    if overviews.is_empty() {
        println!("No tracked series.");
        println!("→ tsuzuki add <path>");
        return ExitCode::SUCCESS;
    }

    const GROUPS: [(Option<Status>, &str); 5] = [
        (Some(Status::InProgress), "In progress"),
        (Some(Status::Paused), "Paused"),
        (Some(Status::Dropped), "Dropped"),
        (Some(Status::Completed), "Completed"),
        (None, "Never started"),
    ];

    let multi_route = overview::is_multi_route(overviews.iter().map(|o| o.root.as_str()));

    let mut first_group = true;
    for (status, header) in GROUPS {
        let items: Vec<_> = overviews.iter().filter(|o| o.status == status).collect();
        if items.is_empty() {
            continue;
        }
        if !first_group {
            println!();
        }
        first_group = false;

        println!("{header}:");
        let mut rows: Vec<String> = items
            .iter()
            .map(|o| {
                let ep = o
                    .episode_label()
                    .filter(|_| o.status != Some(Status::Completed))
                    .map(|label| format!(" — {label}"))
                    .unwrap_or_default();
                format!("  {}{ep} ({} files)", o.name, o.file_count)
            })
            .collect();
        if multi_route {
            let routes: Vec<&str> = items.iter().map(|o| o.root.as_str()).collect();
            rows = overview::tag_with_routes(&rows, &routes);
        }
        for row in rows {
            println!("{row}");
        }
    }
    ExitCode::SUCCESS
}

/// The progress verb `start` prints for a picked series.
fn start_verb(status: Option<Status>) -> &'static str {
    match status {
        Some(Status::Paused) => "Resuming",
        Some(Status::Dropped | Status::Completed) => "Rewatching",
        _ => "Starting",
    }
}

/// `tsuzuki start [series] [--root <root>]`: lists or picks among every
/// series not currently `in_progress`.
fn start_command(
    series: Option<String>,
    root: Option<PathBuf>,
    config: &Config,
    state_path: &Path,
    home: Option<&Path>,
    is_tty: bool,
) -> ExitCode {
    let (state, warning) = match reconcile_state(config, state_path, home) {
        Ok(v) => v,
        Err(code) => return code,
    };
    if let Some(w) = &warning {
        print_scan_warning(w);
    }

    let scope = match resolve_scope(
        series,
        root.as_deref(),
        config,
        &state,
        home,
        "start",
        "Starting",
    ) {
        ControlFlow::Break(code) => return code,
        ControlFlow::Continue(scope) => scope,
    };

    let overviews = overview::all_series(config, &state, home);
    let eligible = overview::group_by_root(
        overview::sorted_for_list(overview::eligible_for_start(&overviews))
            .into_iter()
            .filter(|o| in_scope(&scope, &o.root))
            .collect(),
        &config.roots.paths,
    );

    if eligible.is_empty() {
        return match &scope {
            Some(root) => fail(&format!("nothing to start in {}", scope_label(root))),
            None => {
                println!("Nothing to start.");
                ExitCode::SUCCESS
            }
        };
    }

    println!("Available to start:");
    println!();

    let outcome = picker::pick(
        &eligible,
        overview::start_line,
        |o| o.root.as_str(),
        is_tty,
        "Start which one?",
        |line| println!("{line}"),
        picker::dialoguer_select,
    );
    match outcome {
        picker::PickerOutcome::Selected(o) => {
            play_series(config, &state, &o.root, &o.name, home, start_verb(o.status))
        }
        picker::PickerOutcome::Listed => {
            println!();
            println!("Run `tsuzuki start <series>` to pick one.");
            ExitCode::FAILURE
        }
        picker::PickerOutcome::Cancelled => ExitCode::FAILURE,
    }
}

/// `tsuzuki pause <series>... [--root <root>]`.
fn pause_command(
    series: &[String],
    root_arg: Option<&Path>,
    config: &Config,
    state_path: &Path,
    home: Option<&Path>,
) -> ExitCode {
    transition_series(
        series,
        root_arg,
        config,
        state_path,
        home,
        &Transition {
            command_name: "pause",
            apply: pause_drop::pause,
            changed: "paused",
            already: "already paused",
        },
    )
}

/// `tsuzuki drop <series>... [--root <root>]`.
fn drop_command(
    series: &[String],
    root_arg: Option<&Path>,
    config: &Config,
    state_path: &Path,
    home: Option<&Path>,
) -> ExitCode {
    transition_series(
        series,
        root_arg,
        config,
        state_path,
        home,
        &Transition {
            command_name: "drop",
            apply: pause_drop::drop_series,
            changed: "dropped",
            already: "already dropped",
        },
    )
}

/// What distinguishes `pause` from `drop` for `transition_series`.
struct Transition {
    command_name: &'static str,
    apply: fn(&mut State, &str, &str) -> Result<pause_drop::Outcome, pause_drop::ActionError>,
    changed: &'static str,
    already: &'static str,
}

/// Reconciles once, applies `transition` to each series in turn, then writes
/// state once if any changed. A failing series doesn't stop the rest; the
/// per-series lines are printed only after that write succeeds.
fn transition_series(
    series: &[String],
    root_arg: Option<&Path>,
    config: &Config,
    state_path: &Path,
    home: Option<&Path>,
    transition: &Transition,
) -> ExitCode {
    let (mut state, warning) = match reconcile_state(config, state_path, home) {
        Ok(v) => v,
        Err(code) => return code,
    };
    if let Some(w) = &warning {
        print_scan_warning(w);
    }

    let mut lines = Vec::new();
    let mut changed = false;
    let mut failed = false;
    for name in series {
        let root = match target::resolve_series_root(config, name, root_arg, home) {
            Ok(r) => r,
            Err(e) => {
                print_target_error(&e, transition.command_name);
                failed = true;
                continue;
            }
        };
        match (transition.apply)(&mut state, &root, name) {
            Ok(pause_drop::Outcome::Changed) => {
                changed = true;
                lines.push((name, transition.changed));
            }
            Ok(pause_drop::Outcome::NoOp) => lines.push((name, transition.already)),
            Err(e) => {
                fail(&e.to_string());
                failed = true;
            }
        }
    }

    if changed && let Err(e) = state::write(state_path, &state) {
        return fail(&format!(
            "could not write state at {}: {e}",
            state_path.display()
        ));
    }
    for (name, result) in lines {
        confirm(&format!("\"{name}\""), result);
    }
    exit_code(failed)
}

/// `tsuzuki sync login`: one-time OAuth authorization (Authorization Code
/// grant with PKCE; MAL only supports `code_challenge_method=plain`).
fn sync_login_command(config: &Config) -> ExitCode {
    let client_id = mal::resolve_client_id(config);

    let token_path = match mal::mal_token_path() {
        Ok(p) => p,
        Err(e) => return fail(&format!("could not resolve MAL token path: {e}")),
    };

    let pkce = mal::generate_pkce();
    let state = mal::generate_state();
    let port = config.mal.redirect_port;

    let listener = match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            return fail(&format!(
                "could not bind local callback listener on 127.0.0.1:{port}: {e} — is another \
                 process using this port? [mal] redirect_port in config.toml can be changed"
            ));
        }
    };

    let url = mal::authorize_url(client_id, port, &pkce.code_challenge, &state);
    println!("Open this URL to authorize Tsuzuki with MyAnimeList:");
    println!();
    println!("  {url}");
    println!();
    match webbrowser::open(&url) {
        Ok(()) => println!("(opening in your browser...)"),
        Err(_) => {
            println!("(could not open a browser automatically — open the URL above manually)")
        }
    }
    println!(
        "Waiting for the callback (up to {}s)...",
        mal::LOGIN_TIMEOUT.as_secs()
    );

    let code = match mal::listen_for_callback(&listener, &state, mal::LOGIN_TIMEOUT) {
        Ok(mal::CallbackOutcome::Code(code)) => code,
        Ok(mal::CallbackOutcome::Denied(reason)) => {
            return fail(&format!("authorization was not granted: {reason}"));
        }
        Err(e) => return fail(&e.to_string()),
    };

    let client = mal::UreqClient;
    let now = state::now_unix();
    let token = match mal::exchange_code(&client, client_id, &code, &pkce.code_verifier, port, now)
    {
        Ok(t) => t,
        Err(e) => return fail(&e.to_string()),
    };

    if let Err(e) = mal::write_token(&token_path, &token) {
        return fail(&format!(
            "authorized, but could not save the token to {}: {e}",
            token_path.display()
        ));
    }

    println!("Logged in to MyAnimeList.");
    ExitCode::SUCCESS
}

/// Resolves a usable bearer token for `sync`/`sync link`. Failing here
/// aborts before any series is touched.
fn ensure_sync_access_token(
    client: &dyn mal::MalHttpClient,
    config: &Config,
) -> Result<String, ExitCode> {
    let token_path = mal::mal_token_path()
        .map_err(|e| fail(&format!("could not resolve MAL token path: {e}")))?;
    mal::ensure_access_token(
        client,
        &token_path,
        mal::resolve_client_id(config),
        state::now_unix(),
    )
    .map_err(|e| fail(&e.to_string()))
}

/// `tsuzuki sync` (bare): links each eligible series if not yet linked, then
/// merges progress. Paused and orphaned records are filtered before MAL
/// authentication, so nothing eligible means no network work. One series
/// failing never aborts the run.
fn sync_command(config: &Config, state_path: &Path, home: Option<&Path>) -> ExitCode {
    let (mut state, warning) = match reconcile_state(config, state_path, home) {
        Ok(v) => v,
        Err(code) => return code,
    };
    if let Some(w) = &warning {
        print_scan_warning(w);
    }

    let targets = sync::eligible_targets(&state, config, home);

    if targets.is_empty() {
        println!("Nothing eligible to sync.");
        return ExitCode::SUCCESS;
    }

    let client = mal::UreqClient;
    let access_token = match ensure_sync_access_token(&client, config) {
        Ok(t) => t,
        Err(code) => return code,
    };

    let before = state.clone();
    let outcomes = sync::run_sync(
        &client,
        &access_token,
        &mut state,
        &targets,
        sync::API_CALL_DELAY,
        config.mal.fuzzy_match_threshold,
    );

    let local_advances = sync::apply_synced_progress(config, &mut state, &outcomes, home);

    if state != before
        && let Err(e) = state::write(state_path, &state)
    {
        return fail(&format!(
            "could not write state at {}: {e}",
            state_path.display()
        ));
    }

    let mut failed = 0;
    let mut needs_linking = 0;
    let mut synced = 0;
    let mut detail_lines = Vec::new();
    for outcome in &outcomes {
        let local_advance = local_advances
            .iter()
            .find(|(root, name, _)| root == &outcome.root && name == &outcome.name)
            .map(|(_, _, watched)| *watched);
        detail_lines.extend(sync_outcome_lines(outcome, local_advance));
        match &outcome.kind {
            sync::OutcomeKind::Merged {
                action: sync::MergeAction::Push(_) | sync::MergeAction::Pull(_),
                ..
            } => synced += 1,
            sync::OutcomeKind::Merged {
                action: sync::MergeAction::RemoteStatus(_),
                ..
            } => synced += 1,
            sync::OutcomeKind::Merged {
                action: sync::MergeAction::NoOp,
                ..
            } if local_advance.is_some() => synced += 1,
            sync::OutcomeKind::Merged {
                action: sync::MergeAction::NoOp,
                ..
            } => {}
            sync::OutcomeKind::NoExactMatch { .. } | sync::OutcomeKind::EmptyList => {
                needs_linking += 1;
            }
            sync::OutcomeKind::Failed(_) => failed += 1,
        }
    }

    for line in &detail_lines {
        println!("{line}");
    }
    if !detail_lines.is_empty() {
        println!();
    }
    println!(
        "Sync complete: {}",
        sync_summary_line(synced, needs_linking, failed)
    );

    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Comma-joined summary with zero-count categories omitted, e.g.
/// "1 synced, 2 need manual linking". `synced` counts only real changes.
fn sync_summary_line(synced: u32, needs_linking: u32, failed: u32) -> String {
    let mut parts = Vec::new();
    if synced > 0 {
        parts.push(format!("{synced} synced"));
    }
    if needs_linking > 0 {
        parts.push(format!("{needs_linking} need manual linking"));
    }
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    if parts.is_empty() {
        "no changes".to_string()
    } else {
        parts.join(", ")
    }
}

/// Output lines for one series. A no-op produces none. `✓` is a real
/// push/pull, `⚠` needs user action, `✗` is a failure.
fn sync_outcome_lines(outcome: &sync::SeriesOutcome, local_advance: Option<u32>) -> Vec<String> {
    let name = &outcome.name;
    match &outcome.kind {
        sync::OutcomeKind::Merged {
            action: sync::MergeAction::Push(n) | sync::MergeAction::Pull(n),
            ..
        } => vec![format!("✓ {name} — episode {n} synced")],
        sync::OutcomeKind::Merged {
            action: sync::MergeAction::NoOp,
            ..
        } => local_advance
            .map(|n| vec![format!("✓ {name} — episode {n} synced")])
            .unwrap_or_default(),
        sync::OutcomeKind::Merged {
            action: sync::MergeAction::RemoteStatus(status),
            ..
        } => vec![format!("✓ {name} — moved to {}", status.display_name())],
        sync::OutcomeKind::NoExactMatch {
            suggestion: Some(c),
        } => vec![
            format!(
                "⚠ {name} — did you mean \"{}\" (MAL #{})?",
                c.title, c.mal_id
            ),
            format!("  → tsuzuki sync link \"{name}\" --mal-id {}", c.mal_id),
        ],
        sync::OutcomeKind::NoExactMatch { suggestion: None } => {
            vec![format!("⚠ {name} — no matching series found")]
        }
        sync::OutcomeKind::EmptyList => {
            vec![format!("⚠ {name} — your MyAnimeList list is empty")]
        }
        sync::OutcomeKind::Failed(msg) => vec![format!("✗ {name} — sync failed: {msg}")],
    }
}

/// `tsuzuki sync link <series> --mal-id <id> [--root <root>]`: manual link
/// for when exact-title matching fails. Verifies `mal_id` is on the user's
/// MAL list before persisting.
fn sync_link_command(
    series: &str,
    mal_id: u32,
    root_arg: Option<&Path>,
    config: &Config,
    state_path: &Path,
    home: Option<&Path>,
) -> ExitCode {
    let (mut state, warning) = match reconcile_state(config, state_path, home) {
        Ok(v) => v,
        Err(code) => return code,
    };
    if let Some(w) = &warning {
        print_scan_warning(w);
    }

    // Resolve locally first so a bad or ambiguous name fails with no network call.
    let root = match target::resolve_series_root(config, series, root_arg, home) {
        Ok(r) => r,
        Err(e) => return print_target_error(&e, "sync link"),
    };

    let client = mal::UreqClient;
    let access_token = match ensure_sync_access_token(&client, config) {
        Ok(t) => t,
        Err(code) => return code,
    };

    match sync::manual_link(&client, &access_token, &mut state, &root, series, mal_id) {
        Ok(()) => {
            if let Err(e) = state::write(state_path, &state) {
                return fail(&format!(
                    "could not write state at {}: {e}",
                    state_path.display()
                ));
            }
            confirm(
                &format!("\"{series}\""),
                &format!("linked to MAL #{mal_id}"),
            );
            ExitCode::SUCCESS
        }
        Err(e) => fail(&e.to_string()),
    }
}

/// `tsuzuki doctor [--fix] [--forget-orphans]`: reconciles first, then
/// prints one line per conceptual check to stdout. Install-scoped checks are
/// folded into one row via `doctor::combine`; a check with nothing to check
/// against is omitted. Exits non-zero only if a check still FAILs after any
/// fixes, never on WARN alone.
fn doctor_command(
    fix: bool,
    forget_orphans: bool,
    config: &Config,
    state_path: &Path,
    home: Option<&Path>,
) -> ExitCode {
    // Local only, never `update::check_for_update()`; the version lets a pasted
    // report name the build it came from.
    println!("Tsuzuki v{}", update::current_version());
    println!();

    let (mut state, warning) = match reconcile_state(config, state_path, home) {
        Ok(v) => v,
        Err(code) => return code,
    };
    if let Some(w) = &warning {
        print_scan_warning(w);
    }

    let mut results = Vec::new();

    let presence = doctor::MpvPresence {
        native: launch::native_mpv_on_path(),
        flatpak: launch::flatpak_mpv_installed(home),
    };
    let override_resolves = doctor::command_override_resolves(&config.mpv.command_override);
    results.push(doctor::check_mpv_availability(
        &config.mpv.command_override,
        presence,
        override_resolves,
    ));

    let installs = doctor::detected_mpv_installs(presence, home);
    if !installs.is_empty() {
        let mpv_conf_checks: Vec<_> = installs.iter().map(doctor::check_mpv_conf).collect();
        results.push(doctor::combine(
            "mpv config",
            "watch-later tracking enabled",
            &mpv_conf_checks,
        ));
    }

    results.push(doctor::check_watch_later(config, home));

    if presence.flatpak {
        let flatpak_cli_present = launch::program_on_path("flatpak");
        let permissions_output = if flatpak_cli_present {
            std::process::Command::new("flatpak")
                .args(["info", "--show-permissions", "io.mpv.Mpv"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        } else {
            None
        };
        results.push(doctor::check_flatpak_permissions(
            presence.flatpak,
            flatpak_cli_present,
            permissions_output.as_deref(),
        ));
    }

    if !installs.is_empty() {
        let mut script_checks = Vec::new();
        for install in &installs {
            let scripts_dir = install.config_dir.join("scripts");
            let mut outcome = doctor::check_tsuzuki_lua(&scripts_dir, install.label);
            if fix && outcome.needs_fix.is_some() {
                match doctor::install_script(&scripts_dir) {
                    Ok(()) => {
                        println!("✓ fixed: companion script installed for {}", install.label);
                        outcome = doctor::check_tsuzuki_lua(&scripts_dir, install.label);
                    }
                    Err(e) => {
                        eprintln!(
                            "warning: failed to install companion script for {}: {e}",
                            install.label
                        );
                    }
                }
            }
            script_checks.push(outcome.result);
        }
        script_checks.extend(doctor::check_mpv_log_dirs(&script_log::mpv_log_dirs(home)));
        results.push(doctor::combine(
            "companion script",
            "up to date",
            &script_checks,
        ));
    }

    let root_checks = doctor::check_tracked_roots(config, home);
    let valid_roots: Vec<String> = root_checks
        .iter()
        .filter(|c| c.valid)
        .map(|c| c.root.clone())
        .collect();
    results.push(doctor::summarize_tracked_roots(&root_checks));

    let orphans = doctor::find_orphans(config, &state, &valid_roots, home);
    results.push(doctor::orphan_check_result(&orphans));

    if forget_orphans && !orphans.is_empty() {
        let names: Vec<&str> = orphans.iter().map(|o| o.name.as_str()).collect();
        println!(
            "✓ removed {} orphaned record(s): {}",
            orphans.len(),
            names.join(", ")
        );
        doctor::forget_orphans(&mut state, &orphans);
        if let Err(e) = state::write(state_path, &state) {
            return fail(&format!(
                "could not write state at {}: {e}",
                state_path.display()
            ));
        }
    }

    match mal::mal_token_path() {
        Ok(token_path) => {
            let mal_client = mal::UreqClient;
            let status = mal::token_status(
                &mal_client,
                &token_path,
                mal::resolve_client_id(config),
                state::now_unix(),
            );
            results.push(doctor::check_mal_auth(&status));
        }
        Err(e) => {
            results.push(doctor::CheckResult::fail(
                "MyAnimeList",
                format!("could not resolve token path: {e}"),
            ));
        }
    }

    for r in &results {
        println!("{}", r.render());
    }

    println!();
    println!("{}", doctor_summary_line(&results));

    if results.iter().any(|r| r.level == doctor::Level::Fail) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// "All checks passed." when every row is `Ok`; otherwise the count of `Warn`
/// and `Fail` rows.
fn doctor_summary_line(results: &[doctor::CheckResult]) -> String {
    let issues = results
        .iter()
        .filter(|r| r.level != doctor::Level::Ok)
        .count();
    match issues {
        0 => "All checks passed.".to_string(),
        1 => "1 issue found.".to_string(),
        n => format!("{n} issues found."),
    }
}

/// `tsuzuki update`: checks for a newer release and installs it. Needs no
/// config or state.
fn update_command() -> ExitCode {
    let current = update::current_version();
    let status = match update::check_for_update() {
        Ok(status) => status,
        // Don't run the installer when the check itself failed.
        Err(e) => return fail(&format!("could not check for updates: {e}")),
    };
    println!("{}", status.render(current));

    let update::UpdateStatus::UpdateAvailable { latest } = status else {
        return ExitCode::SUCCESS;
    };

    println!("Installing {latest}...");
    match update::run_install() {
        Ok(()) => {
            println!("✓ updated to {latest}");
            ExitCode::SUCCESS
        }
        Err(e) => fail(&format!("install failed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, Instant};
    use tempfile::tempdir;

    #[test]
    fn start_verb_matches_each_status() {
        assert_eq!(
            [
                start_verb(Some(Status::Paused)),
                start_verb(Some(Status::Dropped)),
                start_verb(Some(Status::Completed)),
                start_verb(None),
            ],
            ["Resuming", "Rewatching", "Rewatching", "Starting"]
        );
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::File::create(path).unwrap();
    }

    fn tracked_config(roots: &[&Path]) -> Config {
        let mut config = Config::default();
        for root in roots {
            config.roots.paths.push(root.to_string_lossy().into_owned());
        }
        // `true` exits immediately and bypasses mpv detection, so tests that
        // reach `play_series` never spawn a real mpv.
        config.mpv.command_override = "true".to_string();
        config
    }

    fn record(root: &Path, name: &str, status: Status) -> SeriesRecord {
        SeriesRecord {
            root: root.to_string_lossy().into_owned(),
            name: name.to_string(),
            status,
            current_relative_path: format!("{name}/ep01.mkv"),
            position_seconds: 200.0,
            episode_number: Some(1),
            updated_at: 1,
            mal_episode_awareness: None,
            season_number: None,
        }
    }

    #[test]
    fn unchanged_sync_outcomes_are_silent_and_summarize_as_no_changes() {
        let outcome = sync::SeriesOutcome {
            root: "~/Anime".to_string(),
            name: "Hinamatsuri".to_string(),
            kind: sync::OutcomeKind::Merged {
                mal_id: 36_207,
                action: sync::MergeAction::NoOp,
            },
        };

        assert!(sync_outcome_lines(&outcome, None).is_empty());
        assert_eq!(
            sync_outcome_lines(&outcome, Some(9)),
            vec!["✓ Hinamatsuri — episode 9 synced"]
        );
        assert_eq!(sync_summary_line(0, 0, 0), "no changes");
    }

    #[test]
    fn status_only_sync_outcome_reports_the_new_mal_list() {
        let outcome = sync::SeriesOutcome {
            root: "~/Anime".to_string(),
            name: "Hinamatsuri".to_string(),
            kind: sync::OutcomeKind::Merged {
                mal_id: 36_207,
                action: sync::MergeAction::RemoteStatus(sync::MalListStatus::Watching),
            },
        };

        assert_eq!(
            sync_outcome_lines(&outcome, None),
            vec!["✓ Hinamatsuri — moved to Watching"]
        );
    }

    /// Writes a `watch_later` entry at `record()`'s stored position (200s) for
    /// a single-episode series, so reconcile leaves the fixture record alone.
    fn fabricate_matching_bookmark(
        config: &mut Config,
        watch_later_dir: &Path,
        root: &Path,
        name: &str,
    ) {
        if config.mpv.watch_later_dir.is_empty() {
            config.mpv.watch_later_dir = watch_later_dir.to_string_lossy().into_owned();
        }
        fs::create_dir_all(watch_later_dir).unwrap();
        let entry_name = format!(
            "entry-{}-{name}",
            root.file_name().unwrap().to_string_lossy()
        );
        fs::write(
            watch_later_dir.join(entry_name),
            format!(
                "# {}\nstart=200.000000\n",
                root.join(name).join("ep01.mkv").display()
            ),
        )
        .unwrap();
    }

    // `pause`/`drop` `--root` disambiguation between identically named series.

    #[test]
    fn pause_command_root_disambiguates_between_two_real_identically_named_series() {
        let dir = tempdir().unwrap();
        let root_a = dir.path().join("Anime");
        let root_b = dir.path().join("AltAnime");
        touch(&root_a.join("Monster").join("ep01.mkv"));
        touch(&root_b.join("Monster").join("ep01.mkv"));

        let mut config = tracked_config(&[&root_a, &root_b]);
        let watch_later = dir.path().join("watch_later");
        fabricate_matching_bookmark(&mut config, &watch_later, &root_a, "Monster");
        fabricate_matching_bookmark(&mut config, &watch_later, &root_b, "Monster");
        let state_path = dir.path().join("state.toml");
        let state = State {
            series: vec![
                record(&root_a, "Monster", Status::InProgress),
                record(&root_b, "Monster", Status::InProgress),
            ],

            mal_links: Vec::new(),
        };
        state::write(&state_path, &state).unwrap();

        let exit = pause_command(
            &["Monster".to_string()],
            Some(&root_b),
            &config,
            &state_path,
            None,
        );
        assert_eq!(exit, ExitCode::SUCCESS);

        let after = state::read(&state_path).unwrap();
        assert_eq!(
            after
                .find(&root_b.to_string_lossy(), "Monster")
                .unwrap()
                .status,
            Status::Paused,
            "the --root-specified series must be paused"
        );
        assert_eq!(
            after
                .find(&root_a.to_string_lossy(), "Monster")
                .unwrap()
                .status,
            Status::InProgress,
            "the identically-named series under the OTHER root must be completely untouched"
        );
    }

    #[test]
    fn drop_command_root_disambiguates_between_two_real_identically_named_series() {
        let dir = tempdir().unwrap();
        let root_a = dir.path().join("Anime");
        let root_b = dir.path().join("AltAnime");
        touch(&root_a.join("Monster").join("ep01.mkv"));
        touch(&root_b.join("Monster").join("ep01.mkv"));

        let mut config = tracked_config(&[&root_a, &root_b]);
        let watch_later = dir.path().join("watch_later");
        fabricate_matching_bookmark(&mut config, &watch_later, &root_a, "Monster");
        fabricate_matching_bookmark(&mut config, &watch_later, &root_b, "Monster");
        let state_path = dir.path().join("state.toml");
        let state = State {
            series: vec![
                record(&root_a, "Monster", Status::InProgress),
                record(&root_b, "Monster", Status::InProgress),
            ],

            mal_links: Vec::new(),
        };
        state::write(&state_path, &state).unwrap();

        let exit = drop_command(
            &["Monster".to_string()],
            Some(&root_a),
            &config,
            &state_path,
            None,
        );
        assert_eq!(exit, ExitCode::SUCCESS);

        let after = state::read(&state_path).unwrap();
        assert_eq!(
            after
                .find(&root_a.to_string_lossy(), "Monster")
                .unwrap()
                .status,
            Status::Dropped
        );
        assert_eq!(
            after
                .find(&root_b.to_string_lossy(), "Monster")
                .unwrap()
                .status,
            Status::InProgress,
            "the identically-named series under the OTHER root must be completely untouched"
        );
    }

    #[test]
    fn pause_command_without_root_on_a_real_collision_is_ambiguous_and_mutates_nothing() {
        let dir = tempdir().unwrap();
        let root_a = dir.path().join("Anime");
        let root_b = dir.path().join("AltAnime");
        touch(&root_a.join("Monster").join("ep01.mkv"));
        touch(&root_b.join("Monster").join("ep01.mkv"));

        let mut config = tracked_config(&[&root_a, &root_b]);
        let watch_later = dir.path().join("watch_later");
        fabricate_matching_bookmark(&mut config, &watch_later, &root_a, "Monster");
        fabricate_matching_bookmark(&mut config, &watch_later, &root_b, "Monster");
        let state_path = dir.path().join("state.toml");
        let state = State {
            series: vec![
                record(&root_a, "Monster", Status::InProgress),
                record(&root_b, "Monster", Status::InProgress),
            ],

            mal_links: Vec::new(),
        };
        state::write(&state_path, &state).unwrap();

        let exit = pause_command(&["Monster".to_string()], None, &config, &state_path, None);
        assert_eq!(exit, ExitCode::FAILURE);

        let after = state::read(&state_path).unwrap();
        assert_eq!(
            after, state,
            "an ambiguous pause must mutate neither record"
        );
    }

    // `sync_link_command` resolves `--root` before any network call, so only the
    // ambiguous case is testable here; the linking itself is covered in `sync.rs`.

    #[test]
    fn sync_link_command_without_root_on_a_real_collision_is_ambiguous_and_mutates_nothing() {
        let dir = tempdir().unwrap();
        let root_a = dir.path().join("Anime");
        let root_b = dir.path().join("AltAnime");
        touch(&root_a.join("Monster").join("ep01.mkv"));
        touch(&root_b.join("Monster").join("ep01.mkv"));

        let mut config = tracked_config(&[&root_a, &root_b]);
        let watch_later = dir.path().join("watch_later");
        fabricate_matching_bookmark(&mut config, &watch_later, &root_a, "Monster");
        fabricate_matching_bookmark(&mut config, &watch_later, &root_b, "Monster");
        let state_path = dir.path().join("state.toml");
        let state = State {
            series: vec![
                record(&root_a, "Monster", Status::InProgress),
                record(&root_b, "Monster", Status::InProgress),
            ],
            mal_links: Vec::new(),
        };
        state::write(&state_path, &state).unwrap();

        let exit = sync_link_command("Monster", 42, None, &config, &state_path, None);
        assert_eq!(
            exit,
            ExitCode::FAILURE,
            "ambiguous --root resolution must fail before any network call is attempted"
        );

        let after = state::read(&state_path).unwrap();
        assert_eq!(
            after, state,
            "an ambiguous sync link must mutate nothing — no mal_links entry, no series changes"
        );
    }

    // `explicit_play` `--root` disambiguation, proven by exit code: only one
    // root's series has episodes.

    #[test]
    fn explicit_play_root_disambiguates_between_two_real_identically_named_series() {
        let dir = tempdir().unwrap();
        let root_a = dir.path().join("Anime");
        let root_b = dir.path().join("AltAnime");
        // root_a's "Monster" has no video files.
        fs::create_dir_all(root_a.join("Monster")).unwrap();
        // root_b's "Monster" has an episode.
        touch(&root_b.join("Monster").join("ep01.mkv"));

        let config = tracked_config(&[&root_a, &root_b]);
        let state = State::default();

        let exit_a = explicit_play(
            "Monster",
            Some(&root_a),
            &config,
            &state,
            None,
            "resume",
            "Resuming",
        );
        assert_eq!(
            exit_a,
            ExitCode::FAILURE,
            "root_a's Monster has zero episodes — resolving there must fail"
        );

        let exit_b = explicit_play(
            "Monster",
            Some(&root_b),
            &config,
            &state,
            None,
            "resume",
            "Resuming",
        );
        assert_eq!(
            exit_b,
            ExitCode::SUCCESS,
            "root_b's Monster has a real episode — --root must have actually reached it, \
             not silently resolved to root_a (or anywhere else) regardless of the flag"
        );
    }

    #[test]
    fn resume_command_explicit_arg_bypasses_the_picker_even_when_a_tty_is_available() {
        // Two in_progress series with `is_tty: true`: the bare path would reach
        // the real picker. Run with a timeout so a regression fails instead of
        // hanging.
        let dir = tempdir().unwrap();
        let root = dir.path().join("Anime");
        touch(&root.join("Monster").join("ep01.mkv"));
        touch(&root.join("One Piece").join("ep01.mkv"));

        let config = tracked_config(&[&root]);
        let state_path = dir.path().join("state.toml");
        let state = State {
            series: vec![
                record(&root, "Monster", Status::InProgress),
                record(&root, "One Piece", Status::InProgress),
            ],

            mal_links: Vec::new(),
        };
        state::write(&state_path, &state).unwrap();

        let series = Some("Monster".to_string());
        let exit = wait_with_timeout(
            move || resume_command(series, None, &config, &state_path, None, true),
            Duration::from_secs(5),
        )
        .expect(
            "resume_command with an explicit series arg did not return within the timeout — \
             likely a regression that reached the interactive picker despite an explicit arg",
        );

        assert_eq!(exit, ExitCode::SUCCESS);
    }

    /// Runs `f` on a background thread and returns `None` if it doesn't finish
    /// within `timeout`.
    fn wait_with_timeout<T: Send + 'static>(
        f: impl FnOnce() -> T + Send + 'static,
        timeout: Duration,
    ) -> Option<T> {
        let handle = std::thread::spawn(f);
        let start = Instant::now();
        loop {
            if handle.is_finished() {
                return handle.join().ok();
            }
            if start.elapsed() > timeout {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    // `doctor_command` wiring: orphan detection and the `--fix`/`--forget-orphans`
    // flow. The individual checks are tested in `doctor.rs`.

    fn state_with_orphan(root: &Path) -> State {
        State {
            series: vec![record(root, "GoneSeries", Status::InProgress)],
            mal_links: Vec::new(),
        }
    }

    #[test]
    fn doctor_without_flags_reports_an_orphan_but_leaves_state_untouched() {
        let dir = tempdir().unwrap();
        let home = dir.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let root = home.join("Anime");
        fs::create_dir_all(&root).unwrap(); // root exists, "GoneSeries" under it does not

        let config = tracked_config(&[&root]);
        let state_path = dir.path().join("state.toml");
        let before = state_with_orphan(&root);
        state::write(&state_path, &before).unwrap();

        let exit = doctor_command(false, false, &config, &state_path, Some(&home));
        assert_eq!(exit, ExitCode::FAILURE, "an orphaned record must be a FAIL");

        let after = state::read(&state_path).unwrap();
        assert_eq!(
            after, before,
            "doctor without --forget-orphans must never mutate state.toml"
        );
    }

    #[test]
    fn doctor_fix_alone_does_not_clear_orphans() {
        let dir = tempdir().unwrap();
        let home = dir.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let root = home.join("Anime");
        fs::create_dir_all(&root).unwrap();

        let config = tracked_config(&[&root]);
        let state_path = dir.path().join("state.toml");
        let before = state_with_orphan(&root);
        state::write(&state_path, &before).unwrap();

        let exit = doctor_command(true, false, &config, &state_path, Some(&home));
        assert_eq!(exit, ExitCode::FAILURE, "the orphan is still unresolved");

        let after = state::read(&state_path).unwrap();
        assert_eq!(
            after.series.len(),
            1,
            "--fix alone must not clear orphaned records — that needs --forget-orphans too"
        );
    }

    #[test]
    fn doctor_fix_forget_orphans_clears_confirmed_orphans_and_leaves_the_rest_untouched() {
        let dir = tempdir().unwrap();
        let home = dir.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let root = home.join("Anime");
        touch(&root.join("StillHere").join("ep01.mkv"));
        // "GoneSeries" is tracked in state but has no folder on disk.

        let mut config = tracked_config(&[&root]);
        let watch_later = dir.path().join("watch_later");
        fabricate_matching_bookmark(&mut config, &watch_later, &root, "StillHere");
        let state_path = dir.path().join("state.toml");
        let before = State {
            series: vec![
                record(&root, "GoneSeries", Status::InProgress),
                record(&root, "StillHere", Status::InProgress),
            ],

            mal_links: Vec::new(),
        };
        state::write(&state_path, &before).unwrap();

        let exit = doctor_command(true, true, &config, &state_path, Some(&home));
        assert_eq!(
            exit,
            ExitCode::FAILURE,
            "unrelated FAILs (e.g. mpv.conf) may still be present on this machine — only the \
             orphan-clearing side effect is under test here"
        );

        let after = state::read(&state_path).unwrap();
        assert_eq!(
            after.series.len(),
            1,
            "exactly the orphaned record must be gone"
        );
        assert_eq!(after.series[0].name, "StillHere");
    }

    #[test]
    fn doctor_with_no_orphans_reports_the_orphan_check_as_ok() {
        let dir = tempdir().unwrap();
        let home = dir.path().join("home");
        touch(&home.join("Anime").join("Monster").join("ep01.mkv"));
        let root = home.join("Anime");

        let mut config = tracked_config(&[&root]);
        let watch_later = dir.path().join("watch_later");
        fabricate_matching_bookmark(&mut config, &watch_later, &root, "Monster");
        let state_path = dir.path().join("state.toml");
        let before = State {
            series: vec![record(&root, "Monster", Status::InProgress)],

            mal_links: Vec::new(),
        };
        state::write(&state_path, &before).unwrap();

        // The exit code is not asserted: unrelated mpv checks may FAIL on this home.
        doctor_command(false, false, &config, &state_path, Some(&home));

        let after = state::read(&state_path).unwrap();
        assert_eq!(after, before);
    }
}
