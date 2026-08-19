use std::collections::BTreeMap;
use std::time::Instant;

use axum::{
    body::Body,
    extract::OriginalUri,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use llmux_core::adapters::{self, execute_provider_request, ProviderRequest};
use llmux_core::dispatcher::{self, get_accounts_by_ids, get_active_accounts, is_retryable_status};
use llmux_core::proxy::openai_anthropic::{
    anthropic_to_openai_response, is_unsupported_api_for_model, openai_to_anthropic_request,
    AnthropicSseConverter,
};
use llmux_core::proxy::{build_anthropic_target_url, anthropic_openai::{parse_sse_chunks, sse_data_payload}};

use crate::app::{AppState, TuiEvent};
use crate::middleware::{self, AuthContext};

use super::helpers::{log_usage, normalize_base_url, send_tui_request};

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
    _headers: HeaderMap,
    body: Value,
    endpoint: &str,
) -> Response {
    let normalized_uri = crate::app::normalize_gateway_uri(&uri);
    let is_anthropic = false;

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

    // Filter: for gemini provider, only use accounts with openai_compatible enabled.
    // These will route through Gemini's OpenAI-compatible endpoint
    // (https://generativelanguage.googleapis.com/v1beta/openai).
    let accounts: Vec<_> = accounts.into_iter().filter(|a| {
        a.provider_id != "gemini" || a.openai_compatible == 1
    }).collect();

    if accounts.is_empty() {
        return middleware::send_error(
            &format!("No active accounts available for model '{}' — Gemini accounts must enable OpenAI compatible mode", model_resolution.target_model),
            "server_error",
            StatusCode::SERVICE_UNAVAILABLE,
            is_anthropic,
        );
    }

    let dispatch_key = model_resolution
        .alias_name
        .as_deref()
        .map(|n| format!("alias:{}", n))
        .unwrap_or_else(|| format!("provider:{}", model_resolution.provider_id));

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

    // Patch the model field in the body to the resolved target model.
    let mut patched_body = body;
    patched_body["model"] = serde_json::Value::String(model_resolution.target_model.clone());

    let start = Instant::now();
    let mut last_error: Option<String> = None;

    for account in &ordered_accounts {
        let default_base = if account.provider_id == "gemini" {
            "https://generativelanguage.googleapis.com/v1beta/openai"
        } else {
            "https://api.openai.com/v1"
        };
        let base_url = normalize_base_url(
            account
                .base_url
                .as_deref()
                .filter(|u| !u.is_empty())
                .unwrap_or(default_base),
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

        tracing::info!(
            "⚡ {} → {} → {}/{}",
            account.alias,
            model_resolution.target_model,
            base_url,
            endpoint,
        );
        if let Some(tx) = &state.tui_tx {
            let _ = tx.send(TuiEvent::Dispatch {
                timestamp: time::OffsetDateTime::now_local()
                    .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
                    .format(&time::format_description::parse("[hour]:[minute]:[second]").unwrap())
                    .unwrap_or_default(),
                account: account.alias.clone(),
                model: model_resolution.target_model.clone(),
                url: format!("{}/{}", base_url, endpoint),
                tag: dispatch_tag.clone(),
            });
        }

        let response = match execute_provider_request(&provider_request).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(
                    "🔀 Account {} (id={}) request failed: {e}",
                    account.alias,
                    account.id
                );
                last_error = Some(format!("Provider request failed: {e}"));
                if let Some(tx) = &state.tui_tx {
                    let _ = tx.send(TuiEvent::Retry {
                        account: account.alias.clone(),
                        status: 0,
                        message: format!("Network error: {e}"),
                    });
                }
                if account.id == preferred_id {
                    let mut router = state.dispatch_router.lock().await;
                    router.record_result(&dispatch_key, &dispatch_meta, None, false);
                }
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
                if let Some(tx) = &state.tui_tx {
                    let _ = tx.send(TuiEvent::Retry {
                        account: account.alias.clone(),
                        status: status.as_u16(),
                        message: error_body.clone(),
                    });
                }
                if account.id == preferred_id {
                    let mut router = state.dispatch_router.lock().await;
                    router.record_result(&dispatch_key, &dispatch_meta, None, false);
                }
                continue;
            }
            // Some provider gateways (GitHub Copilot) serve GPT-5.x models only
            // via the Anthropic Messages endpoint and reject /chat/completions
            // with `unsupported_api_for_model`. When that happens and the
            // account has a valid anthropic_base_url, retry the request
            // through its /v1/messages endpoint (OpenAI → Anthropic conversion).
            if endpoint == "chat/completions"
                && is_unsupported_api_for_model(&error_body)
                && account
                    .anthropic_base_url
                    .as_deref()
                    .map(str::trim)
                    .is_some_and(|u| !u.is_empty())
            {
                tracing::info!(
                    "↩️ {} rejected /chat/completions — retrying via /v1/messages",
                    account.alias
                );
                if let Some(resp) = anthropic_fallback_response(
                    &patched_body,
                    account,
                    &model_resolution.target_model,
                    streaming,
                    state.pool.clone(),
                    start,
                )
                .await
                {
                    {
                        let mut router = state.dispatch_router.lock().await;
                        router.record_result(&dispatch_key, &dispatch_meta, Some(account.id), true);
                    }
                    send_tui_request(
                        &state.tui_tx,
                        normalized_uri.path(),
                        200,
                        start,
                        &model_resolution.target_model,
                    );
                    return resp;
                }
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
            send_tui_request(&state.tui_tx, normalized_uri.path(), status.as_u16(), start, &model_resolution.target_model);
            if let Ok(json_val) = serde_json::from_str::<Value>(&error_body) {
                return (status, Json(json_val)).into_response();
            }
            return (status, error_body).into_response();
        }

        if streaming {
            {
                let mut router = state.dispatch_router.lock().await;
                router.record_result(&dispatch_key, &dispatch_meta, Some(account.id), true);
            }
            send_tui_request(&state.tui_tx, normalized_uri.path(), status.as_u16(), start, &model_resolution.target_model);
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

        {
            let mut router = state.dispatch_router.lock().await;
            router.record_result(&dispatch_key, &dispatch_meta, Some(account.id), true);
        }
        send_tui_request(&state.tui_tx, normalized_uri.path(), 200, start, &model_resolution.target_model);
        return Json(data).into_response();
    }

    {
        let mut router = state.dispatch_router.lock().await;
        router.record_result(&dispatch_key, &dispatch_meta, None, false);
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
    send_tui_request(&state.tui_tx, normalized_uri.path(), 502, start, &model_resolution.target_model);
    middleware::send_error(
        &error_msg,
        "upstream_error",
        StatusCode::BAD_GATEWAY,
        is_anthropic,
    )
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

/// Retry an OpenAI chat/completions request through the account's Anthropic
/// `/v1/messages` endpoint. Used when the upstream rejects `/chat/completions`
/// for a model that IS served on `/v1/messages` (GitHub Copilot GPT-5.x).
/// Returns `None` when the account has no usable anthropic_base_url or the
/// Anthropic attempt fails (the caller keeps the original error).
async fn anthropic_fallback_response(
    openai_body: &Value,
    account: &adapters::Account,
    model: &str,
    streaming: bool,
    pool: sqlx::SqlitePool,
    start: Instant,
) -> Option<Response> {
    let anthropic_base = account
        .anthropic_base_url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())?;

    let anthropic_body = match openai_to_anthropic_request(openai_body, model) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("↩️ OpenAI→Anthropic conversion failed: {e}");
            return None;
        }
    };

    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    headers.insert("x-api-key".to_string(), account.api_key.clone());
    headers.insert("anthropic-version".to_string(), "2023-06-01".to_string());

    let provider_request = ProviderRequest {
        method: "POST".to_string(),
        url: build_anthropic_target_url(anthropic_base),
        headers,
        body: anthropic_body,
    };

    let response = match execute_provider_request(&provider_request).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("↩️ Anthropic fallback request failed: {e}");
            return None;
        }
    };

    if !response.status().is_success() {
        tracing::warn!(
            "↩️ Anthropic fallback returned {}",
            response.status()
        );
        return None;
    }

    if streaming {
        Some(anthropic_fallback_streaming(response, model, account, pool, start).await)
    } else {
        let bytes = match response.bytes().await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("↩️ Anthropic fallback read failed: {e}");
                return None;
            }
        };
        let data: Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("↩️ Anthropic fallback parse failed: {e}");
                return None;
            }
        };
        let openai_resp = anthropic_to_openai_response(&data, model);
        let usage = &data["usage"];
        let _ = log_usage(
            &pool,
            account,
            model,
            &account.provider_id,
            usage["input_tokens"].as_i64().unwrap_or(0),
            usage["output_tokens"].as_i64().unwrap_or(0),
            0,
            0,
            start.elapsed().as_millis() as i64,
            true,
            &None,
        )
        .await;
        Some(Json(openai_resp).into_response())
    }
}

/// Stream an Anthropic `/v1/messages` SSE response, translating each event to
/// OpenAI chat.completion.chunk SSE frames.
async fn anthropic_fallback_streaming(
    response: reqwest::Response,
    model: &str,
    account: &adapters::Account,
    pool: sqlx::SqlitePool,
    start: Instant,
) -> Response {
    let model = model.to_string();
    let account = account.clone();

    let (tx, rx) = mpsc::channel::<Result<Bytes, axum::Error>>(64);
    tokio::spawn(async move {
        let mut buffer: Vec<u8> = Vec::with_capacity(4096);
        let mut converter = AnthropicSseConverter::new(&model);
        let mut sse = response.bytes_stream();
        let mut usage: Option<Value> = None;

        while let Some(chunk) = sse.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("↩️ upstream stream read error: {e}");
                    break;
                }
            };
            buffer.extend_from_slice(&chunk);
            for event_text in parse_sse_chunks(&mut buffer, 0) {
                let Some(payload) = sse_data_payload(&event_text) else {
                    continue;
                };
                let Ok(parsed) = serde_json::from_str::<Value>(payload) else {
                    continue;
                };
                match parsed.get("type").and_then(Value::as_str) {
                    Some("message_start") => {
                        usage = parsed
                            .get("message")
                            .and_then(|m| m.get("usage"))
                            .cloned();
                    }
                    Some("message_delta") => {
                        // Merge output_tokens into the running usage.
                        if let Some(out) = parsed
                            .get("usage")
                            .and_then(|u| u.get("output_tokens"))
                            .and_then(Value::as_i64)
                        {
                            let u = usage.get_or_insert_with(|| serde_json::json!({}));
                            u["output_tokens"] = serde_json::json!(out);
                        }
                    }
                    Some("message_stop") => {
                        // End of the Anthropic stream.
                        for line in converter.finish(usage.as_ref()) {
                            if tx.send(Ok(Bytes::from(format!("{line}\n\n")))).await.is_err() {
                                return;
                            }
                        }
                        let (prompt, completion) = {
                            let u = usage.unwrap_or_default();
                            (
                                u.get("input_tokens").and_then(Value::as_i64).unwrap_or(0),
                                u.get("output_tokens").and_then(Value::as_i64).unwrap_or(0),
                            )
                        };
                        let _ = log_usage(
                            &pool,
                            &account,
                            &model,
                            &account.provider_id,
                            prompt,
                            completion,
                            0,
                            0,
                            start.elapsed().as_millis() as i64,
                            true,
                            &None,
                        )
                        .await;
                        return;
                    }
                    _ => {}
                }
                for line in converter.feed(&parsed) {
                    if tx.send(Ok(Bytes::from(format!("{line}\n\n")))).await.is_err() {
                        return;
                    }
                }
            }
        }

        // Stream ended without message_stop (upstream error / truncation).
        // Drain any complete SSE events still buffered first — otherwise a burst
        // of trailing tool_use/finish_reason frames can be dropped, truncating
        // the turn right before a tool call.
        loop {
            let events = parse_sse_chunks(&mut buffer, 0);
            if events.is_empty() {
                break;
            }
            for event_text in events {
                let Some(payload) = sse_data_payload(&event_text) else {
                    continue;
                };
                let Ok(parsed) = serde_json::from_str::<Value>(payload) else {
                    continue;
                };
                for line in converter.feed(&parsed) {
                    if tx.send(Ok(Bytes::from(format!("{line}\n\n")))).await.is_err() {
                        return;
                    }
                }
            }
        }
        for line in converter.finish(usage.as_ref()) {
            if tx.send(Ok(Bytes::from(format!("{line}\n\n")))).await.is_err() {
                return;
            }
        }
    });

    let body = Body::from_stream(ReceiverStream::new(rx));
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .body(body)
        .unwrap()
        .into_response()
}
