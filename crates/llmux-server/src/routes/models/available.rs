use axum::{
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde_json::{json, Value};

use llmux_core::dispatcher::get_active_accounts;
use llmux_core::repo;

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

    if force {
        tracing::info!("🤖 Force refresh requested, bypassing cache");
        let data = do_fetch_models(&state).await;
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

    // stale-while-revalidate: return cached data, background refresh only when cache expired
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
                // 协议直接取厂商 protocol，无需再查 providers 表
                let provider_type = account.protocol.clone();
                let (models, fetch_error) = fetch_provider_models(&account, &provider_type).await;
                (account.name, account.vendor_id.clone(), models, fetch_error)
            }
        })
        .collect();

    let results: Vec<(String, String, Vec<Value>, Option<String>)> =
        futures_util::future::join_all(futures).await;

    let mut all_models: Vec<Value> = Vec::new();
    let mut seen_keys = std::collections::HashSet::new();

    for (name, vendor_id, models, fetch_error) in results {
        if models.is_empty() {
            let key = format!("{}:__unavailable__", name);
            if seen_keys.insert(key) {
                let mut placeholder = json!({
                    "id": format!("{}-models-unavailable", name),
                    "name": name,
                    "object": "model",
                    "created": 0,
                    // owned_by 语义 = 提供模型的厂商 id（alias 路由按 vendor_id 走）
                    "owned_by": vendor_id,
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
            let key = format!("{}:{}", vendor_id, id);
            if seen_keys.insert(key) {
                if let Value::Object(obj) = &mut m {
                    obj.insert("owned_by".to_string(), json!(vendor_id));
                }
                all_models.push(m);
            }
        }
    }

    // Merge custom models from aliases
    let alias_models = repo::list_alias_custom_models(&state.pool)
        .await
        .unwrap_or_default();

    let alias_model_count = alias_models.len();

    for (model_id, vendor) in alias_models {
        if model_id.is_empty() {
            continue;
        }
        let owned_by = vendor.unwrap_or_else(|| "custom".to_string());
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
            tracing::error!("🤖 [{}] listModels error for {}: {e}", provider_type, account.name);
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
            account.name,
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
                account.name,
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
        account.name
    );

    (models, None)
}
