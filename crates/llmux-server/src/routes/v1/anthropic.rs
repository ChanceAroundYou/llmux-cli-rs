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
use llmux_core::dispatcher::{self, get_accounts_by_ids, get_active_accounts, is_retryable_status, select_accounts_for_dispatch};
use llmux_core::proxy::{build_anthropic_passthrough_request, extract_anthropic_usage_from_sse};

use crate::app::AppState;
use crate::middleware::{self, AuthContext};

use super::helpers::log_usage;

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
        match get_active_accounts(&state.pool, Some(&model_resolution.provider_id), &state.master_key).await {
            Ok(a) => a,
            Err(e) => return middleware::send_error(&format!("Failed to load accounts: {e}"), "server_error", StatusCode::INTERNAL_SERVER_ERROR, is_anthropic),
        }
    };

    if accounts.is_empty() {
        return middleware::send_error(
            &format!("No active accounts available for model '{}'", model_resolution.target_model),
            "server_error",
            StatusCode::SERVICE_UNAVAILABLE,
            is_anthropic,
        );
    }

    let ordered_accounts = {
        let mut ds = state.dispatcher_state.lock().unwrap();
        select_accounts_for_dispatch(&accounts, &model_resolution.provider_id, &mut ds)
    };

    let start = Instant::now();
    let mut last_error: Option<String> = None;

    for account in &ordered_accounts {
        // Determine the base URL for Anthropic passthrough.
        // Prefer anthropic_base_url; fall back to base_url if anthropic_base_url is not set.
        let base_url = account
            .anthropic_base_url
            .as_deref()
            .or(account.base_url.as_deref())
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
                    account.alias,
                    model_resolution.target_model,
                    base_url,
                );
                r
            }
            Err(e) => {
                tracing::error!("📡 Failed to build passthrough request: {e}");
                last_error = Some(format!("Failed to build passthrough request: {e}"));
                continue;
            }
        };

        let response = match execute_provider_request(&provider_request).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("📡 passthrough failed: {e}");
                last_error = Some(format!("Passthrough request failed: {e}"));
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
                    account.alias,
                    account.id,
                    status.as_u16()
                );
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
            return anthropic_streaming_passthrough(
                response,
                &model_resolution.target_model,
                account,
                state.pool.clone(),
                &model_resolution.provider_id,
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
            &model_resolution.provider_id,
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_read_input_tokens,
            usage.cache_creation_input_tokens,
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

        return Json(data).into_response();
    }

    // All accounts exhausted
    let latency_ms = start.elapsed().as_millis() as i64;
    let error_msg = last_error.unwrap_or_else(|| "All accounts exhausted".to_string());
    if let Some(account) = ordered_accounts.first() {
        let _ = log_usage(
            &state.pool,
            account,
            &model_resolution.target_model,
            &model_resolution.provider_id,
            0,
            0,
            0,
            0,
            latency_ms,
            false,
            &Some(error_msg.clone()),
        )
        .await;
    }
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
    provider_id: &str,
    start: Instant,
) -> Response {
    let model = model.to_string();
    let account = account.clone();
    let provider_id = provider_id.to_string();

    tracing::info!(
        "⚡ streaming {} → {}",
        account.alias,
        model,
    );

    let stream = response.bytes_stream().map(move |chunk| {
        chunk.map_err(axum::Error::new)
    });

    let pool_clone = pool.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let latency_ms = start.elapsed().as_millis() as i64;
        let _ = log_usage(
            &pool_clone,
            &account,
            &model,
            &provider_id,
            0,
            0,
            0,
            0,
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
