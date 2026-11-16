mod config;
mod daemon;
mod detect;
mod install;
mod port;
mod run;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use run::{prepare_and_run, RunOptions};
use std::process::ExitCode;

/// Background port helper for local API work.
#[derive(Debug, Parser)]
#[command(name = "portguard", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Install and start the background agent (launchd)
    Install,
    /// Stop and remove the background agent
    Uninstall,
    /// Show whether the background agent is loaded
    Status,
    /// Run the background watcher (used by launchd)
    Daemon,
    /// Show who is listening on a port
    Who { port: u16 },
    /// Stop whatever is listening on a port
    Free {
        port: u16,
        /// Allow killing listeners on protected ports (db/system)
        #[arg(long)]
        force: bool,
    },
    /// Ensure a port is usable, then run a command (manual mode)
    Run {
        /// Preferred port for your app
        #[arg(short, long)]
        port: u16,
        /// If busy, pick the next free port and inject PORT/SERVER_PORT
        #[arg(long)]
        realloc: bool,
        /// Allow acting on protected ports
        #[arg(long)]
        force: bool,
        /// Command and args, after `--`
        #[arg(required = true, last = true)]
        cmd: Vec<String>,
    },
}

fn main() -> ExitCode {
    match real_main() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("portguard: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn real_main() -> Result<ExitCode> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Install => {
            install::install()?;
            Ok(ExitCode::SUCCESS)
        }
        Commands::Uninstall => {
            install::uninstall()?;
            Ok(ExitCode::SUCCESS)
        }
        Commands::Status => {
            install::status()?;
            Ok(ExitCode::SUCCESS)
        }
        Commands::Daemon => {
            daemon::run_daemon()?;
            Ok(ExitCode::SUCCESS)
        }
        Commands::Who { port } => {
            let listeners = port::listeners_on_port(port)?;
            if listeners.is_empty() {
                println!("port {port}: free");
            } else {
                for listener in listeners {
                    println!(
                        "port {port}: {} (pid {})",
                        listener.command, listener.pid
                    );
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Commands::Free { port, force } => {
            let killed = port::free_port(port, force)?;
            if killed.is_empty() {
                println!("port {port}: already free");
            } else {
                for listener in killed {
                    println!(
                        "stopped {} (pid {}) on port {port}",
                        listener.command, listener.pid
                    );
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Commands::Run {
            port,
            realloc,
            force,
            cmd,
        } => {
            if cmd.is_empty() {
                bail!("missing command after `--`");
            }
            let program = &cmd[0];
            let args = &cmd[1..];
            let mode = if realloc {
                config::ConflictMode::Realloc
            } else {
                config::ConflictMode::Free
            };
            let status = prepare_and_run(RunOptions {
                port,
                mode,
                force,
                program,
                args,
            })?;
            Ok(exit_code_from_status(status))
        }
    }
}

fn exit_code_from_status(status: std::process::ExitStatus) -> ExitCode {
    match status.code() {
        Some(code) if (0..=255).contains(&code) => ExitCode::from(code as u8),
        Some(_) => ExitCode::from(1),
        None => ExitCode::from(1),
    }
}
