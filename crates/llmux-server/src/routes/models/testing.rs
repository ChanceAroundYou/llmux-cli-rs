use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde_json::{json, Value};
use sqlx::Row;

use llmux_core::crypto::decrypt_api_key;
use llmux_core::dispatcher::{get_active_accounts, resolve_model, resolve_provider_type, ModelResolution};
use llmux_core::proxy::build_anthropic_target_url;

use crate::app::AppState;

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
            let account_id_override = model_entry.get("accountId").and_then(|v| v.as_i64());
            let provider_id_override = model_entry
                .get("providerId")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());

            // 若前端已指定 accountId，直接定向到该账户，避免同名模型串到 provider 的首账户
            let targeted_accounts: Option<Vec<llmux_core::adapters::Account>> = if let Some(acc_id) = account_id_override {
                match sqlx::query(
                    "SELECT id, alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight, openai_compatible FROM accounts WHERE id = ? AND is_active = 1",
                )
                .bind(acc_id)
                .fetch_optional(&pool)
                .await
                {
                    Ok(Some(row)) => {
                        let enc: String = row.try_get("api_key").unwrap_or_default();
                        match decrypt_api_key(&enc, &master_key) {
                            Ok(api_key) => Some(vec![llmux_core::adapters::Account {
                                id: row.try_get("id").unwrap_or_default(),
                                alias: row.try_get("alias").unwrap_or_default(),
                                provider_id: row.try_get("provider_id").unwrap_or_default(),
                                api_key,
                                base_url: row.try_get("base_url").ok(),
                                anthropic_base_url: row.try_get("anthropic_base_url").ok(),
                                is_active: row.try_get::<i64, _>("is_active").unwrap_or(1),
                                weight: row.try_get("weight").unwrap_or(1),
                                openai_compatible: row.try_get("openai_compatible").unwrap_or(0),
                                chat_endpoint: row.try_get("chat_endpoint").ok(),
                                responses_endpoint: row.try_get("responses_endpoint").ok(),
                                messages_endpoint: row.try_get("messages_endpoint").ok(),
                                default_protocol: row.try_get("default_protocol").ok(),
                            }]),
                            Err(_) => Some(vec![]),
                        }
                    }
                    Ok(None) => Some(vec![]),
                    Err(_) => Some(vec![]),
                }
            } else { None };

            // Resolve provider and get accounts
            // Try resolve_model first (by alias), fall back to providerId, then prefix guess
            let resolution = resolve_model(&pool, model_name).await.unwrap_or_else(|_| {
                ModelResolution {
                    provider_id: provider_id_override.unwrap_or("openai").to_string(),
                    target_model: model_name.to_string(),
                    account_ids: vec![],
                    preferred_account_id: None,
                    alias_name: None,
                    upstream_api: Default::default(),
                }
            });

            // Override resolved provider with explicit providerId when resolution guessed wrong
            let effective_provider = if resolution.provider_id == "openai" || resolution.provider_id == "gemini" || resolution.provider_id == "anthropic" {
                provider_id_override.unwrap_or(&resolution.provider_id)
            } else {
                &resolution.provider_id
            };

            let accounts_for_test: Vec<llmux_core::adapters::Account> = if let Some(v) = targeted_accounts {
                v
            } else if let Ok(acs) = get_active_accounts(&pool, Some(effective_provider), &master_key).await {
                acs
            } else { vec![] };
            if let Some(account) = accounts_for_test.first() {
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
                                let url = build_anthropic_target_url(base);
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
                                let base = account.base_url.as_deref().filter(|u| !u.is_empty()).unwrap_or(
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
                                // OpenAI and custom. Accounts with a valid
                                // anthropic_base_url are served via the Anthropic
                                // Messages endpoint in real traffic (mirrors the
                                // /v1/messages passthrough routing) — e.g. GitHub
                                // Copilot's gateway rejects GPT-5.x models on
                                // /chat/completions but serves them on /v1/messages.
                                if let Some(anthropic_base) = account
                                    .anthropic_base_url
                                    .as_deref()
                                    .map(str::trim)
                                    .filter(|u| !u.is_empty())
                                {
                                    let url = build_anthropic_target_url(anthropic_base);
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
                                } else {
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
                        openai_compatible: row.try_get("openai_compatible").unwrap_or(0),
                        chat_endpoint: row.try_get("chat_endpoint").ok(),
                        responses_endpoint: row.try_get("responses_endpoint").ok(),
                        messages_endpoint: row.try_get("messages_endpoint").ok(),
                        default_protocol: row.try_get("default_protocol").ok(),
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
            let url = build_anthropic_target_url(base);
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
            let base = account.base_url.as_deref().filter(|u| !u.is_empty()).unwrap_or(
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
            // OpenAI and custom providers. Accounts with a valid
            // anthropic_base_url are probed via the Anthropic Messages endpoint
            // (mirrors the /v1/messages passthrough routing) — GitHub Copilot's
            // gateway rejects GPT-5.x models on /chat/completions but serves
            // them on /v1/messages.
            if let Some(anthropic_base) = account
                .anthropic_base_url
                .as_deref()
                .map(str::trim)
                .filter(|u| !u.is_empty())
            {
                let url = build_anthropic_target_url(anthropic_base);
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
            } else {
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
