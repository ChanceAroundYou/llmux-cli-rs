//! Codex 模型清单的「客户端版本」发现、探测与安全应用。
//!
//! 背景：codex 私有后端的 `GET /models?client_version=` 会按版本过滤模型。
//! 版本写死时，一旦官方发布新模型而我们停在旧版本，上游会回 **200 + 空数组**，
//! 旧代码把它当成成功缓存 24 小时，面板只剩空列表。
//!
//! 本模块只负责「模型清单」这一层：发现公开最新版本 → 用真实 codex 账户探测
//! → 安全门（不得丢掉任何现有别名的 target）→ 切换版本 → 刷新 codex 模型缓存
//! → 对新模型自动建别名。推理/协议路径不读这个版本号。
//!
//! 事实取法：
//! - 生效版本：`effective_client_version()`（手动 pin > last_good > 编译期兜底）
//! - 公开最新：`discover_latest()`（npm → GitHub，10s 超时）
//! - 状态行：`codex_update_state`（单行表，刻意不放进 `settings`）
//!
//! 所有网络失败只记录到 `last_error` 并返回错误，不影响网关请求。

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex as StdMutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};
use tokio::sync::Mutex as AsyncMutex;

use llmux_core::codex_compat::CODEX_MODELS_CLIENT_VERSION;
use llmux_core::dispatcher::{get_active_accounts, resolve_provider_type};
use llmux_core::settings::SettingsService;

use crate::app::AppState;

pub const KEY_CLIENT_VERSION: &str = "codex_client_version";
pub const KEY_AUTO_CHECK: &str = "codex_update_auto_check";
pub const KEY_AUTO_APPLY: &str = "codex_update_auto_apply";
pub const KEY_AUTO_ALIAS: &str = "codex_update_auto_alias";
pub const KEY_INTERVAL_HOURS: &str = "codex_update_interval_hours";

const MAX_ALIASES_PER_ROUND: usize = 50;
const VERSION_CACHE_TTL_SECS: u64 = 60;

// ---------------------------------------------------------------------------
// 版本号：纯函数
// ---------------------------------------------------------------------------

/// 只接受 `major.minor.patch` 三段纯数字。prerelease（`-alpha` / `-rc`）和
/// `rust-v` 前缀由调用方先剥离；带任何其它字符一律视为垃圾值。
pub fn is_valid_version(raw: &str) -> bool {
    let s = raw.trim();
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    parts
        .iter()
        .all(|p| !p.is_empty() && p.len() <= 6 && p.chars().all(|c| c.is_ascii_digit()))
}

pub fn parse_version(raw: &str) -> Option<(u64, u64, u64)> {
    if !is_valid_version(raw) {
        return None;
    }
    let mut it = raw.trim().split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    let patch = it.next()?.parse().ok()?;
    Some((major, minor, patch))
}

/// 同版本不算更新；只有严格更大才是。
pub fn version_gt(a: &str, b: &str) -> bool {
    match (parse_version(a), parse_version(b)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// 去掉 GitHub release tag 的 `rust-v` / `v` 前缀。
pub fn strip_tag_prefix(tag: &str) -> &str {
    tag.trim()
        .strip_prefix("rust-v")
        .or_else(|| tag.trim().strip_prefix('v'))
        .unwrap_or_else(|| tag.trim())
}

// ---------------------------------------------------------------------------
// 运行期状态（单行表）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct UpdateState {
    pub latest_version: Option<String>,
    pub last_good_version: Option<String>,
    pub source: Option<String>,
    pub last_checked_at: i64,
    pub last_error: Option<String>,
    pub pending_reason: Option<String>,
    pub auto_created: Vec<Value>,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub async fn load_state(pool: &SqlitePool) -> UpdateState {
    let row = sqlx::query(
        "SELECT latest_version, last_good_version, source, last_checked_at, \
         last_error, pending_reason, auto_created_json \
         FROM codex_update_state WHERE id = 1",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let Some(row) = row else {
        return UpdateState::default();
    };
    let latest_version: Option<String> = row.try_get("latest_version").ok().flatten();
    let last_good_version: Option<String> = row.try_get("last_good_version").ok().flatten();
    let source: Option<String> = row.try_get("source").ok().flatten();
    let last_error: Option<String> = row.try_get("last_error").ok().flatten();
    let pending_reason: Option<String> = row.try_get("pending_reason").ok().flatten();
    let auto_created_json: String = row
        .try_get("auto_created_json")
        .unwrap_or_else(|_| "[]".to_string());
    UpdateState {
        latest_version,
        last_good_version,
        source,
        last_checked_at: row.try_get("last_checked_at").unwrap_or(0),
        last_error,
        pending_reason,
        auto_created: serde_json::from_str(&auto_created_json).unwrap_or_default(),
    }
}

pub async fn save_state(pool: &SqlitePool, state: &UpdateState) {
    let auto_created = serde_json::to_string(&state.auto_created).unwrap_or_else(|_| "[]".into());
    if let Err(e) = sqlx::query(
        "INSERT INTO codex_update_state \
         (id, latest_version, last_good_version, source, last_checked_at, last_error, pending_reason, auto_created_json) \
         VALUES (1, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET \
           latest_version=excluded.latest_version, \
           last_good_version=excluded.last_good_version, \
           source=excluded.source, \
           last_checked_at=excluded.last_checked_at, \
           last_error=excluded.last_error, \
           pending_reason=excluded.pending_reason, \
           auto_created_json=excluded.auto_created_json",
    )
    .bind(&state.latest_version)
    .bind(&state.last_good_version)
    .bind(&state.source)
    .bind(state.last_checked_at)
    .bind(&state.last_error)
    .bind(&state.pending_reason)
    .bind(auto_created)
    .execute(pool)
    .await
    {
        tracing::warn!("codex-update: save state failed: {e}");
    }
}

// ---------------------------------------------------------------------------
// 偏好与生效版本
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Prefs {
    pub manual_version: Option<String>,
    pub auto_check: bool,
    pub auto_apply: bool,
    pub auto_alias: bool,
    pub interval_hours: u64,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            manual_version: None,
            auto_check: true,
            auto_apply: true,
            auto_alias: true,
            interval_hours: 6,
        }
    }
}

pub async fn load_prefs(pool: &SqlitePool) -> Prefs {
    let settings = SettingsService::new(pool.clone())
        .get_all()
        .await
        .unwrap_or_default();
    let manual_version = settings
        .get(KEY_CLIENT_VERSION)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let bool_pref = |key: &str, default: bool| {
        settings
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(default)
    };
    let interval_hours = settings
        .get(KEY_INTERVAL_HOURS)
        .and_then(Value::as_u64)
        .unwrap_or(6)
        .clamp(1, 168);
    Prefs {
        manual_version,
        auto_check: bool_pref(KEY_AUTO_CHECK, true),
        auto_apply: bool_pref(KEY_AUTO_APPLY, true),
        auto_alias: bool_pref(KEY_AUTO_ALIAS, true),
        interval_hours,
    }
}

/// 生效版本优先级：手动 pin > 探测通过的 last_good > 编译期兜底。
pub fn resolve_effective_version(prefs: &Prefs, state: &UpdateState) -> (String, &'static str) {
    if let Some(v) = prefs
        .manual_version
        .as_deref()
        .filter(|v| is_valid_version(v))
    {
        return (v.to_string(), "manual");
    }
    if let Some(v) = state
        .last_good_version
        .as_deref()
        .filter(|v| is_valid_version(v))
    {
        return (v.to_string(), "auto");
    }
    (CODEX_MODELS_CLIENT_VERSION.to_string(), "fallback")
}

static VERSION_CACHE: LazyLock<StdMutex<Option<(String, Instant)>>> =
    LazyLock::new(|| StdMutex::new(None));

/// 模型清单路径每次抓取都要读的版本号。60s 进程内缓存，apply 时主动失效。
pub async fn effective_client_version(pool: &SqlitePool) -> String {
    if let Ok(guard) = VERSION_CACHE.lock() {
        if let Some((cached, at)) = guard.as_ref() {
            if at.elapsed() < Duration::from_secs(VERSION_CACHE_TTL_SECS) {
                return cached.clone();
            }
        }
    }
    let prefs = load_prefs(pool).await;
    let state = load_state(pool).await;
    let (version, _) = resolve_effective_version(&prefs, &state);
    if let Ok(mut guard) = VERSION_CACHE.lock() {
        *guard = Some((version.clone(), Instant::now()));
    }
    version
}

pub fn invalidate_version_cache() {
    if let Ok(mut guard) = VERSION_CACHE.lock() {
        *guard = None;
    }
}

// ---------------------------------------------------------------------------
// 发现最新版本
// ---------------------------------------------------------------------------

static DISCOVERY_OVERRIDE: LazyLock<StdMutex<Option<String>>> =
    LazyLock::new(|| StdMutex::new(None));

/// 测试用版本源。生产代码永远不调用；`None` 恢复真实网络发现。
#[doc(hidden)]
pub fn set_discovery_override(version: Option<String>) {
    if let Ok(mut guard) = DISCOVERY_OVERRIDE.lock() {
        *guard = version;
    }
}

static BACKEND_BASE_OVERRIDE: LazyLock<StdMutex<Option<String>>> =
    LazyLock::new(|| StdMutex::new(None));

pub fn codex_backend_base() -> String {
    BACKEND_BASE_OVERRIDE
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(|| llmux_core::codex_compat::CODEX_BACKEND_BASE.to_string())
}

/// 测试用后端地址（本地 mock `/models`）。`None` 恢复真实 codex 后端。
#[doc(hidden)]
pub fn set_codex_backend_base_for_test(base: Option<String>) {
    if let Ok(mut guard) = BACKEND_BASE_OVERRIDE.lock() {
        *guard = base;
    }
}

/// 返回 `(版本, 来源)`。先 npm registry，再 GitHub latest release；10s 超时。
pub async fn discover_latest() -> Result<(String, String), String> {
    if let Some(v) = DISCOVERY_OVERRIDE.lock().ok().and_then(|g| g.clone()) {
        return if is_valid_version(&v) {
            Ok((v, "override".to_string()))
        } else {
            Err(format!("invalid version override: {v}"))
        };
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;

    if let Ok(resp) = client
        .get("https://registry.npmjs.org/@openai/codex/latest")
        .header("accept", "application/json")
        .send()
        .await
    {
        if resp.status().is_success() {
            if let Ok(doc) = resp.json::<Value>().await {
                if let Some(v) = doc.get("version").and_then(Value::as_str) {
                    if is_valid_version(v) {
                        return Ok((v.trim().to_string(), "npm".to_string()));
                    }
                }
            }
        }
    }

    if let Ok(resp) = client
        .get("https://api.github.com/repos/openai/codex/releases/latest")
        .header("accept", "application/vnd.github+json")
        .header("user-agent", "llmux")
        .send()
        .await
    {
        if resp.status().is_success() {
            if let Ok(doc) = resp.json::<Value>().await {
                if let Some(tag) = doc.get("tag_name").and_then(Value::as_str) {
                    let version = strip_tag_prefix(tag);
                    if is_valid_version(version) {
                        return Ok((version.to_string(), "github".to_string()));
                    }
                }
            }
        }
    }

    Err("neither npm nor GitHub returned a valid codex version".to_string())
}

// ---------------------------------------------------------------------------
// 账户与探测
// ---------------------------------------------------------------------------

/// codex 原生账户的唯一判据：`category=codex` 且 `provider_id` 能解析到 codex。
#[doc(hidden)]
pub fn is_codex_native_account(category: &str, provider_id: &str) -> bool {
    category == "codex" && resolve_provider_type(None, provider_id) == "codex"
}

static ACCOUNTS_OVERRIDE: LazyLock<StdMutex<Option<Vec<(i64, String)>>>> =
    LazyLock::new(|| StdMutex::new(None));

/// 测试用 codex 账户清单 `(id, alias)`。生产代码永远不调用；`None` 恢复真实查询。
///
/// 为什么需要：真实路径 `get_active_accounts` 对每一行都调 `decrypt_api_key`，
/// 而 KDF 是 `scrypt(Params::recommended())` —— release 下几十毫秒，debug 下
/// 单次实测 65s，足以把这几条契约测试从毫秒拖到几分钟。
#[doc(hidden)]
pub fn set_accounts_override_for_test(accounts: Option<Vec<(i64, String)>>) {
    if let Ok(mut guard) = ACCOUNTS_OVERRIDE.lock() {
        *guard = accounts;
    }
}

fn accounts_override() -> Option<Vec<llmux_core::adapters::Account>> {
    let list = ACCOUNTS_OVERRIDE.lock().ok()?.clone()?;
    Some(
        list.into_iter()
            .map(|(id, alias)| llmux_core::adapters::Account {
                id,
                alias,
                provider_id: "codex".to_string(),
                api_key: String::new(),
                base_url: None,
                anthropic_base_url: None,
                is_active: 1,
                weight: 1,
                openai_compatible: 0,
                chat_endpoint: None,
                responses_endpoint: None,
                messages_endpoint: None,
                default_protocol: None,
                balance_provider: String::new(),
                balance_auth: String::new(),
                // 非空 = 「已导入凭据」，与真实 codex 账户同形。
                codex_auth_json: "{}".to_string(),
                category: "codex".to_string(),
            })
            .collect(),
    )
}

async fn codex_accounts(state: &AppState) -> Vec<llmux_core::adapters::Account> {
    if let Some(accounts) = accounts_override() {
        return accounts;
    }
    match get_active_accounts(&state.pool, None, &state.master_key).await {
        Ok(accounts) => accounts
            .into_iter()
            .filter(|a| is_codex_native_account(&a.category, &a.provider_id))
            .collect(),
        Err(e) => {
            tracing::warn!("codex-update: list accounts failed: {e}");
            Vec::new()
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ProbeOutcome {
    pub ok: bool,
    pub per_account: Vec<(i64, String, Vec<Value>)>,
    pub errors: Vec<String>,
}

static PROBE_OVERRIDE: LazyLock<StdMutex<Option<Vec<(i64, String, Vec<Value>)>>>> =
    LazyLock::new(|| StdMutex::new(None));

/// 测试用探测结果。生产代码永远不调用；`None` 恢复真实 HTTP 探测。
#[doc(hidden)]
pub fn set_probe_override_for_test(probed: Option<Vec<(i64, String, Vec<Value>)>>) {
    if let Ok(mut guard) = PROBE_OVERRIDE.lock() {
        *guard = probed;
    }
}

static REFRESH_OVERRIDE: LazyLock<StdMutex<Option<Vec<(i64, String, Vec<Value>)>>>> =
    LazyLock::new(|| StdMutex::new(None));

/// 测试用刷新结果。调用方应先把清单写进 `account_model_cache`（模拟 refresh 的
/// 持久化结果）；`apply` 只拿这里返回值做诊断。`None` 恢复真实抓取。
#[doc(hidden)]
pub fn set_refresh_override_for_test(refreshed: Option<Vec<(i64, String, Vec<Value>)>>) {
    if let Ok(mut guard) = REFRESH_OVERRIDE.lock() {
        *guard = refreshed;
    }
}

/// 用真实 codex 账户探测指定版本，不落缓存。
///
/// 只要至少一个账户返回非空清单即视为成功；单账户失败不阻塞其他账户。
pub async fn probe_version(state: &AppState, version: &str) -> ProbeOutcome {
    if let Some(probed) = PROBE_OVERRIDE.lock().ok().and_then(|g| g.clone()) {
        return ProbeOutcome {
            ok: !probed.is_empty(),
            per_account: probed,
            errors: Vec::new(),
        };
    }

    let accounts = codex_accounts(state).await;
    let results = futures_util::future::join_all(accounts.into_iter().map(|account| async move {
        if account.codex_auth_json.trim().is_empty() {
            return (account, Vec::new(), Some("no codex credentials".to_string()));
        }
        let (models, error) =
            crate::routes::models::available::fetch_provider_models_with_version(
                &state.pool,
                &account,
                "codex",
                version,
            )
            .await;
        (account, models, error)
    }))
    .await;

    let mut outcome = ProbeOutcome::default();
    for (account, models, error) in results {
        if models.is_empty() {
            let reason = error.unwrap_or_else(|| "empty model list".to_string());
            outcome.errors.push(format!("{}: {reason}", account.alias));
        } else {
            outcome.ok = true;
            outcome
                .per_account
                .push((account.id, account.alias, models));
        }
    }
    outcome
}

/// 现有别名的绑定：`(alias, target_model, 绑定的 codex 账户 id)`。
///
/// `account_ids` 为空时按 `provider_id` 展开（与路由侧语义一致）；纯重定向
/// 别名（provider_id/account_ids 都空）不参与安全门。
pub async fn alias_bindings(state: &AppState) -> Vec<(String, String, Vec<i64>)> {
    let accounts = codex_accounts(state).await;
    if accounts.is_empty() {
        return Vec::new();
    }
    let rows = sqlx::query(
        "SELECT alias, target_model, provider_id, account_ids FROM model_aliases",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    let mut out = Vec::new();
    for row in rows {
        let alias: String = row.try_get("alias").unwrap_or_default();
        let target: String = row.try_get("target_model").unwrap_or_default();
        let provider: Option<String> = row.try_get("provider_id").ok();
        let raw_ids: Option<String> = row.try_get("account_ids").ok();
        let mut ids: Vec<i64> = raw_ids
            .as_deref()
            .and_then(|s| serde_json::from_str::<Vec<i64>>(s).ok())
            .unwrap_or_default();
        ids.retain(|id| accounts.iter().any(|a| a.id == *id));
        if ids.is_empty() {
            if let Some(p) = provider.as_deref().filter(|p| !p.is_empty()) {
                for a in &accounts {
                    if a.provider_id == p || a.alias == p {
                        ids.push(a.id);
                    }
                }
            }
        }
        ids.sort_unstable();
        ids.dedup();
        if !ids.is_empty() {
            out.push((alias, target, ids));
        }
    }
    out
}

/// 安全门：返回「探测清单里已经不存在、但现有别名仍指向它」的
/// `(alias, target_model)`。
///
/// 只看该别名绑定的账户：绑了多个账户时，任一账户的清单里有这个 target 即视为
/// 仍然可用（dispatch 会挑有它的账户）。
pub fn compute_alias_loss(
    probed: &[(i64, String, Vec<Value>)],
    bindings: &[(String, String, Vec<i64>)],
) -> Vec<(String, String)> {
    let per_account: HashMap<i64, HashSet<String>> = probed
        .iter()
        .map(|(id, _, models)| {
            (
                *id,
                models
                    .iter()
                    .filter_map(|m| m.get("id").and_then(Value::as_str))
                    .map(|s| s.to_string())
                    .collect(),
            )
        })
        .collect();

    let mut loss = Vec::new();
    for (alias, target, ids) in bindings {
        let mut any_probed = false;
        let mut available = false;
        for id in ids {
            if let Some(models) = per_account.get(id) {
                any_probed = true;
                if models.contains(target) {
                    available = true;
                    break;
                }
            }
        }
        if any_probed && !available {
            loss.push((alias.clone(), target.clone()));
        }
    }
    loss
}

// ---------------------------------------------------------------------------
// 模型 diff
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct ModelDiff {
    /// 既没有被别名/聚合别名名字覆盖、也没有被任何绑定 target 覆盖的新 slug。
    pub new_models: Vec<Value>,
    /// 绑定到 codex 账户、但 target 已不在任何 codex 模型清单里的别名（只报告）。
    pub stale_aliases: Vec<Value>,
}

pub async fn compute_model_diff(state: &AppState) -> ModelDiff {
    let accounts = codex_accounts(state).await;
    if accounts.is_empty() {
        return ModelDiff::default();
    }
    let id_to_alias: HashMap<i64, String> =
        accounts.iter().map(|a| (a.id, a.alias.clone())).collect();

    let rows = sqlx::query("SELECT account_id, alias, models_json FROM account_model_cache")
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default();

    let mut by_slug: HashMap<String, (Vec<i64>, Vec<String>, Option<u64>)> = HashMap::new();
    let mut all_slugs: HashSet<String> = HashSet::new();
    for row in rows {
        let account_id: i64 = row.try_get("account_id").unwrap_or(0);
        if !id_to_alias.contains_key(&account_id) {
            continue;
        }
        let alias: String = row.try_get("alias").unwrap_or_default();
        let models_json: String = row
            .try_get("models_json")
            .unwrap_or_else(|_| "[]".to_string());
        let models: Vec<Value> = serde_json::from_str(&models_json).unwrap_or_default();
        let placeholder = crate::routes::models::available::placeholder_id(&alias);
        for model in models {
            let slug = model
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if slug.is_empty() || slug == placeholder {
                continue;
            }
            if model
                .get("disabled")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                continue;
            }
            all_slugs.insert(slug.to_string());
            let entry = by_slug
                .entry(slug.to_string())
                .or_insert_with(|| (Vec::new(), Vec::new(), None));
            if !entry.0.contains(&account_id) {
                entry.0.push(account_id);
            }
            if !entry.1.contains(&alias) {
                entry.1.push(alias.clone());
            }
            if entry.2.is_none() {
                entry.2 = model.get("context_length").and_then(Value::as_u64);
            }
        }
    }

    let mut covered_names: HashSet<String> = HashSet::new();
    if let Ok(rows) = sqlx::query(
        "SELECT alias FROM model_aliases UNION SELECT alias FROM aggregate_aliases",
    )
    .fetch_all(&state.pool)
    .await
    {
        for row in rows {
            if let Ok(alias) = row.try_get::<String, _>("alias") {
                covered_names.insert(alias);
            }
        }
    }

    let bindings = alias_bindings(state).await;
    let mut covered_targets: HashSet<String> = HashSet::new();
    let mut stale_aliases = Vec::new();
    for (alias, target, _ids) in &bindings {
        covered_targets.insert(target.clone());
        if !all_slugs.contains(target) {
            stale_aliases.push(json!({"alias": alias, "target_model": target}));
        }
    }

    let mut new_models: Vec<Value> = by_slug
        .into_iter()
        .filter(|(slug, _)| {
            !covered_names.contains(slug) && !covered_targets.contains(slug)
        })
        .map(|(slug, (mut account_ids, mut account_aliases, context_window))| {
            // 稳定输出：账户/别名顺序不受 SQL 行顺序影响，UI 和自动别名的
            // `provider_id = aliases.first()` 才是确定性的。
            account_ids.sort_unstable();
            account_aliases.sort();
            json!({
                "slug": slug,
                "account_ids": account_ids,
                "account_aliases": account_aliases,
                "context_window": context_window,
                "suggested_alias": slug,
            })
        })
        .collect();
    new_models.sort_by(|a, b| {
        a.get("slug")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(b.get("slug").and_then(Value::as_str).unwrap_or(""))
    });
    stale_aliases.sort_by(|a, b| {
        a.get("alias")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(b.get("alias").and_then(Value::as_str).unwrap_or(""))
    });

    ModelDiff {
        new_models,
        stale_aliases,
    }
}

// ---------------------------------------------------------------------------
// 应用 / 检查 / 状态
// ---------------------------------------------------------------------------

static APPLY_LOCK: LazyLock<AsyncMutex<()>> = LazyLock::new(|| AsyncMutex::new(()));

/// 探测 + 安全门 + 切换 + 刷新 + 自动建别名。
///
/// - `version=None` 时用最近发现的 `latest_version`；
/// - `pin=true` 写 `codex_client_version`（手动 pin），否则写 `last_good_version`；
/// - `force=true` 越过「不回退」和 alias-target 安全门（人工确认路径）。
pub async fn apply_version(
    state: &AppState,
    version: Option<String>,
    pin: bool,
    force: bool,
) -> Result<Value, String> {
    let _guard = APPLY_LOCK.lock().await;

    let prefs = load_prefs(&state.pool).await;
    let mut update_state = load_state(&state.pool).await;

    let version = match version {
        Some(v) => {
            let v = strip_tag_prefix(&v).to_string();
            if !is_valid_version(&v) {
                return Err(format!("invalid version: {v}"));
            }
            v
        }
        None => update_state
            .latest_version
            .clone()
            .filter(|v| is_valid_version(v))
            .ok_or_else(|| "no latest version discovered yet".to_string())?,
    };

    let (effective, _) = resolve_effective_version(&prefs, &update_state);
    let manual_pin = prefs
        .manual_version
        .as_deref()
        .filter(|v| is_valid_version(v));
    // 手动 pin 是用户的显式覆盖：自动应用不能偷偷改它。要么用户显式 pin 新版本，
    // 要么先清空 pin（POST /codex-update/reset）。否则 last_good 会一直更新而
    // 生效版本纹丝不动，下一轮又「发现更新」→ 反复探测/建别名。
    if !pin && !force {
        if let Some(pinned) = manual_pin {
            if pinned != version {
                return Ok(json!({
                    "applied": false,
                    "reason": "manual pin is set",
                    "version": version,
                    "pinned_version": pinned,
                }));
            }
        }
    }
    if !force && !version_gt(&version, &effective) {
        return Ok(json!({
            "applied": false,
            "reason": "not newer",
            "version": version,
            "effective_version": effective,
        }));
    }

    let probe = probe_version(state, &version).await;
    if !probe.ok {
        let message = format!(
            "probe failed for {version}: {}",
            if probe.errors.is_empty() {
                "no non-empty model list".to_string()
            } else {
                probe.errors.join("; ")
            }
        );
        update_state.last_error = Some(message.clone());
        update_state.last_checked_at = now_secs();
        save_state(&state.pool, &update_state).await;
        return Err(message);
    }

    let bindings = alias_bindings(state).await;
    let loss = compute_alias_loss(&probe.per_account, &bindings);
    if !loss.is_empty() && !force {
        let reason = format!(
            "version {version} would drop {} existing alias target(s): {}",
            loss.len(),
            loss.iter()
                .map(|(alias, target)| format!("{alias}->{target}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        update_state.pending_reason = Some(reason.clone());
        update_state.last_checked_at = now_secs();
        update_state.last_error = None;
        save_state(&state.pool, &update_state).await;
        return Ok(json!({
            "applied": false,
            "pending": true,
            "version": version,
            "reason": reason,
        }));
    }

    if pin {
        let _ = SettingsService::new(state.pool.clone())
            .set(KEY_CLIENT_VERSION, Value::String(version.clone()))
            .await;
    }
    update_state.latest_version = Some(version.clone());
    update_state.last_good_version = Some(version.clone());
    update_state.last_checked_at = now_secs();
    update_state.last_error = None;
    update_state.pending_reason = None;
    save_state(&state.pool, &update_state).await;
    invalidate_version_cache();

    // 只失效 codex 账户的模型缓存；非 codex 行的时间戳不动。
    let ids: Vec<i64> = probe.per_account.iter().map(|(id, _, _)| *id).collect();
    if !ids.is_empty() {
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql =
            format!("UPDATE account_model_cache SET updated_at = 0 WHERE account_id IN ({placeholders})");
        let mut query = sqlx::query(&sql);
        for id in &ids {
            query = query.bind(id);
        }
        let _ = query.execute(&state.pool).await;
    }

    let refreshed = if let Some(over) = REFRESH_OVERRIDE.lock().ok().and_then(|g| g.clone()) {
        over
    } else {
        crate::routes::models::available::refresh_codex_models(state).await
    };
    let created = if prefs.auto_alias {
        auto_create_aliases(state, &mut update_state).await
    } else {
        Vec::new()
    };
    save_state(&state.pool, &update_state).await;
    if let Ok(mut cache) = state.models_cache.lock() {
        *cache = None;
    }

    Ok(json!({
        "applied": true,
        "version": version,
        "source": if pin { "manual" } else { "auto" },
        "pin": pin,
        "forced": force,
        "auto_created": created,
        "refreshed_accounts": refreshed
            .iter()
            .map(|(id, alias, models)| json!({
                "account_id": id,
                "alias": alias,
                "models": models.len(),
            }))
            .collect::<Vec<_>>(),
    }))
}

/// 只发现最新版本并写状态；`auto_apply=true` 且发现更新时继续走 `apply_version`。
pub async fn check(state: &AppState, auto_apply: bool) -> Result<Value, String> {
    let mut update_state = load_state(&state.pool).await;
    let (version, source) = match discover_latest().await {
        Ok(found) => found,
        Err(e) => {
            update_state.last_error = Some(e.clone());
            update_state.last_checked_at = now_secs();
            save_state(&state.pool, &update_state).await;
            return Err(e);
        }
    };
    update_state.latest_version = Some(version.clone());
    update_state.source = Some(source.clone());
    update_state.last_checked_at = now_secs();
    update_state.last_error = None;
    // 新发现的 latest 可能和上一次 pending 的版本不同；先清掉，自动应用若被
    // 安全门挡住会重新写入当前版本的原因。
    update_state.pending_reason = None;
    save_state(&state.pool, &update_state).await;

    let prefs = load_prefs(&state.pool).await;
    let (effective, effective_source) = resolve_effective_version(&prefs, &update_state);
    let newer = version_gt(&version, &effective);
    let has_manual_pin = prefs
        .manual_version
        .as_deref()
        .filter(|v| is_valid_version(v))
        .is_some();

    if auto_apply && prefs.auto_apply && newer && !has_manual_pin {
        return apply_version(state, Some(version), false, false).await;
    }

    Ok(json!({
        "checked": true,
        "latest_version": version,
        "source": source,
        "effective_version": effective,
        "effective_source": effective_source,
        "newer": newer,
        "status": status(state).await,
    }))
}

pub async fn status(state: &AppState) -> Value {
    let prefs = load_prefs(&state.pool).await;
    let update_state = load_state(&state.pool).await;
    let (effective, effective_source) = resolve_effective_version(&prefs, &update_state);
    let accounts = codex_accounts(state).await;

    let rows = sqlx::query(
        "SELECT account_id, error, updated_at, models_json FROM account_model_cache",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    let mut cache_map: HashMap<i64, (Option<String>, i64, usize)> = HashMap::new();
    for row in rows {
        let id: i64 = row.try_get("account_id").unwrap_or(0);
        let error: Option<String> = row.try_get("error").ok().flatten();
        let updated_at: i64 = row.try_get("updated_at").unwrap_or(0);
        let models_json: String = row
            .try_get("models_json")
            .unwrap_or_else(|_| "[]".to_string());
        let count = serde_json::from_str::<Vec<Value>>(&models_json)
            .map(|models| models.len())
            .unwrap_or(0);
        cache_map.insert(id, (error, updated_at, count));
    }

    let account_health: Vec<Value> = accounts
        .iter()
        .map(|a| {
            let configured = !a.codex_auth_json.trim().is_empty();
            let token_expired = llmux_core::codex_auth::load_creds(a).map(|c| c.is_expired());
            let (error, updated_at, models_count) =
                cache_map.get(&a.id).cloned().unwrap_or((None, 0, 0));
            json!({
                "id": a.id,
                "alias": a.alias,
                "provider_id": a.provider_id,
                "configured": configured,
                "token_expired": token_expired,
                "models_count": models_count,
                "error": error,
                "updated_at": updated_at,
            })
        })
        .collect();

    let diff = compute_model_diff(state).await;

    json!({
        "effective_version": effective,
        "effective_source": effective_source,
        "manual_version": prefs.manual_version,
        "latest_version": update_state.latest_version,
        "last_good_version": update_state.last_good_version,
        "discovered_source": update_state.source,
        "last_checked_at": update_state.last_checked_at,
        "last_error": update_state.last_error,
        "pending_reason": update_state.pending_reason,
        "auto_created": update_state.auto_created,
        "prefs": {
            "auto_check": prefs.auto_check,
            "auto_apply": prefs.auto_apply,
            "auto_alias": prefs.auto_alias,
            "interval_hours": prefs.interval_hours,
            "manual_version": prefs.manual_version,
        },
        "accounts": account_health,
        "new_models": diff.new_models,
        "stale_aliases": diff.stale_aliases,
        "cli": {
            "installed": crate::routes::system::helpers::check_tool("codex"),
        },
    })
}

// ---------------------------------------------------------------------------
// 自动建别名
// ---------------------------------------------------------------------------

async fn auto_create_aliases(state: &AppState, update_state: &mut UpdateState) -> Vec<Value> {
    let diff = compute_model_diff(state).await;
    create_aliases(state, &diff.new_models, update_state).await
}

/// 对指定 slug 手动建别名（幂等：已存在/同名聚合/disabled 都跳过）。
pub async fn create_aliases_for_slugs(
    state: &AppState,
    slugs: &[String],
) -> Result<Vec<Value>, String> {
    let diff = compute_model_diff(state).await;
    let selected: Vec<Value> = diff
        .new_models
        .into_iter()
        .filter(|m| {
            m.get("slug")
                .and_then(Value::as_str)
                .map(|slug| slugs.iter().any(|s| s == slug))
                .unwrap_or(false)
        })
        .collect();
    let mut update_state = load_state(&state.pool).await;
    let created = create_aliases(state, &selected, &mut update_state).await;
    save_state(&state.pool, &update_state).await;
    Ok(created)
}

async fn create_aliases(
    state: &AppState,
    models: &[Value],
    update_state: &mut UpdateState,
) -> Vec<Value> {
    let mut created = Vec::new();
    for model in models.iter().take(MAX_ALIASES_PER_ROUND) {
        let slug = model
            .get("slug")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if slug.is_empty() || slug.len() > 200 || slug.chars().any(|c| c.is_control()) {
            continue;
        }

        // 账户级 disable：只看**本模型的提供账户**。别的账户禁用同名模型
        // （例如 Copilot 禁了 `gpt-6.1-terra`）不阻塞 codex 建别名；只有提供该模型
        // 的账户全部被禁用时才跳过。`disabled_models` 是 (account_id, model) 按账户，
        // 此前的全局 `WHERE model = ?` 会把别的账户的禁用误判成处处禁用。
        let account_ids = model
            .get("account_ids")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if account_ids.is_empty() {
            continue;
        }
        let model_account_ids: Vec<i64> = account_ids.iter().filter_map(Value::as_i64).collect();
        if model_account_ids.is_empty() {
            continue;
        }
        let disabled_accounts: Vec<i64> = sqlx::query_scalar::<_, i64>(
            "SELECT account_id FROM disabled_models WHERE model = ?",
        )
        .bind(slug)
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default();
        if model_account_ids
            .iter()
            .all(|id| disabled_accounts.contains(id))
        {
            continue;
        }

        // 与 UI `pickModel` 一致：provider_id 用**账户 alias**，不是字符串 "codex"。
        // 写成 "codex" 会让模型列表同时出现 `codex:<slug>` 和 `<account>:<slug>`
        // 两条重复行（见 available::alias_model_rows 的注释）。
        let provider = model
            .get("account_aliases")
            .and_then(Value::as_array)
            .and_then(|aliases| aliases.first())
            .and_then(Value::as_str)
            .unwrap_or("");
        if provider.is_empty() {
            continue;
        }
        let account_ids_json =
            serde_json::to_string(&account_ids).unwrap_or_else(|_| "[]".to_string());
        let alias = model
            .get("suggested_alias")
            .and_then(Value::as_str)
            .unwrap_or(slug);

        match crate::routes::models::aliases::upsert_model_alias(
            state,
            alias,
            slug,
            Some(provider.to_string()),
            Some(account_ids_json),
            None,
            "responses".to_string(),
            None,
            None,
            false,
        )
        .await
        {
            Ok(()) => {
                let record = json!({
                    "alias": alias,
                    "target_model": slug,
                    "created_at": now_secs(),
                });
                update_state.auto_created.push(record.clone());
                created.push(record);
            }
            Err(crate::routes::models::aliases::AliasWriteError::OrdinaryConflict { .. })
            | Err(crate::routes::models::aliases::AliasWriteError::AggregateConflict) => {
                // 已存在：绝不覆盖。下一轮 diff 会把它算成 covered。
                continue;
            }
            Err(crate::routes::models::aliases::AliasWriteError::Internal(e)) => {
                tracing::warn!("codex-update: create alias {alias} failed: {e}");
            }
        }
    }

    // 撤销记录只留最近 200 条，避免状态行无限增长。
    if update_state.auto_created.len() > 200 {
        let drain = update_state.auto_created.len() - 200;
        update_state.auto_created.drain(0..drain);
    }
    created
}

// ---------------------------------------------------------------------------
// 后台循环
// ---------------------------------------------------------------------------

/// 启动后台检查循环：首轮延迟 30s（不跑在启动路径上），之后按偏好间隔运行。
pub fn spawn_codex_update(state: AppState) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(30)).await;
        loop {
            let prefs = load_prefs(&state.pool).await;
            let accounts = codex_accounts(&state).await;
            if accounts.is_empty() {
                // 没有 codex 账户时完全不联网；1h 后再看偏好有没有变。
                tokio::time::sleep(Duration::from_secs(3600)).await;
                continue;
            }

            if prefs.auto_check {
                match check(&state, prefs.auto_apply).await {
                    Ok(_) => {
                        let st = load_state(&state.pool).await;
                        tracing::info!(
                            "codex-update: check ok (latest={:?}, last_good={:?})",
                            st.latest_version,
                            st.last_good_version
                        );
                    }
                    Err(e) => tracing::warn!("codex-update: check failed: {e}"),
                }
                tokio::time::sleep(Duration::from_secs(prefs.interval_hours * 3600)).await;
            } else {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        }
    });
}
