use crate::config::ConflictMode;
use crate::port::{find_free_port, free_port, listeners_on_port, port_is_free};
use anyhow::{bail, Context, Result};
use std::process::{Command, ExitStatus};

pub struct RunOptions<'a> {
    pub port: u16,
    pub mode: ConflictMode,
    pub force: bool,
    pub program: &'a str,
    pub args: &'a [String],
}

pub fn prepare_and_run(opts: RunOptions<'_>) -> Result<ExitStatus> {
    let chosen_port = match opts.mode {
        ConflictMode::Free => {
            ensure_free(opts.port, opts.force)?;
            opts.port
        }
        ConflictMode::Realloc => {
            if port_is_free(opts.port) {
                println!("portguard: port {} is free", opts.port);
                opts.port
            } else {
                let listeners = listeners_on_port(opts.port)?;
                let who = listeners
                    .iter()
                    .map(|l| format!("{}(pid {})", l.command, l.pid))
                    .collect::<Vec<_>>()
                    .join(", ");
                let next = find_free_port(opts.port.saturating_add(1))?;
                println!(
                    "portguard: port {} busy [{}] -> using {}",
                    opts.port,
                    if who.is_empty() {
                        "unknown".to_string()
                    } else {
                        who
                    },
                    next
                );
                next
            }
        }
    };

    run_command(opts.program, opts.args, chosen_port, opts.mode)
}

fn ensure_free(port: u16, force: bool) -> Result<()> {
    if port_is_free(port) {
        println!("portguard: port {port} is free");
        return Ok(());
    }

    let listeners = listeners_on_port(port)?;
    if listeners.is_empty() {
        bail!("port {port} appears busy but lsof found no LISTEN process");
    }

    for listener in &listeners {
        println!(
            "portguard: port {port} held by {} (pid {})",
            listener.command, listener.pid
        );
    }

    let killed = free_port(port, force)?;
    println!(
        "portguard: freed port {port} (stopped {} process{})",
        killed.len(),
        if killed.len() == 1 { "" } else { "es" }
    );
    Ok(())
}

fn run_command(
    program: &str,
    args: &[String],
    port: u16,
    mode: ConflictMode,
) -> Result<ExitStatus> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    cmd.env("PORT", port.to_string());
    cmd.env("SERVER_PORT", port.to_string());
    cmd.env("APP_PORT", port.to_string());
    cmd.env("PORTGUARD_PORT", port.to_string());

    if mode == ConflictMode::Realloc {
        println!("portguard: starting with PORT={port}");
    } else {
        println!("portguard: starting {program}");
    }

    let status = cmd
        .status()
        .with_context(|| format!("failed to start `{program}`"))?;
    Ok(status)
}
