use crate::crypto::{decrypt_api_key, encrypt_api_key};
use crate::models::SettingRow;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExportAccount {
    pub alias: String,
    pub provider_id: String,
    pub api_key: String,
    pub base_url: Option<String>,
    pub anthropic_base_url: Option<String>,
    pub is_active: i64,
    pub weight: i64,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExportAlias {
    pub alias: String,
    pub target_model: String,
    pub provider_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_ids: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExportApiKey {
    pub name: String,
    pub key: String,
    pub allowed_models: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConfigExport {
    pub version: i64,
    pub accounts: Vec<ExportAccount>,
    pub aliases: Vec<ExportAlias>,
    pub keys: Vec<ExportApiKey>,
    pub settings: Vec<SettingRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImportCounts {
    pub accounts: usize,
    pub aliases: usize,
    pub keys: usize,
}

pub async fn export_config(pool: &SqlitePool, encryption_secret: &str) -> Result<ConfigExport> {
    let account_rows = sqlx::query(
        "SELECT alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight, notes
         FROM accounts
         ORDER BY id",
    )
    .fetch_all(pool)
    .await?;

    let mut accounts = Vec::with_capacity(account_rows.len());
    for row in account_rows {
        let encrypted_key: String = row.try_get("api_key")?;
        accounts.push(ExportAccount {
            alias: row.try_get("alias")?,
            provider_id: row.try_get("provider_id")?,
            api_key: decrypt_api_key(&encrypted_key, encryption_secret)?,
            base_url: row.try_get("base_url")?,
            anthropic_base_url: row.try_get("anthropic_base_url")?,
            is_active: row.try_get("is_active")?,
            weight: row.try_get("weight")?,
            notes: row.try_get("notes")?,
        });
    }

    let aliases = sqlx::query_as::<_, (String, String, Option<String>, Option<String>)>(
        "SELECT alias, target_model, provider_id, account_ids FROM model_aliases ORDER BY id",
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(alias, target_model, provider_id, account_ids)| ExportAlias {
        alias,
        target_model,
        provider_id,
        account_ids,
    })
    .collect();

    let keys = sqlx::query_as::<_, (String, String, String)>(
        "SELECT name, key, allowed_models FROM api_keys ORDER BY id",
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(name, key, allowed_models)| ExportApiKey {
        name,
        key,
        allowed_models,
    })
    .collect();

    let settings =
        sqlx::query_as::<_, (String, String)>("SELECT key, value FROM settings ORDER BY key")
            .fetch_all(pool)
            .await?
            .into_iter()
            .map(|(key, value)| SettingRow { key, value })
            .collect();

    Ok(ConfigExport {
        version: 1,
        accounts,
        aliases,
        keys,
        settings,
    })
}

pub async fn import_config(
    pool: &SqlitePool,
    config: ConfigExport,
    encryption_secret: &str,
) -> Result<ImportCounts> {
    let mut tx = pool.begin().await?;

    for account in &config.accounts {
        let encrypted_key = encrypt_api_key(&account.api_key, encryption_secret)?;
        sqlx::query(
            "INSERT OR REPLACE INTO accounts
             (alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight, notes)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&account.alias)
        .bind(&account.provider_id)
        .bind(encrypted_key)
        .bind(&account.base_url)
        .bind(&account.anthropic_base_url)
        .bind(account.is_active)
        .bind(account.weight)
        .bind(&account.notes)
        .execute(&mut *tx)
        .await?;
    }

    for alias in &config.aliases {
        sqlx::query(
            "INSERT OR REPLACE INTO model_aliases (alias, target_model, provider_id, account_ids)
             VALUES (?, ?, ?, ?)",
        )
        .bind(&alias.alias)
        .bind(&alias.target_model)
        .bind(&alias.provider_id)
        .bind(&alias.account_ids)
        .execute(&mut *tx)
        .await?;
    }

    for key in &config.keys {
        sqlx::query(
            "INSERT OR REPLACE INTO api_keys (name, key, allowed_models)
             VALUES (?, ?, ?)",
        )
        .bind(&key.name)
        .bind(&key.key)
        .bind(if key.allowed_models.is_empty() {
            "*"
        } else {
            &key.allowed_models
        })
        .execute(&mut *tx)
        .await?;
    }

    for setting in &config.settings {
        sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)")
            .bind(&setting.key)
            .bind(&setting.value)
            .execute(&mut *tx)
            .await?;
    }

    tx.commit().await?;

    Ok(ImportCounts {
        accounts: config.accounts.len(),
        aliases: config.aliases.len(),
        keys: config.keys.len(),
    })
}
