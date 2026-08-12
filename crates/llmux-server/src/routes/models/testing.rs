use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde_json::{json, Value};

use llmux_core::dispatcher::{
    get_accounts_by_ids, get_active_accounts, resolve_model, ModelResolution,
};
use llmux_core::repo;

use crate::app::AppState;

#[utoipa::path(
    get,
    path = "/api/models/test-queue/status",
    responses(
        (status = 200, description = "模型批量测试队列状态（isRunning/total/current/progress）", body = crate::api_schemas::TestQueueStatus)
    )
)]
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

#[utoipa::path(
    post,
    path = "/api/models/test-all",
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "启动全量模型批量测试队列", body = crate::api_schemas::QueueStartResponse),
        (status = 400, description = "models 必须为数组", body = crate::api_schemas::ErrorResponse)
    )
)]
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
                .get("vendorId")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());

            // Resolve provider and get accounts
            // Try resolve_model first (by alias), fall back to providerId, then prefix guess
            let resolution = resolve_model(&pool, model_name).await.unwrap_or_else(|_| {
                ModelResolution {
                    vendor_id: provider_id_override.unwrap_or("openai").to_string(),
                    target_model: model_name.to_string(),
                    account_ids: vec![],
                    preferred_account_id: None,
                    alias_name: None,
                }
            });

            // Override resolved vendor with explicit vendorId when resolution guessed wrong
            let effective_vendor = if resolution.vendor_id == "openai"
                || resolution.vendor_id == "gemini"
                || resolution.vendor_id == "anthropic"
            {
                provider_id_override.unwrap_or(&resolution.vendor_id)
            } else {
                &resolution.vendor_id
            };

            if let Ok(accounts) =
                get_active_accounts(&pool, Some(effective_vendor), &master_key).await
            {
                if let Some(account) = accounts.first() {
                        // 协议直接取厂商 protocol
                        let provider_type = account.protocol.clone();

                        let (url, headers, body) = match provider_type.as_str() {
                            "anthropic" => {
                                // 与 v1/anthropic.rs 一致：按「是否显式配置」决策，避免 COALESCE 默认值吃掉代理 base_url。
                                let base = if account.custom_anthropic_base_url {
                                    account.anthropic_base_url.as_deref()
                                } else if account.custom_base_url {
                                    account.base_url.as_deref()
                                } else {
                                    account.anthropic_base_url.as_deref()
                                }
                                .unwrap_or("https://api.anthropic.com/v1");
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
                        // 拨测结果持久化到 model_health（健康页/别名卡展示；不入 usage_logs 以免污染真实请求监控）
                        if let Err(e) = repo::record_model_health(
                            &pool,
                            account.id,
                            model_name,
                            test_success,
                            latency_ms,
                            None,
                        )
                        .await
                        {
                            tracing::debug!("🧪 record_model_health failed: {e}");
                        }
                        tracing::debug!(
                            "🧪 test {} via {}: success={} {}ms",
                            model_name,
                            account.name,
                            test_success,
                            latency_ms
                        );
                    }
                }

            {
                let mut queue = queue_state.lock().unwrap();
                queue.current = i + 1;
                queue.progress = ((i + 1) * 100).checked_div(queue.total).unwrap_or(0);
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

#[utoipa::path(
    post,
    path = "/api/models/test",
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "单模型连通性测试结果", body = crate::api_schemas::ModelTestResponse),
        (status = 400, description = "缺少 model", body = crate::api_schemas::ErrorResponse)
    )
)]
pub async fn test_model(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let Some(model_name) = body.get("model").and_then(Value::as_str) else {
        return crate::error::simple_error("No model provided", StatusCode::BAD_REQUEST);
    };

    let provider_id_override = body
        .get("vendorId")
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

    // Use vendorId override if provided (matching Bun behavior)
    let effective_vendor = provider_id_override
        .as_deref()
        .unwrap_or(&resolution.vendor_id);

    let accounts = if let Some(acc_id) = account_id_override {
        // Directly fetch the specified account（enabled 过滤与 decrypt 由 get_accounts_by_ids 处理）
        get_accounts_by_ids(&state.pool, &[acc_id], &state.master_key)
            .await
            .unwrap_or_default()
    } else {
        match get_active_accounts(&state.pool, Some(effective_vendor), &state.master_key).await {
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
            "error": format!("No active account found for vendor {}", effective_vendor)
        }))
        .into_response();
    };

    // 协议直接取厂商 protocol
    let provider_type = account.protocol.clone();

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

    // 拨测结果持久化到 model_health（健康页/别名卡展示；不入 usage_logs 以免污染真实请求监控）
    if let Err(e) = repo::record_model_health(
        &state.pool,
        account.id,
        model_name,
        success,
        latency_ms,
        error_msg.as_deref(),
    )
    .await
    {
        tracing::debug!("🧪 record_model_health failed: {e}");
    }

    if success {
        tracing::info!(
            "🧪 {} | {} | {} | {}ms | OK",
            model_name,
            account.name,
            effective_vendor,
            latency_ms
        );
    } else {
        tracing::warn!(
            "🧪 {} | {} | {} | {}ms | FAILED: {}",
            model_name,
            account.name,
            effective_vendor,
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
