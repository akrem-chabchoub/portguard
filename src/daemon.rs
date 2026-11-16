use crate::config::{self, ConflictMode, Config};
use crate::detect::{
    infer_wanted_port, is_go_run_command, is_go_temp_exe, looks_like_server_process,
};
use crate::port::{find_free_port, free_port, is_protected, listeners_on_port};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone)]
struct Proc {
    pid: u32,
    command: String,
}

pub fn run_daemon() -> Result<()> {
    let dir = config::ensure_dirs()?;
    let cfg = config::load_config()?;
    let log_path = dir.join("daemon.log");
    log(
        &log_path,
        &format!(
            "daemon started mode={:?} interval_ms={}",
            cfg.mode, cfg.interval_ms
        ),
    )?;

    let mut seen: HashSet<u32> = list_processes()?
        .into_iter()
        .map(|p| p.pid)
        .collect();
    let mut managed: HashSet<u32> = HashSet::new();

    loop {
        if let Err(err) = tick(&cfg, &mut seen, &mut managed, &log_path) {
            log(&log_path, &format!("tick error: {err:#}"))?;
        }
        thread::sleep(Duration::from_millis(cfg.interval_ms.max(200)));
    }
}

fn tick(
    cfg: &Config,
    seen: &mut HashSet<u32>,
    managed: &mut HashSet<u32>,
    log_path: &std::path::Path,
) -> Result<()> {
    let procs = list_processes()?;
    let current: HashSet<u32> = procs.iter().map(|p| p.pid).collect();
    managed.retain(|pid| current.contains(pid));

    for proc in &procs {
        if seen.contains(&proc.pid) || managed.contains(&proc.pid) {
            continue;
        }
        if proc.command.contains("portguard") {
            continue;
        }
        if !looks_like_server_process(&proc.command) {
            continue;
        }

        let cwd = process_cwd(proc.pid).ok();
        if is_go_run_command(&proc.command) || is_go_temp_exe(&proc.command) {
            let bad_cwd = match cwd.as_deref() {
                None => true,
                Some(path) => path == Path::new("/") || path.as_os_str().is_empty(),
            };
            if bad_cwd {
                continue;
            }
        }

        let Some(wanted) = infer_wanted_port(&proc.command, cwd.as_deref()) else {
            continue;
        };
        if is_protected(wanted) {
            continue;
        }

        let holders = listeners_on_port(wanted)?;
        let others: Vec<_> = holders
            .into_iter()
            .filter(|h| h.pid != proc.pid)
            .collect();
        if others.is_empty() {
            continue;
        }

        // Prefer acting on `go run`. Skip temp exe if parent go run exists.
        if is_go_temp_exe(&proc.command)
            && procs.iter().any(|p| {
                is_go_run_command(&p.command) && process_cwd(p.pid).ok() == cwd
            })
        {
            continue;
        }

        log(
            log_path,
            &format!(
                "conflict: pid {} cmd=`{}` wanted={} others={}",
                proc.pid,
                proc.command,
                wanted,
                others
                    .iter()
                    .map(|h| format!("{}:{}", h.command, h.pid))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        )?;

        match cfg.mode {
            ConflictMode::Free => {
                log(
                    log_path,
                    &format!(
                        "free mode: pid {} wants busy port {} -> stopping old listener(s)",
                        proc.pid, wanted
                    ),
                )?;
                let _ = free_port(wanted, false)?;
                notify(&format!(
                    "portguard freed port {wanted} for pid {}",
                    proc.pid
                ));
            }
            ConflictMode::Realloc => {
                let next = find_free_port(wanted.saturating_add(1))?;
                log(
                    log_path,
                    &format!(
                        "realloc mode: pid {} wanted {} busy -> restart on {} (cwd={})",
                        proc.pid,
                        wanted,
                        next,
                        cwd.as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "?".into())
                    ),
                )?;
                if let Some(new_pid) =
                    restart_on_port(proc, cwd.as_deref(), wanted, next, log_path)?
                {
                    managed.insert(new_pid);
                }
                notify(&format!(
                    "portguard moved app to port {next} (wanted {wanted})"
                ));
            }
        }
    }

    *seen = current;
    Ok(())
}

fn restart_on_port(
    proc: &Proc,
    cwd: Option<&Path>,
    wanted: u16,
    next: u16,
    log_path: &std::path::Path,
) -> Result<Option<u32>> {
    let argv = restart_argv(&proc.command, cwd);
    if argv.is_empty() {
        anyhow::bail!("empty argv for pid {}", proc.pid);
    }

    let _ = Command::new("kill")
        .args(["-TERM", &proc.pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    thread::sleep(Duration::from_millis(150));
    let _ = Command::new("kill")
        .args(["-KILL", &proc.pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    let program = resolve_program(&argv[0]);
    let rewritten = rewrite_args(&argv[1..], wanted, next);
    let mut child = Command::new(&program);
    child.args(&rewritten);
    if let Some(cwd) = cwd {
        child.current_dir(cwd);
    }
    child.env("PATH", enriched_path());
    child.env("PORTGUARD_MANAGED", "1");
    child.env("PORT", next.to_string());
    child.env("SERVER_PORT", next.to_string());
    child.env("APP_PORT", next.to_string());
    child.env("PORTGUARD_PORT", next.to_string());

    match child.spawn() {
        Ok(spawned) => {
            let new_pid = spawned.id();
            log(
                log_path,
                &format!("restarted `{program}` as pid {new_pid} on port {next}"),
            )?;
            let _ = fs::write(
                config::portguard_dir()?.join("last-mapping"),
                format!("{wanted}->{next}\n"),
            );
            Ok(Some(new_pid))
        }
        Err(err) => {
            log(
                log_path,
                &format!("failed to restart `{program}` on port {next}: {err}"),
            )?;
            Ok(None)
        }
    }
}

fn restart_argv(command: &str, cwd: Option<&Path>) -> Vec<String> {
    // Always relaunch Go via `go run` in the project folder when possible.
    if is_go_run_command(command) || is_go_temp_exe(command) {
        if let Some(cwd) = cwd {
            if cwd.join("main.go").exists() {
                return vec!["go".into(), "run".into(), "main.go".into()];
            }
        }
    }
    shell_split(command)
}

fn rewrite_args(args: &[String], wanted: u16, next: u16) -> Vec<String> {
    let wanted_s = wanted.to_string();
    let next_s = next.to_string();
    let mut out = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if matches!(arg.as_str(), "--port" | "-port" | "-p" | "--listen-port")
            && i + 1 < args.len()
            && args[i + 1] == wanted_s
        {
            out.push(arg.clone());
            out.push(next_s.clone());
            i += 2;
            continue;
        }
        if let Some(rest) = arg.strip_prefix("-Dserver.port=") {
            if rest == wanted_s {
                out.push(format!("-Dserver.port={next_s}"));
                i += 1;
                continue;
            }
        }
        out.push(arg.replace(&format!(":{wanted_s}"), &format!(":{next_s}")));
        i += 1;
    }
    out
}

fn process_cwd(pid: u32) -> Result<PathBuf> {
    let output = Command::new("lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
        .output()
        .context("failed to run lsof for cwd")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(path) = line.strip_prefix('n') {
            return Ok(PathBuf::from(path));
        }
    }
    anyhow::bail!("cwd not found for pid {pid}")
}

fn list_processes() -> Result<Vec<Proc>> {
    let output = Command::new("ps")
        .args(["-ax", "-o", "pid=,command="])
        .output()
        .context("failed to run ps")?;
    if !output.status.success() {
        anyhow::bail!("ps failed");
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut procs = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((pid_s, command)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let Ok(pid) = pid_s.trim().parse() else {
            continue;
        };
        procs.push(Proc {
            pid,
            command: command.trim().to_string(),
        });
    }
    Ok(procs)
}

fn shell_split(command: &str) -> Vec<String> {
    command.split_whitespace().map(str::to_string).collect()
}

fn enriched_path() -> String {
    let mut parts = vec![
        "/usr/local/go/bin".to_string(),
        "/opt/homebrew/bin".to_string(),
        "/usr/local/bin".to_string(),
        "/usr/bin".to_string(),
        "/bin".to_string(),
    ];
    if let Ok(home) = std::env::var("HOME") {
        parts.insert(0, format!("{home}/go/bin"));
        parts.insert(0, format!("{home}/.cargo/bin"));
    }
    if let Ok(current) = std::env::var("PATH") {
        for part in current.split(':') {
            if !part.is_empty() && !parts.iter().any(|p| p == part) {
                parts.push(part.to_string());
            }
        }
    }
    parts.join(":")
}

fn resolve_program(program: &str) -> String {
    if program.contains('/') {
        return program.to_string();
    }
    let output = Command::new("/usr/bin/which")
        .arg(program)
        .env("PATH", enriched_path())
        .output();
    if let Ok(output) = output {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return path;
            }
        }
    }
    program.to_string()
}

fn log(path: &std::path::Path, message: &str) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    writeln!(file, "{message}")?;
    Ok(())
}

fn notify(message: &str) {
    let escaped = message.replace('\\', "\\\\").replace('"', "\\\"");
    let _ = Command::new("osascript")
        .args([
            "-e",
            &format!("display notification \"{escaped}\" with title \"portguard\""),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}
