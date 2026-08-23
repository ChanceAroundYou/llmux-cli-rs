use std::collections::BTreeMap;
use std::sync::LazyLock;
use std::time::Instant;

static DISPATCH_TIME_FMT: LazyLock<Vec<time::format_description::BorrowedFormatItem<'static>>> = LazyLock::new(|| time::format_description::parse_borrowed::<1>("[hour]:[minute]:[second]").unwrap());

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
use llmux_core::aggregate::get_account_by_id;
use llmux_core::dispatcher::{self, get_accounts_by_ids, get_active_accounts, is_retryable_status};
use llmux_core::proxy::openai_anthropic::{
    anthropic_to_openai_response, is_unsupported_api_for_model, openai_to_anthropic_request,
    AnthropicSseConverter,
};
use llmux_core::proxy::{build_anthropic_target_url, anthropic_openai::{parse_sse_chunks, sse_data_payload}};

use crate::app::{AppState, TuiEvent};
use crate::middleware::{self, AuthContext};

use super::helpers::{spawn_log_usage, normalize_base_url, send_tui_request};

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
    // Resolve early to check upstream_api — chat ingress may be routed to responses upstream
    let raw_model = body.get("model").and_then(Value::as_str).unwrap_or("");
    // Aggregate alias takes precedence over ordinary alias/prefix
    if let Ok(Some(agg)) = state.resolve_aggregate_cached(raw_model).await {
        // honor aggregate's upstream_api
        let endpoint = match agg.upstream_api {
            llmux_core::upstream_api::UpstreamApi::Responses | llmux_core::upstream_api::UpstreamApi::Auto => "responses",
            _ => "chat/completions",
        };
        // if responses preferred, use responses dispatch with translation
        if endpoint == "responses" {
            return dispatch_aggregate_openai_responses(state, auth, uri, headers, body, agg).await;
        }
        return dispatch_aggregate_openai(state, auth, uri, headers, body, "chat/completions", agg).await;
    }
    // Check ordinary alias upstream_api — if responses/auto, route chat via responses
    if let Ok(res) = state.resolve_model_cached(raw_model).await {
        if res.upstream_api.wants_responses() {
            return chat_via_responses(state, auth, uri, body, res).await;
        }
    }
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
    if let Ok(Some(agg)) = state.resolve_aggregate_cached(body.get("model").and_then(Value::as_str).unwrap_or_default()).await {
        return dispatch_aggregate_openai(state, auth, uri, headers, body, "responses", agg).await;
    }
    openai_dispatch(state, auth, uri, headers, body, "responses").await
}

// --- chat/messages -> responses routing (upstream_api = responses/auto) ---
async fn chat_via_responses(
    state: AppState,
    auth: AuthContext,
    uri: axum::http::Uri,
    body: Value,
    res: llmux_core::dispatcher::ModelResolution,
) -> Response {
    let target = res.target_model.clone();
    let is_auto = res.upstream_api == llmux_core::upstream_api::UpstreamApi::Auto;
    let streaming = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let translated = llmux_core::proxy::responses::chat_to_responses(&body, &target);
    let resp = dispatch_openai_responses(state.clone(), auth.clone(), uri.clone(), translated, &res, streaming).await;
    if is_auto && is_responses_error(&resp) {
        tracing::warn!("[chat_via_responses] {} responses unsupported, fallback to chat", target);
        return openai_dispatch(state, auth, uri, HeaderMap::new(), body, "chat/completions").await;
    }
    resp
}

async fn dispatch_aggregate_openai_responses(
    state: AppState,
    auth: AuthContext,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: Value,
    agg: llmux_core::aggregate::AggregateResolution,
) -> Response {
    // Per-candidate chat -> responses translation + responses->chat back-translation
    dispatch_aggregate_openai_with_responses(state, auth, uri, headers, body, agg).await
}

fn is_responses_error(resp: &Response) -> bool {
    matches!(
        resp.status(),
        StatusCode::NOT_FOUND | StatusCode::NOT_IMPLEMENTED | StatusCode::METHOD_NOT_ALLOWED | StatusCode::BAD_GATEWAY
    )
}

#[allow(dead_code)]
async fn openai_dispatch_via_body(
    state: AppState,
    auth: AuthContext,
    uri: axum::http::Uri,
    body: Value,
    endpoint: &str,
    res: &llmux_core::dispatcher::ModelResolution,
) -> Response {
    let mut patched = body;
    patched["model"] = Value::String(res.target_model.clone());
    openai_dispatch(state, auth, uri, HeaderMap::new(), patched, endpoint).await
}

/// Direct dispatch to /responses with back-translation to chat completions
async fn dispatch_openai_responses(
    state: AppState,
    _auth: AuthContext,
    uri: axum::http::Uri,
    translated_body: Value,
    res: &llmux_core::dispatcher::ModelResolution,
    streaming: bool,
) -> Response {
    let normalized_uri = crate::app::normalize_gateway_uri(&uri);
    let is_anthropic = false;
    let model_name = res.target_model.clone();

    // Load accounts (same as openai_dispatch but bypass re-resolution)
    let accounts = if !res.account_ids.is_empty() {
        match get_accounts_by_ids(&state.pool, &res.account_ids, &state.master_key).await {
            Ok(a) => a,
            Err(e) => return middleware::send_error(&format!("Failed to load accounts: {e}"), "server_error", StatusCode::INTERNAL_SERVER_ERROR, is_anthropic),
        }
    } else {
        match get_active_accounts(&state.pool, Some(&res.provider_id), &state.master_key).await {
            Ok(a) => a,
            Err(e) => return middleware::send_error(&format!("Failed to load accounts: {e}"), "server_error", StatusCode::INTERNAL_SERVER_ERROR, is_anthropic),
        }
    };
    if accounts.is_empty() {
        return middleware::send_error(&format!("No active accounts for model '{}'", model_name), "server_error", StatusCode::SERVICE_UNAVAILABLE, is_anthropic);
    }
    let accounts: Vec<_> = accounts.into_iter().filter(|a| a.provider_id != "gemini" || a.openai_compatible == 1).collect();
    if accounts.is_empty() {
        return middleware::send_error("No Gemini-compatible accounts", "server_error", StatusCode::SERVICE_UNAVAILABLE, is_anthropic);
    }
    let dispatch_key = res.alias_name.as_deref().map(|n| format!("alias:{}", n)).unwrap_or_else(|| format!("provider:{}", res.provider_id));
    let preferred_id = res.preferred_account_id.unwrap_or_else(|| accounts.first().map(|a| a.id).unwrap_or(0));
    let (ordered_accounts, dispatch_meta) = { let mut r = state.dispatch_router.lock().unwrap(); r.select(&dispatch_key, &accounts, preferred_id) };
    let dispatch_tag = if dispatch_meta.is_probe { Some("probe".to_string()) } else if ordered_accounts.first().map(|a| a.id) != Some(preferred_id) { Some("fallback".to_string()) } else { None };
    let start = Instant::now();
    let mut last_error: Option<String> = None;
    for account in &ordered_accounts {
        let default_base = if account.provider_id == "gemini" { "https://generativelanguage.googleapis.com/v1beta/openai" } else { "https://api.openai.com/v1" };
        let base_url = normalize_base_url(account.base_url.as_deref().filter(|u| !u.is_empty()).unwrap_or(default_base));
        let mut req_headers = BTreeMap::from([("content-type".to_string(), "application/json".to_string())]);
        req_headers.insert("authorization".to_string(), format!("Bearer {}", account.api_key));
        let provider_request = ProviderRequest { method: "POST".to_string(), url: format!("{base_url}/responses"), headers: req_headers, body: translated_body.clone() };
        tracing::info!("⚡ {} → {} → {}/responses [chat→responses]", account.alias, model_name, base_url);
        if let Some(tx) = &state.tui_tx { let _ = tx.send(TuiEvent::Dispatch { timestamp: time::OffsetDateTime::now_utc().format(&DISPATCH_TIME_FMT).unwrap_or_default(), account: account.alias.clone(), model: model_name.clone(), url: format!("{}/responses", base_url), tag: dispatch_tag.clone() }); }
        let response = match execute_provider_request(&provider_request).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("🔀 Account {} (id={}) request failed: {e}", account.alias, account.id);
                last_error = Some(format!("Provider request failed: {e}"));
                if account.id == preferred_id { let mut r = state.dispatch_router.lock().unwrap(); r.record_result(&dispatch_key, &dispatch_meta, None, false); }
                continue;
            }
        };
        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            // Detect responses unsupported for Auto fallback
            if llmux_core::proxy::responses::is_responses_unsupported(&error_body) && res.upstream_api == llmux_core::upstream_api::UpstreamApi::Auto {
                return middleware::send_error(&error_body, "responses_unsupported", StatusCode::NOT_FOUND, is_anthropic);
            }
            last_error = Some(format!("Provider returned {status}: {error_body}"));
            if is_retryable_status(status.as_u16()) {
                if account.id == preferred_id { let mut r = state.dispatch_router.lock().unwrap(); r.record_result(&dispatch_key, &dispatch_meta, None, false); }
                continue;
            }
            let latency_ms = start.elapsed().as_millis() as i64;
            spawn_log_usage(state.pool.clone(), (*account).clone(), model_name.clone(), res.provider_id.clone(), 0, 0, 0, 0, latency_ms, false, last_error.clone());
            send_tui_request(&state.tui_tx, normalized_uri.path(), status.as_u16(), start, &model_name);
            if let Ok(v) = serde_json::from_str::<Value>(&error_body) { return (status, Json(v)).into_response(); }
            return (status, error_body).into_response();
        }
        if streaming {
            { let mut r = state.dispatch_router.lock().unwrap(); r.record_result(&dispatch_key, &dispatch_meta, Some(account.id), true); }
            send_tui_request(&state.tui_tx, normalized_uri.path(), status.as_u16(), start, &model_name);
            return responses_to_chat_streaming(response, &model_name, account, state.pool.clone(), start).await;
        }
        let body_bytes = match response.bytes().await { Ok(b) => b, Err(e) => { last_error = Some(format!("Failed to read response: {e}")); continue; } };
        let data: Value = match serde_json::from_slice(&body_bytes) { Ok(v) => v, Err(e) => { last_error = Some(format!("Failed to parse response: {e}")); continue; } };
        let chat_resp = llmux_core::proxy::responses::responses_to_chat(&data, &model_name);
        let (prompt_tokens, completion_tokens) = adapters::usage_from_openai_response_body(&chat_resp);
        let latency_ms = start.elapsed().as_millis() as i64;
        spawn_log_usage(state.pool.clone(), (*account).clone(), model_name.clone(), res.provider_id.clone(), prompt_tokens, completion_tokens, 0, 0, latency_ms, true, None);
        { let mut r = state.dispatch_router.lock().unwrap(); r.record_result(&dispatch_key, &dispatch_meta, Some(account.id), true); }
        send_tui_request(&state.tui_tx, normalized_uri.path(), 200, start, &model_name);
        return Json(chat_resp).into_response();
    }
    { let mut r = state.dispatch_router.lock().unwrap(); r.record_result(&dispatch_key, &dispatch_meta, None, false); }
    let error_msg = last_error.unwrap_or_else(|| "All accounts exhausted".to_string());
    send_tui_request(&state.tui_tx, normalized_uri.path(), 502, start, &model_name);
    middleware::send_error(&error_msg, "upstream_error", StatusCode::BAD_GATEWAY, is_anthropic)
}

async fn dispatch_aggregate_openai_with_responses(
    state: AppState,
    _auth: AuthContext,
    uri: axum::http::Uri,
    _headers: HeaderMap,
    body: Value,
    agg: llmux_core::aggregate::AggregateResolution,
) -> Response {
    let normalized_uri = crate::app::normalize_gateway_uri(&uri);
    let is_anthropic = false;
    let streaming = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let len = agg.candidates.len();
    let alias = agg.alias.clone();
    let active = agg.active.min(len.saturating_sub(1));
    let start = Instant::now();
    let mut last_error: Option<String> = None;
    let mut hit_index: Option<usize> = None;
    let mut hit_stream_resp: Option<reqwest::Response> = None;
    let mut hit_data: Option<Value> = None;
    let mut hit_account: Option<adapters::Account> = None;
    for i in active..len {
        let cand = &agg.candidates[i];
        let account = match get_account_by_id(&state.pool, cand.account_id, &state.master_key).await {
            Ok(Some(a)) => a, Ok(None) => { state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len); last_error = Some(format!("Candidate {} account {} not found", i, cand.account_id)); continue; }
            Err(e) => { state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len); last_error = Some(format!("Failed to load account {}: {e}", cand.account_id)); continue; }
        };
        let translated = llmux_core::proxy::responses::chat_to_responses(&body, &cand.model);
        let default_base = if account.provider_id == "gemini" { "https://generativelanguage.googleapis.com/v1beta/openai" } else { "https://api.openai.com/v1" };
        let base_url = normalize_base_url(account.base_url.as_deref().filter(|u| !u.is_empty()).unwrap_or(default_base));
        let mut req_headers = BTreeMap::from([("content-type".to_string(), "application/json".to_string())]);
        req_headers.insert("authorization".to_string(), format!("Bearer {}", account.api_key));
        let provider_request = ProviderRequest { method: "POST".to_string(), url: format!("{base_url}/responses"), headers: req_headers, body: translated };
        tracing::info!("🔀 [agg:{} V={}] {} → {} → {}/responses [chat→responses]", alias, active, account.alias, cand.model, base_url);
        let response = match execute_provider_request(&provider_request).await { Ok(r) => r, Err(e) => { state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len); last_error = Some(format!("Provider request failed: {e}")); continue; } };
        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            last_error = Some(format!("Provider returned {status}: {error_body}"));
            if is_retryable_status(status.as_u16()) { state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len); continue; }
            state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len);
            continue;
        }
        if streaming {
            state.aggregate_router.lock().unwrap().note_candidate_success(&alias, i, len);
            hit_index = Some(i); hit_account = Some(account.clone()); hit_stream_resp = Some(response); break;
        }
        let body_bytes = match response.bytes().await { Ok(b) => b, Err(e) => { last_error = Some(format!("Failed to read response: {e}")); state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len); continue; } };
        let data: Value = match serde_json::from_slice(&body_bytes) { Ok(v) => v, Err(e) => { last_error = Some(format!("Failed to parse response: {e}")); state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len); continue; } };
        let chat_resp = llmux_core::proxy::responses::responses_to_chat(&data, &cand.model);
        let (pt, ct) = adapters::usage_from_openai_response_body(&chat_resp);
        spawn_log_usage(state.pool.clone(), account.clone(), cand.model.clone(), account.provider_id.clone(), pt, ct, 0, 0, start.elapsed().as_millis() as i64, true, None);
        state.aggregate_router.lock().unwrap().note_candidate_success(&alias, i, len);
        hit_index = Some(i); hit_account = Some(account); hit_data = Some(chat_resp); break;
    }
    if let Some(hit) = hit_index {
        let switched = state.aggregate_router.lock().unwrap().record_request_outcome(&alias, hit, len);
        if switched { tracing::info!("🔀 [agg:{}] V migrated -> {} (after 3-confirm)", alias, hit); }
        if let Some(resp) = hit_stream_resp {
            let cand_model = agg.candidates[hit].model.clone();
            let account = hit_account.unwrap();
            send_tui_request(&state.tui_tx, normalized_uri.path(), 200, start, &cand_model);
            return responses_to_chat_streaming(resp, &cand_model, &account, state.pool.clone(), start).await;
        }
        if let Some(data) = hit_data { send_tui_request(&state.tui_tx, normalized_uri.path(), 200, start, &agg.candidates[hit].model); return Json(data).into_response(); }
    }
    let switched = state.aggregate_router.lock().unwrap().record_request_all_failed(&alias, len);
    if switched { tracing::info!("🔀 [agg:{}] all failed — V reset to 0", alias); }
    let error_msg = last_error.unwrap_or_else(|| "All aggregate candidates exhausted".to_string());
    send_tui_request(&state.tui_tx, normalized_uri.path(), 502, start, &agg.alias);
    middleware::send_error(&error_msg, "upstream_error", StatusCode::BAD_GATEWAY, is_anthropic)
}

async fn responses_to_chat_streaming(
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
        let mut converter = llmux_core::proxy::responses::ResponsesToChatConverter::new(&model);
        let mut sse = response.bytes_stream();
        let mut chunks: u64 = 0;
        let mut saw_done = false;
        let mut saw_completed = false;
        let mut last_usage: Option<Value> = None;
        while let Some(chunk) = sse.next().await {
            match chunk {
                Ok(c) => {
                    chunks += 1;
                    buffer.extend_from_slice(&c);
                    for event_text in parse_sse_chunks(&mut buffer, 0) {
                        if event_text.contains("[DONE]") { saw_done = true; }
                        if event_text.contains("response.completed") { saw_completed = true; }
                        for out in converter.feed(&event_text) {
                            if out.contains("[DONE]") { saw_done = true; }
                            if let Ok(v) = serde_json::from_str::<Value>(out.trim_start_matches("data:").trim()) {
                                if let Some(u) = v.get("usage") { last_usage = Some(u.clone()); }
                            }
                            if tx.send(Ok(Bytes::from(out))).await.is_err() { return; }
                        }
                        if saw_completed { break; }
                    }
                    if saw_completed { break; }
                }
                Err(e) => { tracing::warn!("[responses→chat:{}] read error: {e}", model); break; }
            }
        }
        // drain buffered
        loop {
            let events = parse_sse_chunks(&mut buffer, 0);
            if events.is_empty() { break; }
            for event_text in events {
                if event_text.contains("response.completed") { saw_completed = true; }
                for out in converter.feed(&event_text) {
                    if out.contains("[DONE]") { saw_done = true; }
                    if let Ok(v) = serde_json::from_str::<Value>(out.trim_start_matches("data:").trim()) {
                        if let Some(u) = v.get("usage") { last_usage = Some(u.clone()); }
                    }
                    if tx.send(Ok(Bytes::from(out))).await.is_err() { return; }
                }
            }
        }
        for out in converter.finish() {
            if out.contains("[DONE]") { saw_done = true; }
            if let Ok(v) = serde_json::from_str::<Value>(out.trim_start_matches("data:").trim()) {
                if let Some(u) = v.get("usage") { last_usage = Some(u.clone()); }
            }
            if tx.send(Ok(Bytes::from(out))).await.is_err() { return; }
        }
        let (prompt_tokens, completion_tokens) = match &last_usage {
            Some(u) => (u.get("prompt_tokens").or_else(|| u.get("input_tokens")).and_then(Value::as_i64).unwrap_or(0), u.get("completion_tokens").or_else(|| u.get("output_tokens")).and_then(Value::as_i64).unwrap_or(0)),
            None => (0, 0),
        };
        let done = saw_done || saw_completed || completion_tokens > 0;
        let truncated = !done && chunks <= 4;
        let latency_ms = start.elapsed().as_millis() as i64;
        spawn_log_usage(pool.clone(), account.clone(), model.clone(), account.provider_id.clone(), prompt_tokens, completion_tokens, 0, 0, latency_ms, !truncated, if truncated { Some(format!("truncated: chunks={chunks} saw_done={saw_done} completed={saw_completed} tokens={completion_tokens}")) } else { None });
    });
    let body = Body::from_stream(ReceiverStream::new(rx));
    Response::builder().status(StatusCode::OK).header("content-type", "text/event-stream").header("cache-control", "no-cache").header("connection", "keep-alive").body(body).unwrap().into_response()
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
        match state.resolve_model_cached(&model_name).await {
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
                timestamp: time::OffsetDateTime::now_utc().format(&DISPATCH_TIME_FMT).unwrap_or_default(),
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
                        let mut router = state.dispatch_router.lock().unwrap();
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
                last_error.clone(),
            );
            send_tui_request(&state.tui_tx, normalized_uri.path(), status.as_u16(), start, &model_resolution.target_model);
            if let Ok(json_val) = serde_json::from_str::<Value>(&error_body) {
                return (status, Json(json_val)).into_response();
            }
            return (status, error_body).into_response();
        }

        if streaming {
            {
                let mut router = state.dispatch_router.lock().unwrap();
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
        spawn_log_usage(
            state.pool.clone(),
            (*account).clone(),
            model_resolution.target_model.clone(),
            model_resolution.provider_id.clone(),
            prompt_tokens,
            completion_tokens,
            0,
            0,
            latency_ms,
            true,
            None,
        );

        {
            let mut router = state.dispatch_router.lock().unwrap();
            router.record_result(&dispatch_key, &dispatch_meta, Some(account.id), true);
        }
        send_tui_request(&state.tui_tx, normalized_uri.path(), 200, start, &model_resolution.target_model);
        return Json(data).into_response();
    }

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
    send_tui_request(&state.tui_tx, normalized_uri.path(), 502, start, &model_resolution.target_model);
    middleware::send_error(
        &error_msg,
        "upstream_error",
        StatusCode::BAD_GATEWAY,
        is_anthropic,
    )
}

// ---------------------------------------------------------------------------
// Aggregate (cross-model) OpenAI dispatch — V-anchored, 3-confirm
// ---------------------------------------------------------------------------

async fn dispatch_aggregate_openai(
    state: AppState,
    auth: AuthContext,
    uri: axum::http::Uri,
    _headers: HeaderMap,
    body: Value,
    endpoint: &str,
    agg: llmux_core::aggregate::AggregateResolution,
) -> Response {
    let normalized_uri = crate::app::normalize_gateway_uri(&uri);
    let is_anthropic = false;
    let streaming = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    if !middleware::is_model_allowed(&auth.allowed_models, &agg.alias) {
        return middleware::send_error(
            &format!("Model '{}' is not allowed for this API key", agg.alias),
            "permission_error",
            StatusCode::UNAUTHORIZED,
            is_anthropic,
        );
    }

    let len = agg.candidates.len();
    if len == 0 {
        return middleware::send_error("Aggregate alias has no candidates", "server_error", StatusCode::INTERNAL_SERVER_ERROR, is_anthropic);
    }

    let start = Instant::now();
    let alias = agg.alias.clone();
    let active = agg.active.min(len.saturating_sub(1));
    let mut last_error: Option<String> = None;
    let mut hit_index: Option<usize> = None;
    let mut hit_account: Option<adapters::Account> = None;
    let mut hit_data: Option<Value> = None;
    let mut hit_stream_resp: Option<reqwest::Response> = None;

    for i in active..len {
        let cand = &agg.candidates[i];
        let account = match get_account_by_id(&state.pool, cand.account_id, &state.master_key).await {
            Ok(Some(a)) => a,
            Ok(None) => {
                state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len);
                last_error = Some(format!("Candidate {} account {} not found or inactive", i, cand.account_id));
                continue;
            }
            Err(e) => {
                state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len);
                last_error = Some(format!("Failed to load account {}: {e}", cand.account_id));
                continue;
            }
        };

        // Patch body model to this candidate's model
        let mut patched_body = body.clone();
        patched_body["model"] = Value::String(cand.model.clone());

        // Build provider request (OpenAI-compatible)
        let default_base = if account.provider_id == "gemini" { "https://generativelanguage.googleapis.com/v1beta/openai" } else { "https://api.openai.com/v1" };
        let base_url = normalize_base_url(account.base_url.as_deref().filter(|u| !u.is_empty()).unwrap_or(default_base));
        let mut req_headers = BTreeMap::from([("content-type".to_string(), "application/json".to_string())]);
        req_headers.insert("authorization".to_string(), format!("Bearer {}", account.api_key));
        let provider_request = ProviderRequest { method: "POST".to_string(), url: format!("{base_url}/{endpoint}"), headers: req_headers, body: patched_body.clone() };

        tracing::info!("🔀 [agg:{} V={}] {} → {} → {}/{}", alias, active, account.alias, cand.model, base_url, endpoint);
        if let Some(tx) = &state.tui_tx {
            let _ = tx.send(TuiEvent::Dispatch { timestamp: time::OffsetDateTime::now_utc().format(&DISPATCH_TIME_FMT).unwrap_or_default(), account: account.alias.clone(), model: cand.model.clone(), url: format!("{}/{}", base_url, endpoint), tag: Some(format!("agg:{alias}")) });
        }

        let response = match execute_provider_request(&provider_request).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("🔀 [agg:{}] Account {} request failed: {e}", alias, account.alias);
                last_error = Some(format!("Provider request failed: {e}"));
                state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len);
                if let Some(tx) = &state.tui_tx { let _ = tx.send(TuiEvent::Retry { account: account.alias.clone(), status: 0, message: format!("Network error: {e}") }); }
                continue;
            }
        };

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            last_error = Some(format!("Provider returned {status}: {error_body}"));
            if is_retryable_status(status.as_u16()) {
                tracing::warn!("🔀 [agg:{}] Account {} failed ({}) — trying next...", alias, account.alias, status.as_u16());
                state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len);
                if let Some(tx) = &state.tui_tx { let _ = tx.send(TuiEvent::Retry { account: account.alias.clone(), status: status.as_u16(), message: error_body.clone() }); }
                continue;
            }
            if endpoint == "chat/completions" && is_unsupported_api_for_model(&error_body) && account.anthropic_base_url.as_deref().map(str::trim).is_some_and(|u| !u.is_empty()) {
                tracing::info!("↩️ [agg:{}] {} rejected /chat/completions — retrying via /v1/messages", alias, account.alias);
                if let Some(resp) = anthropic_fallback_response(&patched_body, &account, &cand.model, streaming, state.pool.clone(), start).await {
                    state.aggregate_router.lock().unwrap().record_request_outcome(&alias, i, len);
                    send_tui_request(&state.tui_tx, normalized_uri.path(), 200, start, &cand.model);
                    return resp;
                }
            }
            let latency_ms = start.elapsed().as_millis() as i64;
            spawn_log_usage(state.pool.clone(), account.clone(), cand.model.clone(), account.provider_id.clone(), 0, 0, 0, 0, latency_ms, false, last_error.clone());
            send_tui_request(&state.tui_tx, normalized_uri.path(), status.as_u16(), start, &cand.model);
            // Non-retryable final for this candidate — treat as candidate failure, try next candidate
            state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len);
            // For non-retryable upstream errors, we still continue to next candidate (spec: failover down the list)
            // But to avoid hiding the upstream error when all candidates fail with non-retryable, we keep last_error.
            continue;
        }

        if streaming {
            state.aggregate_router.lock().unwrap().note_candidate_success(&alias, i, len);
            hit_index = Some(i);
            hit_account = Some(account.clone());
            hit_stream_resp = Some(response);
            break;
        }

        let body_bytes = match response.bytes().await { Ok(b) => b, Err(e) => { last_error = Some(format!("Failed to read response: {e}")); state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len); continue; } };
        let data: Value = match serde_json::from_slice(&body_bytes) { Ok(v) => v, Err(e) => { last_error = Some(format!("Failed to parse response: {e}")); state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len); continue; } };
        let (prompt_tokens, completion_tokens) = adapters::usage_from_openai_response_body(&data);
        let latency_ms = start.elapsed().as_millis() as i64;
        spawn_log_usage(state.pool.clone(), account.clone(), cand.model.clone(), account.provider_id.clone(), prompt_tokens, completion_tokens, 0, 0, latency_ms, true, None);
        state.aggregate_router.lock().unwrap().note_candidate_success(&alias, i, len);
        hit_index = Some(i);
        hit_account = Some(account);
        hit_data = Some(data);
        break;
    }

    if let Some(hit) = hit_index {
        let switched = state.aggregate_router.lock().unwrap().record_request_outcome(&alias, hit, len);
        if switched {
            tracing::info!("🔀 [agg:{}] V migrated -> {} (after 3-confirm)", alias, hit);
        } else if hit != active {
            tracing::info!("🔀 [agg:{}] hit {} pending V migration ({}/3)", alias, hit, state.aggregate_router.lock().unwrap().entries.get(&alias).map(|e| e.confirm_count).unwrap_or(0));
        }
        if let Some(resp) = hit_stream_resp {
            // need cand model for stream
            let cand_model = agg.candidates[hit].model.clone();
            let account = hit_account.unwrap();
            send_tui_request(&state.tui_tx, normalized_uri.path(), 200, start, &cand_model);
            return openai_streaming_passthrough(resp, &cand_model, &account, state.pool.clone(), start).await;
        }
        if let Some(data) = hit_data {
            send_tui_request(&state.tui_tx, normalized_uri.path(), 200, start, &agg.candidates[hit].model);
            return Json(data).into_response();
        }
        // fallback path (anthropic fallback already returned above)
    }

    // All candidates exhausted
    let switched = state.aggregate_router.lock().unwrap().record_request_all_failed(&alias, len);
    if switched {
        tracing::info!("🔀 [agg:{}] all failed — V reset to 0 (after 3-confirm)", alias);
    }
    let latency_ms = start.elapsed().as_millis() as i64;
    let error_msg = last_error.unwrap_or_else(|| "All aggregate candidates exhausted".to_string());
    if let Some(idx) = hit_index {
        if let Some(account) = hit_account {
            spawn_log_usage(state.pool.clone(), account, agg.candidates[idx].model.clone(), String::new(), 0, 0, 0, 0, latency_ms, false, Some(error_msg.clone()));
        }
    } else if len > 0 {
        // best-effort: log with alias name
        if let Ok(Some(acc)) = get_account_by_id(&state.pool, agg.candidates[0].account_id, &state.master_key).await {
            spawn_log_usage(state.pool.clone(), acc, agg.candidates[0].model.clone(), String::new(), 0, 0, 0, 0, latency_ms, false, Some(error_msg.clone()));
        }
    }
    send_tui_request(&state.tui_tx, normalized_uri.path(), 502, start, &agg.alias);
    middleware::send_error(&error_msg, "upstream_error", StatusCode::BAD_GATEWAY, is_anthropic)
}

// ---------------------------------------------------------------------------
// Streaming helpers
// ---------------------------------------------------------------------------

/// Passthrough streaming for OpenAI-compatible responses.
/// Bytes are forwarded as-is (SSE) but the stream is also parsed for
/// observability: `finish_reason`, `usage`, and truncation (`[DONE]` / null
/// finish_reason without tail usage) are extracted so `ag` (agnes-2.5-flash)
/// truncations are no longer invisible (`output_tokens` 0 + `success=1`).
async fn openai_streaming_passthrough(
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
        let mut sse = response.bytes_stream();
        let mut chunks: u64 = 0;
        let mut saw_done = false;
        let mut last_finish: Option<String> = None;
        let mut last_usage: Option<Value> = None;

        tracing::debug!("[openai:{model}] upstream stream started (account={})", account.alias);

        while let Some(chunk) = sse.next().await {
            match chunk {
                Ok(c) => {
                    chunks += 1;
                    // forward raw bytes to client before parsing
                    if tx.send(Ok(Bytes::from(c.to_vec()))).await.is_err() {
                        return;
                    }
                    buffer.extend_from_slice(&c);
                    for event_text in parse_sse_chunks(&mut buffer, 0) {
                        let Some(payload) = sse_data_payload(&event_text) else {
                            continue;
                        };
                        if payload.trim() == "[DONE]" {
                            saw_done = true;
                            continue;
                        }
                        let Ok(parsed) = serde_json::from_str::<Value>(payload) else {
                            continue;
                        };
                        if let Some(u) = parsed.get("usage") {
                            if u.is_object() {
                                last_usage = Some(u.clone());
                            }
                        }
                        if let Some(choice) = parsed
                            .get("choices")
                            .and_then(Value::as_array)
                            .and_then(|a| a.first())
                        {
                            if let Some(fr) = choice.get("finish_reason") {
                                if fr.is_null() {
                                    last_finish = None;
                                } else if let Some(s) = fr.as_str() {
                                    if !s.is_empty() {
                                        last_finish = Some(s.to_string());
                                    }
                                }
                            }
                            // some gateways emit usage alongside the last choice
                            if let Some(u) = parsed.get("usage") {
                                last_usage = Some(u.clone());
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "[openai:{model}] upstream stream read error: {e} (account={}, chunks={})",
                        account.alias,
                        chunks
                    );
                    break;
                }
            }
        }

        // drain any complete events still buffered
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
                    saw_done = true;
                    continue;
                }
                let Ok(parsed) = serde_json::from_str::<Value>(payload) else {
                    continue;
                };
                if let Some(u) = parsed.get("usage") {
                    last_usage = Some(u.clone());
                }
                if let Some(choice) = parsed
                    .get("choices")
                    .and_then(Value::as_array)
                    .and_then(|a| a.first())
                {
                    if let Some(fr) = choice.get("finish_reason") {
                        if fr.is_null() {
                            last_finish = None;
                        } else if let Some(s) = fr.as_str() {
                            if !s.is_empty() {
                                last_finish = Some(s.to_string());
                            }
                        }
                    }
                }
            }
        }

        // trailing partial without blank-line terminator
        if !buffer.is_empty() {
            let text = String::from_utf8_lossy(&buffer).to_string();
            if let Some(payload) = sse_data_payload(&text) {
                if payload.trim() == "[DONE]" {
                    saw_done = true;
                } else if let Ok(parsed) = serde_json::from_str::<Value>(payload) {
                    if let Some(choice) = parsed
                        .get("choices")
                        .and_then(Value::as_array)
                        .and_then(|a| a.first())
                    {
                        if let Some(fr) = choice.get("finish_reason").and_then(Value::as_str) {
                            if !fr.is_empty() {
                                last_finish = Some(fr.to_string());
                            }
                        }
                    }
                    if let Some(u) = parsed.get("usage") {
                        last_usage = Some(u.clone());
                    }
                }
            }
        }

        let (prompt_tokens, completion_tokens) = match &last_usage {
            Some(u) => (
                u.get("prompt_tokens").and_then(Value::as_i64).unwrap_or(0),
                u.get("completion_tokens").and_then(Value::as_i64).unwrap_or(0),
            ),
            None => (0, 0),
        };
        // ponytail: truncation = no [DONE]/finish_reason; empty stream with 0 tokens also truncated
        let done = saw_done || last_finish.as_deref().is_some_and(|s| !s.is_empty());
        let truncated = !done;
        let empty_content = !truncated && completion_tokens == 0 && chunks <= 4;
        let final_truncated = truncated || empty_content;
        let overflow = llmux_core::context::lookup_context_length(&model)
            .is_some_and(|limit| (prompt_tokens as u64) > limit);
        let latency_ms = start.elapsed().as_millis() as i64;

        tracing::debug!(
            "[openai:{model}] stream complete: done={done} saw_done={saw_done} finish={:?} chunks={chunks} buffer_remaining={} truncated={final_truncated} overflow={overflow} usage=({prompt_tokens},{completion_tokens})",
            last_finish,
            buffer.len()
        );
        if final_truncated {
            tracing::warn!(
                "[openai:{model}] stream truncated: account={} finish_reason=null chunks={} saw_done={} empty={empty_content} overflow={overflow}",
                account.alias,
                chunks,
                saw_done
            );
        }

        spawn_log_usage(
            pool.clone(),
            account.clone(),
            model.clone(),
            account.provider_id.clone(),
            prompt_tokens,
            completion_tokens,
            0,
            0,
            latency_ms,
            !final_truncated,
            if final_truncated {
                Some(format!(
                    "truncated: finish_reason=null chunks={chunks} saw_done={saw_done} empty={empty_content} overflow={overflow}"
                ))
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
        spawn_log_usage(
            pool.clone(),
            (*account).clone(),
            model.to_string(),
            account.provider_id.clone(),
            usage["input_tokens"].as_i64().unwrap_or(0),
            usage["output_tokens"].as_i64().unwrap_or(0),
            0,
            0,
            start.elapsed().as_millis() as i64,
            true,
            None,
        );
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
                        spawn_log_usage(
                            pool.clone(),
                            account.clone(),
                            model.clone(),
                            account.provider_id.clone(),
                            prompt,
                            completion,
                            0,
                            0,
                            start.elapsed().as_millis() as i64,
                            true,
                            None,
                        );
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
