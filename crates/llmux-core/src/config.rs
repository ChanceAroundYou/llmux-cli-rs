use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    pub port: u16,
    pub data_dir: PathBuf,
    pub database_path: PathBuf,
    pub master_key: Option<String>,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("PORT must be a valid integer between 1 and 65535")]
    InvalidPort,
}

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_env_map(|key| std::env::var(key).ok())
    }

    pub fn from_env_map<F>(get: F) -> Result<Self, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let port = match get("PORT") {
            Some(raw) => raw.parse::<u16>().map_err(|_| ConfigError::InvalidPort)?,
            None => 25976,
        };

        let data_dir = match get("DATA_DIR") {
            Some(raw) => PathBuf::from(raw),
            None => default_data_dir(),
        };

        let database_path = data_dir.join("llmux_db.db");
        let master_key = get("MASTER_KEY").filter(|v| !v.trim().is_empty());

        Ok(Self {
            port,
            data_dir,
            database_path,
            master_key,
        })
    }
}

fn default_data_dir() -> PathBuf {
    if cfg!(windows) {
        let appdata = std::env::var("APPDATA").unwrap_or_else(|_| {
            let home = std::env::var("USERPROFILE").unwrap_or_default();
            format!("{home}\\AppData\\Roaming")
        });
        PathBuf::from(appdata).join("llmux")
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(home).join(".config").join("llmux")
    }
}
