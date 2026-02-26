use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[cfg(unix)]
fn set_file_mode(path: &std::path::Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("Failed to set permissions on {}", path.display()))
}

pub fn ensure_config_dir() -> Result<PathBuf> {
    let dir = config_dir()?;
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    set_file_mode(&dir, 0o700)?;
    Ok(dir)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_watch_clipboard")]
    pub watch_clipboard: bool,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_ms: u64,
    #[serde(default = "default_server_url")]
    pub server_url: String,
    #[serde(default = "default_database_name")]
    pub database_name: String,
}

fn default_watch_clipboard() -> bool {
    true
}

fn default_poll_interval() -> u64 {
    500
}

fn default_server_url() -> String {
    "https://maincloud.spacetimedb.com".to_string()
}

fn default_database_name() -> String {
    "clipsync".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            watch_clipboard: true,
            poll_interval_ms: default_poll_interval(),
            server_url: default_server_url(),
            database_name: default_database_name(),
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_dir()?.join("config.toml");
        if path.exists() {
            let contents = std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read config from {}", path.display()))?;
            toml::from_str(&contents)
                .with_context(|| format!("Failed to parse config from {}", path.display()))
        } else {
            Ok(Self::default())
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = config_dir()?.join("config.toml");
        ensure_config_dir()?;
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(&path, contents)
            .with_context(|| format!("Failed to write config to {}", path.display()))?;
        #[cfg(unix)]
        set_file_mode(&path, 0o600)?;
        Ok(())
    }
}

pub fn config_dir() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine config directory"))?
        .join("clipsync"))
}

fn device_id_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("device_id"))
}

fn token_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("token"))
}

pub fn load_device_id() -> Result<Option<String>> {
    let path = device_id_path()?;
    if path.exists() {
        let id = std::fs::read_to_string(&path)
            .with_context(|| "Failed to read device_id")?
            .trim()
            .to_string();
        Ok(Some(id))
    } else {
        Ok(None)
    }
}

pub fn save_device_id(id: &str) -> Result<()> {
    let path = device_id_path()?;
    ensure_config_dir()?;
    std::fs::write(&path, id).with_context(|| "Failed to write device_id")?;
    #[cfg(unix)]
    set_file_mode(&path, 0o600)?;
    Ok(())
}

pub fn load_token() -> Result<Option<String>> {
    let path = token_path()?;
    if path.exists() {
        let token = std::fs::read_to_string(&path)
            .with_context(|| "Failed to read token")?
            .trim()
            .to_string();
        Ok(Some(token))
    } else {
        Ok(None)
    }
}

pub fn save_token(token: &str) -> Result<()> {
    let path = token_path()?;
    ensure_config_dir()?;
    std::fs::write(&path, token).with_context(|| "Failed to write token")?;
    #[cfg(unix)]
    set_file_mode(&path, 0o600)?;
    Ok(())
}

fn user_id_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("user_id"))
}

pub fn load_user_id() -> Result<Option<u64>> {
    let path = user_id_path()?;
    if path.exists() {
        let content = std::fs::read_to_string(&path)
            .with_context(|| "Failed to read user_id")?;
        let id: u64 = content.trim().parse().context("Failed to parse user_id")?;
        Ok(Some(id))
    } else {
        Ok(None)
    }
}

pub fn save_user_id(user_id: u64) -> Result<()> {
    let path = user_id_path()?;
    ensure_config_dir()?;
    std::fs::write(&path, user_id.to_string()).with_context(|| "Failed to write user_id")?;
    #[cfg(unix)]
    set_file_mode(&path, 0o600)?;
    Ok(())
}

pub fn socket_path() -> PathBuf {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime_dir).join("clipsync.sock");
    }
    if let Ok(tmpdir) = std::env::var("TMPDIR") {
        return PathBuf::from(tmpdir).join("clipsync.sock");
    }
    let uid = nix::unistd::getuid().as_raw();
    PathBuf::from(format!("/tmp/clipsync-{}.sock", uid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_returns_valid_path() {
        let path = socket_path();
        assert!(path.to_str().is_some());
        assert!(path.to_str().unwrap().contains("clipsync"));
    }

    #[test]
    fn default_config_values() {
        let config = Config::default();
        assert!(config.watch_clipboard);
        assert_eq!(config.poll_interval_ms, 500);
        assert_eq!(config.database_name, "clipsync");
    }

    #[test]
    fn config_round_trip() {
        let config = Config {
            watch_clipboard: true,
            poll_interval_ms: 1000,
            server_url: "https://example.com".to_string(),
            database_name: "test".to_string(),
        };
        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.watch_clipboard, true);
        assert_eq!(deserialized.poll_interval_ms, 1000);
        assert_eq!(deserialized.server_url, "https://example.com");
        assert_eq!(deserialized.database_name, "test");
    }
}
