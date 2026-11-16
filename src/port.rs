use anyhow::{bail, Context, Result};
use std::net::{SocketAddr, TcpListener};
use std::process::Command;
use std::thread;
use std::time::Duration;

const PROTECTED_PORTS: &[u16] = &[22, 53, 80, 443, 3306, 5432, 6379, 27017];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listener {
    pub pid: u32,
    pub command: String,
}

pub fn is_protected(port: u16) -> bool {
    PROTECTED_PORTS.contains(&port)
}

pub fn port_is_free(port: u16) -> bool {
    TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).is_ok()
}

pub fn find_free_port(start: u16) -> Result<u16> {
    let mut port = start;
    for _ in 0..1000 {
        if port_is_free(port) {
            return Ok(port);
        }
        port = port.saturating_add(1);
        if port == 0 {
            bail!("ran out of ports while searching from {start}");
        }
    }
    bail!("no free port found near {start}")
}

pub fn listeners_on_port(port: u16) -> Result<Vec<Listener>> {
    let output = Command::new("lsof")
        .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN"])
        .output()
        .context("failed to run lsof (is it installed?)")?;

    if !output.status.success() {
        // lsof exits 1 when nothing matches
        if output.stdout.is_empty() {
            return Ok(Vec::new());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("lsof failed for port {port}: {stderr}");
    }

    parse_lsof_listen_output(&String::from_utf8_lossy(&output.stdout))
}

fn parse_lsof_listen_output(stdout: &str) -> Result<Vec<Listener>> {
    let mut listeners = Vec::new();
    for (idx, line) in stdout.lines().enumerate() {
        if idx == 0 && line.starts_with("COMMAND") {
            continue;
        }
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 2 {
            continue;
        }
        let command = cols[0].to_string();
        let pid: u32 = cols[1]
            .parse()
            .with_context(|| format!("invalid pid in lsof line: {line}"))?;
        listeners.push(Listener { pid, command });
    }
    Ok(listeners)
}

pub fn free_port(port: u16, force: bool) -> Result<Vec<Listener>> {
    if is_protected(port) && !force {
        bail!(
            "port {port} is protected (database/system). pass --force if you really mean it"
        );
    }

    let listeners = listeners_on_port(port)?;
    if listeners.is_empty() {
        return Ok(listeners);
    }

    for listener in &listeners {
        terminate_pid(listener.pid)?;
    }

    for _ in 0..20 {
        if port_is_free(port) {
            return Ok(listeners);
        }
        thread::sleep(Duration::from_millis(50));
    }

    bail!("port {port} still busy after terminating listeners")
}

fn terminate_pid(pid: u32) -> Result<()> {
    let status = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .with_context(|| format!("failed to send SIGTERM to pid {pid}"))?;

    if !status.success() {
        bail!("kill -TERM {pid} failed");
    }

    thread::sleep(Duration::from_millis(200));

    // If still alive, escalate.
    let still_there = Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if still_there {
        let status = Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()
            .with_context(|| format!("failed to send SIGKILL to pid {pid}"))?;
        if !status.success() {
            bail!("kill -KILL {pid} failed");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_ports_include_postgres() {
        assert!(is_protected(5432));
        assert!(!is_protected(8080));
    }

    #[test]
    fn parse_lsof_extracts_pid_and_command() {
        let sample = "\
COMMAND   PID USER   FD   TYPE DEVICE SIZE/OFF NODE NAME
Python  12345 me    3u  IPv4 0xtest      0t0  TCP *:8080 (LISTEN)
node    12346 me    4u  IPv6 0xtest      0t0  TCP *:8080 (LISTEN)
";
        let got = parse_lsof_listen_output(sample).unwrap();
        assert_eq!(
            got,
            vec![
                Listener {
                    pid: 12345,
                    command: "Python".into(),
                },
                Listener {
                    pid: 12346,
                    command: "node".into(),
                },
            ]
        );
    }

    #[test]
    fn find_free_port_returns_open_port() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let taken = listener.local_addr().unwrap().port();
        let free = find_free_port(taken).unwrap();
        assert_ne!(free, taken);
        assert!(port_is_free(free));
    }
}
