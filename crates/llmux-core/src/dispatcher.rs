use crate::adapters::Account;
use crate::crypto::decrypt_api_key;
use crate::models::ModelAlias;
use serde_json::Value;
use sqlx::{Row, SqlitePool};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelResolution {
    pub provider_id: String,
    pub target_model: String,
    /// If set, load only these account IDs (cross-provider). Supersedes provider_id grouping.
    pub account_ids: Vec<i64>,
}

#[derive(Debug, Default, Clone)]
pub struct DispatcherState {
    indices: HashMap<String, usize>,
}

impl DispatcherState {
    pub fn next_start_index(&mut self, provider_id: &str, account_len: usize) -> usize {
        if account_len == 0 {
            return 0;
        }
        let idx = self.indices.get(provider_id).copied().unwrap_or_default() % account_len;
        self.indices
            .insert(provider_id.to_string(), (idx + 1) % account_len);
        idx
    }
}

/// Strip ANSI escape sequences, control characters, and Claude Code
/// `[1m]` long-context suffix from model names.
pub fn sanitize_model_name(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    let mut chars = name.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Skip the entire ANSI escape sequence
            if chars.peek() == Some(&'[') {
                chars.next(); // skip '['
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_alphabetic() {
                        chars.next(); // skip the terminator
                        break;
                    }
                    chars.next();
                }
            }
        } else if !ch.is_control() || ch == '\n' || ch == '\r' || ch == '\t' {
            result.push(ch);
        }
    }
    // Strip Claude Code [1m] long-context suffix (e.g. "d4p[1m]" → "d4p")
    if let Some(stripped) = result.strip_suffix("[1m]") {
        result = stripped.to_string();
    }
    result.trim().to_string()
}

pub fn resolve_model_by_prefix(model_name: &str) -> ModelResolution {
    let provider_id = if model_name.starts_with("claude-") {
        "anthropic"
    } else if model_name.starts_with("gemini-") || model_name.starts_with("models/gemini-") {
        "gemini"
    } else {
        "openai"
    };
    ModelResolution {
        provider_id: provider_id.to_string(),
        target_model: model_name.to_string(),
        account_ids: Vec::new(),
    }
}

pub async fn resolve_model(pool: &SqlitePool, model_name: &str) -> anyhow::Result<ModelResolution> {
    let model_name = sanitize_model_name(model_name);
    let alias = sqlx::query_as::<_, ModelAlias>(
        "SELECT id, alias, target_model, provider_id, account_ids FROM model_aliases WHERE alias = ?",
    )
    .bind(&model_name)
    .fetch_optional(pool)
    .await?;

    if let Some(alias) = alias {
        // Parse account_ids JSON array e.g. "[1,5,7]"
        let account_ids: Vec<i64> = alias
            .account_ids
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();

        if !account_ids.is_empty() {
            return Ok(ModelResolution {
                provider_id: alias.provider_id.unwrap_or_default(),
                target_model: alias.target_model,
                account_ids,
            });
        }

        if let Some(provider_id) = alias.provider_id {
            return Ok(ModelResolution {
                provider_id,
                target_model: alias.target_model,
                account_ids: Vec::new(),
            });
        }
    }
    Ok(resolve_model_by_prefix(&model_name))
}

/// Normalize a provider type string to one of the canonical categories:
/// "openai", "anthropic", "gemini", or "custom".
pub fn resolve_provider_type(provider_type: Option<&str>, provider_id: &str) -> String {
    let raw = provider_type.unwrap_or(provider_id);
    match raw {
        "openai" => "openai".to_string(),
        "anthropic" | "custom-anthropic" | "claude-native" => "anthropic".to_string(),
        "gemini" => "gemini".to_string(),
        "custom" | "poe" | "claude" | "qwen" | "deepseek" | "kimi" | "moonshot" | "step" => {
            "custom".to_string()
        }
        other => other.to_string(),
    }
}

pub fn is_retryable_status(status: u16) -> bool {
    matches!(status, 401 | 403 | 429)
}

pub fn order_accounts_for_attempts(accounts: &[Account], start_index: usize) -> Vec<Account> {
    if accounts.is_empty() {
        return Vec::new();
    }
    (0..accounts.len())
        .map(|offset| accounts[(start_index + offset) % accounts.len()].clone())
        .collect()
}

pub fn select_accounts_for_dispatch(
    accounts: &[Account],
    provider_id: &str,
    state: &mut DispatcherState,
) -> Vec<Account> {
    let start = state.next_start_index(provider_id, accounts.len());
    order_accounts_for_attempts(accounts, start)
}

/// Load accounts by explicit IDs (for cross-provider alias binding).
pub async fn get_accounts_by_ids(
    pool: &SqlitePool,
    ids: &[i64],
    encryption_secret: &str,
) -> anyhow::Result<Vec<Account>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    // Build query with dynamic placeholders
    let placeholders: Vec<String> = ids.iter().map(|_| "?".to_string()).collect();
    let sql = format!(
        "SELECT id, alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight \
         FROM accounts WHERE id IN ({}) AND is_active = 1 ORDER BY weight DESC, id ASC",
        placeholders.join(",")
    );
    let mut query = sqlx::query(&sql);
    for id in ids {
        query = query.bind(id);
    }
    let rows = query.fetch_all(pool).await?;

    let mut accounts = Vec::new();
    for row in rows {
        let encrypted_key: String = row.try_get("api_key")?;
        if let Ok(api_key) = decrypt_api_key(&encrypted_key, encryption_secret) {
            accounts.push(Account {
                id: row.try_get("id")?,
                alias: row.try_get("alias")?,
                provider_id: row.try_get("provider_id")?,
                api_key,
                base_url: row.try_get("base_url").ok(),
                anthropic_base_url: row.try_get("anthropic_base_url").ok(),
                is_active: row.try_get::<i64, _>("is_active").unwrap_or(1),
                weight: row.try_get("weight").unwrap_or(1),
            });
        } else {
            let alias: String = row.try_get("alias").unwrap_or_default();
            let id: i64 = row.try_get("id").unwrap_or_default();
            tracing::warn!(
                "failed to decrypt API key for account {} (id={}), check MASTER_KEY",
                alias,
                id
            );
        }
    }
    Ok(accounts)
}

pub async fn get_active_accounts(
    pool: &SqlitePool,
    provider_or_alias: Option<&str>,
    encryption_secret: &str,
) -> anyhow::Result<Vec<Account>> {
    let rows = if let Some(provider) = provider_or_alias {
        let provider_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(1) FROM accounts WHERE provider_id = ? AND is_active = 1",
        )
        .bind(provider)
        .fetch_one(pool)
        .await?;
        if provider_count > 0 {
            sqlx::query("SELECT id, alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight FROM accounts WHERE provider_id = ? AND is_active = 1 ORDER BY weight DESC, id ASC")
                .bind(provider)
                .fetch_all(pool)
                .await?
        } else {
            sqlx::query("SELECT id, alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight FROM accounts WHERE alias = ? AND is_active = 1 ORDER BY weight DESC, id ASC")
                .bind(provider)
                .fetch_all(pool)
                .await?
        }
    } else {
        sqlx::query("SELECT id, alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight FROM accounts WHERE is_active = 1 ORDER BY weight DESC, id ASC")
            .fetch_all(pool)
            .await?
    };

    let mut accounts = Vec::new();
    for row in rows {
        let encrypted_key: String = row.try_get("api_key")?;
        if let Ok(api_key) = decrypt_api_key(&encrypted_key, encryption_secret) {
            accounts.push(Account {
                id: row.try_get("id")?,
                alias: row.try_get("alias")?,
                provider_id: row.try_get("provider_id")?,
                api_key,
                base_url: row.try_get("base_url").ok(),
                anthropic_base_url: row.try_get("anthropic_base_url").ok(),
                is_active: row.try_get::<i64, _>("is_active").unwrap_or(1),
                weight: row.try_get("weight").unwrap_or(1),
            });
        } else {
            let alias: String = row.try_get("alias").unwrap_or_default();
            let id: i64 = row.try_get("id").unwrap_or_default();
            tracing::warn!(
                "failed to decrypt API key for account {} (id={}), check MASTER_KEY",
                alias,
                id
            );
        }
    }
    Ok(accounts)
}

pub fn estimate_stream_usage_from_chunks(messages: &[Value], chunk_count: i64) -> (i64, i64) {
    let input_chars = serde_json::to_string(messages)
        .map(|text| text.len())
        .unwrap_or_default();
    let prompt_tokens = ((input_chars as f64) / 4.0).ceil() as i64;
    let completion_tokens = if chunk_count > 0 {
        ((chunk_count as f64) * 1.2).floor().max(1.0) as i64
    } else {
        0
    };
    (prompt_tokens, completion_tokens)
}
