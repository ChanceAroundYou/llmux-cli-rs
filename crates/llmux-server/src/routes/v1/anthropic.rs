use std::time::Instant;

use axum::{
    body::Body,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use futures_util::StreamExt;
use serde_json::Value;

use llmux_core::adapters::{self, execute_provider_request};
use llmux_core::dispatcher::{self, get_accounts_by_ids, get_active_accounts, is_retryable_status};
use llmux_core::proxy::{build_anthropic_passthrough_request, extract_anthropic_usage_from_sse};

use crate::app::{AppState, TuiEvent};
use crate::middleware::{self, AuthContext};

use super::helpers::{log_usage, send_tui_request};

// ---------------------------------------------------------------------------
// /v1/messages  (Anthropic Messages API) — pure passthrough only
// ---------------------------------------------------------------------------

pub async fn messages(
    Extension(state): Extension<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let is_anthropic = true;

    let model_name = dispatcher::sanitize_model_name(
        body["model"].as_str().unwrap_or_default()
    );
    if model_name.is_empty() {
        return middleware::send_error(
            "Missing required field: model",
            "invalid_request_error",
            StatusCode::BAD_REQUEST,
            true,
        );
    }
    tracing::info!("📨 model={model_name} key={}", auth.key_name);
    let anthropic_beta = headers
        .get("anthropic-beta")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // Check allowed_models
    if !middleware::is_model_allowed(&auth.allowed_models, &model_name)
    {
        return middleware::send_error(
            &format!("Model '{}' is not allowed for this API key", model_name),
            "permission_error",
            StatusCode::UNAUTHORIZED,
            is_anthropic,
        );
    }

    let streaming = body["stream"].as_bool().unwrap_or(false);

    // Resolve model
    let model_resolution = match dispatcher::resolve_model(&state.pool, &model_name).await {
        Ok(r) => r,
        Err(e) => {
            return middleware::send_error(
                &format!("Model resolution failed: {e}"),
                "server_error",
                StatusCode::INTERNAL_SERVER_ERROR,
                is_anthropic,
            );
        }
    };

    // Get accounts — prefer account_ids from alias, fall back to provider
    let accounts = if !model_resolution.account_ids.is_empty() {
        match get_accounts_by_ids(&state.pool, &model_resolution.account_ids, &state.master_key).await {
            Ok(a) => a,
            Err(e) => return middleware::send_error(&format!("Failed to load accounts: {e}"), "server_error", StatusCode::INTERNAL_SERVER_ERROR, is_anthropic),
        }
    } else {
        match get_active_accounts(&state.pool, Some(&model_resolution.vendor_id), &state.master_key).await {
            Ok(a) => a,
            Err(e) => return middleware::send_error(&format!("Failed to load accounts: {e}"), "server_error", StatusCode::INTERNAL_SERVER_ERROR, is_anthropic),
        }
    };

    // 按协议过滤：厂商声明支持 anthropic（多协议厂商如 deepseek）、显式配置了
    // anthropic_base_url（Anthropic 兼容端点）或自定义代理的账户均可服务 /v1/messages。
    let accounts: Vec<_> = accounts
        .into_iter()
        .filter(|a| a.serves_anthropic || a.custom_anthropic_base_url || a.custom_base_url)
        .collect();

    if accounts.is_empty() {
        return middleware::send_error(
            &format!("No active accounts available for model '{}'", model_resolution.target_model),
            "server_error",
            StatusCode::SERVICE_UNAVAILABLE,
            is_anthropic,
        );
    }

    let dispatch_key = model_resolution
        .alias_name
        .as_deref()
        .map(|n| format!("alias:{}", n))
        .unwrap_or_else(|| format!("vendor:{}", model_resolution.vendor_id));

    let preferred_id = model_resolution
        .preferred_account_id
        .unwrap_or_else(|| accounts.first().map(|a| a.id).unwrap_or(0));

    let (ordered_accounts, dispatch_meta) = {
        let mut router = state.dispatch_router.lock().await;
        router.select(&dispatch_key, &accounts, preferred_id)
    };

    let dispatch_tag = if dispatch_meta.is_probe {
        Some("probe".to_string())
    } else if ordered_accounts.first().map(|a| a.id) != Some(preferred_id) {
        Some("fallback".to_string())
    } else {
        None
    };

    let start = Instant::now();
    let mut last_error: Option<String> = None;

    for account in &ordered_accounts {
        // Determine the base URL for Anthropic passthrough.
        // dispatcher 已把未显式配置的 anthropic_base_url 填成厂商默认值，
        // 因此按「是否用户显式配置」决策，而不是按字段是否为空：
        // - 显式配置了 anthropic 兼容端点 → 用它
        // - 只显式配置了自定义 base_url（代理）→ anthropic 请求也走它
        // - 都没配置 → 用 dispatcher 解析出的厂商默认 anthropic 端点
        let base_url = if account.custom_anthropic_base_url {
            account.anthropic_base_url.as_deref()
        } else if account.custom_base_url {
            account.base_url.as_deref()
        } else {
            account.anthropic_base_url.as_deref()
        }
        .unwrap_or("https://api.anthropic.com/v1");

        let provider_request = match build_anthropic_passthrough_request(
            &body,
            account,
            base_url,
            &model_resolution.target_model,
            anthropic_beta.as_deref(),
        ) {
            Ok(r) => {
                tracing::info!(
                    "⚡ {} → {} → {}",
                    account.name,
                    model_resolution.target_model,
                    base_url,
                );
                if let Some(tx) = &state.tui_tx {
                    let _ = tx.send(TuiEvent::Dispatch {
                        timestamp: time::OffsetDateTime::now_local()
                            .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
                            .format(&time::format_description::parse_borrowed::<2>("[hour]:[minute]:[second]").unwrap())
                            .unwrap_or_default(),
                        account: account.name.clone(),
                        model: model_resolution.target_model.clone(),
                        url: base_url.to_string(),
                        tag: dispatch_tag.clone(),
                    });
                }
                r
            }
            Err(e) => {
                tracing::error!("📡 Failed to build passthrough request: {e}");
                last_error = Some(format!("Failed to build passthrough request: {e}"));
                if let Some(tx) = &state.tui_tx {
                    let _ = tx.send(TuiEvent::Retry {
                        account: account.name.clone(),
                        status: 0,
                        message: format!("Build error: {e}"),
                    });
                }
                if account.id == preferred_id {
                    let mut router = state.dispatch_router.lock().await;
                    router.record_result(&dispatch_key, &dispatch_meta, None, false);
                }
                // 记录该账户失败（failover 统计完整化）
                let latency_ms = start.elapsed().as_millis() as i64;
                let _ = log_usage(
                    &state.pool,
                    account,
                    &model_resolution.target_model,
                    latency_ms,
                    false,
                    &last_error,
                )
                .await;
                continue;
            }
        };

        let response = match execute_provider_request(&provider_request).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("📡 passthrough failed: {e}");
                last_error = Some(format!("Passthrough request failed: {e}"));
                if let Some(tx) = &state.tui_tx {
                    let _ = tx.send(TuiEvent::Retry {
                        account: account.name.clone(),
                        status: 0,
                        message: format!("Network error: {e}"),
                    });
                }
                if account.id == preferred_id {
                    let mut router = state.dispatch_router.lock().await;
                    router.record_result(&dispatch_key, &dispatch_meta, None, false);
                }
                // 记录该账户失败（failover 统计完整化）
                let latency_ms = start.elapsed().as_millis() as i64;
                let _ = log_usage(
                    &state.pool,
                    account,
                    &model_resolution.target_model,
                    latency_ms,
                    false,
                    &last_error,
                )
                .await;
                continue;
            }
        };

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            last_error = Some(format!("Provider returned {status}: {error_body}"));

            if is_retryable_status(status.as_u16()) {
                tracing::warn!(
                    "🔀 Account {} (id={}) failed ({}) — trying next...",
                    account.name,
                    account.id,
                    status.as_u16()
                );
                if let Some(tx) = &state.tui_tx {
                    let _ = tx.send(TuiEvent::Retry {
                        account: account.name.clone(),
                        status: status.as_u16(),
                        message: error_body.clone(),
                    });
                }
                if account.id == preferred_id {
                    let mut router = state.dispatch_router.lock().await;
                    router.record_result(&dispatch_key, &dispatch_meta, None, false);
                }
                // 记录该账户失败（failover 统计完整化）
                let latency_ms = start.elapsed().as_millis() as i64;
                let _ = log_usage(
                    &state.pool,
                    account,
                    &model_resolution.target_model,
                    latency_ms,
                    false,
                    &last_error,
                )
                .await;
                continue;
            }

            // Non-retryable — return upstream error in Anthropic format
            let error_type = if status == StatusCode::UNAUTHORIZED {
                "authentication_error"
            } else {
                "api_error"
            };
            let message = serde_json::from_str::<Value>(&error_body)
                .ok()
                .and_then(|v| {
                    v["error"]["message"]
                        .as_str()
                        .or_else(|| v["error"].as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| error_body.clone());
            send_tui_request(&state.tui_tx, "/v1/messages", status.as_u16(), start, &model_resolution.target_model);
            return (
                status,
                Json(serde_json::json!({
                    "type": "error",
                    "error": {
                        "type": error_type,
                        "message": message,
                    }
                })),
            )
                .into_response();
        }

        // Success — streaming or non-streaming
        if streaming {
            {
                let mut router = state.dispatch_router.lock().await;
                router.record_result(&dispatch_key, &dispatch_meta, Some(account.id), true);
            }
            send_tui_request(&state.tui_tx, "/v1/messages", status.as_u16(), start, &model_resolution.target_model);
            return anthropic_streaming_passthrough(
                response,
                &model_resolution.target_model,
                account,
                state.pool.clone(),
                start,
            )
            .await;
        }

        // Non-streaming: read full body, extract usage, return as-is
        let body_bytes = match response.bytes().await {
            Ok(b) => b,
            Err(e) => {
                last_error = Some(format!("Failed to read response: {e}"));
                continue;
            }
        };

        let usage =
            extract_anthropic_usage_from_sse(&String::from_utf8_lossy(&body_bytes));
        tracing::info!(
            account = account.id,
            model = %model_resolution.target_model,
            input = usage.input_tokens,
            output = usage.output_tokens,
            cache_read = usage.cache_read_input_tokens,
            cache_create = usage.cache_creation_input_tokens,
            "📊 account={} model={} input={} cacheRead={} cacheCreate={} output={}",
            account.id,
            model_resolution.target_model,
            usage.input_tokens,
            usage.cache_read_input_tokens,
            usage.cache_creation_input_tokens,
            usage.output_tokens,
        );
        let latency_ms = start.elapsed().as_millis() as i64;
        let _ = log_usage(
            &state.pool,
            account,
            &model_resolution.target_model,
            latency_ms,
            true,
            &None,
        )
        .await;

        let data: Value = match serde_json::from_slice(&body_bytes) {
            Ok(v) => v,
            Err(_) => {
                return middleware::send_error(
                    "Invalid JSON in passthrough response",
                    "server_error",
                    StatusCode::INTERNAL_SERVER_ERROR,
                    true,
                );
            }
        };

        {
            let mut router = state.dispatch_router.lock().await;
            router.record_result(&dispatch_key, &dispatch_meta, Some(account.id), true);
        }
        send_tui_request(&state.tui_tx, "/v1/messages", 200, start, &model_resolution.target_model);
        return Json(data).into_response();
    }

    // All accounts exhausted
    {
        let mut router = state.dispatch_router.lock().await;
        router.record_result(&dispatch_key, &dispatch_meta, None, false);
    }

    let error_msg = last_error.unwrap_or_else(|| "All accounts exhausted".to_string());
    // 各账户失败已在循环内逐条记录，这里不重复记 first
    send_tui_request(&state.tui_tx, "/v1/messages", 502, start, &model_resolution.target_model);
    middleware::send_error(&error_msg, "upstream_error", StatusCode::BAD_GATEWAY, is_anthropic)
}

/// Passthrough streaming for Anthropic responses.
/// Bytes are forwarded as-is. Usage is not captured (requires SSE parsing —
/// a future improvement would parse the stream inline like Bun's wrapStreamWithUsage).
async fn anthropic_streaming_passthrough(
    response: reqwest::Response,
    model: &str,
    account: &adapters::Account,
    pool: sqlx::SqlitePool,
    start: Instant,
) -> Response {
    let model = model.to_string();
    let account = account.clone();

    tracing::info!(
        "⚡ streaming {} → {}",
        account.name,
        model,
    );

    let stream = response.bytes_stream().map(move |chunk| {
        chunk.map_err(axum::Error::new)
    });

    let pool_clone = pool.clone();
    tokio::spawn(async move {
        // 流式已返回 200：记成功 usage，latency 为上游返回首字节前的真实耗时（不固定 1s）
        let latency_ms = start.elapsed().as_millis() as i64;
        let _ = log_usage(
            &pool_clone,
            &account,
            &model,
            latency_ms,
            true,
            &None,
        )
        .await;
    });

    let body = Body::from_stream(stream);
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .body(body)
        .unwrap()
        .into_response()
}
