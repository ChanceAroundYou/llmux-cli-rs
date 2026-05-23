use std::collections::BTreeMap;
use std::time::Instant;

use axum::{
    body::Body,
    extract::OriginalUri,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use futures_util::StreamExt;
use serde_json::Value;

use llmux_core::adapters::{
    self, execute_provider_request, ProviderRequest,
};
use llmux_core::dispatcher::{
    self, get_accounts_by_ids, get_active_accounts, is_retryable_status,
    select_accounts_for_dispatch,
};
use llmux_core::proxy::{build_anthropic_passthrough_request, extract_anthropic_usage_from_sse};

use crate::app::AppState;
use crate::middleware::{self, AuthContext};

// ---------------------------------------------------------------------------
// /v1/chat/completions  — pure OpenAI-compatible passthrough
// ---------------------------------------------------------------------------

pub async fn chat_completions(
    Extension(state): Extension<AppState>,
    Extension(auth): Extension<AuthContext>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    openai_dispatch(state, auth, uri, headers, body, "chat/completions").await
}

/// POST /v1/responses — OpenAI Responses API pure passthrough
pub async fn responses(
    Extension(state): Extension<AppState>,
    Extension(auth): Extension<AuthContext>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    openai_dispatch(state, auth, uri, headers, body, "responses").await
}

/// Shared OpenAI-protocol passthrough dispatcher.
/// `endpoint` is the URL path segment appended to the provider's base URL
/// (e.g. "chat/completions" or "responses").
async fn openai_dispatch(
    state: AppState,
    auth: AuthContext,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: Value,
    endpoint: &str,
) -> Response {
    let normalized_uri = crate::app::normalize_gateway_uri(&uri);
    let is_anthropic =
        headers.contains_key("x-api-key") || normalized_uri.path().ends_with("/messages");

    // Extract model and stream from the JSON body without requiring a
    // specific schema — the Responses API has different fields ("input"
    // instead of "messages") so we can't deserialize as ChatRequest.
    let model_name = match body.get("model").and_then(Value::as_str) {
        Some(name) => {
            let sanitized = dispatcher::sanitize_model_name(name);
            if sanitized.is_empty() {
                return middleware::send_error(
                    "Missing required field: model",
                    "invalid_request_error",
                    StatusCode::BAD_REQUEST,
                    is_anthropic,
                );
            }
            sanitized
        }
        None => {
            return middleware::send_error(
                "Missing required field: model",
                "invalid_request_error",
                StatusCode::BAD_REQUEST,
                is_anthropic,
            );
        }
    };

    if !middleware::is_model_allowed(&auth.allowed_models, &model_name) {
        return middleware::send_error(
            &format!("Model '{}' is not allowed for this API key", model_name),
            "permission_error",
            StatusCode::UNAUTHORIZED,
            is_anthropic,
        );
    }

    let streaming = body.get("stream").and_then(Value::as_bool).unwrap_or(false);

    let model_resolution =
        match dispatcher::resolve_model(&state.pool, &model_name).await {
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

    // Patch the model field in the body to the resolved target model.
    let mut patched_body = body;
    patched_body["model"] = serde_json::Value::String(model_resolution.target_model.clone());

    let start = Instant::now();
    let mut last_error: Option<String> = None;

    for account in &ordered_accounts {
        // Build passthrough request: same body, just add auth header + endpoint path.
        let base_url = normalize_base_url(
            account
                .base_url
                .as_deref()
                .unwrap_or("https://api.openai.com/v1"),
        );
        let mut req_headers = BTreeMap::from([(
            "content-type".to_string(),
            "application/json".to_string(),
        )]);
        req_headers.insert(
            "authorization".to_string(),
            format!("Bearer {}", account.api_key),
        );
        let provider_request = ProviderRequest {
            method: "POST".to_string(),
            url: format!("{base_url}/{endpoint}"),
            headers: req_headers,
            body: patched_body.clone(),
        };

        tracing::debug!(
            account = %account.alias,
            provider = %account.provider_id,
            model = %model_resolution.target_model,
            url = %provider_request.url,
            "[Gateway] Dispatching request to {endpoint}"
        );

        let response = match execute_provider_request(&provider_request).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(
                    "[Dispatcher] Account {} (id={}) request failed: {e}",
                    account.alias,
                    account.id
                );
                last_error = Some(format!("Provider request failed: {e}"));
                continue;
            }
        };

        let status = response.status();

        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            last_error = Some(format!("Provider returned {status}: {error_body}"));

            if is_retryable_status(status.as_u16()) {
                tracing::warn!(
                    "[Dispatcher] Account {} (id={}) failed with status {}. Trying next...",
                    account.alias,
                    account.id,
                    status.as_u16()
                );
                continue;
            }
            let latency_ms = start.elapsed().as_millis() as i64;
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
                &last_error,
            )
            .await;
            if let Ok(json_val) = serde_json::from_str::<Value>(&error_body) {
                return (status, Json(json_val)).into_response();
            }
            return (status, error_body).into_response();
        }

        if streaming {
            return openai_streaming_passthrough(
                response,
                &model_resolution.target_model,
                account,
                state.pool.clone(),
                start,
            )
            .await;
        }

        let body_bytes = match response.bytes().await {
            Ok(b) => b,
            Err(e) => {
                last_error = Some(format!("Failed to read response: {e}"));
                continue;
            }
        };

        let data: Value = match serde_json::from_slice(&body_bytes) {
            Ok(v) => v,
            Err(e) => {
                last_error = Some(format!("Failed to parse response: {e}"));
                continue;
            }
        };

        let (prompt_tokens, completion_tokens) =
            adapters::usage_from_openai_response_body(&data);
        let latency_ms = start.elapsed().as_millis() as i64;
        let _ = log_usage(
            &state.pool,
            account,
            &model_resolution.target_model,
            &model_resolution.provider_id,
            prompt_tokens,
            completion_tokens,
            0,
            0,
            latency_ms,
            true,
            &None,
        )
        .await;

        return Json(data).into_response();
    }

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
    middleware::send_error(
        &error_msg,
        "upstream_error",
        StatusCode::BAD_GATEWAY,
        is_anthropic,
    )
}

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
    tracing::info!("[v1/messages] model={model_name} key={}", auth.key_name);
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
                    url = %r.url,
                    model = %model_resolution.target_model,
                    account = %account.alias,
                    "[Proxy] passthrough -> {} (model=\"{}\")",
                    r.url,
                    model_resolution.target_model
                );
                r
            }
            Err(e) => {
                tracing::error!("[Proxy] Failed to build passthrough request: {e}");
                last_error = Some(format!("Failed to build passthrough request: {e}"));
                continue;
            }
        };

        let response = match execute_provider_request(&provider_request).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("[Proxy] passthrough failed: {e}");
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
                    "[Dispatcher] Account {} (id={}) failed with status {}. Trying next...",
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
            "[Proxy][Usage][JSON] account={} model={} input={} cacheRead={} cacheCreate={} output={}",
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

// ---------------------------------------------------------------------------
// /v1beta/models/{model}:{action}  — Gemini native protocol passthrough
// ---------------------------------------------------------------------------

pub async fn gemini(
    Extension(state): Extension<AppState>,
    Extension(auth): Extension<AuthContext>,
    OriginalUri(uri): OriginalUri,
    _headers: HeaderMap,
    axum::extract::Path(path): axum::extract::Path<String>,
    Json(body): Json<Value>,
) -> Response {
    // Parse the captured path segment: "gemini-2.0-flash:generateContent"
    // Extract model name and action.
    let path = path.trim_start_matches('/');
    let (raw_model, _action) = if path.starts_with("models/") {
        let rest = &path["models/".len()..];
        rest.split_once(':').unwrap_or((rest, "generateContent"))
    } else {
        // Allow direct "model:action" without the models/ prefix
        path.split_once(':').unwrap_or((path, "generateContent"))
    };
    let model_name = dispatcher::sanitize_model_name(raw_model);

    if model_name.is_empty() {
        return middleware::send_error(
            "Missing model name in URL path",
            "invalid_request_error",
            StatusCode::BAD_REQUEST,
            false,
        );
    }

    // Check allowed_models
    if !middleware::is_model_allowed(&auth.allowed_models, &model_name) {
        return middleware::send_error(
            &format!("Model '{}' is not allowed for this API key", model_name),
            "permission_error",
            StatusCode::UNAUTHORIZED,
            false,
        );
    }

    // Resolve model alias
    let model_resolution = match dispatcher::resolve_model(&state.pool, &model_name).await {
        Ok(r) => r,
        Err(e) => {
            return middleware::send_error(
                &format!("Model resolution failed: {e}"),
                "server_error",
                StatusCode::INTERNAL_SERVER_ERROR,
                false,
            );
        }
    };

    // Get accounts — prefer account_ids from alias, fall back to provider
    let accounts = if !model_resolution.account_ids.is_empty() {
        match get_accounts_by_ids(&state.pool, &model_resolution.account_ids, &state.master_key).await {
            Ok(a) => a,
            Err(e) => return middleware::send_error(&format!("Failed to load accounts: {e}"), "server_error", StatusCode::INTERNAL_SERVER_ERROR, false),
        }
    } else {
        match get_active_accounts(&state.pool, Some(&model_resolution.provider_id), &state.master_key).await {
            Ok(a) => a,
            Err(e) => return middleware::send_error(&format!("Failed to load accounts: {e}"), "server_error", StatusCode::INTERNAL_SERVER_ERROR, false),
        }
    };

    if accounts.is_empty() {
        return middleware::send_error(
            &format!("No active accounts available for model '{}'", model_resolution.target_model),
            "server_error",
            StatusCode::SERVICE_UNAVAILABLE,
            false,
        );
    }

    let ordered_accounts = {
        let mut ds = state.dispatcher_state.lock().unwrap();
        select_accounts_for_dispatch(&accounts, &model_resolution.provider_id, &mut ds)
    };

    let start = Instant::now();
    let mut last_error: Option<String> = None;

    for account in &ordered_accounts {
        let base_url = normalize_base_url(
            account
                .base_url
                .as_deref()
                .unwrap_or("https://generativelanguage.googleapis.com/v1beta"),
        );

        // Rebuild URL with resolved model and API key
        let target_model = if model_resolution.target_model.starts_with("models/") {
            model_resolution.target_model.clone()
        } else {
            format!("models/{}", model_resolution.target_model)
        };
        let new_path = format!("{target_model}:{_action}");

        // Preserve query params from the original URI, add API key
        let query: String = uri
            .query()
            .map(|q| format!("{q}&key={}", account.api_key))
            .unwrap_or_else(|| format!("key={}", account.api_key));
        let url = format!("{base_url}/{new_path}?{query}");

        let provider_request = ProviderRequest {
            method: "POST".to_string(),
            url,
            headers: BTreeMap::from([(
                "content-type".to_string(),
                "application/json".to_string(),
            )]),
            body: body.clone(),
        };

        tracing::debug!(
            account = %account.alias,
            provider = "gemini",
            model = %model_resolution.target_model,
            url = %provider_request.url,
            "[Gateway] Gemini passthrough dispatching"
        );

        let response = match execute_provider_request(&provider_request).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(
                    "[Dispatcher] Account {} (id={}) request failed: {e}",
                    account.alias,
                    account.id
                );
                last_error = Some(format!("Provider request failed: {e}"));
                continue;
            }
        };

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            last_error = Some(format!("Provider returned {status}: {error_body}"));

            if is_retryable_status(status.as_u16()) {
                tracing::warn!(
                    "[Dispatcher] Account {} (id={}) failed with status {}. Trying next...",
                    account.alias,
                    account.id,
                    status.as_u16()
                );
                continue;
            }
            let latency_ms = start.elapsed().as_millis() as i64;
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
                &last_error,
            )
            .await;
            return (status, Json(serde_json::from_str::<Value>(&error_body)
                .unwrap_or(Value::String(error_body))))
                .into_response();
        }

        // Check content-type — Gemini returns JSON or SSE depending on ?alt=sse
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/json");
        let is_stream = content_type.contains("text/event-stream");

        if is_stream {
            return gemini_streaming_passthrough(
                response,
                &model_resolution.target_model,
                account,
                state.pool.clone(),
                &model_resolution.provider_id,
                start,
            )
            .await;
        }

        // Non-streaming: read body, extract usage, return as-is
        let body_bytes = match response.bytes().await {
            Ok(b) => b,
            Err(e) => {
                last_error = Some(format!("Failed to read response: {e}"));
                continue;
            }
        };

        let data: Value = match serde_json::from_slice(&body_bytes) {
            Ok(v) => v,
            Err(e) => {
                last_error = Some(format!("Failed to parse response: {e}"));
                continue;
            }
        };

        // Extract usage from Gemini format: usageMetadata.{promptTokenCount, candidatesTokenCount}
        let (prompt_tokens, completion_tokens) = gemini_usage(&data);
        let latency_ms = start.elapsed().as_millis() as i64;
        let _ = log_usage(
            &state.pool,
            account,
            &model_resolution.target_model,
            &model_resolution.provider_id,
            prompt_tokens,
            completion_tokens,
            0,
            0,
            latency_ms,
            true,
            &None,
        )
        .await;

        return Json(data).into_response();
    }

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
    middleware::send_error(
        &error_msg,
        "upstream_error",
        StatusCode::BAD_GATEWAY,
        false,
    )
}

/// Extract token counts from Gemini response format.
fn gemini_usage(data: &Value) -> (i64, i64) {
    let meta = &data["usageMetadata"];
    (
        meta["promptTokenCount"].as_i64().unwrap_or_default(),
        meta["candidatesTokenCount"]
            .as_i64()
            .or_else(|| meta["totalTokenCount"].as_i64().map(|t| {
                let prompt = meta["promptTokenCount"].as_i64().unwrap_or_default();
                t.saturating_sub(prompt)
            }))
            .unwrap_or_default(),
    )
}

/// Passthrough streaming for Gemini responses (SSE).
async fn gemini_streaming_passthrough(
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
        account_id = account.id,
        model = %model,
        provider = %provider_id,
        "Gemini streaming: per-request token usage is not captured (requires stream parsing)",
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

// ---------------------------------------------------------------------------
// /v1/models
// ---------------------------------------------------------------------------

pub async fn models(Extension(state): Extension<AppState>, headers: HeaderMap) -> Response {
    tracing::info!("[v1/models] Request received");
    let is_anthropic =
        headers.contains_key("x-api-key") || headers.contains_key("anthropic-version");

    let alias_names: Vec<String> = match sqlx::query_scalar::<_, String>(
        "SELECT alias FROM model_aliases",
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            return middleware::send_error(
                &format!("Failed to load models: {e}"),
                "server_error",
                StatusCode::INTERNAL_SERVER_ERROR,
                is_anthropic,
            );
        }
    };

    if is_anthropic {
        let created_at = iso8601_now();
        let data: Vec<Value> = alias_names
            .iter()
            .map(|alias| {
                serde_json::json!({
                    "type": "model",
                    "id": alias,
                    "display_name": alias,
                    "created_at": created_at,
                })
            })
            .collect();
        let first_id = data
            .first()
            .and_then(|m| m["id"].as_str().map(str::to_string));
        let last_id = data
            .last()
            .and_then(|m| m["id"].as_str().map(str::to_string));
        return Json(serde_json::json!({
            "data": data,
            "has_more": false,
            "first_id": first_id,
            "last_id": last_id,
        }))
        .into_response();
    }

    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let data: Vec<Value> = alias_names
        .into_iter()
        .map(|alias| {
            serde_json::json!({
                "id": alias,
                "object": "model",
                "created": created,
                "owned_by": "llmux",
            })
        })
        .collect();

    Json(serde_json::json!({
        "object": "list",
        "data": data
    }))
    .into_response()
}

// ---------------------------------------------------------------------------
// Streaming helpers
// ---------------------------------------------------------------------------

/// Passthrough streaming for OpenAI-compatible responses.
/// Bytes are forwarded as-is (SSE). Usage is logged asynchronously with
/// estimated tokens since we don't parse the stream.
async fn openai_streaming_passthrough(
    response: reqwest::Response,
    model: &str,
    account: &adapters::Account,
    pool: sqlx::SqlitePool,
    start: Instant,
) -> Response {
    let model = model.to_string();
    let account = account.clone();

    let stream = response.bytes_stream().map(move |chunk| {
        chunk.map_err(|err| {
            tracing::warn!("upstream stream read error: {err}");
            axum::Error::new(err)
        })
    });

    let pool_clone = pool.clone();
    let model_clone = model.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let latency_ms = start.elapsed().as_millis() as i64;
        let _ = log_usage(
            &pool_clone,
            &account,
            &model_clone,
            &account.provider_id,
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
        account_id = account.id,
        model = %model,
        provider = %provider_id,
        "Passthrough streaming: per-request token usage is not captured (requires SSE parsing)",
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

fn normalize_base_url(value: &str) -> String {
    value.trim_end_matches('/').to_string()
}

// ---------------------------------------------------------------------------
// ISO 8601 timestamp (UTC, ms precision)
// ---------------------------------------------------------------------------

fn iso8601_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let ts = dur.as_secs() as i64;
    let ms = dur.subsec_millis();

    // Howard Hinnant's civil-from-days algorithm
    let days = ts / 86400;
    let sec_of_day = (ts % 86400) as u32;
    let h = sec_of_day / 3600;
    let m = (sec_of_day % 3600) / 60;
    let s = sec_of_day % 60;

    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 {
        (mp + 3) as u32
    } else {
        (mp - 9) as u32
    };
    let year = if month <= 2 { y + 1 } else { y };

    format!("{year:04}-{month:02}-{d:02}T{h:02}:{m:02}:{s:02}.{ms:03}Z")
}

// ---------------------------------------------------------------------------
// Usage logging
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn log_usage(
    pool: &sqlx::SqlitePool,
    account: &adapters::Account,
    model: &str,
    provider_id: &str,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
    latency_ms: i64,
    success: bool,
    error_message: &Option<String>,
) -> anyhow::Result<()> {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;

    let result = sqlx::query(
        "INSERT INTO usage_logs (
            timestamp, account_id, provider_id, model,
            input_tokens, output_tokens,
            cache_read_input_tokens, cache_creation_input_tokens,
            latency_ms, success, error_message, is_test
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(timestamp)
    .bind(account.id)
    .bind(provider_id)
    .bind(model)
    .bind(input_tokens)
    .bind(output_tokens)
    .bind(cache_read_input_tokens)
    .bind(cache_creation_input_tokens)
    .bind(latency_ms)
    .bind(if success { 1 } else { 0 })
    .bind(error_message.as_deref())
    .bind(0)
    .execute(pool)
    .await;

    match &result {
        Err(e) => {
            tracing::error!("[UsageService] Failed to insert usage log: {e}");
            Err(anyhow::anyhow!("{e}"))
        }
        Ok(_) => Ok(()),
    }
}
