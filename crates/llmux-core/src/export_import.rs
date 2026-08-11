use crate::crypto::{decrypt_api_key, encrypt_api_key};
use crate::models::SettingRow;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use std::collections::HashMap;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct ExportAccount {
    /// 源库账户 id，import 时用于把 alias 绑定重映射到目标库新 id。
    #[serde(default)]
    pub id: i64,
    /// 旧版 version 1 字段名 provider_id（反序列化兼容）
    #[serde(default, alias = "provider_id")]
    pub vendor_id: String,
    /// 旧版 version 1 字段名 alias
    #[serde(default, alias = "alias")]
    pub name: String,
    pub api_key: String,
    pub base_url: Option<String>,
    pub anthropic_base_url: Option<String>,
    #[serde(default)]
    pub openai_compatible: i64,
    /// 旧版 version 1 字段名 is_active
    #[serde(default, alias = "is_active")]
    pub enabled: i64,
    #[serde(default)]
    pub weight: i64,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct ExportAlias {
    pub alias: String,
    pub target_model: String,
    pub vendor_id: Option<String>,
    /// 绑定账户 id 列表（写时快照，import 时重建绑定）
    #[serde(default)]
    pub account_ids: Vec<i64>,
    #[serde(default)]
    pub preferred_account_id: Option<i64>,
}

/// 网关 key 明文存储，可直接导出/导入。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct ExportApiKey {
    pub name: String,
    pub key: String,
    #[serde(default)]
    pub allowed_models: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct ConfigExport {
    #[serde(default)]
    pub version: i64,
    #[serde(default)]
    pub accounts: Vec<ExportAccount>,
    #[serde(default)]
    pub aliases: Vec<ExportAlias>,
    #[serde(default)]
    pub keys: Vec<ExportApiKey>,
    #[serde(default)]
    pub settings: Vec<SettingRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct ImportCounts {
    pub accounts: usize,
    pub aliases: usize,
    pub keys: usize,
}

pub async fn export_config(pool: &SqlitePool, encryption_secret: &str) -> Result<ConfigExport> {
    let account_rows = sqlx::query(
        "SELECT id, vendor_id, name, api_key_enc, base_url, anthropic_base_url, openai_compatible, enabled, weight, notes
         FROM accounts
         ORDER BY id",
    )
    .fetch_all(pool)
    .await?;

    let mut accounts = Vec::with_capacity(account_rows.len());
    for row in account_rows {
        let encrypted_key: String = row.try_get("api_key_enc")?;
        accounts.push(ExportAccount {
            id: row.try_get("id")?,
            vendor_id: row.try_get("vendor_id")?,
            name: row.try_get("name")?,
            api_key: decrypt_api_key(&encrypted_key, encryption_secret)?,
            base_url: row.try_get("base_url")?,
            anthropic_base_url: row.try_get("anthropic_base_url")?,
            openai_compatible: row.try_get("openai_compatible")?,
            enabled: row.try_get("enabled")?,
            weight: row.try_get("weight")?,
            notes: row.try_get("notes")?,
        });
    }

    let alias_rows = sqlx::query_as::<_, (i64, String, String, Option<String>)>(
        "SELECT id, alias, target_model, vendor_id FROM model_aliases ORDER BY id",
    )
    .fetch_all(pool)
    .await?;

    let mut aliases_out = Vec::with_capacity(alias_rows.len());
    for (id, alias, target_model, vendor_id) in alias_rows {
        let bindings = sqlx::query_as::<_, (i64, i64)>(
            "SELECT account_id, is_preferred FROM model_alias_accounts WHERE alias_id = ? ORDER BY position",
        )
        .bind(id)
        .fetch_all(pool)
        .await?;
        aliases_out.push(ExportAlias {
            alias,
            target_model,
            vendor_id,
            account_ids: bindings.iter().map(|(aid, _)| *aid).collect(),
            preferred_account_id: bindings
                .iter()
                .find(|(_, pref)| *pref == 1)
                .map(|(aid, _)| *aid),
        });
    }

    let key_rows = sqlx::query_as::<_, (i64, String, String)>(
        "SELECT id, name, key FROM api_keys ORDER BY id",
    )
    .fetch_all(pool)
    .await?;
    let mut keys = Vec::with_capacity(key_rows.len());
    for (id, name, key) in key_rows {
        let models: Vec<String> =
            sqlx::query_scalar("SELECT model FROM api_key_models WHERE api_key_id = ?")
                .bind(id)
                .fetch_all(pool)
                .await?;
        keys.push(ExportApiKey {
            name,
            key,
            allowed_models: models,
        });
    }

    // gateway_key 是目标库本机生成的凭据，不随配置导出（否则 import 可覆盖它锁死客户端）
    let settings =
        sqlx::query_as::<_, (String, String)>("SELECT key, value FROM app_settings WHERE key != 'gateway_key' ORDER BY key")
            .fetch_all(pool)
            .await?
            .into_iter()
            .map(|(key, value)| SettingRow { key, value })
            .collect();

    Ok(ConfigExport {
        version: 2,
        accounts,
        aliases: aliases_out,
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

    // 补齐缺失 vendor：accounts.vendor_id 是外键，跨机导入自定义厂商配置时
    // 目标库可能不存在该厂商，先补占位 vendor 避免 FOREIGN KEY 违例
    let mut vendor_ids: Vec<String> = Vec::new();
    for account in &config.accounts {
        vendor_ids.push(account.vendor_id.clone());
    }
    for alias in &config.aliases {
        if let Some(v) = &alias.vendor_id {
            vendor_ids.push(v.clone());
        }
    }
    for vid in vendor_ids {
        let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM vendors WHERE id = ?")
            .bind(&vid)
            .fetch_optional(&mut *tx)
            .await?;
        if exists.is_none() {
            sqlx::query(
                "INSERT OR IGNORE INTO vendors (id, name, protocol, protocols, openai_responses, builtin)
                 VALUES (?, ?, 'openai', '[\"openai\"]', 0, 0)",
            )
            .bind(&vid)
            .bind(&vid)
            .execute(&mut *tx)
            .await?;
        }
    }

    // 源库账户 id → 目标库新 id 映射（alias 绑定据此重映射）
    let mut account_id_map: HashMap<i64, i64> = HashMap::new();
    for account in &config.accounts {
        let encrypted_key = encrypt_api_key(&account.api_key, encryption_secret)?;
        // 用 ON CONFLICT DO UPDATE 而非 INSERT OR REPLACE：REPLACE 会先删旧行再插入，
        // 触发 ON DELETE CASCADE 清空该账户的 alias 绑定；DO UPDATE 保留原行，
        // rowid 稳定、绑定不丢，仅更新字段
        sqlx::query(
            "INSERT INTO accounts
             (vendor_id, name, api_key_enc, base_url, anthropic_base_url, openai_compatible, enabled, weight, notes)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(name) DO UPDATE SET
                vendor_id = excluded.vendor_id,
                api_key_enc = excluded.api_key_enc,
                base_url = excluded.base_url,
                anthropic_base_url = excluded.anthropic_base_url,
                openai_compatible = excluded.openai_compatible,
                enabled = excluded.enabled,
                weight = excluded.weight,
                notes = excluded.notes",
        )
        .bind(&account.vendor_id)
        .bind(&account.name)
        .bind(encrypted_key)
        .bind(&account.base_url)
        .bind(&account.anthropic_base_url)
        .bind(account.openai_compatible)
        .bind(account.enabled)
        .bind(account.weight)
        .bind(&account.notes)
        .execute(&mut *tx)
        .await?;
        // 冲突更新时 last_insert_rowid 不可靠，直接按唯一键 name 取 id
        let new_account_id: i64 = sqlx::query_scalar("SELECT id FROM accounts WHERE name = ?")
            .bind(&account.name)
            .fetch_one(&mut *tx)
            .await?;
        account_id_map.insert(account.id, new_account_id);
    }

    for alias in &config.aliases {
        let result = sqlx::query(
            "INSERT OR REPLACE INTO model_aliases (alias, target_model, vendor_id)
             VALUES (?, ?, ?)",
        )
        .bind(&alias.alias)
        .bind(&alias.target_model)
        .bind(&alias.vendor_id)
        .execute(&mut *tx)
        .await?;
        let alias_id = result.last_insert_rowid();

        // 重建绑定：先清空再写入
        sqlx::query("DELETE FROM model_alias_accounts WHERE alias_id = ?")
            .bind(alias_id)
            .execute(&mut *tx)
            .await?;
        for (position, account_id) in alias.account_ids.iter().enumerate() {
            // 绑定的是源库账户 id，必须映射到导入后新分配的 id；找不到则跳过该绑定。
            let Some(&new_account_id) = account_id_map.get(account_id) else {
                tracing::warn!("import: alias '{}' 的账户 id={} 未导入，跳过该绑定", alias.alias, account_id);
                continue;
            };
            let is_preferred = if Some(*account_id) == alias.preferred_account_id {
                1
            } else {
                0
            };
            sqlx::query(
                "INSERT OR REPLACE INTO model_alias_accounts (alias_id, account_id, position, is_preferred)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(alias_id)
            .bind(new_account_id)
            .bind(position as i64)
            .bind(is_preferred)
            .execute(&mut *tx)
            .await?;
        }
    }

    for key in &config.keys {
        // 同样用 ON CONFLICT DO UPDATE：REPLACE 会删旧行级联清 api_key_models 白名单
        sqlx::query(
            "INSERT INTO api_keys (name, key) VALUES (?, ?)
             ON CONFLICT(key) DO UPDATE SET name = excluded.name",
        )
        .bind(&key.name)
        .bind(&key.key)
        .execute(&mut *tx)
        .await?;
        let key_id: i64 = sqlx::query_scalar("SELECT id FROM api_keys WHERE key = ?")
            .bind(&key.key)
            .fetch_one(&mut *tx)
            .await?;
        // 白名单是合并语义（INSERT OR IGNORE）：重复导入时旧条目保留、新条目追加，
        // 不做整体替换；如需整体替换需先 DELETE 该 key 的白名单再写
        for model in &key.allowed_models {
            sqlx::query("INSERT OR IGNORE INTO api_key_models (api_key_id, model) VALUES (?, ?)")
                .bind(key_id)
                .bind(model)
                .execute(&mut *tx)
                .await?;
        }
    }

    for setting in &config.settings {
        sqlx::query("INSERT OR REPLACE INTO app_settings (key, value) VALUES (?, ?)")
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
