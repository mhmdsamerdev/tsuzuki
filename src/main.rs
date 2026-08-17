mod anitomy;
mod cli;
mod config;
mod duration;
mod exclude;
mod mpv;
mod roots;
mod series;
mod state;

use std::path::Path;
use std::process::ExitCode;

use clap::{CommandFactory, Parser};

use cli::{Cli, Command, ConfigCommand, SyncCommand};
use config::Config;

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
    if let Err(e) = state::read(&state_path) {
        return fail(&format!(
            "could not read state at {}: {e}",
            state_path.display()
        ));
    }

    dispatch(
        cli.command.unwrap_or(Command::Resume),
        &mut config,
        &config_path,
    )
}

fn dispatch(command: Command, config: &mut Config, config_path: &Path) -> ExitCode {
    match command {
        Command::Resume => stub("resume"),
        Command::Status => stub("status"),
        Command::Current => stub("current"),
        Command::List => stub("list"),
        Command::Add { path } => return add_command(&path, config, config_path),
        Command::Start => stub("start"),
        Command::Pause { .. } => stub("pause"),
        Command::Drop { .. } => stub("drop"),
        Command::Exclude { path: None, .. } => suggest_candidates_command(config),
        Command::Exclude {
            path: Some(path),
            root,
        } => return exclude_command(&path, root.as_deref(), config, config_path),
        Command::Sync { action } => match action {
            None => stub("sync"),
            Some(SyncCommand::Login) => stub("sync login"),
            Some(SyncCommand::Link { .. }) => stub("sync link"),
        },
        Command::Config { action } => match action {
            None => print_config_help(),
            Some(ConfigCommand::Get { .. }) => stub("config get"),
            Some(ConfigCommand::Set { .. }) => stub("config set"),
            Some(ConfigCommand::List) => stub("config list"),
            Some(ConfigCommand::Edit) => stub("config edit"),
        },
        Command::Doctor => stub("doctor"),
    }
    ExitCode::SUCCESS
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
