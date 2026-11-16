use std::fs;
use std::path::Path;

/// Parse a likely listen port from a process command line.
pub fn port_from_command_line(command: &str) -> Option<u16> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }

    for flags in ["--port", "-port", "-p", "--listen-port"] {
        if let Some(port) = port_after_flag(&tokens, flags) {
            return Some(port);
        }
    }

    for token in &tokens {
        if let Some(port) = port_after_prefix(token, "-Dserver.port=") {
            return Some(port);
        }
        if let Some(port) = port_after_prefix(token, "--server.port=") {
            return Some(port);
        }
        if let Some(port) = port_after_prefix(token, "PORT=") {
            return Some(port);
        }
        if let Some(port) = port_after_prefix(token, "SERVER_PORT=") {
            return Some(port);
        }
    }

    for token in &tokens {
        if let Some(port) = port_from_host_port(token) {
            return Some(port);
        }
    }

    None
}

/// Infer port from cmdline, then project files in cwd, then Go defaults.
pub fn infer_wanted_port(command: &str, cwd: Option<&Path>) -> Option<u16> {
    if let Some(port) = port_from_command_line(command) {
        return Some(port);
    }
    if let Some(cwd) = cwd {
        if let Some(port) = port_from_env_file(cwd) {
            return Some(port);
        }
        if let Some(port) = port_from_go_sources(cwd) {
            return Some(port);
        }
    }
    // Only guess 8080 for real project launches with a known cwd.
    if cwd.is_some() && is_go_run_command(command) {
        return Some(8080);
    }
    None
}

pub fn port_from_env_file(cwd: &Path) -> Option<u16> {
    let text = fs::read_to_string(cwd.join(".env")).ok()?;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == "PORT" || key.trim() == "SERVER_PORT" {
            if let Some(port) = parse_port(value.trim()) {
                return Some(port);
            }
            if let Some(port) = port_from_host_port(value.trim()) {
                return Some(port);
            }
        }
    }
    None
}

fn port_from_go_sources(cwd: &Path) -> Option<u16> {
    for name in ["main.go", "server.go", "app.go"] {
        let path = cwd.join(name);
        if let Some(port) = scan_go_file(&path) {
            return Some(port);
        }
    }
    None
}

fn scan_go_file(path: &Path) -> Option<u16> {
    let text = fs::read_to_string(path).ok()?;

    // Prefer explicit defaults on port-related lines: port = "8080"
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if !(lower.contains("port") || lower.contains("addr") || lower.contains("listen")) {
            continue;
        }
        if let Some(port) = first_port_literal(line) {
            return Some(port);
        }
    }

    // config.Get("PORT", "8080")
    if let Some(idx) = text.find("Get(\"PORT\"") {
        let end = idx.saturating_add(64).min(text.len());
        if let Some(port) = first_port_literal(&text[idx..end]) {
            return Some(port);
        }
    }

    None
}

fn first_port_literal(slice: &str) -> Option<u16> {
    let mut in_quote = false;
    let mut buf = String::new();
    for ch in slice.chars() {
        if ch == '"' || ch == '\'' {
            if in_quote {
                if let Ok(port) = buf.parse::<u16>() {
                    if (1024..=65535).contains(&port) {
                        return Some(port);
                    }
                }
                buf.clear();
                in_quote = false;
            } else {
                in_quote = true;
                buf.clear();
            }
            continue;
        }
        if in_quote {
            buf.push(ch);
        }
    }
    None
}

fn port_after_flag(tokens: &[&str], flag: &str) -> Option<u16> {
    tokens.windows(2).find_map(|pair| {
        if pair[0] == flag {
            parse_port(pair[1])
        } else {
            None
        }
    })
}

fn port_after_prefix(token: &str, prefix: &str) -> Option<u16> {
    token.strip_prefix(prefix).and_then(parse_port)
}

fn port_from_host_port(token: &str) -> Option<u16> {
    let cleaned = token.trim_matches(|c| c == '"' || c == '\'');
    let (_, maybe_port) = cleaned.rsplit_once(':')?;
    let port = parse_port(maybe_port)?;
    if (1024..=65535).contains(&port) {
        Some(port)
    } else {
        None
    }
}

fn parse_port(raw: &str) -> Option<u16> {
    let cleaned = raw.trim().trim_matches(|c| c == ',' || c == '"' || c == '\'');
    cleaned.parse().ok()
}

pub fn is_go_run_command(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    lower.contains("go run")
}

pub fn is_go_temp_exe(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    // go run temp builds live under /var/folders/.../T/go-build.../exe/main
    // Persistent cache binaries under Library/Caches are not safe to restart.
    lower.contains("/t/go-build") && lower.contains("/exe/main")
}

pub fn looks_like_server_process(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    if is_go_run_command(command) || is_go_temp_exe(command) {
        return true;
    }
    let names = [
        "java ",
        "/java ",
        "node ",
        "/node ",
        "npm ",
        "pnpm ",
        "yarn ",
        "python ",
        "python3 ",
        "uvicorn ",
        "gunicorn ",
        "ruby ",
        "php ",
        "dotnet ",
        "mvn ",
        "gradle ",
    ];
    names.iter().any(|n| lower.contains(n))
        || lower.contains("spring-boot")
        || lower.contains("-jar ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn detects_double_dash_port() {
        assert_eq!(
            port_from_command_line("myapi --port 8080 --debug"),
            Some(8080)
        );
    }

    #[test]
    fn detects_spring_port() {
        assert_eq!(
            port_from_command_line("java -Dserver.port=9090 -jar app.jar"),
            Some(9090)
        );
    }

    #[test]
    fn detects_host_port() {
        assert_eq!(
            port_from_command_line("bin/api -addr 0.0.0.0:3001"),
            Some(3001)
        );
    }

    #[test]
    fn ignores_non_serverish_short_ports_in_versions() {
        assert_eq!(port_from_command_line("tool --version 1:2"), None);
    }

    #[test]
    fn infers_port_from_env_file() {
        let dir = std::env::temp_dir().join(format!("portguard-env-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut file = fs::File::create(dir.join(".env")).unwrap();
        writeln!(file, "PORT=8091").unwrap();
        assert_eq!(infer_wanted_port("go run main.go", Some(&dir)), Some(8091));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn go_run_defaults_to_8080_with_cwd() {
        let dir = std::env::temp_dir().join(format!("portguard-go-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(infer_wanted_port("go run main.go", Some(&dir)), Some(8080));
        assert_eq!(infer_wanted_port("go run main.go", None), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_go_temp_exe() {
        assert!(looks_like_server_process(
            "/var/folders/xx/T/go-build123/b001/exe/main"
        ));
        assert!(!looks_like_server_process(
            "/Users/me/Library/Caches/go-build/ab/abcdef/main"
        ));
    }
}
