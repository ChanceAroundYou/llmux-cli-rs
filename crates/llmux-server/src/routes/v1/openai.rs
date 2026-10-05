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
use llmux_core::protocol::{target_protocol, default_protocol_for, DownstreamMode, Protocol};
use llmux_core::proxy::openai_anthropic::{
    anthropic_to_openai_response, is_unsupported_api_for_model, AnthropicSseConverter,
};
use llmux_core::proxy::anthropic_openai::{cache_usage_from_openai, openai_to_anthropic_response, anthropic_to_openai_request};
use llmux_core::proxy::responses::{
    anthropic_to_responses, anthropic_resp_to_responses_resp, chat_resp_to_responses_resp,
    chat_to_responses, responses_to_anthropic, responses_to_chat, responses_req_to_anthropic_req,
    responses_req_to_chat_req,
};
use llmux_core::proxy::{build_anthropic_target_url, anthropic_openai::{parse_sse_chunks, sse_data_payload}};

use crate::app::{AppState, TuiEvent};
use crate::middleware::{self, AuthContext};

use super::helpers::{spawn_log_usage, spawn_log_usage_ip, normalize_base_url, send_tui_request};

// ---------------------------------------------------------------------------
// /v1/chat/completions  — pure OpenAI-compatible passthrough
// ---------------------------------------------------------------------------

pub async fn chat_completions(
    Extension(state): Extension<AppState>,
    Extension(auth): Extension<AuthContext>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Response {
    // 客户端历史消息可能带 `tool_calls: []` 或双重编码成字符串的
    // `reasoning_details`（hermes 曾踩中，DeepSeek/Console Go 上游分别以
    // minLength 1、expected array 拒绝 → 400 → 502）。透传前统一清洗。
    llmux_core::proxy::sanitize_chat_messages(&mut body);
    // Resolve early to check upstream_api — chat ingress may be routed to responses upstream
    let raw_model = body.get("model").and_then(Value::as_str).unwrap_or("");
    // Aggregate alias takes precedence over ordinary alias/prefix
    if let Ok(Some(agg)) = state.resolve_aggregate_cached(raw_model).await {
        // honor aggregate's upstream_api
        let endpoint = if agg.upstream_api.wants_responses() { "responses" } else { "chat/completions" };
        // if responses preferred, use responses dispatch with translation
        if endpoint == "responses" {
            return dispatch_aggregate_openai_responses(state, auth, uri, headers, body, agg).await;
        }
        return dispatch_aggregate_openai(state, auth, uri, headers, body, "chat/completions", agg).await;
    }
    // Ordinary alias resolved → unified per-account decision table
    // (target_protocol(Chat ingress, mode, account)).
    if let Ok(res) = state.resolve_model_cached(raw_model).await {
        return dispatch_with_conversion(Protocol::Chat, state, auth, uri, headers, body, res).await;
    }
    openai_dispatch(state, auth, uri, headers, body, "chat/completions").await
}

/// POST /v1/responses — passthrough governed by the per-account protocol
/// decision table (`target_protocol(Responses ingress, mode, account)`):
/// supported → passthrough, else convert to the account's default protocol.
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
    let model_for_check = dispatcher::sanitize_model_name(
        body.get("model").and_then(Value::as_str).unwrap_or(""),
    );
    if !model_for_check.is_empty() {
        if let Ok(res) = state.resolve_model_cached(&model_for_check).await {
            return dispatch_with_conversion(Protocol::Responses, state, auth, uri, headers, body, res).await;
        }
    }
    openai_dispatch(state, auth, uri, headers, body, "responses").await
}

async fn dispatch_aggregate_openai_responses(
    state: AppState,
    auth: AuthContext,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: Value,
    agg: llmux_core::aggregate::AggregateResolution,
) -> Response {
    dispatch_aggregate_with_conversion(Protocol::Chat, state, auth, uri, headers, body, agg).await
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
/// Unified ingress→target dispatcher (Task 6).
///
/// For the given `ingress` protocol (Chat or Messages), computes the outbound
/// `target` protocol per account via `target_protocol(ingress, mode, &account)`
/// where `mode` comes from the alias `upstream_api`. When `ingress == target`
/// the request is forwarded as-is (the proven passthrough transport). When they
/// differ, the request is converted to the target protocol, dispatched, and the
/// response is back-converted to the ingress protocol. This subsumes the old
/// `chat_via_responses` / `messages_via_responses` helpers.
pub(crate) async fn dispatch_with_conversion(
    ingress: Protocol,
    state: AppState,
    auth: AuthContext,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: Value,
    res: llmux_core::dispatcher::ModelResolution,
) -> Response {
    let is_anthropic = ingress == Protocol::Messages;
    let normalized_uri = crate::app::normalize_gateway_uri(&uri);
    let streaming = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let model_name = res.target_model.clone();
    let anthropic_beta = headers
        .get("anthropic-beta")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let mode = DownstreamMode::from_str(res.upstream_api.as_str());

    // Load accounts (identical selection to openai_dispatch / messages handler).
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

    // Patch the model field in the body to the resolved target model.
    let mut patched = body;
    patched["model"] = Value::String(model_name.clone());

    let start = Instant::now();
    let mut last_error: Option<String> = None;
    // 上游最后回的状态码。耗尽时用它决定回 429（该退避）还是 502（网关问题）——
    // 瞬时 429 不冷却，`last_error` 里没有 "cooling down"，只能靠这个判断。
    let mut last_status: Option<u16> = None;
    // 耗尽成因分类：全因配额冷却 vs 掺了别的原因（见 helpers::exhausted_status）
    let mut cooled_skips = 0usize;
    let mut failed_candidates = 0usize;

    for account in &ordered_accounts {
        // 配额/限流冷却中的账户跳过。裸模型名走的就是这条路径 —— Ling
        // 每日 100 次配额耗尽后，不跳的话每次请求都原样吃一个 429。
        if super::helpers::rate_limit_suspended(&state.pool, account.id, &model_name).await {
            tracing::debug!("⏸️  跳过 {} | 账户 {}：冷却中", model_name, account.alias);
            super::helpers::note_skip_reason(&mut last_error, format!("Account {} rate limited, cooling down", account.alias));
            cooled_skips += 1;
            continue;
        }
        // Per-account target computation (Task 6): routing decision table.
        let target = target_protocol(ingress, mode, &account);
        // Guard: a forced mode (or unsupported-ingress fallback) may target a
        // protocol this account doesn't serve — skip it instead of falling
        // through to build_passthrough's api.openai.com fallback URL.
        if !llmux_core::protocol::supports(account, target) {
            super::helpers::note_skip_reason(&mut last_error, format!("Account {} does not support {:?} target", account.alias, target));
            failed_candidates += 1;
            continue;
        }

        // Forward body + back-conversion spec for this (ingress, target) pair.
        #[derive(Clone, Copy)]
        enum Back {
            Passthrough,
            ChatFromResponses,
            ChatFromMessages,
            MessagesFromResponses,
            MessagesFromChat,
            ResponsesFromChat,
            ResponsesFromMessages,
        }
        let (forward_body, back) = match (ingress, target) {
            (Protocol::Chat, Protocol::Responses) => (Ok(chat_to_responses(&patched, &model_name)), Back::ChatFromResponses),
            (Protocol::Chat, Protocol::Messages) => (anthropic_to_openai_request(&patched, &model_name).map_err(|e| e.to_string()), Back::ChatFromMessages),
            (Protocol::Messages, Protocol::Responses) => (Ok(anthropic_to_responses(&patched, &model_name)), Back::MessagesFromResponses),
            (Protocol::Messages, Protocol::Chat) => (anthropic_to_openai_request(&patched, &model_name).map_err(|e| e.to_string()), Back::MessagesFromChat),
            (Protocol::Responses, Protocol::Chat) => (Ok(responses_req_to_chat_req(&patched)), Back::ResponsesFromChat),
            (Protocol::Responses, Protocol::Messages) => (Ok(responses_req_to_anthropic_req(&patched, &model_name)), Back::ResponsesFromMessages),
            (a, b) if a == b => (Ok(patched.clone()), Back::Passthrough),
            _ => unreachable!(),
        };
        let forward_body = match forward_body {
            Ok(b) => b,
            Err(e) => {
                last_error = Some(format!("Request conversion failed: {e}"));
                failed_candidates += 1;
                continue;
            }
        };

        // Build the provider request for the target protocol, preserving the
        // existing base_url / anthropic_base_url transport (read-only).
        let provider_request = build_target_request(account, target, &forward_body, &anthropic_beta);
        let log_url = provider_request.url.clone();
        tracing::info!("⚡ {} → {} → {} [ingress={:?} target={:?}]", account.alias, model_name, log_url, ingress, target);
        if let Some(tx) = &state.tui_tx {
            let _ = tx.send(TuiEvent::Dispatch {
                timestamp: time::OffsetDateTime::now_utc().format(&DISPATCH_TIME_FMT).unwrap_or_default(),
                account: account.alias.clone(),
                model: model_name.clone(),
                url: log_url.clone(),
                tag: dispatch_tag.clone(),
            });
        }

        let response = match execute_provider_request(&provider_request).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("🔀 Account {} (id={}) request failed: {e}", account.alias, account.id);
                last_error = Some(format!("Provider request failed: {e}"));
                if account.id == preferred_id { let mut r = state.dispatch_router.lock().unwrap(); r.record_result(&dispatch_key, &dispatch_meta, None, false); }
                failed_candidates += 1;
                continue;
            }
        };
        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            provider_request.effort.record_rejection(&error_body);
            // 上游拒了这个档位 → 记下来，后续请求不再重复失败。
            // 没有这一步，请求前解析只生效一次，整个机制等于空操作。

            // Runtime fallback: responses unsupported → account default protocol
            // (NOT hard-coded chat). Only relevant for Default/Auto mode.
            if target == Protocol::Responses
                && llmux_core::proxy::responses::is_responses_unsupported(&error_body)
                && mode == DownstreamMode::Default
            {
                let fallback = default_protocol_for(&account);
                tracing::warn!("[dispatch_with_conversion] {} responses unsupported, fallback to {:?}", model_name, fallback);
                if fallback != Protocol::Responses {
                    // Reverse-convert the original ingress body to the fallback protocol and
                    // re-enter the main v1 dispatcher (ingress-aware, headers preserved via
                    // dispatch_with_conversion's own auth handling — pass original headers).
                    let fallback_body = match (ingress, fallback) {
                        (Protocol::Chat, Protocol::Chat) => patched.clone(),
                        (Protocol::Chat, Protocol::Messages) => match anthropic_to_openai_request(&patched, &model_name) {
                            Ok(b) => b, Err(_) => patched.clone(),
                        },
                        (Protocol::Messages, Protocol::Messages) => patched.clone(),
                        (Protocol::Messages, Protocol::Chat) => match llmux_core::proxy::anthropic_openai::anthropic_to_openai_request(&patched, &model_name) {
                            Ok(b) => b, Err(_) => patched.clone(),
                        },
                        (Protocol::Messages, Protocol::Responses) => match llmux_core::proxy::responses::anthropic_to_responses(&patched, &model_name) { b => b },
                        (Protocol::Chat, Protocol::Responses) => match llmux_core::proxy::responses::chat_to_responses(&patched, &model_name) { b => b },
                        (Protocol::Responses, Protocol::Chat) => llmux_core::proxy::responses::responses_req_to_chat_req(&patched),
                        (Protocol::Responses, Protocol::Messages) => llmux_core::proxy::responses::responses_req_to_anthropic_req(&patched, &model_name),
                        _ => patched.clone(),
                    };
                    let endpoint = match fallback {
                        Protocol::Chat => "chat/completions",
                        Protocol::Responses => "responses",
                        Protocol::Messages => "v1/messages",
                    };
                    return openai_dispatch(state, auth, uri, headers, fallback_body, endpoint).await;
                }
            }
            last_error = Some(format!("Provider returned {status}: {error_body}"));
            last_status = Some(status.as_u16());
            if is_retryable_status(status.as_u16()) {
                // 429 → 记冷却，见 anthropic.rs 同处注释。
                if status.as_u16() == 429 {
                    super::helpers::note_rate_limit(&state.pool, account.id, &model_name, &error_body).await;
                }
                if account.id == preferred_id { let mut r = state.dispatch_router.lock().unwrap(); r.record_result(&dispatch_key, &dispatch_meta, None, false); }
                failed_candidates += 1;
                continue;
            }
            let latency_ms = start.elapsed().as_millis() as i64;
            spawn_log_usage(state.pool.clone(), (*account).clone(), model_name.clone(), res.provider_id.clone(), 0, 0, 0, 0, latency_ms, false, last_error.clone(), Some(forward_body.to_string()), None, Some(latency_ms), false);
            send_tui_request(&state.tui_tx, normalized_uri.path(), status.as_u16(), start, &model_name);
            if let Ok(v) = serde_json::from_str::<Value>(&error_body) { return (status, Json(v)).into_response(); }
            return (status, error_body).into_response();
        }

        // Success — back-convert the response according to (ingress, target).
        // 发出去的那档被接受了 → 记下来。缺这一步未知 provider 永远学不会
        // 「自己其实能用哪几档」，fail-closed 就变成永久失能而不是一轮降级。
        provider_request.effort.record_success();
        match (back, streaming) {
            (Back::Passthrough, true) => {
                if is_anthropic {
                    return super::anthropic::anthropic_streaming_passthrough(response, &model_name, account, state.pool.clone(), &account.provider_id, start, Some(forward_body.to_string())).await;
                }
                return openai_streaming_passthrough(response, &model_name, account, state.pool.clone(), start, Some(forward_body.to_string())).await;
            }
            (Back::Passthrough, false) => {
                let body_bytes = match response.bytes().await { Ok(b) => b, Err(e) => { last_error = Some(format!("Failed to read response: {e}")); failed_candidates += 1; continue; } };
                let data: Value = match serde_json::from_slice(&body_bytes) { Ok(v) => v, Err(e) => { last_error = Some(format!("Failed to parse response: {e}")); failed_candidates += 1; continue; } };
                let latency_ms = start.elapsed().as_millis() as i64;
                let (prompt_tokens, completion_tokens, cache_read, cache_create) = passthrough_usage(&data);
                spawn_log_usage(state.pool.clone(), (*account).clone(), model_name.clone(), res.provider_id.clone(), prompt_tokens, completion_tokens, cache_read, cache_create, latency_ms, true, None, Some(forward_body.to_string()), Some(data.to_string()), Some(latency_ms), false);
                { let mut r = state.dispatch_router.lock().unwrap(); r.record_result(&dispatch_key, &dispatch_meta, Some(account.id), true); }
                send_tui_request(&state.tui_tx, normalized_uri.path(), 200, start, &model_name);
                return Json(data).into_response();
            }
            (Back::ChatFromResponses, true) => {
                { let mut r = state.dispatch_router.lock().unwrap(); r.record_result(&dispatch_key, &dispatch_meta, Some(account.id), true); }
                send_tui_request(&state.tui_tx, normalized_uri.path(), 200, start, &model_name);
                return responses_to_chat_streaming(response, &model_name, account, state.pool.clone(), start, Some(forward_body.to_string())).await;
            }
            (Back::ChatFromResponses, false) => {
                let body_bytes = match response.bytes().await { Ok(b) => b, Err(e) => { last_error = Some(format!("Failed to read response: {e}")); failed_candidates += 1; continue; } };
                let data: Value = match serde_json::from_slice(&body_bytes) { Ok(v) => v, Err(e) => { last_error = Some(format!("Failed to parse response: {e}")); failed_candidates += 1; continue; } };
                let chat_resp = responses_to_chat(&data, &model_name);
                let (raw_prompt, completion_tokens) = adapters::usage_from_openai_response_body(&chat_resp);
                let (cache_read, _) = cache_usage_from_openai(&data["usage"]);
                let prompt_tokens = (raw_prompt - cache_read).max(0);
                let cache_create = 0;
                let latency_ms = start.elapsed().as_millis() as i64;
                spawn_log_usage(state.pool.clone(), (*account).clone(), model_name.clone(), res.provider_id.clone(), prompt_tokens, completion_tokens, cache_read, cache_create, latency_ms, true, None, Some(forward_body.to_string()), Some(chat_resp.to_string()), Some(latency_ms), false);
                { let mut r = state.dispatch_router.lock().unwrap(); r.record_result(&dispatch_key, &dispatch_meta, Some(account.id), true); }
                send_tui_request(&state.tui_tx, normalized_uri.path(), 200, start, &model_name);
                return Json(chat_resp).into_response();
            }
            // Cross-protocol Responses ingress runs buffered end-to-end (the request
            // converters drop `stream`, so upstream returned JSON): back-convert once
            // and reply non-streaming. ponytail ceiling: SSE for Responses→{Chat,Messages}
            // needs dedicated state machines; add when a client needs live tokens here.
            (Back::ResponsesFromChat, _) => {
                let body_bytes = match response.bytes().await { Ok(b) => b, Err(e) => { last_error = Some(format!("Failed to read response: {e}")); failed_candidates += 1; continue; } };
                let data: Value = match serde_json::from_slice(&body_bytes) { Ok(v) => v, Err(e) => { last_error = Some(format!("Failed to parse response: {e}")); failed_candidates += 1; continue; } };
                let resp_body = chat_resp_to_responses_resp(&data, &model_name);
                let (raw_prompt, completion_tokens) = adapters::usage_from_openai_response_body(&data);
                let (cache_read, _) = cache_usage_from_openai(&data["usage"]);
                let prompt_tokens = (raw_prompt - cache_read).max(0);
                let cache_create = 0;
                let latency_ms = start.elapsed().as_millis() as i64;
                spawn_log_usage(state.pool.clone(), (*account).clone(), model_name.clone(), res.provider_id.clone(), prompt_tokens, completion_tokens, cache_read, cache_create, latency_ms, true, None, Some(forward_body.to_string()), Some(resp_body.to_string()), Some(latency_ms), false);
                { let mut r = state.dispatch_router.lock().unwrap(); r.record_result(&dispatch_key, &dispatch_meta, Some(account.id), true); }
                send_tui_request(&state.tui_tx, normalized_uri.path(), 200, start, &model_name);
                return Json(resp_body).into_response();
            }
            (Back::ResponsesFromMessages, _) => {
                let body_bytes = match response.bytes().await { Ok(b) => b, Err(e) => { last_error = Some(format!("Failed to read response: {e}")); failed_candidates += 1; continue; } };
                let data: Value = match serde_json::from_slice(&body_bytes) { Ok(v) => v, Err(e) => { last_error = Some(format!("Failed to parse response: {e}")); failed_candidates += 1; continue; } };
                let resp_body = anthropic_resp_to_responses_resp(&data, &model_name);
                let (prompt_tokens, completion_tokens) = (data["usage"]["input_tokens"].as_i64().unwrap_or(0), data["usage"]["output_tokens"].as_i64().unwrap_or(0));
                let cache_read = data["usage"]["cache_read_input_tokens"].as_i64().unwrap_or(0);
                let cache_create = data["usage"]["cache_creation_input_tokens"].as_i64().unwrap_or(0);
                let latency_ms = start.elapsed().as_millis() as i64;
                spawn_log_usage(state.pool.clone(), (*account).clone(), model_name.clone(), res.provider_id.clone(), prompt_tokens, completion_tokens, cache_read, cache_create, latency_ms, true, None, Some(forward_body.to_string()), Some(resp_body.to_string()), Some(latency_ms), false);
                { let mut r = state.dispatch_router.lock().unwrap(); r.record_result(&dispatch_key, &dispatch_meta, Some(account.id), true); }
                send_tui_request(&state.tui_tx, normalized_uri.path(), 200, start, &model_name);
                return Json(resp_body).into_response();
            }
            (Back::ChatFromMessages, true) => {
                { let mut r = state.dispatch_router.lock().unwrap(); r.record_result(&dispatch_key, &dispatch_meta, Some(account.id), true); }
                send_tui_request(&state.tui_tx, normalized_uri.path(), 200, start, &model_name);
                return super::anthropic::anthropic_to_openai_streaming(response, &model_name, account, state.pool.clone(), &account.provider_id, start, Some(forward_body.to_string())).await;
            }
            (Back::ChatFromMessages, false) => {
                let body_bytes = match response.bytes().await { Ok(b) => b, Err(e) => { last_error = Some(format!("Failed to read response: {e}")); failed_candidates += 1; continue; } };
                let data: Value = match serde_json::from_slice(&body_bytes) { Ok(v) => v, Err(e) => { last_error = Some(format!("Failed to parse response: {e}")); failed_candidates += 1; continue; } };
                let chat_resp = anthropic_to_openai_response(&data, &model_name);
                let (raw_prompt, completion_tokens) = adapters::usage_from_openai_response_body(&chat_resp);
                let (cache_read, _) = cache_usage_from_openai(&chat_resp["usage"]);
                let prompt_tokens = (raw_prompt - cache_read).max(0);
                let cache_create = 0;
                let latency_ms = start.elapsed().as_millis() as i64;
                spawn_log_usage(state.pool.clone(), (*account).clone(), model_name.clone(), res.provider_id.clone(), prompt_tokens, completion_tokens, cache_read, cache_create, latency_ms, true, None, Some(forward_body.to_string()), Some(chat_resp.to_string()), Some(latency_ms), false);
                { let mut r = state.dispatch_router.lock().unwrap(); r.record_result(&dispatch_key, &dispatch_meta, Some(account.id), true); }
                send_tui_request(&state.tui_tx, normalized_uri.path(), 200, start, &model_name);
                return Json(chat_resp).into_response();
            }
            (Back::MessagesFromResponses, true) => {
                { let mut r = state.dispatch_router.lock().unwrap(); r.record_result(&dispatch_key, &dispatch_meta, Some(account.id), true); }
                send_tui_request(&state.tui_tx, normalized_uri.path(), 200, start, &model_name);
                return super::anthropic::responses_to_anthropic_streaming(response, &model_name, account, state.pool.clone(), &account.provider_id, start, Some(forward_body.to_string())).await;
            }
            (Back::MessagesFromResponses, false) => {
                let body_bytes = match response.bytes().await { Ok(b) => b, Err(e) => { last_error = Some(format!("Failed to read response: {e}")); failed_candidates += 1; continue; } };
                let data: Value = match serde_json::from_slice(&body_bytes) { Ok(v) => v, Err(e) => { last_error = Some(format!("Failed to parse response: {e}")); failed_candidates += 1; continue; } };
                let anth_resp = responses_to_anthropic(&data, &model_name);
                let (input_tokens, output_tokens, cache_read, cache_create) = anthropic_usage(&data);
                let latency_ms = start.elapsed().as_millis() as i64;
                spawn_log_usage(state.pool.clone(), (*account).clone(), model_name.clone(), res.provider_id.clone(), input_tokens, output_tokens, cache_read, cache_create, latency_ms, true, None, Some(forward_body.to_string()), Some(anth_resp.to_string()), Some(latency_ms), false);
                { let mut r = state.dispatch_router.lock().unwrap(); r.record_result(&dispatch_key, &dispatch_meta, Some(account.id), true); }
                send_tui_request(&state.tui_tx, normalized_uri.path(), 200, start, &model_name);
                return Json(anth_resp).into_response();
            }
            (Back::MessagesFromChat, true) => {
                { let mut r = state.dispatch_router.lock().unwrap(); r.record_result(&dispatch_key, &dispatch_meta, Some(account.id), true); }
                send_tui_request(&state.tui_tx, normalized_uri.path(), 200, start, &model_name);
                return super::anthropic::anthropic_to_openai_streaming(response, &model_name, account, state.pool.clone(), &account.provider_id, start, Some(forward_body.to_string())).await;
            }
            (Back::MessagesFromChat, false) => {
                let body_bytes = match response.bytes().await { Ok(b) => b, Err(e) => { last_error = Some(format!("Failed to read response: {e}")); failed_candidates += 1; continue; } };
                let data: Value = match serde_json::from_slice(&body_bytes) { Ok(v) => v, Err(e) => { last_error = Some(format!("Failed to parse response: {e}")); failed_candidates += 1; continue; } };
                let anth_resp = openai_to_anthropic_response(&data, &model_name);
                let (raw_prompt, completion_tokens) = adapters::usage_from_openai_response_body(&data);
                let (cache_read, _) = cache_usage_from_openai(&data["usage"]);
                let prompt_tokens = (raw_prompt - cache_read).max(0);
                let cache_create = 0;
                let latency_ms = start.elapsed().as_millis() as i64;
                spawn_log_usage(state.pool.clone(), (*account).clone(), model_name.clone(), res.provider_id.clone(), prompt_tokens, completion_tokens, cache_read, cache_create, latency_ms, true, None, Some(forward_body.to_string()), Some(anth_resp.to_string()), Some(latency_ms), false);
                { let mut r = state.dispatch_router.lock().unwrap(); r.record_result(&dispatch_key, &dispatch_meta, Some(account.id), true); }
                send_tui_request(&state.tui_tx, normalized_uri.path(), 200, start, &model_name);
                return Json(anth_resp).into_response();
            }
        }
    }
    { let mut r = state.dispatch_router.lock().unwrap(); r.record_result(&dispatch_key, &dispatch_meta, None, false); }
    let error_msg = last_error.clone().unwrap_or_else(|| "All accounts exhausted".to_string());
    // 耗尽也要落库 —— 之前这条路径不记，前端失败率面板完全看不到这些 502。
    if let Some(account) = ordered_accounts.first() {
        let latency_ms = start.elapsed().as_millis() as i64;
        spawn_log_usage(state.pool.clone(), (*account).clone(), model_name.clone(), res.provider_id.clone(), 0, 0, 0, 0, latency_ms, false, Some(error_msg.clone()), Some(patched.to_string()), None, Some(latency_ms), false);
    }
    let exhausted_status = super::helpers::exhausted_status(
        cooled_skips,
        cooled_skips + failed_candidates,
        last_status,
    );
    send_tui_request(&state.tui_tx, normalized_uri.path(), if exhausted_status == Some(429) { 429 } else { 502 }, start, &model_name);
    // 全部账户都因 429 耗尽（冷却跳过 or 上游回 429）→ 回 429 + Retry-After，
    // 让调用方退避。502 对它是「网关坏了」，会立刻重试再吃一次 429。
    if exhausted_status == Some(429) {
        return super::helpers::rate_limited_response(&error_msg, is_anthropic);
    }
    middleware::send_error(&error_msg, "upstream_error", StatusCode::BAD_GATEWAY, is_anthropic)
}

/// Build the upstream `ProviderRequest` for a given target protocol.
///
/// Delegates to `adapters::build_passthrough_with_beta` so the new
/// `chat/responses/messages_endpoint` columns (with legacy `base_url` fallback
/// via `protocol::endpoint_for`) are honored. Anthropic headers are set when
/// the target is `Messages`.
fn build_target_request(
    account: &adapters::Account,
    target: Protocol,
    body: &Value,
    anthropic_beta: &Option<String>,
) -> ProviderRequest {
    adapters::build_passthrough_with_beta(account, target, body, anthropic_beta.as_deref())
}

/// Extract (input, output, cache_read, cache_create) tokens from an Anthropic
/// (Messages) usage object.
fn anthropic_usage(data: &Value) -> (i64, i64, i64, i64) {
    let usage = &data["usage"];
    (
        usage["input_tokens"].as_i64().unwrap_or(0),
        usage["output_tokens"].as_i64().unwrap_or(0),
        usage["cache_read_input_tokens"].as_i64().unwrap_or(0),
        usage["cache_creation_input_tokens"].as_i64().unwrap_or(0),
    )
}

/// Usage for same-protocol passthrough (no back-conversion): upstream `data`
/// is returned verbatim, so its shape matches the ingress protocol. Detect the
/// shape and extract cache tokens accordingly.
fn passthrough_usage(data: &Value) -> (i64, i64, i64, i64) {
    let usage = &data["usage"];
    if !usage.is_object() {
        return (0, 0, 0, 0);
    }
    // 4-store-3-display: Anthropic native already orthogonal (has cache_*
    // fields) — trust directly. OpenAI/Responses: fresh = prompt - read,
    // creation always 0 for non-Anthropic.
    if usage.get("cache_read_input_tokens").is_some()
        || usage.get("cache_creation_input_tokens").is_some()
    {
        return (
            usage["input_tokens"].as_i64().unwrap_or(0),
            usage["output_tokens"].as_i64().unwrap_or(0),
            usage["cache_read_input_tokens"].as_i64().unwrap_or(0),
            usage["cache_creation_input_tokens"].as_i64().unwrap_or(0),
        );
    }
    if usage.get("input_tokens").is_some() {
        let raw = usage["input_tokens"].as_i64().unwrap_or(0);
        let out = usage["output_tokens"].as_i64().unwrap_or(0);
        let (read, _) = cache_usage_from_openai(usage);
        let fresh = (raw - read).max(0);
        return (fresh, out, read, 0);
    }
    let (raw_prompt, completion_tokens) = adapters::usage_from_openai_response_body(data);
    let (read, _) = cache_usage_from_openai(usage);
    let fresh = (raw_prompt - read).max(0);
    (fresh, completion_tokens, read, 0)
}

/// Unified aggregate dispatch with per-candidate target computation (Task 6).
/// Handles ingress Chat or Messages routed to the Responses upstream (computing
/// the target protocol per candidate), back-translating responses→chat or
/// responses→anthropic on the way out.
pub(crate) async fn dispatch_aggregate_with_conversion(
    ingress: Protocol,
    state: AppState,
    _auth: AuthContext,
    uri: axum::http::Uri,
    _headers: HeaderMap,
    body: Value,
    agg: llmux_core::aggregate::AggregateResolution,
) -> Response {
    let normalized_uri = crate::app::normalize_gateway_uri(&uri);
    let is_anthropic = ingress == Protocol::Messages;
    let streaming = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let len = agg.candidates.len();
    let alias = agg.alias.clone();
    let active = agg.active.min(len.saturating_sub(1));
    let mode = DownstreamMode::from_str(agg.upstream_api.as_str());
    let start = Instant::now();
    let mut last_error: Option<String> = None;
    let mut last_status: Option<u16> = None;
    // 耗尽成因分类：全因配额冷却 vs 掺了别的原因（见 helpers::exhausted_status）
    let mut cooled_skips = 0usize;
    let mut failed_candidates = 0usize;
    let mut hit_index: Option<usize> = None;
    let mut hit_stream_resp: Option<reqwest::Response> = None;
    let mut hit_data: Option<Value> = None;
    let mut hit_account: Option<adapters::Account> = None;
    for i in active..len {
        let cand = &agg.candidates[i];
        // 配额/限流冷却中的候选跳过，见 670 行同处注释。
        if crate::routes::v1::helpers::rate_limit_suspended(&state.pool, cand.account_id, &cand.model).await {
            tracing::debug!("⏸️  [agg:{}] 跳过 {} | 账户 {}：冷却中", alias, cand.model, cand.account_id);
            crate::routes::v1::helpers::note_skip_reason(&mut last_error, format!("Candidate {} ({}) rate limited, cooling down", i, cand.model));
            cooled_skips += 1;
            state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len);
            continue;
        }
        let account = match get_account_by_id(&state.pool, cand.account_id, &state.master_key).await {
            Ok(Some(a)) => a,
            Ok(None) => { state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len); crate::routes::v1::helpers::note_skip_reason(&mut last_error, format!("Candidate {} account {} not found", i, cand.account_id)); failed_candidates += 1; continue; }
            Err(e) => { state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len); crate::routes::v1::helpers::note_skip_reason(&mut last_error, format!("Failed to load account {}: {e}", cand.account_id)); failed_candidates += 1; continue; }
        };
        // Per-candidate target computation (Task 6).
        let target = target_protocol(ingress, mode, &account);
        if target != Protocol::Responses {
            // This candidate's account can't serve the Responses target; skip it.
            // (Explicit responses aggregates resolve uniformly in practice.)
            state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len);
            crate::routes::v1::helpers::note_skip_reason(&mut last_error, format!("Candidate {} account {} cannot serve Responses target", i, cand.account_id));
            failed_candidates += 1;
            continue;
        }
        let translated = match ingress {
            Protocol::Chat => chat_to_responses(&body, &cand.model),
            Protocol::Messages => anthropic_to_responses(&body, &cand.model),
            _ => unreachable!(),
        };
        let provider_request = build_target_request(&account, Protocol::Responses, &translated, &None);
        tracing::info!("🔀 [agg:{} V={}] {} → {} → {} [{:?}→responses]", alias, active, account.alias, cand.model, provider_request.url, ingress);
        let response = match execute_provider_request(&provider_request).await {
            Ok(r) => r,
            Err(e) => { state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len); last_error = Some(format!("Provider request failed: {e}")); failed_candidates += 1; continue; }
        };
        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            provider_request.effort.record_rejection(&error_body);
            // 上游拒了这个档位 → 记下来，后续请求不再重复失败。
            // 没有这一步，请求前解析只生效一次，整个机制等于空操作。

            // Runtime fallback: responses unsupported → candidate default protocol.
            if llmux_core::proxy::responses::is_responses_unsupported(&error_body) && mode == DownstreamMode::Default {
                let fb = default_protocol_for(&account);
                tracing::warn!("[agg:{}] candidate {} responses unsupported, default={:?}", alias, i, fb);
            }
            last_error = Some(format!("Provider returned {status}: {error_body}"));
            last_status = Some(status.as_u16());
            if is_retryable_status(status.as_u16()) {
                // 429 → 记冷却，见 anthropic.rs 同处注释。
                if status.as_u16() == 429 {
                    super::helpers::note_rate_limit(&state.pool, account.id, &cand.model, &error_body).await;
                }
                state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len); failed_candidates += 1; continue;
            }
            tracing::warn!("🔀 [agg:{}] Account {} (id={}) failed ({}) — trying next (non-retryable): {}", alias, account.alias, account.id, status.as_u16(), error_body.chars().take(200).collect::<String>());
            state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len);
            failed_candidates += 1;
            continue;
        }
        if streaming {
            state.aggregate_router.lock().unwrap().note_candidate_success(&alias, i, len);
            hit_index = Some(i); hit_account = Some(account.clone()); hit_stream_resp = Some(response); break;
        }
        let body_bytes = match response.bytes().await {
            Ok(b) => b,
            Err(e) => { last_error = Some(format!("Failed to read response: {e}")); state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len); failed_candidates += 1; continue; }
        };
        let data: Value = match serde_json::from_slice(&body_bytes) {
            Ok(v) => v,
            Err(e) => { last_error = Some(format!("Failed to parse response: {e}")); state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len); continue; }
        };
        let (converted, pt, ct, cr, cc) = match ingress {
            Protocol::Chat => {
                let chat_resp = responses_to_chat(&data, &cand.model);
                let (raw_p, c) = adapters::usage_from_openai_response_body(&chat_resp);
                let (r, _) = cache_usage_from_openai(&data["usage"]);
                let p = (raw_p - r).max(0);
                let k = 0;
                (chat_resp, p, c, r, k)
            }
            Protocol::Messages => {
                let anth_resp = responses_to_anthropic(&data, &cand.model);
                let (p, c, r, k) = anthropic_usage(&data);
                (anth_resp, p, c, r, k)
            }
            _ => unreachable!(),
        };
        let latency_ms = start.elapsed().as_millis() as i64;
        spawn_log_usage(state.pool.clone(), account.clone(), cand.model.clone(), account.provider_id.clone(), pt, ct, cr, cc, latency_ms, true, None, Some(body.to_string()), Some(converted.to_string()), Some(latency_ms), false);
        provider_request.effort.record_success();
        state.aggregate_router.lock().unwrap().note_candidate_success(&alias, i, len);
        hit_index = Some(i); hit_account = Some(account); hit_data = Some(converted); break;
    }
    if let Some(hit) = hit_index {
        let switched = state.aggregate_router.lock().unwrap().record_request_outcome(&alias, hit, len);
        if switched { tracing::info!("🔀 [agg:{}] V migrated -> {} (after 3-confirm)", alias, hit); }
        if let Some(resp) = hit_stream_resp {
            let cand_model = agg.candidates[hit].model.clone();
            let account = hit_account.unwrap();
            send_tui_request(&state.tui_tx, normalized_uri.path(), 200, start, &cand_model);
            return match ingress {
                Protocol::Chat => responses_to_chat_streaming(resp, &cand_model, &account, state.pool.clone(), start, Some(body.to_string())).await,
                Protocol::Messages => super::anthropic::responses_to_anthropic_streaming(resp, &cand_model, &account, state.pool.clone(), &account.provider_id, start, Some(body.to_string())).await,
                _ => unreachable!(),
            };
        }
        if let Some(data) = hit_data { send_tui_request(&state.tui_tx, normalized_uri.path(), 200, start, &agg.candidates[hit].model); return Json(data).into_response(); }
    }
    let switched = state.aggregate_router.lock().unwrap().record_request_all_failed(&alias, len);
    if switched { tracing::info!("🔀 [agg:{}] all failed — V reset to 0", alias); }
    let error_msg = last_error.clone().unwrap_or_else(|| "All aggregate candidates exhausted".to_string());
    // 同上：耗尽也落库，否则聚合链路的失败在前端不可见。
    let latency_ms = start.elapsed().as_millis() as i64;
    if let Some(cand) = agg.candidates.first() {
        if let Ok(Some(acc)) = get_account_by_id(&state.pool, cand.account_id, &state.master_key).await {
            spawn_log_usage(state.pool.clone(), acc, cand.model.clone(), String::new(), 0, 0, 0, 0, latency_ms, false, Some(error_msg.clone()), Some(body.to_string()), None, Some(latency_ms), false);
        }
    }
    let exhausted_status = crate::routes::v1::helpers::exhausted_status(
        cooled_skips,
        cooled_skips + failed_candidates,
        last_status,
    );
    send_tui_request(&state.tui_tx, normalized_uri.path(), if exhausted_status == Some(429) { 429 } else { 502 }, start, &agg.alias);
    // 全部候选都因 429 耗尽（冷却跳过 or 上游回 429）→ 回 429 + Retry-After，让调用方退避而不是立刻重试
    if exhausted_status == Some(429) {
        return super::helpers::rate_limited_response(&error_msg, is_anthropic);
    }
    middleware::send_error(&error_msg, "upstream_error", StatusCode::BAD_GATEWAY, is_anthropic)
}

async fn responses_to_chat_streaming(
    response: reqwest::Response,
    model: &str,
    account: &adapters::Account,
    pool: sqlx::SqlitePool,
    start: Instant,
    request_body: Option<String>,
) -> Response {
    let model = model.to_string();
    let account = account.clone();
    let (tx, rx) = mpsc::channel::<Result<Bytes, axum::Error>>(64);
    let client_ip = super::helpers::current_client_ip();
    // 同 client_ip：流式跑在 tokio::spawn 里，task-local 传不进来，必须在此捕获。
    let api_key_id = super::helpers::current_api_key_id();
    tokio::spawn(async move {
        let mut buffer: Vec<u8> = Vec::with_capacity(4096);
        let mut received: Vec<u8> = Vec::with_capacity(4096);
        let mut converter = llmux_core::proxy::responses::ResponsesToChatConverter::new(&model);
        let mut sse = response.bytes_stream();
        let mut chunks: u64 = 0;
        let mut ttft_ms: Option<i64> = None;
        while let Some(chunk) = sse.next().await {
            match chunk {
                Ok(c) => {
                    chunks += 1;
                    buffer.extend_from_slice(&c);
                    received.extend_from_slice(&c);
                    for event_text in parse_sse_chunks(&mut buffer, 0) {
                        for out in converter.feed(&event_text) {
                            let sent = tx.send(Ok(Bytes::from(out))).await.is_ok();
                            if sent && ttft_ms.is_none() {
                                ttft_ms = Some(start.elapsed().as_millis() as i64);
                            }
                            if !sent { return; }
                        }
                        if converter.is_done() { break; }
                    }
                    if converter.is_done() { break; }
                }
                Err(e) => { tracing::warn!("[responses→chat:{}] read error: {e}", model); break; }
            }
        }
        if !converter.is_done() {
            loop {
                let events = parse_sse_chunks(&mut buffer, 0);
                if events.is_empty() { break; }
                for event_text in events {
                    for out in converter.feed(&event_text) {
                        let sent = tx.send(Ok(Bytes::from(out))).await.is_ok();
                        if sent && ttft_ms.is_none() {
                            ttft_ms = Some(start.elapsed().as_millis() as i64);
                        }
                        if !sent { return; }
                    }
                    if converter.is_done() { break; }
                }
                if converter.is_done() { break; }
            }
        }
        for out in converter.finish() {
            let sent = tx.send(Ok(Bytes::from(out))).await.is_ok();
            if sent && ttft_ms.is_none() {
                ttft_ms = Some(start.elapsed().as_millis() as i64);
            }
            if !sent { return; }
        }
        let (prompt_tokens, completion_tokens) = converter.usage_tokens();
        let (cache_read, cache_create) = converter.usage_cache();
        let complete = converter.is_done() && !converter.is_failed();
        let latency_ms = start.elapsed().as_millis() as i64;
        spawn_log_usage_ip(pool.clone(), account.clone(), model.clone(), account.provider_id.clone(), prompt_tokens, completion_tokens, cache_read, cache_create, latency_ms, complete, if complete { None } else if converter.is_failed() { Some("Upstream SSE reported a failure event".to_string()) } else { Some(format!("Responses upstream ended without terminal event after {chunks} chunks")) }, request_body, Some(String::from_utf8_lossy(&received).into_owned()), ttft_ms, true, client_ip, api_key_id)
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
    // Capture the request body text up-front (body is consumed while building
    // the provider request); fed to the log-detail feature.
    let log_req_body = body.to_string();

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
    let mut last_status: Option<u16> = None;
    // 耗尽成因分类：全因配额冷却 vs 掺了别的原因（见 helpers::exhausted_status）
    let mut cooled_skips = 0usize;
    let mut failed_candidates = 0usize;

    for account in &ordered_accounts {
        // 配额/限流冷却中的账户跳过，见 183 行同处注释。
        if super::helpers::rate_limit_suspended(&state.pool, account.id, &model_resolution.target_model).await {
            tracing::debug!("⏸️  跳过 {} | 账户 {}：冷却中", model_resolution.target_model, account.alias);
            last_error = Some(format!("Account {} rate limited, cooling down", account.alias));
            cooled_skips += 1;
            continue;
        }
        // Resolve the upstream URL via the new *_endpoint columns (with legacy
        // base_url fallback via protocol::endpoint_for). Gemini keeps the
        // special /v1beta/openai base for chat; otherwise use the endpoint_for
        // fallback directly.
        let uses_chat_endpoint = ["chat/completions", "responses"].contains(&endpoint);
        let proto = if endpoint == "v1/messages" {
            llmux_core::protocol::Protocol::Messages
        } else if endpoint == "responses" {
            llmux_core::protocol::Protocol::Responses
        } else {
            llmux_core::protocol::Protocol::Chat
        };
        let base_from_new = llmux_core::protocol::endpoint_for(account, proto);
        let base_for_dispatch = if uses_chat_endpoint && account.provider_id == "gemini" {
            // Gemini OpenAI-compatible base
            base_from_new.unwrap_or("https://generativelanguage.googleapis.com/v1beta/openai")
        } else {
            base_from_new.unwrap_or(if proto == llmux_core::protocol::Protocol::Messages {
                "https://api.anthropic.com/v1"
            } else {
                "https://api.openai.com/v1"
            })
        };
        // Build headers: Messages uses x-api-key, others use Bearer
        let mut req_headers = BTreeMap::from([(
            "content-type".to_string(),
            "application/json".to_string(),
        )]);
        if proto == llmux_core::protocol::Protocol::Messages {
            // Messages endpoint needs Anthropic headers; reuse fallback build helper to get them right.
            let pr = llmux_core::adapters::build_passthrough_with_beta(account, proto, &patched_body, None);
            req_headers = pr.headers.clone();
        } else {
            req_headers.insert(
                "authorization".to_string(),
                format!("Bearer {}", account.api_key),
            );
        }
        let base_url = normalize_base_url(base_for_dispatch);
        let mut forward_body = patched_body.clone();
        let effort = llmux_core::reasoning_effort::apply_reasoning_effort(
            &mut forward_body,
            &account.provider_id,
            &model_resolution.target_model,
            proto,
        );
        let provider_request = ProviderRequest {
            method: "POST".to_string(),
            url: format!("{base_url}/{endpoint}"),
            headers: req_headers,
            body: forward_body,
            effort,
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
                failed_candidates += 1;
                continue;
            }
        };

        let status = response.status();

        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            provider_request.effort.record_rejection(&error_body);
            // 上游拒了这个档位 → 记下来，后续请求不再重复失败。
            // 没有这一步，请求前解析只生效一次，整个机制等于空操作。

            last_error = Some(format!("Provider returned {status}: {error_body}"));
            last_status = Some(status.as_u16());

            if is_retryable_status(status.as_u16()) {
                tracing::warn!(
                    "🔀 Account {} (id={}) failed ({}) — trying next...",
                    account.alias,
                    account.id,
                    status.as_u16()
                );
                // 429 → 记冷却，见 anthropic.rs 同处注释。
                if status.as_u16() == 429 {
                    super::helpers::note_rate_limit(
                        &state.pool,
                        account.id,
                        &model_resolution.target_model,
                        &last_error.clone().unwrap_or_default(),
                    )
                    .await;
                }
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
                failed_candidates += 1;
                continue;
            }
            // Some provider gateways (GitHub Copilot) serve GPT-5.x models only
            // via the Anthropic Messages endpoint and reject /chat/completions
            // with `unsupported_api_for_model`. When that happens and the
            // account has a valid anthropic_base_url, retry the request
            // through its /v1/messages endpoint (OpenAI → Anthropic conversion).
            if endpoint == "chat/completions"
                && is_unsupported_api_for_model(&error_body)
                // Copilot-style unsupported_api retry keys off the resolved Messages
                // endpoint (messages_endpoint → anthropic_base_url; base_url is
                // intentionally not a messages fallback).
                && llmux_core::protocol::endpoint_for(&account, llmux_core::protocol::Protocol::Messages).is_some()
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
                Some(log_req_body.clone()),
                None, Some(latency_ms), false);
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
            provider_request.effort.record_success();
            send_tui_request(&state.tui_tx, normalized_uri.path(), status.as_u16(), start, &model_resolution.target_model);
            return openai_streaming_passthrough(
                response,
                &model_resolution.target_model,
                account,
                state.pool.clone(),
                start,
                Some(log_req_body.clone()),
            )
            .await;
        }

        let body_bytes = match response.bytes().await {
            Ok(b) => b,
            Err(e) => {
                last_error = Some(format!("Failed to read response: {e}"));
                failed_candidates += 1;
                continue;
            }
        };

        let data: Value = match serde_json::from_slice(&body_bytes) {
            Ok(v) => v,
            Err(e) => {
                last_error = Some(format!("Failed to parse response: {e}"));
                failed_candidates += 1;
                continue;
            }
        };

        let (raw_prompt, completion_tokens) =
            adapters::usage_from_openai_response_body(&data);
        let (cache_read, _) = cache_usage_from_openai(&data["usage"]);
        let prompt_tokens = (raw_prompt - cache_read).max(0);
        let cache_create = 0;
        let latency_ms = start.elapsed().as_millis() as i64;
        provider_request.effort.record_success();
        spawn_log_usage(
            state.pool.clone(),
            (*account).clone(),
            model_resolution.target_model.clone(),
            model_resolution.provider_id.clone(),
            prompt_tokens,
            completion_tokens,
            cache_read,
            cache_create,
            latency_ms,
            true,
            None,
            Some(log_req_body.clone()),
            Some(data.to_string()), Some(latency_ms), false);
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
            Some(log_req_body.clone()),
            None, Some(latency_ms), false);
    }
    let exhausted_status = super::helpers::exhausted_status(
        cooled_skips,
        cooled_skips + failed_candidates,
        last_status,
    );
    send_tui_request(&state.tui_tx, normalized_uri.path(), if exhausted_status == Some(429) { 429 } else { 502 }, start, &model_resolution.target_model);
    if exhausted_status == Some(429) {
        return super::helpers::rate_limited_response(&error_msg, is_anthropic);
    }
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
    let mut last_status: Option<u16> = None;
    // 耗尽成因分类：全因配额冷却 vs 掺了别的原因（见 helpers::exhausted_status）
    let mut cooled_skips = 0usize;
    let mut failed_candidates = 0usize;
    let mut hit_index: Option<usize> = None;
    let mut hit_account: Option<adapters::Account> = None;
    let mut hit_data: Option<Value> = None;
    let mut hit_stream_resp: Option<reqwest::Response> = None;

    for i in active..len {
        let cand = &agg.candidates[i];
        // 真配额耗尽冷却中的候选跳过。缺这一步会让本函数每轮都重试一个已
        // 知没配额的账户：同族的 dispatch_aggregate_with_conversion(545 行)、
        // anthropic(682)、以及三条直连路径都有，唯独这条漏了。配额是按天
        // 重置的，不跳的话 30 分钟里的每个请求都在吃同一个 429。
        // 冷却由 note_rate_limit 写入，本函数自己也会写（429 分支）。
        if super::helpers::rate_limit_suspended(&state.pool, cand.account_id, &cand.model).await {
            tracing::debug!("⏸️  [agg:{}] 跳过 {} | 账户 {}：冷却中", alias, cand.model, cand.account_id);
            last_error = Some(format!("Candidate {} ({}) rate limited, cooling down", i, cand.model));
            cooled_skips += 1;
            state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len);
            continue;
        }
        let account = match get_account_by_id(&state.pool, cand.account_id, &state.master_key).await {
            Ok(Some(a)) => a,
            Ok(None) => {
                state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len);
                crate::routes::v1::helpers::note_skip_reason(&mut last_error, format!("Candidate {} account {} not found or inactive", i, cand.account_id));
                failed_candidates += 1;
                continue;
            }
            Err(e) => {
                state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len);
                last_error = Some(format!("Failed to load account {}: {e}", cand.account_id));
                failed_candidates += 1;
                continue;
            }
        };

        // Patch body model to this candidate's model
        let mut patched_body = body.clone();
        patched_body["model"] = Value::String(cand.model.clone());

        // Build provider request via new *_endpoint columns (migrated) with fallback.
        let proto = if endpoint == "v1/messages" {
            llmux_core::protocol::Protocol::Messages
        } else if endpoint == "responses" {
            llmux_core::protocol::Protocol::Responses
        } else {
            llmux_core::protocol::Protocol::Chat
        };
        let base_from_new = llmux_core::protocol::endpoint_for(&account, proto);
        let base_url = normalize_base_url(
            base_from_new.unwrap_or(if proto == llmux_core::protocol::Protocol::Messages {
                "https://api.anthropic.com/v1"
            } else if account.provider_id == "gemini" {
                "https://generativelanguage.googleapis.com/v1beta/openai"
            } else {
                "https://api.openai.com/v1"
            }),
        );
        // Headers: Messages needs x-api-key
        let mut req_headers = BTreeMap::from([("content-type".to_string(), "application/json".to_string())]);
        if proto == llmux_core::protocol::Protocol::Messages {
            let pr = llmux_core::adapters::build_passthrough_with_beta(&account, proto, &patched_body, None);
            req_headers = pr.headers.clone();
        } else {
            req_headers.insert("authorization".to_string(), format!("Bearer {}", account.api_key));
        }
        let mut agg_body = patched_body.clone();
        let effort = llmux_core::reasoning_effort::apply_reasoning_effort(
            &mut agg_body,
            &account.provider_id,
            &cand.model,
            proto,
        );
        let provider_request = ProviderRequest { method: "POST".to_string(), url: format!("{base_url}/{endpoint}"), headers: req_headers, body: agg_body, effort };

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
                failed_candidates += 1;
                continue;
            }
        };

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            provider_request.effort.record_rejection(&error_body);
            // 上游拒了这个档位 → 记下来，后续请求不再重复失败。
            // 没有这一步，请求前解析只生效一次，整个机制等于空操作。

            last_error = Some(format!("Provider returned {status}: {error_body}"));
            last_status = Some(status.as_u16());
            if is_retryable_status(status.as_u16()) {
                tracing::warn!("🔀 [agg:{}] Account {} failed ({}) — trying next...", alias, account.alias, status.as_u16());
                // 429 → 记冷却，见 anthropic.rs 同处注释。
                if status.as_u16() == 429 {
                    super::helpers::note_rate_limit(&state.pool, account.id, &cand.model, &error_body).await;
                }
                state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len);
                if let Some(tx) = &state.tui_tx { let _ = tx.send(TuiEvent::Retry { account: account.alias.clone(), status: status.as_u16(), message: error_body.clone() }); }
                failed_candidates += 1;
                continue;
            }
            if endpoint == "chat/completions" && is_unsupported_api_for_model(&error_body) && llmux_core::protocol::endpoint_for(&account, llmux_core::protocol::Protocol::Messages).is_some() {
                tracing::info!("↩️ [agg:{}] {} rejected /chat/completions — retrying via /v1/messages", alias, account.alias);
                if let Some(resp) = anthropic_fallback_response(&patched_body, &account, &cand.model, streaming, state.pool.clone(), start).await {
                    state.aggregate_router.lock().unwrap().record_request_outcome(&alias, i, len);
                    send_tui_request(&state.tui_tx, normalized_uri.path(), 200, start, &cand.model);
                    return resp;
                }
            }
            let latency_ms = start.elapsed().as_millis() as i64;
            spawn_log_usage(state.pool.clone(), account.clone(), cand.model.clone(), account.provider_id.clone(), 0, 0, 0, 0, latency_ms, false, last_error.clone(), Some(body.to_string()), None, Some(latency_ms), false);
            send_tui_request(&state.tui_tx, normalized_uri.path(), status.as_u16(), start, &cand.model);
            // Non-retryable final for this candidate — treat as candidate failure, try next candidate
            state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len);
            // For non-retryable upstream errors, we still continue to next candidate (spec: failover down the list)
            // But to avoid hiding the upstream error when all candidates fail with non-retryable, we keep last_error.
            failed_candidates += 1;
            continue;
        }

        if streaming {
            provider_request.effort.record_success();
            state.aggregate_router.lock().unwrap().note_candidate_success(&alias, i, len);
            hit_index = Some(i);
            hit_account = Some(account.clone());
            hit_stream_resp = Some(response);
            break;
        }

        let body_bytes = match response.bytes().await { Ok(b) => b, Err(e) => { last_error = Some(format!("Failed to read response: {e}")); state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len); failed_candidates += 1; continue; } };
        let data: Value = match serde_json::from_slice(&body_bytes) { Ok(v) => v, Err(e) => { last_error = Some(format!("Failed to parse response: {e}")); state.aggregate_router.lock().unwrap().note_candidate_failure(&alias, i, len); failed_candidates += 1; continue; } };
        let (prompt_tokens, completion_tokens, cache_read, cache_create) = passthrough_usage(&data);
        let latency_ms = start.elapsed().as_millis() as i64;
        provider_request.effort.record_success();
        spawn_log_usage(state.pool.clone(), account.clone(), cand.model.clone(), account.provider_id.clone(), prompt_tokens, completion_tokens, cache_read, cache_create, latency_ms, true, None, Some(body.to_string()), Some(data.to_string()), Some(latency_ms), false);
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
            return openai_streaming_passthrough(resp, &cand_model, &account, state.pool.clone(), start, Some(body.to_string())).await;
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
            spawn_log_usage(state.pool.clone(), account, agg.candidates[idx].model.clone(), String::new(), 0, 0, 0, 0, latency_ms, false, Some(error_msg.clone()), Some(body.to_string()), None, Some(latency_ms), false);
        }
    } else if len > 0 {
        // best-effort: log with alias name
        if let Ok(Some(acc)) = get_account_by_id(&state.pool, agg.candidates[0].account_id, &state.master_key).await {
            spawn_log_usage(state.pool.clone(), acc, agg.candidates[0].model.clone(), String::new(), 0, 0, 0, 0, latency_ms, false, Some(error_msg.clone()), Some(body.to_string()), None, Some(latency_ms), false);
        }
    }
    let exhausted_status = super::helpers::exhausted_status(
        cooled_skips,
        cooled_skips + failed_candidates,
        last_status,
    );
    send_tui_request(&state.tui_tx, normalized_uri.path(), if exhausted_status == Some(429) { 429 } else { 502 }, start, &agg.alias);
    // 全部候选都因 429 耗尽（冷却跳过 or 上游回 429）→ 回 429 + Retry-After，让调用方退避而不是立刻重试
    if exhausted_status == Some(429) {
        return super::helpers::rate_limited_response(&error_msg, is_anthropic);
    }
    middleware::send_error(&error_msg, "upstream_error", StatusCode::BAD_GATEWAY, is_anthropic)
}

// ---------------------------------------------------------------------------
// Streaming helpers
// ---------------------------------------------------------------------------

/// Decide whether one SSE event should be forwarded to the client.
///
/// Some upstreams close a stream with **two** consecutive
/// `finish_reason: "tool_calls"` events on empty deltas (measured: 279/279
/// `stealth/space-bunny-alpha` streams on 2026-09-26). The duplicate is not part
/// of the OpenAI streaming contract, and a strict client that clears its
/// tool-call buffer on the first terminator reads the second one as an empty
/// buffer and reports "tool_calls without a complete id and function name" —
/// blaming a defect it never received.
///
/// So: the first `tool_calls` terminator passes; a later one that repeats it and
/// carries no tool calls of its own is withheld. A terminator that *does* carry
/// tool_calls is always forwarded — that is a real new call, not a duplicate.
fn should_forward_event(event_text: &str, saw_tool_call_finish: &mut bool) -> bool {
    let Some(payload) = sse_data_payload(event_text) else {
        return true;
    };
    let Ok(parsed) = serde_json::from_str::<Value>(payload) else {
        return true;
    };
    let Some(choice) = parsed
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
    else {
        return true;
    };
    if choice.get("finish_reason").and_then(Value::as_str) != Some("tool_calls") {
        return true;
    }
    let carries_tool_calls = choice
        .get("delta")
        .and_then(|d| d.get("tool_calls"))
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty());
    if carries_tool_calls || !*saw_tool_call_finish {
        *saw_tool_call_finish = true;
        return true;
    }
    false
}

/// Per-stream accounting, fed by **every** event the upstream sends.
///
/// The invariant this type exists to enforce: **forwarding policy must never
/// decide what gets measured.** An earlier version decided forwarding first and
/// `continue`d past the bookkeeping below, so a withheld duplicate terminator
/// also silently discarded the `usage` it carried. On
/// `stealth/space-bunny-alpha` (account `command`) the duplicate terminator is
/// the norm, so every such stream was booked as 0/0 tokens and then flagged
/// `truncated` — 149 of 387 passthrough streams in a day, matching the count of
/// dropped terminators one-for-one.
#[derive(Default)]
struct StreamAccounting {
    /// Network chunks received. **Not** event count — the empty-content
    /// heuristic in `c7b57e2` was calibrated against chunk arrival.
    chunks: u64,
    saw_done: bool,
    last_finish: Option<String>,
    last_usage: Option<Value>,
    ttft_ms: Option<i64>,
    /// Set once a `finish_reason: "tool_calls"` event has gone upstream, so a
    /// second one on an empty delta is recognised as a redundant terminator.
    saw_tool_call_finish: bool,
    dropped_dup_finishes: u64,
}

/// Fold one SSE event into the accounting state.
///
/// Pure observation: it never decides forwarding and never declines an event,
/// so no caller can lose a measurement by taking an early `continue`.
fn observe_event(event_text: &str, acc: &mut StreamAccounting) {
    let Some(payload) = sse_data_payload(event_text) else {
        return;
    };
    if payload.trim() == "[DONE]" {
        acc.saw_done = true;
        return;
    }
    let Ok(parsed) = serde_json::from_str::<Value>(payload) else {
        return;
    };
    if let Some(u) = parsed.get("usage") {
        if u.is_object() {
            acc.last_usage = Some(u.clone());
        }
    }
    if let Some(choice) = parsed
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
    {
        if let Some(fr) = choice.get("finish_reason") {
            if fr.is_null() {
                acc.last_finish = None;
            } else if let Some(s) = fr.as_str() {
                if !s.is_empty() {
                    acc.last_finish = Some(s.to_string());
                }
            }
        }
    }
}

/// Account for one event, then decide whether it reaches the client.
///
/// Observation and the forwarding decision share a single call site on
/// purpose: it makes "the `continue` swallowed the bookkeeping" unrepresentable
/// rather than merely avoided. See `StreamAccounting`.
fn observe_and_should_forward(event_text: &str, acc: &mut StreamAccounting) -> bool {
    observe_event(event_text, acc);
    let forward = should_forward_event(event_text, &mut acc.saw_tool_call_finish);
    if !forward {
        acc.dropped_dup_finishes += 1;
    }
    forward
}

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
    request_body: Option<String>,
) -> Response {
    let model = model.to_string();
    let account = account.clone();

    let (tx, rx) = mpsc::channel::<Result<Bytes, axum::Error>>(64);
    let client_ip = super::helpers::current_client_ip();
    // 同 client_ip：流式跑在 tokio::spawn 里，task-local 传不进来，必须在此捕获。
    let api_key_id = super::helpers::current_api_key_id();
    tokio::spawn(async move {
        let mut buffer: Vec<u8> = Vec::with_capacity(4096);
        let mut received: Vec<u8> = Vec::with_capacity(4096);
        let mut sse = response.bytes_stream();
        let mut acc = StreamAccounting::default();

        tracing::debug!("[openai:{model}] upstream stream started (account={})", account.alias);

        while let Some(chunk) = sse.next().await {
            match chunk {
                Ok(c) => {
                    acc.chunks += 1;
                    buffer.extend_from_slice(&c);
                    received.extend_from_slice(&c);
                    // Forward only whole events, so a redundant terminator can be
                    // withheld before the client ever sees it. Partial data stays
                    // buffered until its `\n\n` arrives — verified: upstreams
                    // delimit with a blank line, so this costs no latency.
                    let mut client_gone = false;
                    for event_text in parse_sse_chunks(&mut buffer, 0) {
                        // observe_and_should_forward already folded this event
                        // into `acc` (including `saw_done` and `usage`) before
                        // returning the forwarding decision.
                        if observe_and_should_forward(&event_text, &mut acc) {
                            if tx.send(Ok(Bytes::from(event_text.into_bytes()))).await.is_ok() {
                                if acc.ttft_ms.is_none() {
                                    acc.ttft_ms = Some(start.elapsed().as_millis() as i64);
                                }
                            } else {
                                client_gone = true;
                                break;
                            }
                        }
                    }
                    if client_gone {
                        return;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "[openai:{model}] upstream stream read error: {e} (account={}, chunks={})",
                        account.alias,
                        acc.chunks
                    );
                    break;
                }
            }
        }

        // Drain any complete events still buffered. The read loop above already
        // forwarded everything it parsed; anything left here arrived in a chunk
        // that ended mid-event and only completed at EOF, so it is bookkeeping
        // only (the trailing partial below is what actually goes to the client).
        loop {
            let events = parse_sse_chunks(&mut buffer, 0);
            if events.is_empty() {
                break;
            }
            for event_text in events {
                observe_event(&event_text, &mut acc);
            }
        }

        // trailing partial without blank-line terminator — forward it too, or the
        // final event (commonly `data: [DONE]`) would never reach the client.
        if !buffer.is_empty() {
            let text = String::from_utf8_lossy(&buffer).to_string();
            let keep = observe_and_should_forward(&text, &mut acc);
            if keep {
                let _ = tx.send(Ok(Bytes::from(buffer.clone()))).await.is_ok();
            }
        }

        if acc.dropped_dup_finishes > 0 {
            tracing::debug!(
                "[openai:{model}] dropped {} redundant tool_calls terminator(s) (account={})",
                acc.dropped_dup_finishes,
                account.alias
            );
        }

        let (raw_prompt, completion_tokens) = match &acc.last_usage {
            Some(u) => (
                u.get("prompt_tokens").and_then(Value::as_i64).unwrap_or(0),
                u.get("completion_tokens").and_then(Value::as_i64).unwrap_or(0),
            ),
            None => (0, 0),
        };
        let (cache_read, _) = acc
            .last_usage
            .as_ref()
            .map(cache_usage_from_openai)
            .unwrap_or((0, 0));
        let prompt_tokens = (raw_prompt - cache_read).max(0);
        let cache_create = 0;
        // ponytail: truncation = no [DONE]/finish_reason; empty stream with 0 tokens also truncated
        let done = acc.saw_done || acc.last_finish.as_deref().is_some_and(|s| !s.is_empty());
        let truncated = !done;
        let empty_content = !truncated && completion_tokens == 0 && acc.chunks <= 4;
        let final_truncated = truncated || empty_content;
        let overflow = llmux_core::context::lookup_context_length(&model)
            .is_some_and(|limit| (prompt_tokens as u64) > limit);
        let latency_ms = start.elapsed().as_millis() as i64;

        // One INFO line per stream, deliberately body-free. The `usage=(…)`
        // pair is the early-warning signal for accounting loss: a stream that
        // completed but booked 0/0 is the shape the duplicate-terminator bug
        // produced. Keeping it at INFO makes that visible without re-enabling
        // the full-body dumps, which cost ~130MB/day.
        tracing::info!(
            "[openai:{model}] stream complete: done={done} saw_done={} finish={:?} chunks={} truncated={final_truncated} overflow={overflow} usage=({prompt_tokens},{completion_tokens})",
            acc.saw_done,
            acc.last_finish,
            acc.chunks
        );
        // This path forwards upstream bytes to the client **verbatim** — it
        // parses only for usage accounting, and never repairs a tool_call. So
        // it is the one route where a malformed tool_use block reaches the
        // client exactly as the upstream sent it, and therefore the one place
        // worth reading when a client reports "tool_calls without a complete id
        // and function name". Without this, the whole route is invisible: the
        // other three streaming paths log their full body, this one did not.
        // TRACE, not DEBUG: a full body is tens-to-hundreds of KB and at DEBUG
        // it dominated the log (129.5 of 132.6 MB on 2026-10-05). Investigation
        // needs it; the steady state does not.
        if tracing::enabled!(tracing::Level::TRACE) {
            tracing::trace!(
                "[openai:{model}] full upstream body ({} bytes, account={}): {}",
                received.len(),
                account.alias,
                String::from_utf8_lossy(&received)
            );
        }
        if final_truncated {
            tracing::warn!(
                "[openai:{model}] stream truncated: account={} finish_reason=null chunks={} saw_done={} empty={empty_content} overflow={overflow}",
                account.alias,
                acc.chunks,
                acc.saw_done
            );
        }

        spawn_log_usage_ip(
            pool.clone(),
            account.clone(),
            model.clone(),
            account.provider_id.clone(),
            prompt_tokens,
            completion_tokens,
            cache_read,
            cache_create,
            latency_ms,
            !final_truncated,
            if final_truncated {
                Some(format!(
                    "truncated: finish_reason=null chunks={} saw_done={} empty={empty_content} overflow={overflow}",
                    acc.chunks, acc.saw_done
                ))
            } else {
                None
            },
            request_body,
            Some(String::from_utf8_lossy(&received).into_owned()),
            acc.ttft_ms, true, client_ip, api_key_id,
        )
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

#[cfg(test)]
mod tests {
    use super::{observe_and_should_forward, should_forward_event, StreamAccounting};

    fn ev(delta: &str, finish: &str) -> String {
        format!("data: {{\"choices\":[{{\"index\":0,\"delta\":{delta},\"finish_reason\":{finish}}}]}}\n\n")
    }

    /// The exact shape measured from stealth/space-bunny-alpha: a well-formed
    /// tool call, then TWO `tool_calls` terminators on empty deltas. The second
    /// is what made a strict client report "tool_calls without a complete id
    /// and function name" — the call it had already delivered was fine.
    #[test]
    fn a_repeated_empty_tool_calls_terminator_is_withheld() {
        let opener = ev(
            r#"{"tool_calls":[{"index":0,"id":"c5abfdc7","type":"function","function":{"name":"Bash","arguments":""}}]}"#,
            "null",
        );
        let finish = ev("{}", r#""tool_calls""#);
        let mut seen = false;
        assert!(should_forward_event(&opener, &mut seen));
        assert!(should_forward_event(&finish, &mut seen), "first terminator passes");
        assert!(
            !should_forward_event(&finish, &mut seen),
            "duplicate terminator on an empty delta is withheld"
        );
    }

    /// A terminator that carries its own tool_calls is a real new call and must
    /// never be swallowed, however many terminators came before it.
    #[test]
    fn a_terminator_carrying_tool_calls_is_always_forwarded() {
        let finish = ev("{}", r#""tool_calls""#);
        let second_call = ev(
            r#"{"tool_calls":[{"index":1,"id":"c5abfdc8","type":"function","function":{"name":"Read","arguments":""}}]}"#,
            r#""tool_calls""#,
        );
        let mut seen = false;
        assert!(should_forward_event(&finish, &mut seen));
        assert!(should_forward_event(&second_call, &mut seen));
        assert!(should_forward_event(&second_call, &mut seen));
    }

    /// Non-tool_calls finish reasons, `[DONE]`, content and malformed payloads
    /// are untouched — the filter only ever drops the one redundant event.
    #[test]
    fn everything_except_the_duplicate_terminator_passes_through() {
        let mut seen = false;
        for e in [
            ev(r#"{"content":"hi"}"#, "null"),
            ev("{}", r#""stop""#),
            ev("{}", r#""length""#),
            "data: [DONE]\n\n".to_string(),
            "data: not-json\n\n".to_string(),
            ": keep-alive comment\n\n".to_string(),
        ] {
            assert!(should_forward_event(&e, &mut seen), "should pass: {e:?}");
        }
    }

    /// The filter must not change what a single-terminator stream looks like:
    /// the common case has exactly one and is forwarded untouched.
    #[test]
    fn a_single_terminator_stream_is_unchanged() {
        let mut seen = false;
        assert!(should_forward_event(&ev("{}", r#""tool_calls""#), &mut seen));
        assert!(seen);
    }

    /// The exact stream measured in production (2026-10-05, account `command`):
    /// a tool call, then TWO `tool_calls` terminators, with `usage` riding the
    /// **second** one — the event the filter withholds from the client.
    ///
    /// Regression test for the accounting loss: the old loop called
    /// `should_forward_event` first and `continue`d, so the withheld event's
    /// `usage` was never read. The stream was then booked as 0/0 and flagged
    /// `truncated`, 149 of 387 passthrough streams in a day. Withholding an
    /// event from the client must never withhold it from accounting.
    #[test]
    fn usage_on_a_withheld_duplicate_terminator_is_still_accounted() {
        let opener = ev(
            r#"{"tool_calls":[{"index":0,"id":"c5abfdc7","type":"function","function":{"name":"Bash","arguments":""}}]}"#,
            "null",
        );
        let first_finish = ev("{}", r#""tool_calls""#);
        let dup_finish_with_usage = "data: {\"choices\":[{\"index\":0,\"delta\":{},\
            \"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":189602,\
            \"completion_tokens\":75,\"prompt_tokens_details\":{\"cached_tokens\":188867}}}\n\n"
            .to_string();

        let mut acc = StreamAccounting::default();
        assert!(observe_and_should_forward(&opener, &mut acc));
        assert!(
            observe_and_should_forward(&first_finish, &mut acc),
            "first terminator reaches the client"
        );
        assert!(
            !observe_and_should_forward(&dup_finish_with_usage, &mut acc),
            "the duplicate terminator is withheld from the client"
        );

        // ...but its usage is not withheld from accounting.
        let usage = acc.last_usage.expect("usage must be recorded");
        assert_eq!(usage.get("prompt_tokens").and_then(|v| v.as_i64()), Some(189602));
        assert_eq!(usage.get("completion_tokens").and_then(|v| v.as_i64()), Some(75));
        assert_eq!(acc.dropped_dup_finishes, 1);
        assert!(
            !acc.saw_done,
            "the duplicate terminator carries no [DONE] marker"
        );
    }

    /// `usage` must survive regardless of which terminator carries it — the
    /// first one, the withheld second one, or a late tail event.
    #[test]
    fn usage_is_accounted_on_any_event_position() {
        let usage_event = |pt: i64| {
            format!("data: {{\"choices\":[{{\"index\":0,\"delta\":{{}}}}],\
                \"usage\":{{\"prompt_tokens\":{pt},\"completion_tokens\":5}}}}\n\n")
        };
        for (label, events) in [
            (
                "on the first terminator",
                vec![ev("{}", r#""tool_calls""#), usage_event(10)],
            ),
            (
                "on the withheld duplicate",
                vec![
                    ev("{}", r#""tool_calls""#),
                    ev("{}", r#""tool_calls""#),
                    usage_event(20),
                ],
            ),
            (
                "after [DONE]",
                vec![ev("{}", r#""tool_calls""#), usage_event(30), "data: [DONE]\n\n".to_string()],
            ),
        ] {
            let mut acc = StreamAccounting::default();
            for e in events {
                observe_and_should_forward(&e, &mut acc);
            }
            assert!(
                acc.last_usage.is_some(),
                "usage must be accounted {label}"
            );
        }
    }

    /// `[DONE]` is bookkeeping, not a forwarding artefact: it must be seen even
    /// when the event carrying it is withheld, so `done` (and therefore the
    /// truncation verdict) cannot be flipped by a forwarding decision.
    #[test]
    fn done_marker_is_accounted_even_when_withheld() {
        let mut acc = StreamAccounting::default();
        for e in [ev("{}", r#""tool_calls""#), ev("{}", r#""tool_calls""#)] {
            observe_and_should_forward(&e, &mut acc);
        }
        assert!(!acc.saw_done, "no [DONE] seen yet");
        observe_and_should_forward("data: [DONE]\n\n", &mut acc);
        assert!(acc.saw_done, "[DONE] is always observed");
    }
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
    // Resolved Messages endpoint (messages_endpoint → anthropic_base_url).
    let anthropic_base =
        llmux_core::protocol::endpoint_for(account, llmux_core::protocol::Protocol::Messages)?;

    let anthropic_body = match llmux_core::proxy::anthropic_openai::anthropic_to_openai_request(openai_body, model) {
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
        // 目标是 Anthropic Messages，没有 reasoning_effort 枚举。
        effort: Default::default(),
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
        Some(anthropic_fallback_streaming(response, model, account, pool, start, Some(openai_body.to_string())).await)
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
            Some(openai_body.to_string()),
            Some(openai_resp.to_string()), None, false);
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
    request_body: Option<String>,
) -> Response {
    let model = model.to_string();
    let account = account.clone();

    let (tx, rx) = mpsc::channel::<Result<Bytes, axum::Error>>(64);
    let client_ip = super::helpers::current_client_ip();
    // 同 client_ip：流式跑在 tokio::spawn 里，task-local 传不进来，必须在此捕获。
    let api_key_id = super::helpers::current_api_key_id();
    tokio::spawn(async move {
        let mut buffer: Vec<u8> = Vec::with_capacity(4096);
        let mut received: Vec<u8> = Vec::with_capacity(4096);
        let mut converter = AnthropicSseConverter::new(&model);
        let mut sse = response.bytes_stream();
        let mut usage: Option<Value> = None;
        let mut ttft_ms: Option<i64> = None;

        while let Some(chunk) = sse.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("↩️ upstream stream read error: {e}");
                    break;
                }
            };
            buffer.extend_from_slice(&chunk);
            received.extend_from_slice(&chunk);
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
                            let sent = tx.send(Ok(Bytes::from(format!("{line}\n\n")))).await.is_ok();
                            if sent && ttft_ms.is_none() {
                                ttft_ms = Some(start.elapsed().as_millis() as i64);
                            }
                            if !sent { return; }
                        }
                        let (prompt, completion) = {
                            let u = usage.unwrap_or_default();
                            (
                                u.get("input_tokens").and_then(Value::as_i64).unwrap_or(0),
                                u.get("output_tokens").and_then(Value::as_i64).unwrap_or(0),
                            )
                        };
                        spawn_log_usage_ip(
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
                            request_body,
                            Some(String::from_utf8_lossy(&received).into_owned()),
                            ttft_ms, true, client_ip, api_key_id,
                        );
                        return;
                    }
                    _ => {}
                }
                for line in converter.feed(&parsed) {
                    let sent = tx.send(Ok(Bytes::from(format!("{line}\n\n")))).await.is_ok();
                    if sent && ttft_ms.is_none() { ttft_ms = Some(start.elapsed().as_millis() as i64); }
                    if !sent { return; }
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
                    let sent = tx.send(Ok(Bytes::from(format!("{line}\n\n")))).await.is_ok();
                    if sent && ttft_ms.is_none() { ttft_ms = Some(start.elapsed().as_millis() as i64); }
                    if !sent { return; }
                }
            }
        }
        for line in converter.finish(usage.as_ref()) {
            // terminal — not counted as TTFT
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