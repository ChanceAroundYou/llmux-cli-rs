use crate::adapters::Account;
use crate::crypto::decrypt_api_key;
use crate::models::ModelAlias;
use sqlx::{Row, SqlitePool};
use std::collections::HashMap;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelResolution {
    /// 请求路由的厂商 id（无显式绑定时按此厂商取账户）。
    pub vendor_id: String,
    pub target_model: String,
    /// 显式绑定账户集（跨厂商），非空时优先于 vendor_id 分组。
    pub account_ids: Vec<i64>,
    /// 绑定集内首选账户（`is_preferred` 标记）。
    pub preferred_account_id: Option<i64>,
    pub alias_name: Option<String>,
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
///
/// 注意：spec 要求 `dispatch_state` 表持久化回退状态；当前实现保持内存态
/// （Instant 计时不可序列化），表已建好供后续持久化改造使用。
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

/// 前缀回退：`claude-` → anthropic，`gemini-` → gemini，其余 → openai。
pub fn resolve_model_by_prefix(model_name: &str) -> ModelResolution {
    let vendor_id = if model_name.starts_with("claude-") {
        "anthropic"
    } else if model_name.starts_with("gemini-") || model_name.starts_with("models/gemini-") {
        "gemini"
    } else {
        "openai"
    };
    ModelResolution {
        vendor_id: vendor_id.to_string(),
        target_model: model_name.to_string(),
        account_ids: Vec::new(),
        preferred_account_id: None,
        alias_name: None,
    }
}

/// 解析模型名 → 路由信息（spec §4.3）：
/// - alias 有绑定 → JOIN model_alias_accounts 取精确账户集（is_preferred 优先）
/// - alias 无绑定但绑了 vendor → 按该厂商路由
/// - 无 alias → 前缀回退
pub async fn resolve_model(pool: &SqlitePool, model_name: &str) -> anyhow::Result<ModelResolution> {
    let model_name = sanitize_model_name(model_name);
    let alias = sqlx::query_as::<_, ModelAlias>(
        "SELECT id, alias, target_model, vendor_id, created_at FROM model_aliases WHERE alias = ?",
    )
    .bind(&model_name)
    .fetch_optional(pool)
    .await?;

    if let Some(alias) = alias {
        let alias_id = alias.id.unwrap_or_default();
        let bindings = sqlx::query_as::<_, (i64, i64)>(
            "SELECT account_id, is_preferred FROM model_alias_accounts
             WHERE alias_id = ? ORDER BY position, id",
        )
        .bind(alias_id)
        .fetch_all(pool)
        .await?;

        let account_ids: Vec<i64> = bindings.iter().map(|(a, _)| *a).collect();
        let preferred_account_id = bindings
            .iter()
            .find(|(_, preferred)| *preferred == 1)
            .map(|(a, _)| *a);
        let alias_name = Some(alias.alias.clone());

        if !account_ids.is_empty() {
            return Ok(ModelResolution {
                vendor_id: alias.vendor_id.unwrap_or_default(),
                target_model: alias.target_model,
                account_ids,
                preferred_account_id,
                alias_name,
            });
        }

        if let Some(vendor_id) = alias.vendor_id {
            return Ok(ModelResolution {
                vendor_id,
                target_model: alias.target_model,
                account_ids: Vec::new(),
                preferred_account_id,
                alias_name,
            });
        }
    }
    Ok(resolve_model_by_prefix(&model_name))
}

/// 账户选择公共 SQL：JOIN vendors 解析 protocol 与有效 base_url。
/// - 有效 openai URL = 账户自定义 base_url（非空）或厂商 default_base_url
/// - 有效 anthropic URL = 账户自定义 anthropic_base_url（非空）或厂商 default_anthropic_url / default_base_url
const ACCOUNT_SELECT: &str = "SELECT
    a.id, a.name, a.vendor_id, a.api_key_enc, a.enabled, a.weight,
    v.protocol,
    a.base_url AS custom_base_url_raw,
    a.anthropic_base_url AS custom_anthropic_base_url_raw,
    COALESCE(NULLIF(a.base_url, ''), v.default_base_url) AS base_url,
    COALESCE(NULLIF(a.anthropic_base_url, ''), v.default_anthropic_url, v.default_base_url) AS anthropic_base_url
  FROM accounts a
  JOIN vendors v ON a.vendor_id = v.id";

fn row_to_account(row: &sqlx::sqlite::SqliteRow, encryption_secret: &str) -> Option<Account> {
    let encrypted_key: String = match row.try_get("api_key_enc") {
        Ok(k) => k,
        Err(_) => {
            tracing::warn!("account row missing api_key_enc");
            return None;
        }
    };
    match decrypt_api_key(&encrypted_key, encryption_secret) {
        Ok(api_key) => {
            let custom_base_url = row
                .try_get::<Option<String>, _>("custom_base_url_raw")
                .ok()
                .flatten()
                .is_some_and(|u| !u.is_empty());
            let custom_anthropic_base_url = row
                .try_get::<Option<String>, _>("custom_anthropic_base_url_raw")
                .ok()
                .flatten()
                .is_some_and(|u| !u.is_empty());
            Some(Account {
                id: row.try_get("id").unwrap_or_default(),
                name: row.try_get("name").unwrap_or_default(),
                vendor_id: row.try_get("vendor_id").unwrap_or_default(),
                protocol: row.try_get("protocol").unwrap_or_default(),
                api_key,
                base_url: row.try_get("base_url").ok(),
                anthropic_base_url: row.try_get("anthropic_base_url").ok(),
                custom_base_url,
                custom_anthropic_base_url,
                enabled: row.try_get("enabled").unwrap_or(1),
                weight: row.try_get("weight").unwrap_or(1),
            })
        }
        Err(e) => {
            let name: String = row.try_get("name").unwrap_or_default();
            let id: i64 = row.try_get("id").unwrap_or_default();
            tracing::warn!(
                "failed to decrypt API key for account {} (id={}), check MASTER_KEY: {e}",
                name,
                id
            );
            None
        }
    }
}

/// Load enabled accounts by explicit IDs（跨厂商绑定集）。
pub async fn get_accounts_by_ids(
    pool: &SqlitePool,
    ids: &[i64],
    encryption_secret: &str,
) -> anyhow::Result<Vec<Account>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders: Vec<String> = ids.iter().map(|_| "?".to_string()).collect();
    let sql = format!(
        "{ACCOUNT_SELECT} WHERE a.id IN ({}) AND a.enabled = 1 ORDER BY a.weight DESC, a.id ASC",
        placeholders.join(",")
    );
    let mut query = sqlx::query(&sql);
    for id in ids {
        query = query.bind(id);
    }
    let rows = query.fetch_all(pool).await?;
    Ok(rows
        .iter()
        .filter_map(|row| row_to_account(row, encryption_secret))
        .collect())
}

/// 加载指定厂商（或全部）的 enabled 账户，按 weight 降序。
pub async fn get_active_accounts(
    pool: &SqlitePool,
    vendor_id: Option<&str>,
    encryption_secret: &str,
) -> anyhow::Result<Vec<Account>> {
    let rows = if let Some(vendor) = vendor_id {
        sqlx::query(&format!(
            "{ACCOUNT_SELECT} WHERE a.vendor_id = ? AND a.enabled = 1 ORDER BY a.weight DESC, a.id ASC"
        ))
        .bind(vendor)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(&format!(
            "{ACCOUNT_SELECT} WHERE a.enabled = 1 ORDER BY a.weight DESC, a.id ASC"
        ))
        .fetch_all(pool)
        .await?
    };
    Ok(rows
        .iter()
        .filter_map(|row| row_to_account(row, encryption_secret))
        .collect())
}

pub fn is_retryable_status(status: u16) -> bool {
    matches!(status, 401 | 403 | 429)
}
