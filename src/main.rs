mod cli;
mod config;
mod state;

use std::process::ExitCode;

use clap::{CommandFactory, Parser};

use cli::{Cli, Command, ConfigCommand, SyncCommand};

fn main() -> ExitCode {
    let cli = Cli::parse();

    let config_path = match config::config_path() {
        Ok(path) => path,
        Err(e) => return fail(&format!("could not resolve config path: {e}")),
    };
    if let Err(e) = config::read(&config_path) {
        return fail(&format!(
            "could not read config at {}: {e}",
            config_path.display()
        ));
    }

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

    dispatch(cli.command.unwrap_or(Command::Resume));
    ExitCode::SUCCESS
}

fn dispatch(command: Command) {
    match command {
        Command::Resume => stub("resume"),
        Command::Status => stub("status"),
        Command::Current => stub("current"),
        Command::List => stub("list"),
        Command::Add { .. } => stub("add"),
        Command::Start => stub("start"),
        Command::Pause { .. } => stub("pause"),
        Command::Drop { .. } => stub("drop"),
        Command::Exclude { .. } => stub("exclude"),
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
}

fn stub(name: &str) {
    println!("not implemented: {name}");
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
