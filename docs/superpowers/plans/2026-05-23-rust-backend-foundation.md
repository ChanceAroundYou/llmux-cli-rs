# LLMux Rust Backend Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a new Rust sibling project at `llmux_cli` that starts an Axum-based local gateway, serves the existing React UI contract, owns a fresh SQLite schema, and preserves the existing import/export JSON shape from `llmux-cli`.

**Architecture:** The existing `llmux-cli` directory remains the Bun/TypeScript reference implementation and should be treated as read-only. The new `llmux_cli` project is a Rust workspace with focused crates: `llmux-core` for config/crypto/database/domain logic, `llmux-server` for HTTP routes/static serving, and `llmux-bin` for the CLI binary. This first plan creates a working foundation only; provider adapters, protocol conversion, and SSE streaming should be implemented in follow-up plans after the API/config/data contracts are stable.

**Tech Stack:** Rust 2021, Tokio, Axum, Tower HTTP, Clap, SQLx SQLite, Serde, Tracing, AES-GCM, Scrypt, Rand, Directories, Reqwest, Rustls.

---

## Scope Boundary

This plan implements the Rust foundation and data/config contract layer. It does not implement the full OpenAI/Anthropic/Gemini gateway yet. Those should be separate plans:

1. OpenAI-compatible `/v1/chat/completions` + dispatcher + usage logging.
2. Anthropic ingress `/v1/messages` + OpenAI/Anthropic protocol mapping.
3. Gemini adapter + streaming conversion.
4. npm release packaging + platform binary distribution.

The foundation must preserve these decisions:

- New Rust project path: `E:\Web\llmux\llmux_cli`.
- Existing Bun project path: `E:\Web\llmux\llmux-cli`, reference only.
- Old `db.sqlite` compatibility is not required.
- Export/import JSON shape must remain compatible with the old Settings import/export feature.
- UI API response shapes should remain stable where implemented.

## File Structure

Create this structure in `E:\Web\llmux\llmux_cli`:

```text
llmux_cli/
  Cargo.toml
  crates/
    llmux-core/
      Cargo.toml
      src/
        lib.rs
        config.rs
        crypto.rs
        db.rs
        export_import.rs
        models.rs
        settings.rs
    llmux-server/
      Cargo.toml
      src/
        lib.rs
        app.rs
        error.rs
        routes/
          mod.rs
          health.rs
          settings.rs
          accounts.rs
          keys.rs
          models.rs
          usage.rs
        static_ui.rs
    llmux-bin/
      Cargo.toml
      src/
        main.rs
  migrations/
    0001_init.sql
  tests/
    config_contract.rs
    crypto_contract.rs
    export_import_contract.rs
    server_contract.rs
  ui-dist-placeholder/
    index.html
```

Responsibilities:

- `llmux-core/src/config.rs`: environment and default data directory handling.
- `llmux-core/src/crypto.rs`: encrypt/decrypt provider API keys for the new Rust database.
- `llmux-core/src/db.rs`: SQLite pool creation and migration runner.
- `llmux-core/src/models.rs`: typed domain and API/export DTOs.
- `llmux-core/src/export_import.rs`: old-compatible `version/accounts/aliases/keys/settings` import/export mapping.
- `llmux-core/src/settings.rs`: settings repository helpers.
- `llmux-server/src/app.rs`: Axum router assembly and shared app state.
- `llmux-server/src/routes/*`: HTTP handlers for the first UI/API contract slice.
- `llmux-server/src/static_ui.rs`: embedded/static UI fallback serving.
- `llmux-bin/src/main.rs`: CLI command parsing and server startup.
- `migrations/0001_init.sql`: clean Rust schema; not required to match old SQLite file.

---

### Task 1: Create Rust Workspace Skeleton

**Files:**
- Create: `Cargo.toml`
- Create: `crates/llmux-core/Cargo.toml`
- Create: `crates/llmux-core/src/lib.rs`
- Create: `crates/llmux-server/Cargo.toml`
- Create: `crates/llmux-server/src/lib.rs`
- Create: `crates/llmux-bin/Cargo.toml`
- Create: `crates/llmux-bin/src/main.rs`

- [ ] **Step 1: Write the workspace manifest**

Create `Cargo.toml`:

```toml
[workspace]
members = [
  "crates/llmux-core",
  "crates/llmux-server",
  "crates/llmux-bin",
]
resolver = "2"

[workspace.package]
edition = "2021"
license = "AGPL-3.0"
authors = ["Moody"]

[workspace.dependencies]
anyhow = "1"
async-trait = "0.1"
axum = "0.7"
base64 = "0.22"
clap = { version = "4", features = ["derive", "env"] }
directories = "5"
http = "1"
rand = "0.8"
reqwest = { version = "0.12", default-features = false, features = ["json", "stream", "rustls-tls"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sqlx = { version = "0.8", features = ["runtime-tokio", "sqlite", "macros", "migrate"] }
thiserror = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread", "signal"] }
tower-http = { version = "0.6", features = ["cors", "trace"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
url = "2"
zeroize = "1"
aes-gcm = "0.10"
scrypt = "0.11"
hex = "0.4"
```

- [ ] **Step 2: Write the core crate manifest**

Create `crates/llmux-core/Cargo.toml`:

```toml
[package]
name = "llmux-core"
version = "0.1.0"
edition.workspace = true
license.workspace = true
authors.workspace = true

[dependencies]
anyhow.workspace = true
serde.workspace = true
serde_json.workspace = true
sqlx.workspace = true
thiserror.workspace = true
directories.workspace = true
rand.workspace = true
zeroize.workspace = true
aes-gcm.workspace = true
scrypt.workspace = true
hex.workspace = true
```

- [ ] **Step 3: Write the core lib entrypoint**

Create `crates/llmux-core/src/lib.rs`:

```rust
pub mod config;
pub mod crypto;
pub mod db;
pub mod export_import;
pub mod models;
pub mod settings;
```

- [ ] **Step 4: Write the server crate manifest**

Create `crates/llmux-server/Cargo.toml`:

```toml
[package]
name = "llmux-server"
version = "0.1.0"
edition.workspace = true
license.workspace = true
authors.workspace = true

[dependencies]
anyhow.workspace = true
axum.workspace = true
http.workspace = true
llmux-core = { path = "../llmux-core" }
serde.workspace = true
serde_json.workspace = true
sqlx.workspace = true
thiserror.workspace = true
tokio.workspace = true
tower-http.workspace = true
tracing.workspace = true
```

- [ ] **Step 5: Write the server lib entrypoint**

Create `crates/llmux-server/src/lib.rs`:

```rust
pub mod app;
pub mod error;
pub mod routes;
pub mod static_ui;
```

- [ ] **Step 6: Write the binary crate manifest**

Create `crates/llmux-bin/Cargo.toml`:

```toml
[package]
name = "llmux"
version = "0.1.0"
edition.workspace = true
license.workspace = true
authors.workspace = true

[[bin]]
name = "llmux"
path = "src/main.rs"

[dependencies]
anyhow.workspace = true
clap.workspace = true
llmux-core = { path = "../llmux-core" }
llmux-server = { path = "../llmux-server" }
tokio.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
```

- [ ] **Step 7: Write a temporary main that compiles**

Create `crates/llmux-bin/src/main.rs`:

```rust
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "llmux")]
#[command(about = "Local AI API gateway and multiplexer")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    Start,
    Status,
    Stop,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Start) {
        Command::Start => println!("LLMux Rust server skeleton"),
        Command::Status => println!("Status command is reserved for daemon management."),
        Command::Stop => println!("Stop command is reserved for daemon management."),
    }
    Ok(())
}
```

- [ ] **Step 8: Run check**

Run:

```bash
cargo check
```

Expected: command exits successfully with no compile errors.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml crates/llmux-core/Cargo.toml crates/llmux-core/src/lib.rs crates/llmux-server/Cargo.toml crates/llmux-server/src/lib.rs crates/llmux-bin/Cargo.toml crates/llmux-bin/src/main.rs
git commit -m "feat: create rust workspace skeleton"
```

---

### Task 2: Implement Configuration Defaults

**Files:**
- Create: `crates/llmux-core/src/config.rs`
- Create: `tests/config_contract.rs`
- Modify: `crates/llmux-core/src/lib.rs`

- [ ] **Step 1: Write failing config contract tests**

Create `tests/config_contract.rs`:

```rust
use llmux_core::config::AppConfig;

#[test]
fn config_uses_default_port() {
    let config = AppConfig::from_env_map(|key| match key {
        "PORT" => None,
        "DATA_DIR" => Some("C:/tmp/llmux-test".to_string()),
        "MASTER_KEY" => None,
        _ => None,
    })
    .expect("config should load");

    assert_eq!(config.port, 25975);
}

#[test]
fn config_accepts_explicit_port_and_data_dir() {
    let config = AppConfig::from_env_map(|key| match key {
        "PORT" => Some("26000".to_string()),
        "DATA_DIR" => Some("C:/tmp/llmux-explicit".to_string()),
        "MASTER_KEY" => Some("secret".to_string()),
        _ => None,
    })
    .expect("config should load");

    assert_eq!(config.port, 26000);
    assert!(config.data_dir.ends_with("llmux-explicit"));
    assert_eq!(config.master_key.as_deref(), Some("secret"));
}

#[test]
fn config_rejects_invalid_port() {
    let err = AppConfig::from_env_map(|key| match key {
        "PORT" => Some("not-a-number".to_string()),
        "DATA_DIR" => Some("C:/tmp/llmux-invalid".to_string()),
        _ => None,
    })
    .expect_err("invalid port should fail");

    assert!(err.to_string().contains("PORT"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cargo test --test config_contract
```

Expected: FAIL because `llmux_core::config::AppConfig` is not implemented.

- [ ] **Step 3: Implement config**

Create `crates/llmux-core/src/config.rs`:

```rust
use directories::ProjectDirs;
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
    #[error("could not determine default data directory")]
    MissingDataDir,
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
            None => 25975,
        };

        let data_dir = match get("DATA_DIR") {
            Some(raw) => PathBuf::from(raw),
            None => default_data_dir()?,
        };

        let database_path = data_dir.join("db.sqlite");
        let master_key = get("MASTER_KEY").filter(|value| !value.trim().is_empty());

        Ok(Self {
            port,
            data_dir,
            database_path,
            master_key,
        })
    }
}

fn default_data_dir() -> Result<PathBuf, ConfigError> {
    let dirs = ProjectDirs::from("", "", "llmux").ok_or(ConfigError::MissingDataDir)?;
    Ok(dirs.config_dir().to_path_buf())
}
```

Confirm `crates/llmux-core/src/lib.rs` includes:

```rust
pub mod config;
pub mod crypto;
pub mod db;
pub mod export_import;
pub mod models;
pub mod settings;
```

- [ ] **Step 4: Run test to verify it passes**

Run:

```bash
cargo test --test config_contract
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/llmux-core/src/config.rs crates/llmux-core/src/lib.rs tests/config_contract.rs
git commit -m "feat: add rust configuration defaults"
```

---

### Task 3: Define Domain and Export/Import DTOs

**Files:**
- Create: `crates/llmux-core/src/models.rs`
- Create: `tests/export_import_contract.rs`
- Modify: `crates/llmux-core/src/lib.rs`

- [ ] **Step 1: Write failing export/import shape tests**

Create `tests/export_import_contract.rs`:

```rust
use llmux_core::models::{
    ExportAccount, ExportApiKey, ExportConfig, ExportModelAlias, ExportSetting,
};

#[test]
fn export_config_matches_existing_json_shape() {
    let config = ExportConfig {
        version: 1,
        accounts: vec![ExportAccount {
            alias: "main".to_string(),
            provider_id: "openai".to_string(),
            api_key: "sk-test".to_string(),
            base_url: None,
            anthropic_base_url: None,
            is_active: Some(1),
            weight: Some(1),
            notes: Some("primary".to_string()),
        }],
        aliases: vec![ExportModelAlias {
            alias: "gpt".to_string(),
            target_model: "gpt-4o".to_string(),
            provider_id: Some("openai".to_string()),
        }],
        keys: vec![ExportApiKey {
            name: "local".to_string(),
            key: "llmux-key".to_string(),
            allowed_models: Some("*".to_string()),
        }],
        settings: vec![ExportSetting {
            key: "port".to_string(),
            value: Some("25975".to_string()),
        }],
    };

    let value = serde_json::to_value(&config).expect("serialize");

    assert_eq!(value["version"], 1);
    assert_eq!(value["accounts"][0]["alias"], "main");
    assert_eq!(value["accounts"][0]["provider_id"], "openai");
    assert_eq!(value["accounts"][0]["api_key"], "sk-test");
    assert_eq!(value["aliases"][0]["target_model"], "gpt-4o");
    assert_eq!(value["keys"][0]["allowed_models"], "*");
    assert_eq!(value["settings"][0]["key"], "port");
}

#[test]
fn import_config_accepts_existing_json_shape_with_missing_optional_fields() {
    let raw = r#"
    {
      "version": 1,
      "accounts": [
        { "alias": "main", "provider_id": "openai", "api_key": "sk-test" }
      ],
      "aliases": [
        { "alias": "sonnet", "target_model": "claude-sonnet", "provider_id": "anthropic" }
      ],
      "keys": [
        { "name": "dev", "key": "llmux-dev", "allowed_models": "*" }
      ],
      "settings": [
        { "key": "port", "value": "25975" }
      ]
    }
    "#;

    let parsed: ExportConfig = serde_json::from_str(raw).expect("old export shape should parse");

    assert_eq!(parsed.version, 1);
    assert_eq!(parsed.accounts[0].alias, "main");
    assert_eq!(parsed.accounts[0].is_active, None);
    assert_eq!(parsed.accounts[0].weight, None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cargo test --test export_import_contract
```

Expected: FAIL because DTOs are not implemented.

- [ ] **Step 3: Implement DTOs and internal models**

Create `crates/llmux-core/src/models.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExportConfig {
    pub version: u32,
    #[serde(default)]
    pub accounts: Vec<ExportAccount>,
    #[serde(default)]
    pub aliases: Vec<ExportModelAlias>,
    #[serde(default)]
    pub keys: Vec<ExportApiKey>,
    #[serde(default)]
    pub settings: Vec<ExportSetting>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExportAccount {
    pub alias: String,
    pub provider_id: String,
    pub api_key: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub anthropic_base_url: Option<String>,
    #[serde(default)]
    pub is_active: Option<i64>,
    #[serde(default)]
    pub weight: Option<i64>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExportModelAlias {
    pub alias: String,
    pub target_model: String,
    #[serde(default)]
    pub provider_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExportApiKey {
    pub name: String,
    pub key: String,
    #[serde(default)]
    pub allowed_models: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExportSetting {
    pub key: String,
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, PartialEq)]
pub struct AccountRow {
    pub id: i64,
    pub alias: String,
    pub provider_id: String,
    pub api_key_encrypted: String,
    pub base_url: Option<String>,
    pub anthropic_base_url: Option<String>,
    pub is_active: i64,
    pub weight: i64,
    pub notes: Option<String>,
    pub limits_cache_json: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, PartialEq)]
pub struct AccountPublic {
    pub id: i64,
    pub alias: String,
    pub provider_id: String,
    pub base_url: Option<String>,
    pub anthropic_base_url: Option<String>,
    pub is_active: i64,
    pub weight: i64,
    pub notes: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, PartialEq)]
pub struct ApiKeyRow {
    pub id: i64,
    pub name: String,
    pub key: String,
    pub allowed_models: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, PartialEq)]
pub struct ModelAliasRow {
    pub id: i64,
    pub alias: String,
    pub target_model: String,
    pub provider_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, PartialEq)]
pub struct SettingRow {
    pub key: String,
    pub value: Option<String>,
}
```

Confirm `crates/llmux-core/src/lib.rs` includes:

```rust
pub mod config;
pub mod crypto;
pub mod db;
pub mod export_import;
pub mod models;
pub mod settings;
```

- [ ] **Step 4: Run test to verify it passes**

Run:

```bash
cargo test --test export_import_contract
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/llmux-core/src/models.rs crates/llmux-core/src/lib.rs tests/export_import_contract.rs
git commit -m "feat: define rust import export contracts"
```

---

### Task 4: Implement Fresh SQLite Schema and Migration Runner

**Files:**
- Create: `migrations/0001_init.sql`
- Create: `crates/llmux-core/src/db.rs`
- Modify: `crates/llmux-core/Cargo.toml`
- Create: `tests/db_contract.rs`

- [ ] **Step 1: Write failing database contract test**

Create `tests/db_contract.rs`:

```rust
use llmux_core::db::{connect_sqlite, migrate};
use sqlx::Row;

#[tokio::test]
async fn migration_creates_core_tables_and_seed_providers() {
    let pool = connect_sqlite("sqlite::memory:").await.expect("connect");
    migrate(&pool).await.expect("migrate");

    let provider_count: i64 = sqlx::query("SELECT COUNT(*) AS count FROM providers")
        .fetch_one(&pool)
        .await
        .expect("query providers")
        .get("count");

    let account_table: String = sqlx::query("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'accounts'")
        .fetch_one(&pool)
        .await
        .expect("accounts table")
        .get("name");

    assert_eq!(account_table, "accounts");
    assert!(provider_count >= 4);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cargo test --test db_contract
```

Expected: FAIL because `connect_sqlite` and `migrate` are not implemented.

- [ ] **Step 3: Add SQLx dependency for tests if needed**

Confirm root `Cargo.toml` already has:

```toml
sqlx = { version = "0.8", features = ["runtime-tokio", "sqlite", "macros", "migrate"] }
```

No change is needed if that line exists.

- [ ] **Step 4: Write migration SQL**

Create `migrations/0001_init.sql`:

```sql
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS providers (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  type TEXT NOT NULL,
  base_url TEXT
);

CREATE TABLE IF NOT EXISTS accounts (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  alias TEXT NOT NULL,
  provider_id TEXT NOT NULL,
  api_key_encrypted TEXT NOT NULL,
  base_url TEXT,
  anthropic_base_url TEXT,
  is_active INTEGER NOT NULL DEFAULT 1,
  weight INTEGER NOT NULL DEFAULT 1,
  notes TEXT,
  limits_cache_json TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  FOREIGN KEY(provider_id) REFERENCES providers(id)
);

CREATE TABLE IF NOT EXISTS model_aliases (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  alias TEXT NOT NULL UNIQUE,
  target_model TEXT NOT NULL,
  provider_id TEXT,
  FOREIGN KEY(provider_id) REFERENCES providers(id)
);

CREATE TABLE IF NOT EXISTS model_prices (
  model_id TEXT PRIMARY KEY,
  vendor TEXT,
  input_price REAL,
  output_price REAL,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS usage_logs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  timestamp_ms INTEGER NOT NULL,
  account_id INTEGER,
  provider_id TEXT,
  model TEXT,
  input_tokens INTEGER NOT NULL DEFAULT 0,
  output_tokens INTEGER NOT NULL DEFAULT 0,
  cache_read_input_tokens INTEGER NOT NULL DEFAULT 0,
  cache_creation_input_tokens INTEGER NOT NULL DEFAULT 0,
  latency_ms INTEGER NOT NULL DEFAULT 0,
  success INTEGER NOT NULL DEFAULT 0,
  error_message TEXT,
  is_test INTEGER NOT NULL DEFAULT 0,
  FOREIGN KEY(account_id) REFERENCES accounts(id)
);

CREATE INDEX IF NOT EXISTS idx_usage_logs_timestamp_ms ON usage_logs(timestamp_ms);
CREATE INDEX IF NOT EXISTS idx_usage_logs_account_id ON usage_logs(account_id);
CREATE INDEX IF NOT EXISTS idx_usage_logs_provider_id ON usage_logs(provider_id);
CREATE INDEX IF NOT EXISTS idx_usage_logs_model ON usage_logs(model);

CREATE TABLE IF NOT EXISTS settings (
  key TEXT PRIMARY KEY,
  value TEXT
);

CREATE TABLE IF NOT EXISTS api_keys (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  key TEXT NOT NULL UNIQUE,
  allowed_models TEXT NOT NULL DEFAULT '*',
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

INSERT OR IGNORE INTO providers (id, name, type) VALUES ('openai', 'OpenAI', 'openai');
INSERT OR IGNORE INTO providers (id, name, type) VALUES ('anthropic', 'Anthropic', 'anthropic');
INSERT OR IGNORE INTO providers (id, name, type) VALUES ('gemini', 'Google Gemini', 'gemini');
INSERT OR IGNORE INTO providers (id, name, type) VALUES ('custom-anthropic', 'Custom (Anthropic Compatible)', 'custom-anthropic');
```

- [ ] **Step 5: Implement database helpers**

Create `crates/llmux-core/src/db.rs`:

```rust
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Executor, SqlitePool};
use std::str::FromStr;

const INIT_SQL: &str = include_str!("../../../migrations/0001_init.sql");

pub async fn connect_sqlite(database_url: &str) -> anyhow::Result<SqlitePool> {
    if database_url == "sqlite::memory:" {
        return Ok(SqlitePoolOptions::new()
            .max_connections(1)
            .connect(database_url)
            .await?);
    }

    let options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);

    Ok(SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?)
}

pub async fn migrate(pool: &SqlitePool) -> anyhow::Result<()> {
    pool.execute("PRAGMA foreign_keys = ON;").await?;
    for statement in INIT_SQL.split(';') {
        let sql = statement.trim();
        if !sql.is_empty() {
            pool.execute(sql).await?;
        }
    }
    Ok(())
}

pub fn sqlite_url_from_path(path: &std::path::Path) -> String {
    format!("sqlite://{}", path.display().to_string().replace('\\', "/"))
}
```

- [ ] **Step 6: Run test to verify it passes**

Run:

```bash
cargo test --test db_contract
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add migrations/0001_init.sql crates/llmux-core/src/db.rs tests/db_contract.rs Cargo.toml crates/llmux-core/Cargo.toml
git commit -m "feat: add rust sqlite schema"
```

---

### Task 5: Implement New Rust Key Encryption

**Files:**
- Create: `crates/llmux-core/src/crypto.rs`
- Create: `tests/crypto_contract.rs`

- [ ] **Step 1: Write failing crypto tests**

Create `tests/crypto_contract.rs`:

```rust
use llmux_core::crypto::{decrypt_key, encrypt_key, get_or_create_master_key};
use std::fs;

#[test]
fn encryption_round_trips_api_key() {
    let encrypted = encrypt_key("sk-test", "master-secret").expect("encrypt");
    let decrypted = decrypt_key(&encrypted, "master-secret").expect("decrypt");

    assert_ne!(encrypted, "sk-test");
    assert_eq!(decrypted, "sk-test");
}

#[test]
fn encryption_rejects_wrong_master_key() {
    let encrypted = encrypt_key("sk-test", "master-secret").expect("encrypt");
    let err = decrypt_key(&encrypted, "wrong-secret").expect_err("wrong key should fail");

    assert!(err.to_string().contains("decrypt"));
}

#[test]
fn master_key_is_persisted() {
    let dir = std::env::temp_dir().join(format!("llmux-crypto-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");

    let first = get_or_create_master_key(&dir, None).expect("first key");
    let second = get_or_create_master_key(&dir, None).expect("second key");

    assert_eq!(first, second);
    assert!(dir.join("master.key").exists());

    let _ = fs::remove_dir_all(&dir);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cargo test --test crypto_contract
```

Expected: FAIL because crypto functions are not implemented.

- [ ] **Step 3: Implement crypto**

Create `crates/llmux-core/src/crypto.rs`:

```rust
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use rand::RngCore;
use scrypt::{scrypt, Params};
use std::fs;
use std::path::Path;
use thiserror::Error;
use zeroize::Zeroize;

const IV_LENGTH: usize = 12;
const SALT: &[u8] = b"llmux-rust-salt-standard";

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("failed to encrypt API key")]
    Encrypt,
    #[error("failed to decrypt API key")]
    Decrypt,
    #[error("invalid encrypted API key format")]
    InvalidFormat,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub fn get_or_create_master_key(data_dir: &Path, explicit: Option<&str>) -> Result<String, CryptoError> {
    if let Some(value) = explicit.filter(|value| !value.trim().is_empty()) {
        return Ok(value.to_string());
    }

    fs::create_dir_all(data_dir)?;
    let path = data_dir.join("master.key");
    if path.exists() {
        return Ok(fs::read_to_string(path)?.trim().to_string());
    }

    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let generated = hex::encode(bytes);
    bytes.zeroize();
    fs::write(&path, &generated)?;
    Ok(generated)
}

pub fn encrypt_key(plain_text: &str, master_key: &str) -> Result<String, CryptoError> {
    let mut iv = [0u8; IV_LENGTH];
    rand::thread_rng().fill_bytes(&mut iv);

    let key = derive_key(master_key)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| CryptoError::Encrypt)?;
    let encrypted = cipher
        .encrypt(Nonce::from_slice(&iv), plain_text.as_bytes())
        .map_err(|_| CryptoError::Encrypt)?;

    Ok(format!("{}.{}", hex::encode(iv), hex::encode(encrypted)))
}

pub fn decrypt_key(encrypted_text: &str, master_key: &str) -> Result<String, CryptoError> {
    let (iv_hex, content_hex) = encrypted_text
        .split_once('.')
        .ok_or(CryptoError::InvalidFormat)?;

    let iv = hex::decode(iv_hex).map_err(|_| CryptoError::InvalidFormat)?;
    let content = hex::decode(content_hex).map_err(|_| CryptoError::InvalidFormat)?;
    if iv.len() != IV_LENGTH {
        return Err(CryptoError::InvalidFormat);
    }

    let key = derive_key(master_key)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| CryptoError::Decrypt)?;
    let decrypted = cipher
        .decrypt(Nonce::from_slice(&iv), content.as_ref())
        .map_err(|_| CryptoError::Decrypt)?;

    String::from_utf8(decrypted).map_err(|_| CryptoError::Decrypt)
}

fn derive_key(master_key: &str) -> Result<[u8; 32], CryptoError> {
    let params = Params::recommended();
    let mut output = [0u8; 32];
    scrypt(master_key.as_bytes(), SALT, &params, &mut output).map_err(|_| CryptoError::Encrypt)?;
    Ok(output)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run:

```bash
cargo test --test crypto_contract
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/llmux-core/src/crypto.rs tests/crypto_contract.rs
git commit -m "feat: add rust api key encryption"
```

---

### Task 6: Implement Import/Export Repository Mapping

**Files:**
- Create: `crates/llmux-core/src/export_import.rs`
- Modify: `tests/export_import_contract.rs`

- [ ] **Step 1: Add failing import/export database round-trip test**

Append this test to `tests/export_import_contract.rs`:

```rust
use llmux_core::crypto::decrypt_key;
use llmux_core::db::{connect_sqlite, migrate};
use llmux_core::export_import::{export_config, import_config};
use sqlx::Row;

#[tokio::test]
async fn import_encrypts_accounts_and_export_returns_plaintext_shape() {
    let pool = connect_sqlite("sqlite::memory:").await.expect("connect");
    migrate(&pool).await.expect("migrate");

    let input = ExportConfig {
        version: 1,
        accounts: vec![ExportAccount {
            alias: "main".to_string(),
            provider_id: "openai".to_string(),
            api_key: "sk-test".to_string(),
            base_url: Some("https://example.com/v1".to_string()),
            anthropic_base_url: None,
            is_active: Some(1),
            weight: Some(2),
            notes: Some("primary".to_string()),
        }],
        aliases: vec![ExportModelAlias {
            alias: "fast".to_string(),
            target_model: "gpt-4o-mini".to_string(),
            provider_id: Some("openai".to_string()),
        }],
        keys: vec![ExportApiKey {
            name: "dev".to_string(),
            key: "llmux-dev".to_string(),
            allowed_models: Some("*".to_string()),
        }],
        settings: vec![ExportSetting {
            key: "port".to_string(),
            value: Some("25975".to_string()),
        }],
    };

    import_config(&pool, &input, "master-secret").await.expect("import");

    let encrypted: String = sqlx::query("SELECT api_key_encrypted FROM accounts WHERE alias = 'main'")
        .fetch_one(&pool)
        .await
        .expect("account row")
        .get("api_key_encrypted");

    assert_ne!(encrypted, "sk-test");
    assert_eq!(decrypt_key(&encrypted, "master-secret").expect("decrypt"), "sk-test");

    let exported = export_config(&pool, "master-secret").await.expect("export");

    assert_eq!(exported.version, 1);
    assert_eq!(exported.accounts[0].api_key, "sk-test");
    assert_eq!(exported.accounts[0].weight, Some(2));
    assert_eq!(exported.aliases[0].alias, "fast");
    assert_eq!(exported.keys[0].allowed_models.as_deref(), Some("*"));
    assert_eq!(exported.settings[0].key, "port");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cargo test --test export_import_contract
```

Expected: FAIL because `export_config` and `import_config` are not implemented.

- [ ] **Step 3: Implement import/export mapping**

Create `crates/llmux-core/src/export_import.rs`:

```rust
use crate::crypto::{decrypt_key, encrypt_key};
use crate::models::{
    ExportAccount, ExportApiKey, ExportConfig, ExportModelAlias, ExportSetting,
};
use sqlx::{Row, SqlitePool};

pub async fn import_config(
    pool: &SqlitePool,
    config: &ExportConfig,
    master_key: &str,
) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;

    for account in &config.accounts {
        let encrypted = encrypt_key(&account.api_key, master_key)?;
        sqlx::query(
            r#"
            INSERT INTO accounts (
              alias, provider_id, api_key_encrypted, base_url, anthropic_base_url,
              is_active, weight, notes
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&account.alias)
        .bind(&account.provider_id)
        .bind(encrypted)
        .bind(&account.base_url)
        .bind(&account.anthropic_base_url)
        .bind(account.is_active.unwrap_or(1))
        .bind(account.weight.unwrap_or(1))
        .bind(&account.notes)
        .execute(&mut *tx)
        .await?;
    }

    for alias in &config.aliases {
        sqlx::query(
            r#"
            INSERT OR REPLACE INTO model_aliases (alias, target_model, provider_id)
            VALUES (?, ?, ?)
            "#,
        )
        .bind(&alias.alias)
        .bind(&alias.target_model)
        .bind(&alias.provider_id)
        .execute(&mut *tx)
        .await?;
    }

    for key in &config.keys {
        sqlx::query(
            r#"
            INSERT OR REPLACE INTO api_keys (name, key, allowed_models)
            VALUES (?, ?, ?)
            "#,
        )
        .bind(&key.name)
        .bind(&key.key)
        .bind(key.allowed_models.as_deref().unwrap_or("*"))
        .execute(&mut *tx)
        .await?;
    }

    for setting in &config.settings {
        sqlx::query(
            r#"
            INSERT OR REPLACE INTO settings (key, value)
            VALUES (?, ?)
            "#,
        )
        .bind(&setting.key)
        .bind(&setting.value)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

pub async fn export_config(pool: &SqlitePool, master_key: &str) -> anyhow::Result<ExportConfig> {
    let account_rows = sqlx::query(
        r#"
        SELECT alias, provider_id, api_key_encrypted, base_url, anthropic_base_url,
               is_active, weight, notes
        FROM accounts
        ORDER BY id ASC
        "#,
    )
    .fetch_all(pool)
    .await?;

    let mut accounts = Vec::with_capacity(account_rows.len());
    for row in account_rows {
        accounts.push(ExportAccount {
            alias: row.get("alias"),
            provider_id: row.get("provider_id"),
            api_key: decrypt_key(row.get::<String, _>("api_key_encrypted").as_str(), master_key)?,
            base_url: row.get("base_url"),
            anthropic_base_url: row.get("anthropic_base_url"),
            is_active: Some(row.get("is_active")),
            weight: Some(row.get("weight")),
            notes: row.get("notes"),
        });
    }

    let aliases = sqlx::query(
        r#"
        SELECT alias, target_model, provider_id
        FROM model_aliases
        ORDER BY id ASC
        "#,
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| ExportModelAlias {
        alias: row.get("alias"),
        target_model: row.get("target_model"),
        provider_id: row.get("provider_id"),
    })
    .collect();

    let keys = sqlx::query(
        r#"
        SELECT name, key, allowed_models
        FROM api_keys
        ORDER BY id ASC
        "#,
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| ExportApiKey {
        name: row.get("name"),
        key: row.get("key"),
        allowed_models: Some(row.get("allowed_models")),
    })
    .collect();

    let settings = sqlx::query(
        r#"
        SELECT key, value
        FROM settings
        ORDER BY key ASC
        "#,
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| ExportSetting {
        key: row.get("key"),
        value: row.get("value"),
    })
    .collect();

    Ok(ExportConfig {
        version: 1,
        accounts,
        aliases,
        keys,
        settings,
    })
}
```

- [ ] **Step 4: Run test to verify it passes**

Run:

```bash
cargo test --test export_import_contract
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/llmux-core/src/export_import.rs tests/export_import_contract.rs
git commit -m "feat: preserve config import export contract"
```

---

### Task 7: Implement Axum App State, Errors, and Health Route

**Files:**
- Create: `crates/llmux-server/src/error.rs`
- Create: `crates/llmux-server/src/app.rs`
- Create: `crates/llmux-server/src/routes/mod.rs`
- Create: `crates/llmux-server/src/routes/health.rs`
- Create: `tests/server_contract.rs`

- [ ] **Step 1: Write failing health route test**

Create `tests/server_contract.rs`:

```rust
use axum::body::Body;
use axum::http::{Request, StatusCode};
use llmux_core::db::{connect_sqlite, migrate};
use llmux_server::app::{build_app, AppState};
use tower::ServiceExt;

#[tokio::test]
async fn health_route_returns_ok() {
    let pool = connect_sqlite("sqlite::memory:").await.expect("connect");
    migrate(&pool).await.expect("migrate");
    let app = build_app(AppState {
        pool,
        master_key: "master-secret".to_string(),
    });

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/health")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
}
```

- [ ] **Step 2: Add test dependency**

Modify root `Cargo.toml` workspace dependencies to include:

```toml
tower = "0.5"
```

Modify `crates/llmux-server/Cargo.toml` dependencies to include:

```toml
tower = { workspace = true }
```

- [ ] **Step 3: Run test to verify it fails**

Run:

```bash
cargo test --test server_contract
```

Expected: FAIL because server app is not implemented.

- [ ] **Step 4: Implement error response type**

Create `crates/llmux-server/src/error.rs`:

```rust
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self {
            ApiError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::Database(_) | ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };

        let body = Json(ErrorBody {
            error: self.to_string(),
        });

        (status, body).into_response()
    }
}
```

- [ ] **Step 5: Implement routes module**

Create `crates/llmux-server/src/routes/mod.rs`:

```rust
pub mod accounts;
pub mod health;
pub mod keys;
pub mod models;
pub mod settings;
pub mod usage;
```

Create placeholder route files so the module compiles:

`crates/llmux-server/src/routes/accounts.rs`:

```rust
use axum::Json;
use serde_json::json;

pub async fn list_accounts_placeholder() -> Json<serde_json::Value> {
    Json(json!([]))
}
```

`crates/llmux-server/src/routes/keys.rs`:

```rust
use axum::Json;
use serde_json::json;

pub async fn list_keys_placeholder() -> Json<serde_json::Value> {
    Json(json!([]))
}
```

`crates/llmux-server/src/routes/models.rs`:

```rust
use axum::Json;
use serde_json::json;

pub async fn list_models_placeholder() -> Json<serde_json::Value> {
    Json(json!([]))
}
```

`crates/llmux-server/src/routes/settings.rs`:

```rust
use axum::Json;
use serde_json::json;

pub async fn get_settings_placeholder() -> Json<serde_json::Value> {
    Json(json!({}))
}
```

`crates/llmux-server/src/routes/usage.rs`:

```rust
use axum::Json;
use serde_json::json;

pub async fn usage_summary_placeholder() -> Json<serde_json::Value> {
    Json(json!({
        "totalInput": 0,
        "totalOutput": 0,
        "totalCacheRead": 0,
        "totalCacheCreate": 0,
        "avgLatency": 0,
        "totalRequests": 0,
        "successRequests": 0
    }))
}
```

- [ ] **Step 6: Implement health route**

Create `crates/llmux-server/src/routes/health.rs`:

```rust
use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::app::AppState;
use crate::error::ApiError;

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub service: &'static str,
}

pub async fn health(State(state): State<AppState>) -> Result<Json<HealthResponse>, ApiError> {
    sqlx::query("SELECT 1").execute(&state.pool).await?;
    Ok(Json(HealthResponse {
        status: "ok",
        service: "llmux",
    }))
}
```

- [ ] **Step 7: Implement app assembly**

Create `crates/llmux-server/src/app.rs`:

```rust
use axum::routing::get;
use axum::Router;
use sqlx::SqlitePool;
use tower_http::trace::TraceLayer;

use crate::routes;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub master_key: String,
}

pub fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(routes::health::health))
        .route("/api/accounts", get(routes::accounts::list_accounts_placeholder))
        .route("/api/keys", get(routes::keys::list_keys_placeholder))
        .route("/api/models/available", get(routes::models::list_models_placeholder))
        .route("/api/settings", get(routes::settings::get_settings_placeholder))
        .route("/api/usage/summary", get(routes::usage::usage_summary_placeholder))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
```

- [ ] **Step 8: Run test to verify it passes**

Run:

```bash
cargo test --test server_contract
```

Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml crates/llmux-server/Cargo.toml crates/llmux-server/src/app.rs crates/llmux-server/src/error.rs crates/llmux-server/src/routes tests/server_contract.rs
git commit -m "feat: add axum health route"
```

---

### Task 8: Implement Settings Export and Import Routes

**Files:**
- Modify: `crates/llmux-server/src/routes/settings.rs`
- Modify: `crates/llmux-server/src/app.rs`
- Modify: `tests/server_contract.rs`

- [ ] **Step 1: Add failing HTTP import/export test**

Append this test to `tests/server_contract.rs`:

```rust
use axum::body::to_bytes;

#[tokio::test]
async fn http_import_then_export_preserves_old_config_shape() {
    let pool = connect_sqlite("sqlite::memory:").await.expect("connect");
    migrate(&pool).await.expect("migrate");
    let app = build_app(AppState {
        pool,
        master_key: "master-secret".to_string(),
    });

    let body = r#"
    {
      "version": 1,
      "accounts": [
        { "alias": "main", "provider_id": "openai", "api_key": "sk-test", "weight": 1 }
      ],
      "aliases": [
        { "alias": "fast", "target_model": "gpt-4o-mini", "provider_id": "openai" }
      ],
      "keys": [
        { "name": "dev", "key": "llmux-dev", "allowed_models": "*" }
      ],
      "settings": [
        { "key": "port", "value": "25975" }
      ]
    }
    "#;

    let import_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/import")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .expect("request"),
        )
        .await
        .expect("import response");

    assert_eq!(import_response.status(), StatusCode::OK);

    let export_response = app
        .oneshot(
            Request::builder()
                .uri("/api/export")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("export response");

    assert_eq!(export_response.status(), StatusCode::OK);

    let bytes = to_bytes(export_response.into_body(), usize::MAX)
        .await
        .expect("body bytes");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");

    assert_eq!(value["version"], 1);
    assert_eq!(value["accounts"][0]["alias"], "main");
    assert_eq!(value["accounts"][0]["api_key"], "sk-test");
    assert_eq!(value["aliases"][0]["alias"], "fast");
    assert_eq!(value["keys"][0]["key"], "llmux-dev");
    assert_eq!(value["settings"][0]["key"], "port");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cargo test --test server_contract http_import_then_export_preserves_old_config_shape
```

Expected: FAIL because `/api/import` and `/api/export` routes are not implemented.

- [ ] **Step 3: Implement settings import/export routes**

Replace `crates/llmux-server/src/routes/settings.rs` with:

```rust
use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::app::AppState;
use crate::error::ApiError;
use llmux_core::export_import::{export_config, import_config};
use llmux_core::models::ExportConfig;

#[derive(Debug, Serialize)]
pub struct ImportResponse {
    pub success: bool,
    pub imported: ImportedCounts,
}

#[derive(Debug, Serialize)]
pub struct ImportedCounts {
    pub accounts: usize,
    pub aliases: usize,
    pub keys: usize,
}

pub async fn get_settings_placeholder() -> Json<serde_json::Value> {
    Json(serde_json::json!({}))
}

pub async fn export_settings(State(state): State<AppState>) -> Result<Json<ExportConfig>, ApiError> {
    let config = export_config(&state.pool, &state.master_key)
        .await
        .map_err(|err| ApiError::Internal(err.to_string()))?;
    Ok(Json(config))
}

pub async fn import_settings(
    State(state): State<AppState>,
    Json(config): Json<ExportConfig>,
) -> Result<Json<ImportResponse>, ApiError> {
    let counts = ImportedCounts {
        accounts: config.accounts.len(),
        aliases: config.aliases.len(),
        keys: config.keys.len(),
    };

    import_config(&state.pool, &config, &state.master_key)
        .await
        .map_err(|err| ApiError::Internal(err.to_string()))?;

    Ok(Json(ImportResponse {
        success: true,
        imported: counts,
    }))
}
```

- [ ] **Step 4: Wire routes into app**

Modify `crates/llmux-server/src/app.rs` to include these routes:

```rust
use axum::routing::{get, post};
use axum::Router;
use sqlx::SqlitePool;
use tower_http::trace::TraceLayer;

use crate::routes;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub master_key: String,
}

pub fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(routes::health::health))
        .route("/api/accounts", get(routes::accounts::list_accounts_placeholder))
        .route("/api/keys", get(routes::keys::list_keys_placeholder))
        .route("/api/models/available", get(routes::models::list_models_placeholder))
        .route("/api/settings", get(routes::settings::get_settings_placeholder))
        .route("/api/export", get(routes::settings::export_settings))
        .route("/api/import", post(routes::settings::import_settings))
        .route("/api/usage/summary", get(routes::usage::usage_summary_placeholder))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
```

- [ ] **Step 5: Run test to verify it passes**

Run:

```bash
cargo test --test server_contract http_import_then_export_preserves_old_config_shape
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/llmux-server/src/routes/settings.rs crates/llmux-server/src/app.rs tests/server_contract.rs
git commit -m "feat: add config import export routes"
```

---

### Task 9: Implement Account List Route for UI Compatibility

**Files:**
- Modify: `crates/llmux-server/src/routes/accounts.rs`
- Modify: `tests/server_contract.rs`

- [ ] **Step 1: Add failing account list route test**

Append this test to `tests/server_contract.rs`:

```rust
#[tokio::test]
async fn accounts_route_returns_public_accounts_without_api_key() {
    let pool = connect_sqlite("sqlite::memory:").await.expect("connect");
    migrate(&pool).await.expect("migrate");
    let app = build_app(AppState {
        pool,
        master_key: "master-secret".to_string(),
    });

    let body = r#"
    {
      "version": 1,
      "accounts": [
        { "alias": "main", "provider_id": "openai", "api_key": "sk-test", "base_url": "https://example.com/v1" }
      ],
      "aliases": [],
      "keys": [],
      "settings": []
    }
    "#;

    let import_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/import")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .expect("request"),
        )
        .await
        .expect("import response");
    assert_eq!(import_response.status(), StatusCode::OK);

    let accounts_response = app
        .oneshot(
            Request::builder()
                .uri("/api/accounts")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("accounts response");

    assert_eq!(accounts_response.status(), StatusCode::OK);

    let bytes = to_bytes(accounts_response.into_body(), usize::MAX)
        .await
        .expect("body bytes");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");

    assert_eq!(value[0]["alias"], "main");
    assert_eq!(value[0]["provider_id"], "openai");
    assert!(value[0].get("api_key").is_none());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cargo test --test server_contract accounts_route_returns_public_accounts_without_api_key
```

Expected: FAIL because account placeholder returns an empty list.

- [ ] **Step 3: Implement account list route**

Replace `crates/llmux-server/src/routes/accounts.rs` with:

```rust
use axum::extract::State;
use axum::Json;

use crate::app::AppState;
use crate::error::ApiError;
use llmux_core::models::AccountPublic;

pub async fn list_accounts(State(state): State<AppState>) -> Result<Json<Vec<AccountPublic>>, ApiError> {
    let accounts = sqlx::query_as::<_, AccountPublic>(
        r#"
        SELECT id, alias, provider_id, base_url, anthropic_base_url,
               is_active, weight, notes, created_at
        FROM accounts
        ORDER BY created_at DESC
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(accounts))
}
```

- [ ] **Step 4: Wire route into app**

Modify `crates/llmux-server/src/app.rs` so `/api/accounts` uses `list_accounts`:

```rust
.route("/api/accounts", get(routes::accounts::list_accounts))
```

The complete `build_app` should now be:

```rust
pub fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(routes::health::health))
        .route("/api/accounts", get(routes::accounts::list_accounts))
        .route("/api/keys", get(routes::keys::list_keys_placeholder))
        .route("/api/models/available", get(routes::models::list_models_placeholder))
        .route("/api/settings", get(routes::settings::get_settings_placeholder))
        .route("/api/export", get(routes::settings::export_settings))
        .route("/api/import", post(routes::settings::import_settings))
        .route("/api/usage/summary", get(routes::usage::usage_summary_placeholder))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
```

- [ ] **Step 5: Run test to verify it passes**

Run:

```bash
cargo test --test server_contract accounts_route_returns_public_accounts_without_api_key
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/llmux-server/src/routes/accounts.rs crates/llmux-server/src/app.rs tests/server_contract.rs
git commit -m "feat: expose public accounts route"
```

---

### Task 10: Implement Static UI Fallback Serving

**Files:**
- Create: `ui-dist-placeholder/index.html`
- Create: `crates/llmux-server/src/static_ui.rs`
- Modify: `crates/llmux-server/src/app.rs`
- Modify: `tests/server_contract.rs`

- [ ] **Step 1: Write failing static UI route test**

Append this test to `tests/server_contract.rs`:

```rust
#[tokio::test]
async fn root_serves_ui_html() {
    let pool = connect_sqlite("sqlite::memory:").await.expect("connect");
    migrate(&pool).await.expect("migrate");
    let app = build_app(AppState {
        pool,
        master_key: "master-secret".to_string(),
    });

    let response = app
        .oneshot(
            Request::builder()
                .uri("/")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").and_then(|v| v.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cargo test --test server_contract root_serves_ui_html
```

Expected: FAIL because `/` is not routed.

- [ ] **Step 3: Create placeholder UI HTML**

Create `ui-dist-placeholder/index.html`:

```html
<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>LLMux</title>
  </head>
  <body>
    <div id="root">LLMux Rust UI placeholder</div>
  </body>
</html>
```

- [ ] **Step 4: Implement static UI serving**

Create `crates/llmux-server/src/static_ui.rs`:

```rust
use axum::http::header::CONTENT_TYPE;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

const INDEX_HTML: &str = include_str!("../../../ui-dist-placeholder/index.html");

pub async fn serve_index() -> Response {
    (
        StatusCode::OK,
        [(CONTENT_TYPE, "text/html; charset=utf-8")],
        INDEX_HTML,
    )
        .into_response()
}
```

- [ ] **Step 5: Wire fallback route into app**

Modify `crates/llmux-server/src/app.rs`:

```rust
use axum::routing::{get, post};
use axum::Router;
use sqlx::SqlitePool;
use tower_http::trace::TraceLayer;

use crate::routes;
use crate::static_ui;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub master_key: String,
}

pub fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(routes::health::health))
        .route("/api/accounts", get(routes::accounts::list_accounts))
        .route("/api/keys", get(routes::keys::list_keys_placeholder))
        .route("/api/models/available", get(routes::models::list_models_placeholder))
        .route("/api/settings", get(routes::settings::get_settings_placeholder))
        .route("/api/export", get(routes::settings::export_settings))
        .route("/api/import", post(routes::settings::import_settings))
        .route("/api/usage/summary", get(routes::usage::usage_summary_placeholder))
        .route("/", get(static_ui::serve_index))
        .fallback(get(static_ui::serve_index))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
```

- [ ] **Step 6: Run test to verify it passes**

Run:

```bash
cargo test --test server_contract root_serves_ui_html
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add ui-dist-placeholder/index.html crates/llmux-server/src/static_ui.rs crates/llmux-server/src/app.rs tests/server_contract.rs
git commit -m "feat: serve ui fallback from rust server"
```

---

### Task 11: Implement Real CLI Server Startup

**Files:**
- Modify: `crates/llmux-bin/src/main.rs`

- [ ] **Step 1: Run current binary to capture existing skeleton behavior**

Run:

```bash
cargo run -p llmux -- start
```

Expected before implementation: prints `LLMux Rust server skeleton` and exits.

- [ ] **Step 2: Implement server startup**

Replace `crates/llmux-bin/src/main.rs` with:

```rust
use clap::{Parser, Subcommand};
use llmux_core::config::AppConfig;
use llmux_core::crypto::get_or_create_master_key;
use llmux_core::db::{connect_sqlite, migrate, sqlite_url_from_path};
use llmux_server::app::{build_app, AppState};
use std::net::SocketAddr;
use tokio::net::TcpListener;

#[derive(Debug, Parser)]
#[command(name = "llmux")]
#[command(about = "Local AI API gateway and multiplexer")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    Start {
        #[arg(long)]
        port: Option<u16>,
    },
    Status,
    Stop,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "llmux=info,tower_http=info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Start { port: None }) {
        Command::Start { port } => start(port).await?,
        Command::Status => println!("Status functionality is reserved for daemon management."),
        Command::Stop => println!("Stop functionality is reserved for daemon management."),
    }
    Ok(())
}

async fn start(port_override: Option<u16>) -> anyhow::Result<()> {
    let mut config = AppConfig::from_env()?;
    if let Some(port) = port_override {
        config.port = port;
    }

    std::fs::create_dir_all(&config.data_dir)?;
    let database_url = sqlite_url_from_path(&config.database_path);
    let pool = connect_sqlite(&database_url).await?;
    migrate(&pool).await?;

    let master_key = get_or_create_master_key(&config.data_dir, config.master_key.as_deref())?;
    let app = build_app(AppState { pool, master_key });

    let addr = SocketAddr::from(([127, 0, 0, 1], config.port));
    let listener = TcpListener::bind(addr).await?;
    println!("[Gateway] Server running at http://localhost:{}", config.port);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
```

- [ ] **Step 3: Run unit and integration tests**

Run:

```bash
cargo test
```

Expected: PASS.

- [ ] **Step 4: Start server manually**

Run:

```bash
cargo run -p llmux -- start --port 25975
```

Expected: process stays running and prints:

```text
[Gateway] Server running at http://localhost:25975
```

- [ ] **Step 5: Verify health in a second terminal**

Run:

```bash
curl http://localhost:25975/api/health
```

Expected response includes:

```json
{"status":"ok","service":"llmux"}
```

- [ ] **Step 6: Stop server**

Press `Ctrl+C` in the terminal running the server.

Expected: process exits cleanly.

- [ ] **Step 7: Commit**

```bash
git add crates/llmux-bin/src/main.rs
git commit -m "feat: start rust llmux server"
```

---

### Task 12: Add Foundation Verification Checklist

**Files:**
- Create: `docs/foundation-verification.md`

- [ ] **Step 1: Write verification document**

Create `docs/foundation-verification.md`:

```markdown
# LLMux Rust Foundation Verification

Run these checks before starting the provider-adapter migration plan.

## Automated

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test
```

Expected: all commands pass.

## Manual

Start the server:

```bash
cargo run -p llmux -- start --port 25975
```

Check health:

```bash
curl http://localhost:25975/api/health
```

Expected:

```json
{"status":"ok","service":"llmux"}
```

Import old-compatible config:

```bash
curl -X POST http://localhost:25975/api/import \
  -H "content-type: application/json" \
  -d '{"version":1,"accounts":[{"alias":"main","provider_id":"openai","api_key":"sk-test"}],"aliases":[],"keys":[],"settings":[]}'
```

Expected response includes:

```json
{"success":true,"imported":{"accounts":1,"aliases":0,"keys":0}}
```

Export config:

```bash
curl http://localhost:25975/api/export
```

Expected response includes a plaintext account key in the old export shape:

```json
{"version":1,"accounts":[{"alias":"main","provider_id":"openai","api_key":"sk-test"}]}
```

Open UI placeholder:

```text
http://localhost:25975/
```

Expected: HTML loads.
```

- [ ] **Step 2: Run formatter**

Run:

```bash
cargo fmt --check
```

Expected: PASS. If it fails, run `cargo fmt`, then rerun `cargo fmt --check`.

- [ ] **Step 3: Run clippy**

Run:

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: PASS.

- [ ] **Step 4: Run tests**

Run:

```bash
cargo test
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add docs/foundation-verification.md
git commit -m "docs: add rust foundation verification"
```

---

## Final Acceptance Criteria

After this plan is complete:

- `cargo test` passes.
- `cargo run -p llmux -- start --port 25975` starts a local server.
- `GET /api/health` returns `{ "status": "ok", "service": "llmux" }`.
- `POST /api/import` accepts old-compatible config JSON.
- `GET /api/export` returns old-compatible config JSON with plaintext provider API keys, matching the existing Bun export behavior.
- `GET /api/accounts` returns account metadata without `api_key`.
- `GET /` serves HTML.
- The old `llmux-cli` directory remains unchanged except for any separate docs the user explicitly requested.

## Self-Review Notes

- Spec coverage: This plan covers the new sibling directory, Rust workspace, fresh database schema, import/export compatibility, basic API routes, static UI fallback, and CLI startup. It intentionally excludes provider adapters and SSE conversion because those are independent subsystems requiring separate plans.
- Placeholder scan: Placeholder routes remain only for unimplemented future API groups and are explicitly named as placeholders. No acceptance criterion depends on those placeholder routes except that they compile.
- Type consistency: `ExportConfig`, `ExportAccount`, `ExportModelAlias`, `ExportApiKey`, and `ExportSetting` are defined before use and reused consistently by import/export tests and routes.
