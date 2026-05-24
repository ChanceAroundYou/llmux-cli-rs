use axum::{
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::sqlite::SqliteRow;
use sqlx::Row;

use llmux_core::crypto::decrypt_api_key;
use llmux_core::dispatcher::{get_active_accounts, resolve_model, resolve_provider_type, ModelResolution};
use llmux_core::models::ModelAlias;

use crate::app::{AppState, ModelsCache};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TestQueueState {
    pub is_running: bool,
    pub total: usize,
    pub current: usize,
    pub progress: usize,
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
                let models = fetch_provider_models(&account, &provider_type).await;
                (account.alias, models)
            }
        })
        .collect();

    let results: Vec<(String, Vec<Value>)> = futures_util::future::join_all(futures).await;

    let mut all_models: Vec<Value> = Vec::new();
    let mut seen_keys = std::collections::HashSet::new();

    for (alias, models) in results {
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
) -> Vec<Value> {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(_) => return vec![],
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
            return vec![];
        }
    };

    if !response.status().is_success() {
        tracing::warn!(
            "🤖 [{}] External API Error: {url} returned {}",
            provider_type,
            response.status().as_u16()
        );
        return vec![];
    }

    let body_text = response.text().await.unwrap_or_default();
    let data: Value = match serde_json::from_str(&body_text) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    // Platform-specific model array extraction
    let models = if provider_type == "gemini" {
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

    tracing::debug!(
        "🤖 [{}] Successfully listed {} models from {}",
        provider_type,
        models.len(),
        account.alias
    );

    models
}

pub async fn get_model_aliases(Extension(state): Extension<AppState>) -> Response {
    match sqlx::query_as::<_, ModelAlias>(
        "SELECT id, alias, target_model, provider_id, account_ids FROM model_aliases ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(aliases) => Json(serde_json::to_value(aliases).unwrap_or(Value::Array(vec![])))
            .into_response(),
        Err(e) => crate::error::simple_error(
            format!("Failed to list aliases: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

pub async fn set_model_alias(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let Some(alias) = body.get("alias").and_then(Value::as_str) else {
        return crate::error::simple_error(
            "Missing required fields: alias, target_model",
            StatusCode::BAD_REQUEST,
        );
    };
    let Some(target_model) = body.get("target_model").and_then(Value::as_str) else {
        return crate::error::simple_error(
            "Missing required fields: alias, target_model",
            StatusCode::BAD_REQUEST,
        );
    };
    let provider_id = body.get("provider_id").and_then(Value::as_str);

    // Parse account_ids: JSON array like [1,5] or comma-separated "1,5"
    let account_ids = body.get("account_ids").and_then(|v| {
        if v.is_array() {
            Some(serde_json::to_string(v).unwrap_or_default())
        } else {
            v.as_str().map(|s| s.to_string())
        }
    });

    match sqlx::query(
        "INSERT OR REPLACE INTO model_aliases (alias, target_model, provider_id, account_ids) VALUES (?, ?, ?, ?)",
    )
    .bind(alias)
    .bind(target_model)
    .bind(provider_id)
    .bind(&account_ids)
    .execute(&state.pool)
    .await
    {
        Ok(_) => {
            // Invalidate models cache so custom models appear immediately
            if let Ok(mut cache) = state.models_cache.lock() {
                *cache = None;
            }
            tracing::info!("🏷️ Set alias {} -> {} (provider: {:?}), cache invalidated", alias, target_model, provider_id);
            Json(json!({ "success": true, "message": "Alias set successfully" })).into_response()
        },
        Err(e) => crate::error::simple_error(
            format!("Failed to set alias: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

pub async fn delete_model_alias(
    Extension(state): Extension<AppState>,
    Path(id): Path<String>,
) -> Response {
    let alias_row = match sqlx::query_as::<_, ModelAlias>(
        "SELECT id, alias, target_model, provider_id, account_ids FROM model_aliases WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            return crate::error::simple_error("Alias not found", StatusCode::NOT_FOUND);
        }
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to lookup alias: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Delete the alias
    if let Err(e) = sqlx::query("DELETE FROM model_aliases WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await
    {
        return crate::error::simple_error(
            format!("Failed to delete alias: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    // Cascade-clean API keys: remove the alias name from allowed_models.
    // Bun stores allowed_models as JSON arrays like ["gpt-4","claude-3"].
    let api_keys: Vec<(i64, String)> =
        match sqlx::query_as("SELECT id, allowed_models FROM api_keys")
            .fetch_all(&state.pool)
            .await
        {
            Ok(rows) => rows,
            Err(_) => {
                return Json(
                    json!({ "success": true, "message": "Alias deleted and API Keys synced successfully" }),
                )
                .into_response();
            }
        };

    for (key_id, allowed_models) in &api_keys {
        if allowed_models == "*" {
            continue;
        }
        if let Ok(mut models) = serde_json::from_str::<Vec<String>>(allowed_models) {
            if models.contains(&alias_row.alias) {
                models.retain(|m| m != &alias_row.alias);
                let updated = if models.is_empty() {
                    "*".to_string()
                } else {
                    serde_json::to_string(&models).unwrap_or_else(|_| "*".to_string())
                };
                let _ = sqlx::query("UPDATE api_keys SET allowed_models = ? WHERE id = ?")
                    .bind(&updated)
                    .bind(key_id)
                    .execute(&state.pool)
                    .await;
                tracing::info!("🔄 Removed alias {} from API Key ID: {}", alias_row.alias, key_id);
            }
        }
    }

    // Invalidate models cache
    if let Ok(mut cache) = state.models_cache.lock() {
        *cache = None;
    }

    Json(json!({ "success": true, "message": "Alias deleted and API Keys synced successfully" })).into_response()
}

pub async fn get_models_health(Extension(state): Extension<AppState>) -> Response {
    // Match Bun backend: for each (account_id, model) group, return the LATEST
    // usage_log row's success, latency_ms as latency, error_message as error,
    // timestamp as last_checked. Also include limits_cache (JSON-parsed),
    // limits_cache_updated_at, and account alias/provider from accounts table.
    let rows: Vec<SqliteRow> = match sqlx::query(
        "SELECT u.account_id, a.provider_id, u.model, u.timestamp AS last_checked, \
                u.success, u.latency_ms AS latency, u.error_message AS error, \
                a.limits_cache, a.limits_cache_updated_at, a.alias AS account_name \
         FROM usage_logs u \
         JOIN accounts a ON u.account_id = a.id \
         WHERE u.id IN ( \
           SELECT MAX(id) FROM usage_logs GROUP BY account_id, model \
         )",
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to get model health: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let health: Vec<Value> = rows
        .iter()
        .map(|row: &SqliteRow| {
            let limits_cache_str: Option<String> =
                row.try_get("limits_cache").unwrap_or_default();
            let limits_cache: Value = limits_cache_str
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(Value::Null);
            json!({
                "account_id": row.try_get::<i64, _>("account_id").unwrap_or_default(),
                "provider_id": row.try_get::<String, _>("provider_id").unwrap_or_default(),
                "model": row.try_get::<String, _>("model").unwrap_or_default(),
                "last_checked": row.try_get::<i64, _>("last_checked").unwrap_or_default(),
                "success": row.try_get::<i64, _>("success").unwrap_or_default(),
                "latency": row.try_get::<i64, _>("latency").unwrap_or_default(),
                "error": row.try_get::<Option<String>, _>("error").unwrap_or_default(),
                "limits_cache": limits_cache,
                "limits_cache_updated_at": row.try_get::<Option<String>, _>("limits_cache_updated_at").unwrap_or_default(),
                "account_name": row.try_get::<String, _>("account_name").unwrap_or_default(),
            })
        })
        .collect();

    Json(Value::Array(health)).into_response()
}

pub async fn get_test_queue_status(Extension(state): Extension<AppState>) -> Response {
    let queue = state.test_queue.lock().unwrap();
    Json(json!({
        "isRunning": queue.is_running,
        "total": queue.total,
        "current": queue.current,
        "progress": queue.progress,
    }))
    .into_response()
}

pub async fn start_test_queue(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let Some(models) = body.get("models").and_then(Value::as_array) else {
        return crate::error::simple_error("Invalid models array", StatusCode::BAD_REQUEST);
    };

    {
        let mut queue = state.test_queue.lock().unwrap();
        if queue.is_running {
            return (
                StatusCode::CONFLICT,
                Json(json!({ "error": "A test queue is already running." })),
            )
                .into_response();
        }
        queue.is_running = true;
        queue.total = models.len();
        queue.current = 0;
        queue.progress = 0;
    }

    tracing::info!("🧪 Starting test for {} models", models.len());
    let pool = state.pool.clone();
    let master_key = state.master_key.clone();
    let queue_state = state.test_queue.clone();
    let models_owned: Vec<Value> = models.to_vec();

    tokio::spawn(async move {
        for (i, model_entry) in models_owned.iter().enumerate() {
            let model_name = model_entry
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let provider_id_override = model_entry
                .get("providerId")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());

            // Resolve provider and get accounts
            // Try resolve_model first (by alias), fall back to providerId, then prefix guess
            let resolution = resolve_model(&pool, model_name).await.unwrap_or_else(|_| {
                ModelResolution {
                    provider_id: provider_id_override.unwrap_or("openai").to_string(),
                    target_model: model_name.to_string(),
                    account_ids: vec![],
                }
            });

            // Override resolved provider with explicit providerId when resolution guessed wrong
            let effective_provider = if resolution.provider_id == "openai" || resolution.provider_id == "gemini" || resolution.provider_id == "anthropic" {
                provider_id_override.unwrap_or(&resolution.provider_id)
            } else {
                &resolution.provider_id
            };

            if let Ok(accounts) =
                get_active_accounts(&pool, Some(effective_provider), &master_key).await
            {
                if let Some(account) = accounts.first() {
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

                        let (url, headers, body) = match provider_type.as_str() {
                            "anthropic" => {
                                let base = account.anthropic_base_url.as_deref().unwrap_or(
                                    account
                                        .base_url
                                        .as_deref()
                                        .unwrap_or("https://api.anthropic.com/v1"),
                                );
                                let url = format!("{}/messages", base.trim_end_matches('/'));
                                let mut headers = std::collections::BTreeMap::new();
                                headers.insert(
                                    "x-api-key".to_string(),
                                    account.api_key.clone(),
                                );
                                headers.insert(
                                    "anthropic-version".to_string(),
                                    "2023-06-01".to_string(),
                                );
                                headers.insert(
                                    "content-type".to_string(),
                                    "application/json".to_string(),
                                );
                                let body = json!({
                                    "model": model_name,
                                    "max_tokens": 10,
                                    "messages": [{"role": "user", "content": "Say OK and nothing else."}]
                                });
                                (url, headers, body)
                            }
                            "gemini" => {
                                let base = account.base_url.as_deref().unwrap_or(
                                    "https://generativelanguage.googleapis.com/v1beta",
                                );
                                let model_id = if model_name.starts_with("models/") {
                                    model_name.to_string()
                                } else {
                                    format!("models/{}", model_name)
                                };
                                let url = format!(
                                    "{}/{}:generateContent?key={}",
                                    base.trim_end_matches('/'),
                                    model_id,
                                    account.api_key
                                );
                                let mut headers = std::collections::BTreeMap::new();
                                headers.insert(
                                    "content-type".to_string(),
                                    "application/json".to_string(),
                                );
                                let body = json!({
                                    "contents": [{"parts": [{"text": "Say OK and nothing else."}]}]
                                });
                                (url, headers, body)
                            }
                            _ => {
                                // OpenAI and custom
                                let base = account
                                    .base_url
                                    .as_deref()
                                    .unwrap_or("https://api.openai.com/v1");
                                let url =
                                    format!("{}/chat/completions", base.trim_end_matches('/'));
                                let mut headers = std::collections::BTreeMap::new();
                                headers.insert(
                                    "authorization".to_string(),
                                    format!("Bearer {}", account.api_key),
                                );
                                headers.insert(
                                    "content-type".to_string(),
                                    "application/json".to_string(),
                                );
                                let body = json!({
                                    "model": model_name,
                                    "messages": [{"role": "user", "content": "Say OK and nothing else."}],
                                    "max_tokens": 10
                                });
                                (url, headers, body)
                            }
                        };

                        // Build reqwest request
                        let start = std::time::Instant::now();
                        let test_success = if let Ok(client) = reqwest::Client::builder()
                            .timeout(std::time::Duration::from_secs(30))
                            .build()
                        {
                            let mut req = client.post(&url);
                            for (k, v) in &headers {
                                req = req.header(k.as_str(), v.as_str());
                            }
                            match req.json(&body).send().await {
                                Ok(response) => response.status().is_success(),
                                Err(_) => false,
                            }
                        } else {
                            false
                        };
                        let latency_ms = start.elapsed().as_millis() as i64;

                        // Log test result
                        let _ = sqlx::query(
                            "INSERT INTO usage_logs \
                             (timestamp, account_id, provider_id, model, input_tokens, output_tokens, \
                              cache_read_input_tokens, cache_creation_input_tokens, \
                              latency_ms, success, error_message, is_test) \
                             VALUES (?, ?, ?, ?, 0, 0, 0, 0, ?, ?, NULL, 1)",
                        )
                        .bind(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as i64,
                        )
                        .bind(account.id)
                        .bind(&account.provider_id)
                        .bind(model_name)
                        .bind(latency_ms)
                        .bind(if test_success { 1 } else { 0 })
                        .execute(&pool)
                        .await;
                    }
                }

            {
                let mut queue = queue_state.lock().unwrap();
                queue.current = i + 1;
                queue.progress = if queue.total > 0 {
                    ((i + 1) * 100) / queue.total
                } else {
                    0
                };
            }
        }

        {
            let mut queue = queue_state.lock().unwrap();
            queue.is_running = false;
        }
    });

    Json(json!({
        "success": true,
        "message": "Queue started",
        "total": models.len()
    }))
    .into_response()
}

pub async fn test_model(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let Some(model_name) = body.get("model").and_then(Value::as_str) else {
        return crate::error::simple_error("No model provided", StatusCode::BAD_REQUEST);
    };

    let provider_id_override = body
        .get("providerId")
        .and_then(Value::as_str)
        .map(String::from);
    let account_id_override = body.get("accountId").and_then(|v| v.as_i64());

    // Resolve model to provider
    let resolution = match resolve_model(&state.pool, model_name).await {
        Ok(r) => r,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to resolve model: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Use providerId override if provided (matching Bun behavior)
    let effective_provider = provider_id_override
        .as_deref()
        .unwrap_or(&resolution.provider_id);

    let accounts = if let Some(acc_id) = account_id_override {
        // Directly fetch the specified account
        match sqlx::query(
            "SELECT id, alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight \
             FROM accounts WHERE id = ? AND is_active = 1",
        )
        .bind(acc_id)
        .fetch_optional(&state.pool)
        .await
        {
            Ok(Some(row)) => {
                let encrypted: String = row.try_get("api_key").unwrap_or_default();
                match decrypt_api_key(&encrypted, &state.master_key) {
                    Ok(api_key) => vec![llmux_core::adapters::Account {
                        id: row.try_get("id").unwrap_or_default(),
                        alias: row.try_get("alias").unwrap_or_default(),
                        provider_id: row.try_get("provider_id").unwrap_or_default(),
                        api_key,
                        base_url: row.try_get("base_url").ok(),
                        anthropic_base_url: row.try_get("anthropic_base_url").ok(),
                        is_active: row
                            .try_get::<i64, _>("is_active")
                            .unwrap_or(1),
                        weight: row.try_get("weight").unwrap_or(1),
                    }],
                    Err(_) => vec![],
                }
            }
            Ok(None) => vec![],
            Err(_) => vec![],
        }
    } else {
        match get_active_accounts(
            &state.pool,
            Some(effective_provider),
            &state.master_key,
        )
        .await
        {
            Ok(a) => a,
            Err(e) => {
                return crate::error::simple_error(
                    format!("Failed to get accounts: {e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        }
    };

    let Some(account) = accounts.first() else {
        return Json(json!({
            "success": false,
            "error": format!("No active account found for provider {}", effective_provider)
        }))
        .into_response();
    };

    let provider_type = {
        let pt =
            sqlx::query_scalar::<_, Option<String>>("SELECT type FROM providers WHERE id = ?")
                .bind(&account.provider_id)
                .fetch_optional(&state.pool)
                .await
                .ok()
                .flatten()
                .flatten();
        resolve_provider_type(pt.as_deref(), &account.provider_id)
    };

    let (url, headers, req_body) = match provider_type.as_str() {
        "anthropic" => {
            let base = account.anthropic_base_url.as_deref().unwrap_or(
                account
                    .base_url
                    .as_deref()
                    .unwrap_or("https://api.anthropic.com/v1"),
            );
            let url = format!("{}/messages", base.trim_end_matches('/'));
            let mut headers = std::collections::BTreeMap::new();
            headers.insert("x-api-key".to_string(), account.api_key.clone());
            headers.insert("anthropic-version".to_string(), "2023-06-01".to_string());
            headers.insert("content-type".to_string(), "application/json".to_string());
            let body = json!({
                "model": model_name,
                "max_tokens": 50,
                "messages": [{"role": "user", "content": "Say exactly: OK"}]
            });
            (url, headers, body)
        }
        "gemini" => {
            let base = account.base_url.as_deref().unwrap_or(
                "https://generativelanguage.googleapis.com/v1beta",
            );
            let model_id = if model_name.starts_with("models/") {
                model_name.to_string()
            } else {
                format!("models/{}", model_name)
            };
            let url = format!(
                "{}/{}:generateContent?key={}",
                base.trim_end_matches('/'),
                model_id,
                account.api_key
            );
            let mut headers = std::collections::BTreeMap::new();
            headers.insert("content-type".to_string(), "application/json".to_string());
            let body = json!({
                "contents": [{"parts": [{"text": "Say exactly: OK"}]}]
            });
            (url, headers, body)
        }
        _ => {
            // OpenAI and custom providers
            let base = account
                .base_url
                .as_deref()
                .unwrap_or("https://api.openai.com/v1");
            let url = format!("{}/chat/completions", base.trim_end_matches('/'));
            let mut headers = std::collections::BTreeMap::new();
            headers.insert(
                "authorization".to_string(),
                format!("Bearer {}", account.api_key),
            );
            headers.insert("content-type".to_string(), "application/json".to_string());
            let body = json!({
                "model": model_name,
                "messages": [{"role": "user", "content": "Say exactly: OK"}],
                "max_tokens": 50
            });
            (url, headers, body)
        }
    };

    let start = std::time::Instant::now();
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return Json(json!({
                "success": false,
                "error": format!("Failed to create HTTP client: {e}")
            }))
            .into_response();
        }
    };

    let mut req = client.post(&url);
    for (k, v) in &headers {
        req = req.header(k.as_str(), v.as_str());
    }

    let response = match req.json(&req_body).send().await {
        Ok(r) => r,
        Err(e) => {
            return Json(json!({
                "success": false,
                "error": format!("Request failed: {e}")
            }))
            .into_response();
        }
    };

    let latency_ms = start.elapsed().as_millis() as i64;
    let status = response.status();
    let body_text = response.text().await.unwrap_or_default();
    let response_json: Value = serde_json::from_str(&body_text).unwrap_or(Value::Null);

    let success = status.is_success();
    let error_msg = if success {
        None
    } else {
        response_json
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .map(String::from)
            .or_else(|| Some(body_text.clone()))
    };

    if success {
        tracing::info!(
            "🧪 {} | {} | {} | {}ms | OK",
            model_name,
            account.alias,
            effective_provider,
            latency_ms
        );
    } else {
        tracing::warn!(
            "🧪 {} | {} | {} | {}ms | FAILED: {}",
            model_name,
            account.alias,
            effective_provider,
            latency_ms,
            error_msg.as_deref().unwrap_or("unknown error")
        );
    }

    Json(json!({
        "success": success,
        "latency": latency_ms,
        "status": status.as_u16(),
        "response": if success { response_json } else { Value::Null },
        "error": error_msg,
    }))
    .into_response()
}
