mod anitomy;
mod cli;
mod config;
mod duration;
mod exclude;
mod launch;
mod mpv;
mod overview;
mod pause_drop;
mod picker;
mod reconcile;
mod roots;
mod series;
mod state;
mod target;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{CommandFactory, Parser};

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

    dispatch(
        cli.command.unwrap_or(Command::Resume {
            series: None,
            root: None,
        }),
        &mut config,
        &config_path,
        &state_path,
        home.as_deref(),
    )
}

fn dispatch(
    command: Command,
    config: &mut Config,
    config_path: &Path,
    state_path: &Path,
    home: Option<&Path>,
) -> ExitCode {
    match command {
        Command::Resume { series, root } => resume_command(series, root, config, state_path, home),
        Command::Status => status_command(config, state_path, home),
        Command::List => list_command(config, state_path, home),
        Command::Add { path } => add_command(&path, config, config_path),
        Command::Start { series, root } => start_command(series, root, config, state_path, home),
        Command::Pause { series, root } => {
            pause_command(&series, root.as_deref(), config, state_path, home)
        }
        Command::Drop { series, root } => {
            drop_command(&series, root.as_deref(), config, state_path, home)
        }
        Command::Exclude { path: None, .. } => {
            suggest_candidates_command(config);
            ExitCode::SUCCESS
        }
        Command::Exclude {
            path: Some(path),
            root,
        } => exclude_command(&path, root.as_deref(), config, config_path),
        Command::Sync { action } => {
            match action {
                None => stub("sync"),
                Some(SyncCommand::Login) => stub("sync login"),
                Some(SyncCommand::Link { .. }) => stub("sync link"),
            }
            ExitCode::SUCCESS
        }
        Command::Config { action } => {
            match action {
                None => print_config_help(),
                Some(ConfigCommand::Get { .. }) => stub("config get"),
                Some(ConfigCommand::Set { .. }) => stub("config set"),
                Some(ConfigCommand::List) => stub("config list"),
                Some(ConfigCommand::Edit) => stub("config edit"),
            }
            ExitCode::SUCCESS
        }
        Command::Doctor => {
            stub("doctor");
            ExitCode::SUCCESS
        }
    }
}

fn stub(name: &str) {
    println!("not implemented: {name}");
}

/// `tsuzuki add <path>` — validates, checks idempotency, and persists a new
/// tracked root (CLAUDE.md's "`tsuzuki add` validation and idempotency").
fn add_command(path: &Path, config: &mut Config, config_path: &Path) -> ExitCode {
    match roots::add_root(config, path) {
        Ok(roots::AddOutcome::AlreadyTracked(as_typed)) => {
            println!("already tracked: {}", as_typed.display());
            ExitCode::SUCCESS
        }
        Ok(roots::AddOutcome::Added(as_typed)) => {
            if let Err(e) = config::write(config_path, config) {
                return fail(&format!(
                    "could not write config at {}: {e}",
                    config_path.display()
                ));
            }
            println!("added tracked root: {}", as_typed.display());
            ExitCode::SUCCESS
        }
        Err(e) => fail(&e.to_string()),
    }
}

/// `tsuzuki exclude <path> [--root <root>]` — resolves `path` against
/// tracked roots and persists the exclusion (CLAUDE.md's "Exclusion
/// persistence and disambiguation"). Ambiguous matches and validation
/// failures print `ExcludeError`'s `Display` output as-is — it already
/// includes the exact `--root` re-invocations design.md §3 specifies.
fn exclude_command(
    path: &str,
    root: Option<&Path>,
    config: &mut Config,
    config_path: &Path,
) -> ExitCode {
    match exclude::exclude(config, Path::new(path), root) {
        Ok(exclude::ExcludeOutcome::AlreadyExcluded {
            root,
            relative_path,
        }) => {
            println!("already excluded: \"{relative_path}\" under {root}");
            ExitCode::SUCCESS
        }
        Ok(exclude::ExcludeOutcome::Excluded {
            root,
            relative_path,
        }) => {
            if let Err(e) = config::write(config_path, config) {
                return fail(&format!(
                    "could not write config at {}: {e}",
                    config_path.display()
                ));
            }
            println!("excluded: \"{relative_path}\" under {root}");
            ExitCode::SUCCESS
        }
        Err(e) => fail(&e.to_string()),
    }
}

/// Bare `tsuzuki exclude` (no path) — lists Anitomy-suggested OVA/Special/
/// Movie/etc candidates plus the exact `tsuzuki exclude "<path>"` command
/// for each (CLAUDE.md's "Anitomy-candidate confirmation stays list-only,
/// not interactive", design.md §5). Never mutates `config` or excludes
/// anything itself — confirming a candidate is a separate, explicit
/// re-invocation of `exclude` with a path.
fn suggest_candidates_command(config: &Config) {
    let candidates = exclude::suggest_candidates(config, dirs::home_dir().as_deref());

    if candidates.is_empty() {
        println!("No Anitomy-suggested candidates found under any tracked root.");
        return;
    }

    println!("Anitomy-suggested candidates (not yet excluded):");
    println!();
    for candidate in &candidates {
        println!("  {}", candidate.relative_path);
        println!("    tsuzuki exclude \"{}\"", candidate.relative_path);
        println!();
    }
}

/// `config` has no single default action (`get`/`set`/`list`/`edit` are all
/// equally valid) — per CLAUDE.md's CLI bare-invocation contract, bare
/// `tsuzuki config` prints help and exits 0 rather than clap's generic
/// "subcommand required" error.
fn print_config_help() {
    let root = Cli::command();
    let bin_name = root.get_name().to_string();
    let mut config_cmd = root
        .find_subcommand("config")
        .expect("config subcommand is defined on Cli")
        .clone()
        .bin_name(format!("{bin_name} config"));
    let _ = config_cmd.print_long_help();
    println!();
}

fn fail(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    ExitCode::FAILURE
}

// --- Phase 5: state machine & core commands ---

/// Reads `state.toml`, runs the shared `reconcile` pass, and — only if it
/// actually changed anything — persists the result (CLAUDE.md's "Every
/// state-touching command reconciles against current mpv reality first").
/// Any `ScanWarning` from the underlying `watch_later` scan is returned
/// alongside, for the caller to print to stderr (CLAUDE.md's "warnings on
/// stderr, real output on stdout").
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
                "warning: configured [mpv] watch_later_dir \"{}\" does not exist",
                p.display()
            );
        }
    }
}

/// Resolves what file + starting position to actually play for a given
/// series: a real `in_progress`/`paused` record resumes at its saved
/// position; a `dropped` record or no record at all starts at the first
/// episode, position 0 (design.md §6's three `start` cases, and — since
/// `resume`/`start` share one selection mechanism per CLAUDE.md — this
/// single function backs both). `None` means the series currently has no
/// video files to play at all.
fn playback_target(
    config: &Config,
    home: Option<&Path>,
    root: &str,
    name: &str,
    record: Option<&SeriesRecord>,
) -> Option<(PathBuf, f64)> {
    let expanded_root = roots::expand_tilde(Path::new(root), home);
    let series_path = expanded_root.join(name);
    let excluded = exclude::excluded_paths_for_root(config, root, home);
    let ordered =
        series::ordered_episodes_excluding(&series_path, &config.roots.video_extensions, &excluded)
            .ok()?;
    if ordered.is_empty() {
        return None;
    }

    if let Some(r) = record
        && r.status != Status::Dropped
    {
        let target = expanded_root.join(&r.current_relative_path);
        let real_target = roots::normalize(&target, home);
        if let Some(file) = ordered
            .iter()
            .find(|p| roots::normalize(p, home) == real_target)
        {
            return Some((file.clone(), r.position_seconds));
        }
        // The record's file isn't in the current ordering (excluded/moved
        // since) — fall through to starting fresh rather than erroring.
    }

    Some((ordered[0].clone(), 0.0))
}

/// Launches `name` under `root`, printing a one-line confirmation on
/// success. Shared by every path that ends in actually playing a series:
/// explicit `resume`/`start <series>`, the bare single-`in_progress` case,
/// and both pickers' selections.
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
        Some((file, position)) => {
            match launch::resume_playback(&config.mpv.command_override, home, &file, position) {
                Ok(()) => {
                    println!("{verb} \"{name}\" — {}", file.display());
                    ExitCode::SUCCESS
                }
                Err(e) => fail(&e.to_string()),
            }
        }
        None => fail(&format!("no episodes found for \"{name}\"")),
    }
}

/// Resolves an explicit `series [--root]` argument and plays it, bypassing
/// any picker entirely — TTY or not (CLAUDE.md: "An explicit series
/// argument always bypasses the picker entirely and proceeds directly").
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

fn print_target_error(err: &target::TargetError, command_name: &str) -> ExitCode {
    match err {
        target::TargetError::NotFound(name) => fail(&format!("no tracked series named \"{name}\"")),
        target::TargetError::RootNotFound(r) => fail(&format!("not a tracked root: {r}")),
        target::TargetError::Ambiguous { series, roots } => {
            eprintln!("Ambiguous: \"{series}\" exists under more than one tracked root:");
            eprintln!();
            for (i, r) in roots.iter().enumerate() {
                eprintln!("  {}. {r}", i + 1);
            }
            eprintln!();
            eprintln!("Re-run with --root to specify which one:");
            for r in roots {
                eprintln!("  tsuzuki {command_name} \"{series}\" --root {r}");
            }
            ExitCode::FAILURE
        }
    }
}

/// `tsuzuki` / `tsuzuki resume [series] [--root <root>]` — design.md §6's
/// 0/1/many logic. An explicit `series` always bypasses this entirely.
fn resume_command(
    series: Option<String>,
    root: Option<PathBuf>,
    config: &Config,
    state_path: &Path,
    home: Option<&Path>,
) -> ExitCode {
    let (state, warning) = match reconcile_state(config, state_path, home) {
        Ok(v) => v,
        Err(code) => return code,
    };
    if let Some(w) = &warning {
        print_scan_warning(w);
    }

    if let Some(series) = series {
        return explicit_play(
            &series,
            root.as_deref(),
            config,
            &state,
            home,
            "resume",
            "Resuming",
        );
    }

    let overviews = overview::all_series(config, &state, home);
    let in_progress: Vec<_> = overviews
        .into_iter()
        .filter(|o| o.status == Some(Status::InProgress))
        .collect();

    match in_progress.len() {
        // Zero `in_progress` series falls through to `start`'s behavior
        // (design.md §6).
        0 => start_command(None, None, config, state_path, home),
        1 => {
            let o = &in_progress[0];
            play_series(config, &state, &o.root, &o.name, home, "Resuming")
        }
        _ => {
            let is_tty = picker::stdin_is_tty();
            let outcome = picker::pick(
                &in_progress,
                |o| {
                    let ep = o
                        .episode_number
                        .map(|e| format!("ep. {e}"))
                        .unwrap_or_else(|| "unknown episode".to_string());
                    format!("{} ({ep})", o.name)
                },
                is_tty,
                "Resume which series?",
                |line| println!("{line}"),
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

/// `tsuzuki status` — every currently `in_progress` series (`current` was
/// cut per CLAUDE.md; this is its replacement).
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
        return ExitCode::SUCCESS;
    }

    for o in &in_progress {
        let ep = o
            .episode_number
            .map(|e| format!("ep. {e}"))
            .unwrap_or_else(|| "unknown episode".to_string());
        println!("{} — {ep}", o.name);
    }
    if in_progress.len() > 1 {
        println!();
        println!("Running `tsuzuki` bare will prompt among these.");
    }
    ExitCode::SUCCESS
}

/// `tsuzuki list` — every tracked series, grouped/ordered per CLAUDE.md's
/// "`list` ordering and episode-number display".
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
        return ExitCode::SUCCESS;
    }

    for o in &overviews {
        let status_label = match o.status {
            Some(Status::InProgress) => "in progress",
            Some(Status::Paused) => "paused",
            Some(Status::Dropped) => "dropped",
            None => "never started",
        };
        let ep = o
            .episode_number
            .map(|e| format!(", ep. {e}"))
            .unwrap_or_default();
        println!("{} — {status_label}{ep} ({} files)", o.name, o.file_count);
    }
    ExitCode::SUCCESS
}

/// `tsuzuki start [series] [--root <root>]` — design.md §6, CLAUDE.md's
/// three-display-word decision. Lists/picks among everything *not*
/// currently `in_progress`.
fn start_command(
    series: Option<String>,
    root: Option<PathBuf>,
    config: &Config,
    state_path: &Path,
    home: Option<&Path>,
) -> ExitCode {
    let (state, warning) = match reconcile_state(config, state_path, home) {
        Ok(v) => v,
        Err(code) => return code,
    };
    if let Some(w) = &warning {
        print_scan_warning(w);
    }

    if let Some(series) = series {
        return explicit_play(
            &series,
            root.as_deref(),
            config,
            &state,
            home,
            "start",
            "Starting",
        );
    }

    let overviews = overview::all_series(config, &state, home);
    let eligible = overview::sorted_for_list(overview::eligible_for_start(&overviews));

    if eligible.is_empty() {
        println!("Nothing to start.");
        return ExitCode::SUCCESS;
    }

    println!("Available to start:");
    println!();

    let is_tty = picker::stdin_is_tty();
    let outcome = picker::pick(
        &eligible,
        overview::start_line,
        is_tty,
        "Start which one?",
        |line| println!("{line}"),
    );
    match outcome {
        picker::PickerOutcome::Selected(o) => {
            let verb = match o.status {
                Some(Status::Paused) => "Resuming",
                Some(Status::Dropped) => "Rewatching",
                _ => "Starting",
            };
            play_series(config, &state, &o.root, &o.name, home, verb)
        }
        picker::PickerOutcome::Listed => {
            println!();
            println!("Run `tsuzuki start <series>` to pick one.");
            ExitCode::FAILURE
        }
        picker::PickerOutcome::Cancelled => ExitCode::FAILURE,
    }
}

/// `tsuzuki pause <series> [--root <root>]`.
fn pause_command(
    series: &str,
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

    let root = match target::resolve_series_root(config, series, root_arg, home) {
        Ok(r) => r,
        Err(e) => return print_target_error(&e, "pause"),
    };

    match pause_drop::pause(&mut state, &root, series) {
        Ok(pause_drop::Outcome::Changed) => {
            if let Err(e) = state::write(state_path, &state) {
                return fail(&format!(
                    "could not write state at {}: {e}",
                    state_path.display()
                ));
            }
            println!("paused: \"{series}\"");
            ExitCode::SUCCESS
        }
        Ok(pause_drop::Outcome::NoOp) => {
            println!("already paused: \"{series}\"");
            ExitCode::SUCCESS
        }
        Err(e) => fail(&e.to_string()),
    }
}

/// `tsuzuki drop <series> [--root <root>]`.
fn drop_command(
    series: &str,
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

    let root = match target::resolve_series_root(config, series, root_arg, home) {
        Ok(r) => r,
        Err(e) => return print_target_error(&e, "drop"),
    };

    match pause_drop::drop_series(&mut state, &root, series) {
        Ok(pause_drop::Outcome::Changed) => {
            if let Err(e) = state::write(state_path, &state) {
                return fail(&format!(
                    "could not write state at {}: {e}",
                    state_path.display()
                ));
            }
            println!("dropped: \"{series}\"");
            ExitCode::SUCCESS
        }
        Ok(pause_drop::Outcome::NoOp) => {
            println!("already dropped: \"{series}\"");
            ExitCode::SUCCESS
        }
        Err(e) => fail(&e.to_string()),
    }
}
