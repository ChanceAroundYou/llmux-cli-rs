use std::collections::HashMap;
use std::path::PathBuf;
use thiserror::Error;

/// 默认网关端口。线上默认 25975；测试环境通过 `.env` 里的 `PORT=25999` 覆盖。
const DEFAULT_PORT: u16 = 25975;

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
    /// 从环境变量读取配置，支持当前工作目录下的 `.env` 文件。
    /// 优先级：进程环境变量 > `.env` 文件 > 内置默认值。
    pub fn from_env() -> Result<Self, ConfigError> {
        let dotenv = load_dotenv();
        Self::from_env_map(|key| {
            std::env::var(key)
                .ok()
                .or_else(|| dotenv.get(key).cloned())
        })
    }

    pub fn from_env_map<F>(get: F) -> Result<Self, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let port = match get("PORT") {
            Some(raw) => raw.parse::<u16>().map_err(|_| ConfigError::InvalidPort)?,
            None => DEFAULT_PORT,
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

/// 读取当前工作目录下的 `.env` 文件（`KEY=VALUE`，`#` 开头为注释，值可带引号）。
/// 返回键值映射；文件不存在或解析失败时返回空映射，不报错。
fn load_dotenv() -> HashMap<String, String> {
    let mut map = HashMap::new();
    let content = match std::fs::read_to_string(".env") {
        Ok(content) => content,
        Err(_) => return map,
    };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches('"').trim();
        if key.is_empty() {
            continue;
        }
        let value = value.trim().trim_matches('"').to_string();
        map.insert(key.to_string(), value);
    }
    map
}
