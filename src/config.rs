use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictMode {
    /// Keep the preferred port by stopping the old listener.
    Free,
    /// Keep the old listener; restart the new app on the next free port.
    Realloc,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub mode: ConflictMode,
    pub interval_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: ConflictMode::Realloc,
            interval_ms: 800,
        }
    }
}

pub fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

pub fn portguard_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join(".portguard"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(portguard_dir()?.join("config"))
}

pub fn ensure_dirs() -> Result<PathBuf> {
    let dir = portguard_dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    Ok(dir)
}

pub fn load_config() -> Result<Config> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(Config::default());
    }
    let text = fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(parse_config(&text))
}

pub fn write_default_config() -> Result<()> {
    ensure_dirs()?;
    let path = config_path()?;
    if path.exists() {
        return Ok(());
    }
    let body = "\
# realloc = move new app to a free port (default)
# free    = stop old process and keep the requested port
mode=realloc
interval_ms=800
";
    fs::write(&path, body).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn parse_config(text: &str) -> Config {
    let mut cfg = Config::default();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "mode" => {
                cfg.mode = match value.trim() {
                    "free" => ConflictMode::Free,
                    _ => ConflictMode::Realloc,
                };
            }
            "interval_ms" => {
                if let Ok(ms) = value.trim().parse() {
                    cfg.interval_ms = ms;
                }
            }
            _ => {}
        }
    }
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_free_mode() {
        let cfg = parse_config("mode=free\ninterval_ms=500\n");
        assert_eq!(cfg.mode, ConflictMode::Free);
        assert_eq!(cfg.interval_ms, 500);
    }
}
