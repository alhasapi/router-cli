//! Credential and endpoint configuration.
//!
//! Resolution precedence (highest first):
//!   1. command-line flag
//!   2. `ROUTER_ADMIN_PASSWORD` environment variable (password only)
//!   3. `~/.config/router-cli/config.toml`
//!   4. built-in defaults (base URL, username)
//!
//! The admin password must never be committed to version control or logged.

use std::path::PathBuf;

pub const DEFAULT_BASE_URL: &str = "http://192.168.0.1";
pub const DEFAULT_USERNAME: &str = "admin";
pub const PASSWORD_ENV_VAR: &str = "ROUTER_ADMIN_PASSWORD";

/// Shape of the optional on-disk config file.
#[derive(Debug, Clone, Default)]
pub struct FileConfig {
    pub base_url: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// Resolved settings for a single invocation.
#[derive(Debug, Clone)]
pub struct Credentials {
    pub base_url: String,
    pub username: String,
    pub password: String,
}

/// `~/.config/router-cli`
pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|h| h.join(".config").join("router-cli"))
}

/// `~/.config/router-cli/config.toml`
pub fn config_file_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config.toml"))
}

/// Read and parse the optional config file, ignoring any missing/unparsable file.
pub fn load_file_config() -> Option<FileConfig> {
    let path = config_file_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    Some(parse_file_config(&raw))
}

/// Minimal `key = value` parser (also accepts `key = "quoted value"` and `#` comments).
fn parse_file_config(raw: &str) -> FileConfig {
    let mut cfg = FileConfig::default();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
        match key {
            "base_url" => cfg.base_url = Some(value.to_string()),
            "username" => cfg.username = Some(value.to_string()),
            "password" => cfg.password = Some(value.to_string()),
            _ => {}
        }
    }
    cfg
}

/// Combine CLI flags, the environment, the config file, and defaults.
pub fn resolve(
    base_url: Option<&str>,
    username: Option<&str>,
    password: Option<&str>,
) -> anyhow::Result<Credentials> {
    resolve_with(
        base_url,
        username,
        password,
        load_file_config().unwrap_or_default(),
    )
}

fn resolve_with(
    base_url: Option<&str>,
    username: Option<&str>,
    password: Option<&str>,
    file: FileConfig,
) -> anyhow::Result<Credentials> {
    let base_url = base_url
        .map(str::to_string)
        .or(file.base_url)
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

    let username = username
        .map(str::to_string)
        .or(file.username)
        .unwrap_or_else(|| DEFAULT_USERNAME.to_string());

    let password = password
        .map(str::to_string)
        .or_else(|| std::env::var(PASSWORD_ENV_VAR).ok())
        .or(file.password)
        .ok_or_else(|| {
            let hint = config_file_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "~/.config/router-cli/config.toml".to_string());
            anyhow::anyhow!(
                "no admin password configured: set {PASSWORD_ENV_VAR} or add `password = \"...\"` to {hint}"
            )
        })?;

    Ok(Credentials {
        base_url,
        username,
        password,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_with(username: Option<&str>) -> FileConfig {
        FileConfig {
            base_url: None,
            username: username.map(str::to_string),
            password: Some("pw".to_string()),
        }
    }

    #[test]
    fn username_precedence_flag_then_file_then_default() {
        // Default when nothing is set.
        let c = resolve_with(None, None, Some("pw"), FileConfig::default()).unwrap();
        assert_eq!(c.username, "admin");

        // Config-file value wins over the default.
        let c = resolve_with(None, None, Some("pw"), file_with(Some("carol"))).unwrap();
        assert_eq!(c.username, "carol");

        // Explicit flag wins over the config file.
        let c = resolve_with(None, Some("bob"), Some("pw"), file_with(Some("carol"))).unwrap();
        assert_eq!(c.username, "bob");
    }
}
