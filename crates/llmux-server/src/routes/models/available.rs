use axum::{
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde_json::{json, Value};
use sqlx::Row;

use llmux_core::context::lookup_context_length;
use llmux_core::dispatcher::{get_active_accounts, resolve_provider_type};

use crate::app::{AppState, ModelsCache};

/// Normalize a model object to a unified format.
///
/// Ensures every model has: `id`, `name`, `object`, `created`.
/// - `id`: extracted from `id` or Gemini's `name` (strips "models/" prefix)
/// - `name`: display name from `displayName` or `name`, fallback to `id`
/// - `object`: "model" if missing
/// - `created`: 0 if missing
fn normalize_model(m: &mut Value) {
    let Value::Object(obj) = m else { return };

    // Resolve id: use "id" if present, otherwise extract from Gemini's "name"
    let id = match (obj.get("id").and_then(Value::as_str), obj.get("name").and_then(Value::as_str)) {
        (Some(existing_id), _) if !existing_id.is_empty() => existing_id.to_string(),
        (_, Some(name)) => name.strip_prefix("models/").unwrap_or(name).to_string(),
        _ => String::new(),
    };

    // Resolve display name
    let display_name = obj
        .get("displayName")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .or_else(|| {
            obj.get("name")
                .and_then(Value::as_str)
                .map(|n| n.strip_prefix("models/").unwrap_or(n).to_string())
        })
        .unwrap_or_else(|| id.clone());

    obj.insert("id".to_string(), json!(id));
    obj.insert("name".to_string(), json!(display_name));
    obj.entry("object".to_string()).or_insert(json!("model"));
    obj.entry("created".to_string()).or_insert(json!(0));

    // Resolve context length: upstream-specific fields first, built-in table as fallback.
    let context_length = extract_context_length(obj).or_else(|| {
        if id.is_empty() {
            None
        } else {
            lookup_context_length(&id)
        }
    });
    if let Some(ctx) = context_length {
        obj.insert("context_length".to_string(), json!(ctx));
    }
}

/// Coerce a JSON number (or numeric string) to u64.
fn as_u64_val(v: &Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_i64().and_then(|i| u64::try_from(i).ok()))
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
}

/// Extract a canonical context length from upstream-specific fields.
fn extract_context_length(obj: &serde_json::Map<String, Value>) -> Option<u64> {
    // GitHub Copilot models: capabilities.limits.max_context_window_tokens
    if let Some(v) = obj
        .get("capabilities")
        .and_then(|c| c.get("limits"))
        .and_then(|l| l.get("max_context_window_tokens"))
        .and_then(as_u64_val)
    {
        return Some(v);
    }
    // Gemini: inputTokenLimit
    if let Some(v) = obj.get("inputTokenLimit").and_then(as_u64_val) {
        return Some(v);
    }
    // OpenAI-compatible variants (OpenRouter `context_length`, vLLM `max_model_len`, …)
    for key in [
        "context_length",
        "max_model_len",
        "max_context_length",
        "context_window",
    ] {
        if let Some(v) = obj.get(key).and_then(as_u64_val) {
            return Some(v);
        }
    }
    None
}

pub async fn get_available_models(
    Extension(state): Extension<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    const CACHE_TTL: i64 = 24 * 60 * 60;
    let force = params.get("force").map(|v| v == "true").unwrap_or(false);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    // Try persistent cache first (survives restarts)
    if let Some((data, per_account, cached_at, stale)) = load_persistent_snapshot(&state.pool).await {
        if !force {
            // keep in-memory cache in sync for v1/models
            if let Ok(mut c) = state.models_cache.lock() {
                *c = Some(ModelsCache { data: data.clone(), created_at: cached_at, refreshing: false });
            }
            tracing::debug!("🤖 Returning {} models from persistent cache (stale={})", data.len(), stale);
            return Json(json!({ "data": data, "stale": stale, "cached_at": cached_at, "per_account": per_account })).into_response();
        }
    }

    if force {
        tracing::info!("🤖 Force refresh requested, bypassing cache");
        let data = do_fetch_models(&state).await;
        persist_snapshot(&state.pool, &data).await;
        {
            let mut cache = state.models_cache.lock().unwrap();
            *cache = Some(ModelsCache {
                data: data.clone(),
                created_at: now,
                refreshing: false,
            });
        }
        return Json(json!({ "data": data, "stale": false, "cached_at": now })).into_response();
    }

    // fallback: memory cache with stale-while-revalidate
    let (cached_data, need_refresh) = {
        let mut c = state.models_cache.lock().unwrap();
        match c.as_mut() {
            None => (None, false),
            Some(entry) => {
                let age = now - entry.created_at;
                let stale = age >= CACHE_TTL;
                let do_refresh = stale && !entry.refreshing;
                if do_refresh {
                    entry.refreshing = true;
                }
                (Some((entry.data.clone(), age, stale, entry.created_at)), do_refresh)
            }
        }
    };
    if let Some((data, age, stale, cached_at)) = cached_data {
        if need_refresh {
            let state_bg = state.clone();
            let cache_bg = state.models_cache.clone();
            tokio::spawn(async move {
                let new_data = do_fetch_models(&state_bg).await;
                persist_snapshot(&state_bg.pool, &new_data).await;
                let now_bg = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                let mut c = cache_bg.lock().unwrap();
                if let Some(e) = c.as_mut() {
                    e.data = new_data;
                    e.created_at = now_bg;
                    e.refreshing = false;
                }
            });
        }
        tracing::debug!(
            "🤖 Returning {} cached models (age: {}s{})",
            data.len(),
            age,
            if stale { ", refreshing in background" } else { "" }
        );
        return Json(json!({ "data": data, "stale": stale, "cached_at": cached_at })).into_response();
    }

    let data = do_fetch_models(&state).await;
    persist_snapshot(&state.pool, &data).await;
    {
        let mut cache = state.models_cache.lock().unwrap();
        *cache = Some(ModelsCache {
            data: data.clone(),
            created_at: now,
            refreshing: false,
        });
    }
    Json(json!({ "data": data, "stale": false, "cached_at": now })).into_response()
}

async fn load_persistent_snapshot(pool: &sqlx::SqlitePool) -> Option<(Vec<Value>, Vec<Value>, i64, bool)> {
    let rows = sqlx::query("SELECT account_id, alias, models_json, error, updated_at FROM account_model_cache ORDER BY updated_at DESC")
        .fetch_all(pool).await.ok()?;
    if rows.is_empty() { return None; }
    let mut all: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut per_account: Vec<Value> = Vec::new();
    let mut max_ts: i64 = 0;
    for r in &rows {
        let alias: String = r.try_get("alias").unwrap_or_default();
        let j: String = r.try_get("models_json").unwrap_or_else(|_| "[]".into());
        let err: Option<String> = r.try_get("error").unwrap_or(None);
        let updated_at: i64 = r.try_get("updated_at").unwrap_or(0);
        let account_id: i64 = r.try_get("account_id").unwrap_or(0);
        max_ts = max_ts.max(updated_at);
        let models: Vec<Value> = serde_json::from_str(&j).unwrap_or_default();
        for m in &models { let id = m.get("id").and_then(Value::as_str).unwrap_or(""); let key = format!("{}:{}", alias, id); if seen.insert(key) { all.push(m.clone()); } }
        per_account.push(json!({"account_id": account_id, "alias": alias, "updated_at": updated_at, "error": err, "count": models.len()}));
    }
    // merge alias custom models
    let alias_rows = sqlx::query("SELECT DISTINCT target_model, provider_id FROM model_aliases WHERE target_model IS NOT NULL AND target_model != ''")
        .fetch_all(pool).await.unwrap_or_default();
    for r in alias_rows { let model_id: String = r.try_get("target_model").unwrap_or_default(); let provider: String = r.try_get("provider_id").unwrap_or_default(); if model_id.is_empty() { continue; } let owned_by = if provider.is_empty() { "custom".to_string() } else { provider.clone() }; let key = format!("{}:{}", owned_by, model_id); if seen.insert(key) { all.push(json!({"id": model_id, "object": "model", "created": 0, "owned_by": owned_by})); } }
    const TTL: i64 = 24*60*60;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let stale = per_account.iter().any(|p| now - p.get("updated_at").and_then(Value::as_i64).unwrap_or(0) >= TTL);
    Some((all, per_account, max_ts, stale))
}

async fn persist_snapshot(pool: &sqlx::SqlitePool, all_models: &[Value]) {
    // group by owned_by (alias)
    let mut by_alias: std::collections::HashMap<String, Vec<Value>> = std::collections::HashMap::new();
    for m in all_models { if let Some(alias) = m.get("owned_by").and_then(Value::as_str) { by_alias.entry(alias.to_string()).or_default().push(m.clone()); } }
    if by_alias.is_empty() { return; }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    // need account_id for each alias
    for (alias, models) in by_alias {
        let row = sqlx::query("SELECT id FROM accounts WHERE alias = ? LIMIT 1").bind(&alias).fetch_optional(pool).await.ok().flatten();
        let account_id: Option<i64> = row.and_then(|r| r.try_get::<i64,_>("id").ok());
        let Some(aid) = account_id else { continue; };
        let j = serde_json::to_string(&models).unwrap_or_else(|_| "[]".into());
        let _ = sqlx::query("INSERT OR REPLACE INTO account_model_cache (account_id, alias, models_json, error, updated_at) VALUES (?, ?, ?, NULL, ?)")
            .bind(aid).bind(&alias).bind(&j).bind(now).execute(pool).await;
    }
}

/// Fetch and merge models from all accounts. Extracted so it can be called
/// both synchronously (cold start) and from a background task (refresh).
async fn do_fetch_models(state: &AppState) -> Vec<Value> {
    let accounts = match get_active_accounts(&state.pool, None, &state.master_key).await {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("Failed to list accounts for model fetch: {e}");
            return vec![];
        }
    };

    if accounts.is_empty() {
        return vec![];
    }

    let futures: Vec<_> = accounts
        .iter()
        .map(|account| {
            let account = account.clone();
            async move {
                let provider_type = resolve_provider_type(None, &account.provider_id);
                let (models, fetch_error) = fetch_provider_models(&account, &provider_type).await;
                (account.alias, models, fetch_error)
            }
        })
        .collect();

    let results: Vec<(String, Vec<Value>, Option<String>)> =
        futures_util::future::join_all(futures).await;

    let mut all_models: Vec<Value> = Vec::new();
    let mut seen_keys = std::collections::HashSet::new();

    for (alias, models, fetch_error) in results {
        if models.is_empty() {
            let key = format!("{}:__unavailable__", alias);
            if seen_keys.insert(key) {
                let mut placeholder = json!({
                    "id": format!("{}-models-unavailable", alias),
                    "name": alias,
                    "object": "model",
                    "created": 0,
                    "owned_by": alias,
                });
                if let Some(err) = &fetch_error {
                    placeholder["error"] = json!(err);
                }
                all_models.push(placeholder);
            }
            continue;
        }
        for mut m in models {
            let id = m.get("id").and_then(Value::as_str).unwrap_or("").to_string();
            let key = format!("{}:{}", alias, id);
            if seen_keys.insert(key) {
                if let Value::Object(obj) = &mut m {
                    obj.insert("owned_by".to_string(), json!(alias));
                }
                all_models.push(m);
            }
        }
    }

    // Merge custom models from aliases
    let alias_model_rows = sqlx::query(
        "SELECT DISTINCT target_model, provider_id FROM model_aliases \
         WHERE target_model IS NOT NULL AND target_model != ''",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    let alias_model_count = alias_model_rows.len();

    for row in alias_model_rows {
        let model_id: String = row.try_get("target_model").unwrap_or_default();
        let provider: String = row.try_get("provider_id").unwrap_or_default();
        if model_id.is_empty() {
            continue;
        }
        let owned_by = if provider.is_empty() {
            "custom".to_string()
        } else {
            provider.clone()
        };
        let key = format!("{}:{}", owned_by, model_id);
        if seen_keys.insert(key) {
            all_models.push(json!({
                "id": model_id,
                "object": "model",
                "created": 0,
                "owned_by": owned_by,
            }));
        }
    }

    tracing::info!(
        "🤖 Fetched {} models ({} from APIs, {} from aliases)",
        all_models.len(),
        all_models.len().saturating_sub(alias_model_count),
        alias_model_count,
    );

    all_models
}

pub async fn fetch_provider_models(
    account: &llmux_core::adapters::Account,
    provider_type: &str,
) -> (Vec<Value>, Option<String>) {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => return (vec![], Some(format!("Failed to create HTTP client: {e}"))),
    };

    let (url, headers): (String, std::collections::BTreeMap<String, String>) = match provider_type
    {
        "openai" => {
            let base = account
                .base_url
                .as_deref()
                .unwrap_or("https://api.openai.com/v1");
            let url = format!("{}/models", base.trim_end_matches('/'));
            let mut headers = std::collections::BTreeMap::new();
            headers.insert(
                "authorization".to_string(),
                format!("Bearer {}", account.api_key),
            );
            (url, headers)
        }
        "anthropic" => {
            let base = account
                .base_url
                .as_deref()
                .unwrap_or("https://api.anthropic.com/v1");
            let url = format!("{}/models", base.trim_end_matches('/'));
            let mut headers = std::collections::BTreeMap::new();
            headers.insert("x-api-key".to_string(), account.api_key.clone());
            headers.insert("anthropic-version".to_string(), "2023-06-01".to_string());
            (url, headers)
        }
        "gemini" => {
            let url = "https://generativelanguage.googleapis.com/v1beta/models".to_string();
            let mut headers = std::collections::BTreeMap::new();
            headers.insert(
                "x-goog-api-key".to_string(),
                account.api_key.clone(),
            );
            (url, headers)
        }
        _ => {
            // Custom / OpenAI-compatible
            let base = account
                .base_url
                .as_deref()
                .unwrap_or("https://api.openai.com/v1");
            let url = format!("{}/models", base.trim_end_matches('/'));
            let mut headers = std::collections::BTreeMap::new();
            headers.insert(
                "authorization".to_string(),
                format!("Bearer {}", account.api_key),
            );
            (url, headers)
        }
    };

    let mut req = client.get(&url);
    for (k, v) in &headers {
        req = req.header(k.as_str(), v.as_str());
    }

    let response = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("🤖 [{}] listModels error for {}: {e}", provider_type, account.alias);
            return (vec![], Some(format!("Network error: {e}")));
        }
    };

    if !response.status().is_success() {
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        let err_msg = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.get("message").and_then(Value::as_str).map(|s| s.to_string())))
            .unwrap_or_else(|| format!("HTTP {}", status));
        tracing::warn!(
            "🤖 [{}] listModels HTTP {} for {}: {}",
            provider_type,
            status,
            account.alias,
            err_msg
        );
        return (vec![], Some(err_msg));
    }

    let body_text = response.text().await.unwrap_or_default();
    let data: Value = match serde_json::from_str(&body_text) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                "🤖 [{}] listModels JSON parse error for {}: {}",
                provider_type,
                account.alias,
                e
            );
            return (vec![], Some(format!("JSON parse error: {e}")));
        }
    };

    // Platform-specific model array extraction
    let mut models = if provider_type == "gemini" {
        // Gemini returns { models: [...] }
        data.get("models")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    } else {
        // OpenAI / Anthropic / Custom all return { data: [...] }
        data.get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };

    // Normalize all model objects to a unified format
    for m in &mut models {
        normalize_model(m);
    }

    tracing::debug!(
        "🤖 [{}] Successfully listed {} models from {}",
        provider_type,
        models.len(),
        account.alias
    );

    (models, None)
}
