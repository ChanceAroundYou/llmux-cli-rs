use std::collections::BTreeMap;
use std::sync::LazyLock;
use std::time::Instant;

static DISPATCH_TIME_FMT: LazyLock<Vec<time::format_description::BorrowedFormatItem<'static>>> = LazyLock::new(|| time::format_description::parse_borrowed::<1>("[hour]:[minute]:[second]").unwrap());

use axum::{
    body::Body,
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
use llmux_core::proxy::anthropic_openai::{
    anthropic_to_openai_request, cache_usage_from_openai, openai_to_anthropic_response,
    parse_sse_chunks, sse_data_payload, OpenAISseConverter,
};
use llmux_core::proxy::{build_anthropic_passthrough_request, extract_anthropic_usage_from_sse};

use crate::app::{AppState, TuiEvent};
use crate::middleware::{self, AuthContext};

use super::helpers::{spawn_log_usage, send_tui_request};

// ---------------------------------------------------------------------------
// /v1/messages  (Anthropic Messages API)
//
// Two outbound protocols per account:
//   - Anthropic passthrough (upstream speaks Anthropic natively, configured
//     via a non-empty `anthropic_base_url`) — body forwarded as-is.
//   - Anthropic → OpenAI conversion (upstream is OpenAI-compatible, only
//     `base_url` set) — request translated to /chat/completions, response
//     translated back to Anthropic Messages format (including SSE streaming).
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
    let model_resolution = match state.resolve_model_cached(&model_name).await {
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

    let dispatch_key = model_resolution
        .alias_name
        .as_deref()
        .map(|n| format!("alias:{}", n))
        .unwrap_or_else(|| format!("provider:{}", model_resolution.provider_id));

    let preferred_id = model_resolution
        .preferred_account_id
        .unwrap_or_else(|| accounts.first().map(|a| a.id).unwrap_or(0));

    let (ordered_accounts, dispatch_meta) = {
        let mut router = state.dispatch_router.lock().unwrap();
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
        // Determine the outbound protocol:
        //   valid anthropic_base_url  → native Anthropic passthrough
        //   else OpenAI base_url      → Anthropic→OpenAI conversion
        //   neither                   → fall back to Anthropic default
        let anthropic_base = account
            .anthropic_base_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty());
        let openai_base = account
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty());

        let (provider_request, is_conversion) = if let Some(anthropic_base) = anthropic_base {
            (
                build_anthropic_passthrough_request(
                    &body,
                    account,
                    anthropic_base,
                    &model_resolution.target_model,
                    anthropic_beta.as_deref(),
                ),
                false,
            )
        } else if let Some(openai_base) = openai_base {
            // Anthropic → OpenAI protocol translation.
            match anthropic_to_openai_request(&body, &model_resolution.target_model) {
                Ok(openai_body) => {
                    let mut req_headers = BTreeMap::new();
                    req_headers.insert(
                        "content-type".to_string(),
                        "application/json".to_string(),
                    );
                    req_headers.insert(
                        "authorization".to_string(),
                        format!("Bearer {}", account.api_key),
                    );
                    (
                        Ok(ProviderRequest {
                            method: "POST".to_string(),
                            url: format!(
                                "{}/chat/completions",
                                openai_base.trim_end_matches('/')
                            ),
                            headers: req_headers,
                            body: openai_body,
                        }),
                        true,
                    )
                }
                Err(e) => (Err(e), true),
            }
        } else {
            (
                build_anthropic_passthrough_request(
                    &body,
                    account,
                    "https://api.anthropic.com/v1",
                    &model_resolution.target_model,
                    anthropic_beta.as_deref(),
                ),
                false,
            )
        };

        let provider_request = match provider_request {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("📡 Failed to build provider request: {e}");
                last_error = Some(format!("Failed to build provider request: {e}"));
                if let Some(tx) = &state.tui_tx {
                    let _ = tx.send(TuiEvent::Retry {
                        account: account.alias.clone(),
                        status: 0,
                        message: format!("Build error: {e}"),
                    });
                }
                if account.id == preferred_id {
                    let mut router = state.dispatch_router.lock().unwrap();
                    router.record_result(&dispatch_key, &dispatch_meta, None, false);
                }
                continue;
            }
        };

        tracing::info!(
            "⚡ {} → {} → {} {}",
            account.alias,
            model_resolution.target_model,
            provider_request.url,
            if is_conversion { "[anthropic→openai]" } else { "" },
        );
        if let Some(tx) = &state.tui_tx {
            let _ = tx.send(TuiEvent::Dispatch {
                timestamp: time::OffsetDateTime::now_utc().format(&DISPATCH_TIME_FMT).unwrap_or_default(),
                account: account.alias.clone(),
                model: model_resolution.target_model.clone(),
                url: provider_request.url.clone(),
                tag: dispatch_tag.clone(),
            });
        }

        let response = match execute_provider_request(&provider_request).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("📡 provider request failed: {e}");
                last_error = Some(format!("Provider request failed: {e}"));
                if let Some(tx) = &state.tui_tx {
                    let _ = tx.send(TuiEvent::Retry {
                        account: account.alias.clone(),
                        status: 0,
                        message: format!("Network error: {e}"),
                    });
                }
                if account.id == preferred_id {
                    let mut router = state.dispatch_router.lock().unwrap();
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
                    let mut router = state.dispatch_router.lock().unwrap();
                    router.record_result(&dispatch_key, &dispatch_meta, None, false);
                }
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
                let mut router = state.dispatch_router.lock().unwrap();
                router.record_result(&dispatch_key, &dispatch_meta, Some(account.id), true);
            }
            send_tui_request(&state.tui_tx, "/v1/messages", status.as_u16(), start, &model_resolution.target_model);
            if is_conversion {
                return anthropic_to_openai_streaming(
                    response,
                    &model_resolution.target_model,
                    account,
                    state.pool.clone(),
                    &model_resolution.provider_id,
                    start,
                )
                .await;
            }
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

        // Non-streaming
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

        let latency_ms = start.elapsed().as_millis() as i64;

        if is_conversion {
            // Translate OpenAI response → Anthropic Messages response.
            let anthropic_resp = openai_to_anthropic_response(&data, &model_resolution.target_model);
            let usage = &data["usage"];
            let (cache_read, cache_create) = cache_usage_from_openai(usage);
            spawn_log_usage(
                state.pool.clone(),
                (*account).clone(),
                model_resolution.target_model.clone(),
                model_resolution.provider_id.clone(),
                usage["prompt_tokens"].as_i64().unwrap_or(0),
                usage["completion_tokens"].as_i64().unwrap_or(0),
                cache_read,
                cache_create,
                latency_ms,
                true,
                None,
            );
            {
                let mut router = state.dispatch_router.lock().unwrap();
                router.record_result(&dispatch_key, &dispatch_meta, Some(account.id), true);
            }
            send_tui_request(&state.tui_tx, "/v1/messages", 200, start, &model_resolution.target_model);
            return Json(anthropic_resp).into_response();
        }

        // Passthrough: extract usage and return upstream body as-is.
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
        spawn_log_usage(
            state.pool.clone(),
            (*account).clone(),
            model_resolution.target_model.clone(),
            model_resolution.provider_id.clone(),
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_read_input_tokens,
            usage.cache_creation_input_tokens,
            latency_ms,
            true,
            None,
        );

        {
            let mut router = state.dispatch_router.lock().unwrap();
            router.record_result(&dispatch_key, &dispatch_meta, Some(account.id), true);
        }
        send_tui_request(&state.tui_tx, "/v1/messages", 200, start, &model_resolution.target_model);
        return Json(data).into_response();
    }

    // All accounts exhausted
    {
        let mut router = state.dispatch_router.lock().unwrap();
        router.record_result(&dispatch_key, &dispatch_meta, None, false);
    }

    let latency_ms = start.elapsed().as_millis() as i64;
    let error_msg = last_error.unwrap_or_else(|| "All accounts exhausted".to_string());
    if let Some(account) = ordered_accounts.first() {
        spawn_log_usage(
            state.pool.clone(),
            (*account).clone(),
            model_resolution.target_model.clone(),
            model_resolution.provider_id.clone(),
            0,
            0,
            0,
            0,
            latency_ms,
            false,
            Some(error_msg.clone()),
        );
    }
    send_tui_request(&state.tui_tx, "/v1/messages", 502, start, &model_resolution.target_model);
    middleware::send_error(&error_msg, "upstream_error", StatusCode::BAD_GATEWAY, is_anthropic)
}

/// Streaming passthrough for Anthropic responses.
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
        spawn_log_usage(
            pool_clone.clone(),
            account.clone(),
            model.clone(),
            provider_id.clone(),
            0,
            0,
            0,
            0,
            latency_ms,
            true,
            None,
        );
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

/// Streaming Anthropic→OpenAI conversion. Reads OpenAI SSE chunks from the
/// upstream, translates them to Anthropic SSE events via `OpenAISseConverter`,
/// and forwards them to the client. Real usage is captured from the
/// `include_usage` tail chunk (when the provider emits one) and logged.
async fn anthropic_to_openai_streaming(
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

    let (tx, rx) = mpsc::channel::<Result<Bytes, axum::Error>>(64);
    let mut converter = OpenAISseConverter::new(&model);

    tokio::spawn(async move {
        let mut buffer: Vec<u8> = Vec::with_capacity(4096);
        let mut sse = response.bytes_stream();
        let mut done = false;
        let mut chunks_received: u64 = 0;

        tracing::debug!("[stream:{model}] upstream stream started");

        while !done {
            let chunk = match sse.next().await {
                Some(Ok(c)) => c,
                Some(Err(e)) => {
                    tracing::warn!("[stream:{model}] upstream stream read error: {e}");
                    break;
                }
                None => {
                    tracing::debug!("[stream:{model}] upstream EOF after {chunks_received} chunks, buffer={} bytes", buffer.len());
                    break;
                }
            };
            chunks_received += 1;
            buffer.extend_from_slice(&chunk);

            for event_text in parse_sse_chunks(&mut buffer, 0) {
                let Some(payload) = sse_data_payload(&event_text) else {
                    continue;
                };
                if payload.trim() == "[DONE]" {
                    tracing::debug!("[stream:{model}] received [DONE] in main loop");
                    done = true;
                    break;
                }
                let Ok(parsed) = serde_json::from_str::<Value>(payload) else {
                    continue;
                };
                for ev in converter.feed(&parsed) {
                    if tx.send(Ok(Bytes::from(ev))).await.is_err() {
                        return; // client gone
                    }
                }
            }

            if done {
                break;
            }
        }

        // Drain any complete SSE events still buffered (e.g. more than one read
        // worth arrived, or `[DONE]` trailed content in the same TCP read). Every
        // frame here may carry a tool_use arg delta / finish_reason — dropping it
        // would truncate the assistant turn right before a tool call.
        loop {
            let events = parse_sse_chunks(&mut buffer, 0);
            if events.is_empty() {
                break;
            }
            for event_text in events {
                let Some(payload) = sse_data_payload(&event_text) else {
                    continue;
                };
                if payload.trim() == "[DONE]" {
                    done = true;
                    continue;
                }
                let Ok(parsed) = serde_json::from_str::<Value>(payload) else {
                    continue;
                };
                for ev in converter.feed(&parsed) {
                    if tx.send(Ok(Bytes::from(ev))).await.is_err() {
                        return;
                    }
                }
            }
            if done {
                break;
            }
        }

        // Handle one trailing partial event that lacked a blank-line terminator.
        if !buffer.is_empty() {
            tracing::debug!("[stream:{model}] partial event handler: {} bytes remaining", buffer.len());
            let text = String::from_utf8_lossy(&buffer).to_string();
            if let Some(payload) = sse_data_payload(&text) {
                if payload.trim() != "[DONE]" {
                    if let Ok(parsed) = serde_json::from_str::<Value>(payload) {
                        for ev in converter.feed(&parsed) {
                            if tx.send(Ok(Bytes::from(ev))).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            }
        }

        for ev in converter.finish() {
            tracing::debug!("[stream:{model}] finish event: {}", &ev[..ev.len().min(120)]);
            if tx.send(Ok(Bytes::from(ev))).await.is_err() {
                tracing::debug!("[stream:{model}] client gone during finish send");
                return;
            }
        }

        tracing::debug!("[stream:{model}] stream complete: done={done}, chunks={chunks_received}, buffer_remaining={}", buffer.len());
        // ponytail: truncation = no [DONE] and buffered as success=0 (mirrors openai passthrough)
        let truncated = !done;
        if truncated {
            tracing::warn!("[stream:{model}] stream truncated: account={} chunks={chunks_received}", account.alias);
        }
        let (input_tokens, output_tokens, cache_read, cache_create) = converter.usage_tokens();
        let latency_ms = start.elapsed().as_millis() as i64;
        spawn_log_usage(
            pool.clone(),
            account.clone(),
            model.clone(),
            provider_id.clone(),
            input_tokens,
            output_tokens,
            cache_read,
            cache_create,
            latency_ms,
            !truncated,
            if truncated {
                Some(format!("truncated: done=false chunks={chunks_received}"))
            } else {
                None
            },
        );
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
