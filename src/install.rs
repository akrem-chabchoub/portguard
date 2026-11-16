use crate::config::{self, home_dir};
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const LABEL: &str = "com.portguard.daemon";

pub fn install() -> Result<()> {
    let dir = config::ensure_dirs()?;
    config::write_default_config()?;

    let binary = current_binary()?;
    let plist_path = launch_agent_path()?;
    let out_log = dir.join("daemon.out.log");
    let err_log = dir.join("daemon.err.log");

    let path_env = enriched_path();
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
    <string>daemon</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{}</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{}</string>
  <key>StandardErrorPath</key>
  <string>{}</string>
</dict>
</plist>
"#,
        binary.display(),
        path_env,
        out_log.display(),
        err_log.display()
    );

    if let Some(parent) = plist_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(&plist_path, plist)
        .with_context(|| format!("failed to write {}", plist_path.display()))?;

    let _ = Command::new("launchctl")
        .args(["unload", &plist_path.to_string_lossy()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    let status = Command::new("launchctl")
        .args(["load", &plist_path.to_string_lossy()])
        .status()
        .context("failed to run launchctl load")?;
    if !status.success() {
        bail!("launchctl load failed for {}", plist_path.display());
    }

    println!("portguard: installed background agent ({LABEL})");
    println!("portguard: binary {}", binary.display());
    println!("portguard: config {}", config::config_path()?.display());
    println!("portguard: logs {}", dir.join("daemon.log").display());
    println!("portguard: default mode is realloc (edit config to use free)");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let plist_path = launch_agent_path()?;
    let _ = Command::new("launchctl")
        .args(["unload", &plist_path.to_string_lossy()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if plist_path.exists() {
        fs::remove_file(&plist_path)
            .with_context(|| format!("failed to remove {}", plist_path.display()))?;
    }
    println!("portguard: unloaded and removed {LABEL}");
    Ok(())
}

pub fn status() -> Result<()> {
    let plist_path = launch_agent_path()?;
    println!("plist: {}", plist_path.display());
    println!(
        "plist exists: {}",
        if plist_path.exists() { "yes" } else { "no" }
    );

    let output = Command::new("launchctl")
        .args(["list", LABEL])
        .output()
        .context("failed to run launchctl list")?;
    if output.status.success() {
        print!("{}", String::from_utf8_lossy(&output.stdout));
    } else {
        println!("agent: not loaded");
    }

    let mapping = config::portguard_dir()?.join("last-mapping");
    if mapping.exists() {
        let text = fs::read_to_string(mapping)?;
        println!("last mapping: {}", text.trim());
    }
    Ok(())
}

fn launch_agent_path() -> Result<PathBuf> {
    Ok(home_dir()?
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

fn current_binary() -> Result<PathBuf> {
    let path = std::env::current_exe().context("failed to resolve current executable")?;
    let cargo_bin = home_dir()?.join(".cargo/bin/portguard");
    if cargo_bin.exists() {
        return Ok(cargo_bin);
    }
    Ok(path)
}

fn enriched_path() -> String {
    let home = home_dir().ok();
    let mut parts = vec![
        "/usr/local/go/bin".to_string(),
        "/opt/homebrew/bin".to_string(),
        "/opt/homebrew/sbin".to_string(),
        "/usr/local/bin".to_string(),
        "/usr/bin".to_string(),
        "/bin".to_string(),
        "/usr/sbin".to_string(),
        "/sbin".to_string(),
    ];
    if let Some(home) = home {
        parts.insert(0, home.join("go/bin").display().to_string());
        parts.insert(0, home.join(".cargo/bin").display().to_string());
    }
    if let Ok(current) = std::env::var("PATH") {
        for part in current.split(':') {
            let part = part.trim();
            if !part.is_empty() && !parts.iter().any(|p| p == part) {
                parts.push(part.to_string());
            }
        }
    }
    parts.join(":")
}
