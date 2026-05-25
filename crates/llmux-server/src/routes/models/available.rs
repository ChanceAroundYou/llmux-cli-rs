use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde_json::{json, Value};
use sqlx::Row;

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
}

pub async fn get_available_models(Extension(state): Extension<AppState>) -> Response {
    const CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(5 * 60);

    // Check cache first
    {
        let cache = state.models_cache.lock().unwrap();
        if let Some(ref entry) = *cache {
            if entry.created.elapsed() < CACHE_TTL {
                tracing::debug!(
                    "🤖 Returning {} cached models (age: {}s)",
                    entry.data.len(),
                    entry.created.elapsed().as_secs()
                );
                return Json(Value::Array(entry.data.clone())).into_response();
            }
        }
    }

    // Fetch active accounts (matching Bun's dispatcher.listAllModels behavior)
    let accounts = match get_active_accounts(&state.pool, None, &state.master_key).await {
        Ok(a) => a,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to list models: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    if accounts.is_empty() {
        tracing::warn!("🔀 No active accounts found for model listing");
        return Json(Value::Array(vec![])).into_response();
    }

    // Fetch models from all accounts in parallel
    let futures: Vec<_> = accounts
        .iter()
        .map(|account| {
            let pool = state.pool.clone();
            let account = account.clone();
            async move {
                let provider_type = {
                    let pt = sqlx::query_scalar::<_, Option<String>>(
                        "SELECT type FROM providers WHERE id = ?",
                    )
                    .bind(&account.provider_id)
                    .fetch_optional(&pool)
                    .await
                    .ok()
                    .flatten()
                    .flatten();
                    resolve_provider_type(pt.as_deref(), &account.provider_id)
                };
                let (models, fetch_error) = fetch_provider_models(&account, &provider_type).await;
                (account.alias, models, fetch_error)
            }
        })
        .collect();

    let results: Vec<(String, Vec<Value>, Option<String>)> = futures_util::future::join_all(futures).await;

    let mut all_models: Vec<Value> = Vec::new();
    let mut seen_keys = std::collections::HashSet::new();

    for (alias, models, fetch_error) in results {
        if models.is_empty() {
            // Provider API failed (e.g., geo-blocked), show the account with error
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

    // Merge custom models from aliases (models not returned by provider APIs)
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
        // Use the provider_id as owned_by — this is the account alias stored in model_aliases
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

    // Update cache
    {
        let mut cache = state.models_cache.lock().unwrap();
        *cache = Some(ModelsCache {
            data: all_models.clone(),
            created: std::time::Instant::now(),
        });
        tracing::info!(
            "🤖 Cached {} models ({} from APIs, {} from aliases)",
            all_models.len(),
            all_models.len().saturating_sub(alias_model_count),
            alias_model_count,
        );
    }

    Json(Value::Array(all_models)).into_response()
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
            let url = format!(
                "https://generativelanguage.googleapis.com/v1beta/models?key={}",
                account.api_key
            );
            (url, std::collections::BTreeMap::new())
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
