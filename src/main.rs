mod anitomy;
mod cli;
mod config;
mod doctor;
mod duration;
mod exclude;
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

/// `is_tty` is resolved once here (the one real call to
/// `picker::stdin_is_tty()` in the whole binary) and threaded down as a
/// plain `bool`, rather than `resume_command`/`start_command` each querying
/// it internally — this is what makes "an explicit series argument bypasses
/// the picker even when a TTY is available" a testable claim rather than
/// just a structural one only provable by reading the code.
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
        Command::Add { path } => add_command(&path, config, config_path),
        Command::Start { series, root } => {
            start_command(series, root, config, state_path, home, is_tty)
        }
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
    }
}

/// `tsuzuki add <path>` — validates, checks idempotency, and persists a new
/// tracked root (CLAUDE.md's "`tsuzuki add` validation and idempotency").
fn add_command(path: &Path, config: &mut Config, config_path: &Path) -> ExitCode {
    match roots::add_root(config, path) {
        Ok(roots::AddOutcome::AlreadyTracked(as_typed)) => {
            confirm(&as_typed.display().to_string(), "already tracked");
            ExitCode::SUCCESS
        }
        Ok(roots::AddOutcome::Added(as_typed)) => {
            if let Err(e) = config::write(config_path, config) {
                return fail(&format!(
                    "could not write config at {}: {e}",
                    config_path.display()
                ));
            }
            confirm(&as_typed.display().to_string(), "added");
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
            confirm(
                &format!("\"{relative_path}\""),
                &format!("already excluded (under {root})"),
            );
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
            confirm(
                &format!("\"{relative_path}\""),
                &format!("excluded (under {root})"),
            );
            ExitCode::SUCCESS
        }
        Err(exclude::ExcludeError::Ambiguous {
            relative_path,
            matches,
        }) => {
            let roots: Vec<String> = matches.into_iter().map(|m| m.root).collect();
            print_ambiguous(&relative_path, "exclude", &roots)
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
        println!("⚠ {}", candidate.relative_path);
        println!("  → tsuzuki exclude \"{}\"", candidate.relative_path);
    }
}

/// Bare `tsuzuki config` — CLAUDE.md's CLI bare-invocation contract: `config`
/// *does* have a single default action, showing the resolved config, the
/// same way bare `sync` runs the sync rather than printing a menu. Prints
/// the real config.toml path as a `#` (valid-TOML) comment header, the
/// fully resolved config — defaults filled in for anything missing from the
/// file on disk, which `cat config.toml` can't show you — and a footer
/// pointing at `config edit`, the one other action here.
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

/// `tsuzuki config edit` — opens the real config.toml in `$EDITOR`,
/// blocking until it exits (unlike `launch.rs`'s fire-and-forget mpv spawn,
/// this one needs the real terminal and needs Tsuzuki to actually wait).
/// No guessed fallback if `$EDITOR` is unset — this project only ever picks
/// an external program from a curated, verified candidate list (mpv's
/// native/Flatpak detection), never a blind guess, and there's no such list
/// for editors. After the editor exits, re-reads the file through the same
/// `config::read` every other invocation uses — a syntax error introduced
/// by the edit is surfaced immediately, not left for the next command.
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

/// `✓ <subject> — <result>` — the one-shot confirmation shape shared by
/// `add`, `exclude`, `pause`, `drop`, and `sync link`, matching the same
/// `✓`/`⚠`/`✗` vocabulary `doctor`/`sync` already established.
fn confirm(subject: &str, result: &str) {
    println!("✓ {subject} — {result}");
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
                "⚠ configured [mpv] watch_later_dir \"{}\" does not exist",
                p.display()
            );
        }
    }
}

/// What to actually play: the resolved file, starting position, and its
/// episode number.
struct PlaybackTarget {
    file: PathBuf,
    position_seconds: f64,
    episode_number: u32,
}

/// Resolves what file + starting position to actually play for a given
/// series: a real `in_progress`/`paused` record resumes at its saved
/// position; a `dropped` record or no record at all starts at the first
/// episode, position 0 (design.md §6's three `start` cases, and — since
/// `resume`/`start` share one selection mechanism per CLAUDE.md — this
/// single function backs both). `None` means the series currently has no
/// video files to play at all.
///
/// The episode number is always freshly resolved here via
/// `anitomy::episode_identity` — the exact same mechanism `reconcile.rs`
/// uses to populate a record's own `episode_number` — rather than trusting
/// `record.episode_number` directly. That field is only meaningful for a
/// non-`Dropped` record continuing at its own stored position; the
/// start-fresh case (no record, or `Dropped`) has no record-side episode
/// number to fall back on at all, and would otherwise show nothing.
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

    // `Some((idx, position))` only when a usable (non-`Dropped`) record's
    // own file was actually found in the current ordering — carrying both
    // its real index and its real stored position together, so there's no
    // way for one to resolve from the record while the other silently
    // defaults (the bug in an earlier draft of this function: treating
    // "index happens to be 0" as indistinguishable from "no record was
    // found," which would have shown position 0 even for a record
    // genuinely paused/in-progress at episode 1's own real position).
    let resolved = record
        .filter(|r| r.status != Status::Dropped)
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

    Some(PlaybackTarget {
        file: ordered[index].clone(),
        position_seconds,
        episode_number,
    })
}

/// `mm:ss` (or `h:mm:ss` past an hour) — matches the display format
/// CLAUDE.md's resume-message wording expects.
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
        Some(target) => {
            match launch::resume_playback(
                &config.mpv.command_override,
                home,
                &target.file,
                target.position_seconds,
            ) {
                Ok(()) => {
                    println!(
                        "✓ {verb} \"{name}\" — episode {} ({})",
                        target.episode_number,
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
            print_ambiguous(series, command_name, roots)
        }
    }
}

/// Shared rendering for "`subject` exists under more than one tracked root" —
/// used by both `print_target_error` (`resume`/`start`/`pause`/`drop`/`sync
/// link`'s `--root` resolution) and `exclude_command` (a previously separate,
/// independently-drifted implementation). Drops the numbered
/// resolved-path enumeration the two previous implementations disagreed on
/// showing — the exact `--root` re-invocation *is* the actionable content,
/// same "detail that's neither the result nor a next step is dropped"
/// reasoning `doctor`'s report already established.
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

/// `tsuzuki` / `tsuzuki resume [series] [--root <root>]` — design.md §6's
/// 0/1/many logic. An explicit `series` always bypasses this entirely.
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
        0 => start_command(None, None, config, state_path, home, is_tty),
        1 => {
            let o = &in_progress[0];
            play_series(config, &state, &o.root, &o.name, home, "Resuming")
        }
        _ => {
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
        println!("→ tsuzuki start");
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
        println!("→ tsuzuki add <path>");
        return ExitCode::SUCCESS;
    }

    const GROUPS: [(Option<Status>, &str); 4] = [
        (Some(Status::InProgress), "In progress"),
        (Some(Status::Paused), "Paused"),
        (Some(Status::Dropped), "Dropped"),
        (None, "Never started"),
    ];

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
        for o in items {
            let ep = o
                .episode_number
                .map(|e| format!(" — ep. {e}"))
                .unwrap_or_default();
            println!("  {}{ep} ({} files)", o.name, o.file_count);
        }
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
    is_tty: bool,
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

    let outcome = picker::pick(
        &eligible,
        overview::start_line,
        is_tty,
        "Start which one?",
        |line| println!("{line}"),
        picker::dialoguer_select,
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
            confirm(&format!("\"{series}\""), "paused");
            ExitCode::SUCCESS
        }
        Ok(pause_drop::Outcome::NoOp) => {
            confirm(&format!("\"{series}\""), "already paused");
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
            confirm(&format!("\"{series}\""), "dropped");
            ExitCode::SUCCESS
        }
        Ok(pause_drop::Outcome::NoOp) => {
            confirm(&format!("\"{series}\""), "already dropped");
            ExitCode::SUCCESS
        }
        Err(e) => fail(&e.to_string()),
    }
}

// --- Phase 8a: MAL OAuth login ---

/// `tsuzuki sync login` — design.md §7's one-time OAuth authorization,
/// Authorization Code grant with PKCE (`code_challenge_method=plain`, MAL
/// doesn't support S256). Orchestration lives here, matching this project's
/// existing convention (`doctor_command` does the same): `mal.rs` exposes
/// small, individually-testable building blocks; the actual sequencing and
/// all user-facing output happen in `main.rs`.
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

// --- Phase 8b: MAL linking and the two-way merge ---

/// Resolves a real, usable bearer token for `sync`/`sync link` — the one
/// top-level failure mode that aborts before touching any series at all
/// (no token, no possible sync), distinct from a per-series failure inside
/// the loop.
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

/// `tsuzuki sync` (bare) — design.md §7 + this phase's own instructions:
/// every available, non-paused series with real progress, attempt linking
/// first if not already linked, then merge if linked. Paused and orphaned
/// records are filtered before MAL authentication, so a library with no
/// eligible records performs no network work. One series failing never
/// aborts the run.
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

/// Comma-joined, zero-count categories omitted — "1 synced, 2 need manual
/// linking", never "1 synced, 2 need manual linking, 0 failed". Keeps the
/// common all-good run's summary a single short clause instead of padding
/// it with categories that didn't happen. `synced` counts only real pushes
/// and pulls; unchanged series are deliberately absent from the report.
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

/// Concise, action-oriented output for one series. A no-op produces no
/// lines: unchanged series are the normal case and listing every one hides
/// the results that matter. `✓` is reserved for a real push/pull, `⚠` for
/// required user action, and `✗` for a failure.
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

/// `tsuzuki sync link <series> --mal-id <id> [--root <root>]` — design.md
/// §7's manual override, for when exact-title matching fails. Verifies
/// `mal_id` actually appears on the user's MAL list before persisting the
/// link (see `sync.rs`'s `manual_link` doc comment for the reasoning).
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

    // Cheap, local validation first — same "check before spawn" ordering
    // `launch.rs` already follows — so an unresolvable/ambiguous series
    // name fails immediately, with no network call attempted at all.
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

// --- Phase 7: doctor ---

/// `tsuzuki doctor [--fix] [--forget-orphans]` — CLAUDE.md's Phase 7 full
/// check list (1a-1i). Reconciles against mpv reality first, per the same
/// "every state-touching command reconciles first" rule every other
/// state-touching command already follows — `--fix --forget-orphans` is a
/// real mutation of `state.toml`, so this counts. Prints one concise,
/// scannable line per conceptual check to stdout (this report *is* the
/// command's real output, not an incidental warning about something else —
/// CLAUDE.md's stderr convention is for warnings a command surfaces about
/// achieving some other goal, and doctor's entire goal is exactly this
/// report). Several install-scoped checks (mpv config, the companion
/// script) fold multiple per-install results into one row via
/// `doctor::combine` — CLAUDE.md's "doctor groups related checks
/// conceptually rather than one row per internal artifact"; a check with
/// nothing to check against (no mpv install detected at all) is omitted
/// entirely rather than printed as a hollow "skipped" row. Exits non-zero
/// only if at least one check is FAIL after any `--fix`/`--forget-orphans`
/// mutation is applied — never on WARN alone.
fn doctor_command(
    fix: bool,
    forget_orphans: bool,
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

/// "All checks passed." when every row is `Ok`; otherwise a plain issue
/// count (`Warn` and `Fail` both count — either is something worth the
/// user's attention, even though only `Fail` affects the exit code).
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, Instant};
    use tempfile::tempdir;

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
        // Any test that reaches `play_series` ends up calling
        // `launch::resume_playback` with the real, unmocked native/Flatpak
        // detection — on a machine with a real Flatpak mpv install (this
        // dev machine has one, discovered during the implementation
        // session), that would actually spawn it. `true` is a standard,
        // always-present, instantly-exiting program — bypasses detection
        // entirely and keeps these tests side-effect-free and fast.
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

    /// Writes a real mpv-style `watch_later` entry matching `record()`'s
    /// own stored position (200.0s) for a single-episode series. Needed
    /// since reconcile now runs before every state-touching command and
    /// auto-drops a single/last-episode `InProgress` record whose current
    /// file has no matching bookmark at all (CLAUDE.md's "When the CURRENT
    /// tracked episode itself completes" — genuinely correct behavior for
    /// a real absence, but these fixtures fabricate a bare `InProgress`
    /// record with no corresponding mpv state at all, which reconcile can
    /// no longer tell apart from "mpv deleted the bookmark, it's done").
    /// Matching the stored position exactly keeps reconcile a true no-op,
    /// restoring these tests' original intent: exercising `--root`
    /// disambiguation, not reconcile's completion handling.
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

    // --- Sweep item: `pause`/`drop` --root disambiguation against a real
    // name collision (two actually identically-named series under two real
    // tracked roots), verified via the resulting state.toml mutation. ---

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

        let exit = pause_command("Monster", Some(&root_b), &config, &state_path, None);
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

        let exit = drop_command("Monster", Some(&root_a), &config, &state_path, None);
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

        let exit = pause_command("Monster", None, &config, &state_path, None);
        assert_eq!(exit, ExitCode::FAILURE);

        let after = state::read(&state_path).unwrap();
        assert_eq!(
            after, state,
            "an ambiguous pause must mutate neither record"
        );
    }

    // --- Phase 8b: sync_link_command's --root disambiguation ---
    //
    // `sync_link_command` resolves `--root` (a cheap, local, no-network
    // check) before ever touching the network, mirroring pause/drop's own
    // ordering — so the ambiguous-without-`--root` case is fully testable
    // here without a real MAL token. A "root disambiguates AND completes a
    // real link" test is not attempted at this level: past target
    // resolution, `sync_link_command` hardcodes the real `mal::UreqClient`
    // and needs a genuine access token, which is exactly what the
    // `#[ignore]`d real-API integration test (`tests/mal_sync_integration.rs`)
    // covers instead — `sync.rs`'s own `manual_link` unit tests already
    // exhaustively cover the actual linking/persistence logic via a fake
    // client, so nothing about *that* is untested, just not re-verified a
    // second time through this real-network-dependent entry point.

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

    // --- Sweep item: `resume`/`start`'s explicit-arg path, --root
    // disambiguation via the shared `explicit_play`/`target::resolve_series_root`
    // mechanism, proven via exit code (one root's series has playable
    // episodes, the other's doesn't — a wrong resolution would flip which
    // call succeeds). ---

    #[test]
    fn explicit_play_root_disambiguates_between_two_real_identically_named_series() {
        let dir = tempdir().unwrap();
        let root_a = dir.path().join("Anime");
        let root_b = dir.path().join("AltAnime");
        // root_a's "Monster" exists but has no video files at all.
        fs::create_dir_all(root_a.join("Monster")).unwrap();
        // root_b's "Monster" has a real episode.
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
        // Multiple in_progress series exist — enough that the bare/no-arg
        // path would hit the `_ =>` many-choices branch and call the real
        // `dialoguer`-backed picker. `is_tty: true` is passed explicitly
        // (the whole point of threading it in from `dispatch` instead of
        // querying it inside this function) — if an explicit series
        // argument ever regressed into reaching the picker code under
        // these exact conditions, `dialoguer::Select::interact_opt()` would
        // either error out (no real controllable terminal in a test
        // harness) or, worse, hang. Run on a background thread with a
        // bounded wait so a real regression fails loudly instead of
        // hanging the whole test binary — same precedent as
        // `series.rs`'s symlink-cycle test.
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

    /// Runs `f` on a background thread and polls for completion instead of
    /// blocking on `.join()`, so a regression that hangs (e.g. an
    /// accidental interactive prompt in a non-interactive test) fails this
    /// test loudly via a bounded wait instead of hanging the whole test
    /// binary indefinitely.
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

    // --- Phase 7: doctor_command wiring ---
    //
    // These exercise the glue in `doctor_command` itself (orphan
    // detection/clearing, the `--fix`/`--forget-orphans` flow) — the
    // individual checks' own logic is already exhaustively unit-tested in
    // `doctor.rs`. `command_override = "true"` (via `tracked_config`) keeps
    // the mpv-availability check deterministic; the mpv.conf/tsuzuki.lua
    // checks still depend on this development machine's real, standing
    // Flatpak `io.mpv.Mpv` install being detected (same accepted dependency
    // `tracked_config`'s own doc comment and `launch.rs`'s tests already
    // rely on) — since the Flatpak candidate path is `home`-relative, this
    // is still fully contained within each test's own fabricated tempdir
    // home, never the real system's mpv config.

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

        // Not asserting the overall exit code here (this machine's real
        // Flatpak mpv install may independently FAIL the mpv.conf/
        // tsuzuki.lua checks against this fabricated, script-free home) —
        // only that state.toml is byte-for-byte unchanged, since nothing
        // here is actually orphaned or otherwise reconcilable.
        doctor_command(false, false, &config, &state_path, Some(&home));

        let after = state::read(&state_path).unwrap();
        assert_eq!(after, before);
    }
}
