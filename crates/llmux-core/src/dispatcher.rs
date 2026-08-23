use crate::adapters::Account;
use crate::crypto::decrypt_api_key;
use crate::models::ModelAlias;
use serde_json::Value;
use sqlx::{Row, SqlitePool};
use std::collections::HashMap;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelResolution {
    pub provider_id: String,
    pub target_model: String,
    /// If set, load only these account IDs (cross-provider). Supersedes provider_id grouping.
    pub account_ids: Vec<i64>,
    pub preferred_account_id: Option<i64>,
    pub alias_name: Option<String>,
    pub upstream_api: crate::upstream_api::UpstreamApi,
}

// ---------------------------------------------------------------------------
// Sticky-session dispatch router
// ---------------------------------------------------------------------------

const PROBE_TRIGGER_COUNT: u32 = 5;
const INITIAL_PROBE_BACKOFF_SECS: u64 = 30;
const MAX_PROBE_BACKOFF_SECS: u64 = 600;

/// The sticky routing mode for a single dispatch key.
#[derive(Debug, Clone)]
enum StickyMode {
    /// Using the preferred account. All requests go to it.
    Primary,
    /// Using a fallback account. Periodically probes preferred.
    Fallback {
        /// The account ID we are currently stuck to as fallback (for cache warmth).
        sticky_fallback_id: i64,
        /// Consecutive successful requests on fallback accounts.
        consecutive_successes: u32,
        /// When we last attempted to probe the preferred account.
        last_probe: Instant,
        /// Current probe backoff in seconds (doubles on each failed probe).
        probe_backoff_secs: u64,
    },
}

#[derive(Debug, Clone)]
struct StickyEntry {
    mode: StickyMode,
}

impl Default for StickyEntry {
    fn default() -> Self {
        Self {
            mode: StickyMode::Primary,
        }
    }
}

/// Metadata about a dispatch decision, returned by `DispatchRouter::select`.
#[derive(Debug, Clone)]
pub struct DispatchMeta {
    pub is_probe: bool,
    pub preferred_id: i64,
}

const MAX_ENTRIES: usize = 1024;

/// Sticky-session routing state machine with failover and exponential backoff.
#[derive(Debug, Clone, Default)]
pub struct DispatchRouter {
    entries: HashMap<String, StickyEntry>,
}

impl DispatchRouter {
    /// Determine the ordered list of accounts to try for this request.
    pub fn select(
        &mut self,
        dispatch_key: &str,
        accounts: &[Account],
        preferred_id: i64,
    ) -> (Vec<Account>, DispatchMeta) {
        let entry = self.entries.entry(dispatch_key.to_string()).or_default();

        let preferred_exists = accounts.iter().any(|a| a.id == preferred_id);
        if !preferred_exists {
            entry.mode = StickyMode::Primary;
        }

        match &entry.mode {
            StickyMode::Primary => {
                let ordered = order_with_preferred_first(accounts, preferred_id);
                (
                    ordered,
                    DispatchMeta {
                        is_probe: false,
                        preferred_id,
                    },
                )
            }
            StickyMode::Fallback {
                sticky_fallback_id,
                consecutive_successes,
                last_probe,
                probe_backoff_secs,
            } => {
                let should_probe = *consecutive_successes >= PROBE_TRIGGER_COUNT
                    || last_probe.elapsed().as_secs() >= *probe_backoff_secs;

                if should_probe {
                    let ordered = order_with_preferred_first(accounts, preferred_id);
                    (
                        ordered,
                        DispatchMeta {
                            is_probe: true,
                            preferred_id,
                        },
                    )
                } else {
                    let ordered =
                        order_with_fallback_first(accounts, preferred_id, *sticky_fallback_id);
                    (
                        ordered,
                        DispatchMeta {
                            is_probe: false,
                            preferred_id,
                        },
                    )
                }
            }
        }
    }

    /// Update state after a dispatch attempt completes.
    /// `used_account_id` is `Some(id)` when a specific account handled the
    /// request, or `None` when no account could be reached.
    pub fn record_result(
        &mut self,
        dispatch_key: &str,
        meta: &DispatchMeta,
        used_account_id: Option<i64>,
        success: bool,
    ) {
        self.maybe_evict();
        let entry = self.entries.entry(dispatch_key.to_string()).or_default();

        match &mut entry.mode {
            StickyMode::Primary => {
                if !success || used_account_id != Some(meta.preferred_id) {
                    let sticky = used_account_id.filter(|&id| id != meta.preferred_id);
                    entry.mode = StickyMode::Fallback {
                        sticky_fallback_id: sticky.unwrap_or(0),
                        consecutive_successes: if sticky.is_some() { 1 } else { 0 },
                        last_probe: Instant::now(),
                        probe_backoff_secs: INITIAL_PROBE_BACKOFF_SECS,
                    };
                }
            }
            StickyMode::Fallback {
                sticky_fallback_id,
                consecutive_successes,
                last_probe,
                probe_backoff_secs,
            } => {
                if meta.is_probe {
                    if success && used_account_id == Some(meta.preferred_id) {
                        entry.mode = StickyMode::Primary;
                    } else {
                        *probe_backoff_secs =
                            (*probe_backoff_secs * 2).min(MAX_PROBE_BACKOFF_SECS);
                        *consecutive_successes = 0;
                        *last_probe = Instant::now();
                        if let Some(id) = used_account_id {
                            if id != meta.preferred_id {
                                *sticky_fallback_id = id;
                            }
                        }
                    }
                } else {
                    if success {
                        *consecutive_successes += 1;
                        if let Some(id) = used_account_id {
                            *sticky_fallback_id = id;
                        }
                    }
                }
            }
        }
    }

    /// Drop Primary entries when the map grows beyond MAX_ENTRIES.
    /// Fallback entries are kept — they represent active failover state.
    fn maybe_evict(&mut self) {
        if self.entries.len() <= MAX_ENTRIES {
            return;
        }
        self.entries.retain(|_, e| {
            matches!(e.mode, StickyMode::Fallback { .. })
        });
        tracing::info!(
            "DispatchRouter evicted stale entries, {} fallback entries retained",
            self.entries.len()
        );
    }
}

fn order_with_preferred_first(accounts: &[Account], preferred_id: i64) -> Vec<Account> {
    let mut result: Vec<Account> = Vec::with_capacity(accounts.len());
    let mut preferred: Option<Account> = None;
    for a in accounts {
        if a.id == preferred_id {
            preferred = Some(a.clone());
        } else {
            result.push(a.clone());
        }
    }
    if let Some(p) = preferred {
        result.insert(0, p);
    }
    result
}

fn order_with_fallback_first(
    accounts: &[Account],
    preferred_id: i64,
    sticky_fallback_id: i64,
) -> Vec<Account> {
    let mut result: Vec<Account> = Vec::with_capacity(accounts.len());
    let mut sticky: Option<Account> = None;
    let mut preferred: Option<Account> = None;
    for a in accounts {
        if a.id == sticky_fallback_id {
            sticky = Some(a.clone());
        } else if a.id == preferred_id {
            preferred = Some(a.clone());
        } else {
            result.push(a.clone());
        }
    }
    if let Some(s) = sticky {
        result.insert(0, s);
    }
    if let Some(p) = preferred {
        result.push(p);
    }
    result
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
        preferred_account_id: None,
        alias_name: None,
        upstream_api: Default::default(),
    }
}

pub async fn resolve_model(pool: &SqlitePool, model_name: &str) -> anyhow::Result<ModelResolution> {
    let model_name = sanitize_model_name(model_name);
    let alias = sqlx::query_as::<_, ModelAlias>(
        "SELECT id, alias, target_model, provider_id, account_ids, preferred_account_id, upstream_api FROM model_aliases WHERE alias = ?",
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

        let alias_name = Some(alias.alias.clone());
        let upstream_api = crate::upstream_api::UpstreamApi::from_str(alias.upstream_api.as_deref().unwrap_or("chat"));

        if !account_ids.is_empty() {
            return Ok(ModelResolution {
                provider_id: alias.provider_id.unwrap_or_default(),
                target_model: alias.target_model,
                account_ids,
                preferred_account_id: alias.preferred_account_id,
                alias_name,
                upstream_api,
            });
        }

        if let Some(provider_id) = alias.provider_id {
            return Ok(ModelResolution {
                provider_id,
                target_model: alias.target_model,
                account_ids: Vec::new(),
                preferred_account_id: alias.preferred_account_id,
                alias_name,
                upstream_api,
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
        "SELECT id, alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight, openai_compatible \
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
                openai_compatible: row.try_get("openai_compatible").unwrap_or(0),
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
        // ponytail: single query path — try provider_id first, alias fallback only if empty (saves one COUNT(*) RTT)
        let rows = sqlx::query("SELECT id, alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight, openai_compatible FROM accounts WHERE provider_id = ? AND is_active = 1 ORDER BY weight DESC, id ASC")
                .bind(provider)
                .fetch_all(pool)
                .await?;
        if rows.is_empty() {
            sqlx::query("SELECT id, alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight, openai_compatible FROM accounts WHERE alias = ? AND is_active = 1 ORDER BY weight DESC, id ASC")
                .bind(provider)
                .fetch_all(pool)
                .await?
        } else {
            rows
        }
    } else {
        sqlx::query("SELECT id, alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight, openai_compatible FROM accounts WHERE is_active = 1 ORDER BY weight DESC, id ASC")
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
                openai_compatible: row.try_get("openai_compatible").unwrap_or(0),
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
