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
use futures_util::StreamExt;
use serde_json::Value;

use llmux_core::adapters::{self, execute_provider_request, ProviderRequest};
use llmux_core::dispatcher::{self, get_accounts_by_ids, get_active_accounts, is_retryable_status};

use crate::app::{AppState, TuiEvent};
use crate::middleware::{self, AuthContext};

use super::helpers::{spawn_log_usage, normalize_base_url, send_tui_request};

// ---------------------------------------------------------------------------
// /v1beta/models/{model}:{action}  — Gemini native protocol passthrough
// ---------------------------------------------------------------------------

pub async fn gemini(
    Extension(state): Extension<AppState>,
    Extension(auth): Extension<AuthContext>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
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
    let model_resolution = match state.resolve_model_cached(&model_name).await {
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
        let is_custom_base = account.base_url.as_deref().is_some_and(|u| !u.is_empty());
        let default_base = "https://generativelanguage.googleapis.com/v1beta";
        let base_url = normalize_base_url(
            account
                .base_url
                .as_deref()
                .filter(|u| !u.is_empty())
                .unwrap_or(default_base),
        );

        // Rebuild URL with resolved model and API key
        let target_model = if model_resolution.target_model.starts_with("models/") {
            model_resolution.target_model.clone()
        } else {
            format!("models/{}", model_resolution.target_model)
        };
        let new_path = format!("{target_model}:{_action}");

        // Preserve x-goog-api-client from the incoming request (gemini-cli sets this).
        let goog_api_client = headers
            .get("x-goog-api-client")
            .and_then(|v| v.to_str().ok());

        // Auth: for default Google API, use x-goog-api-key header.
        // For custom proxies, use Bearer header (common proxy convention).
        let (url, req_headers) = if is_custom_base {
            let url = format!("{base_url}/{new_path}");
            let mut h = BTreeMap::from([(
                "content-type".to_string(),
                "application/json".to_string(),
            )]);
            h.insert("authorization".to_string(), format!("Bearer {}", account.api_key));
            if let Some(v) = goog_api_client {
                h.insert("x-goog-api-client".to_string(), v.to_string());
            }
            (url, h)
        } else {
            let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
            let url = format!("{base_url}/{new_path}{query}");
            let mut h = BTreeMap::from([(
                "content-type".to_string(),
                "application/json".to_string(),
            )]);
            h.insert("x-goog-api-key".to_string(), account.api_key.clone());
            if let Some(v) = goog_api_client {
                h.insert("x-goog-api-client".to_string(), v.to_string());
            }
            (url, h)
        };

        let dispatch_url = url.clone();
        let provider_request = ProviderRequest {
            method: "POST".to_string(),
            url,
            headers: req_headers,
            body: body.clone(),
        };

        tracing::info!(
            "⚡ {} → {} → {}",
            account.alias,
            model_resolution.target_model,
            base_url,
        );
        if let Some(tx) = &state.tui_tx {
            let _ = tx.send(TuiEvent::Dispatch {
                timestamp: time::OffsetDateTime::now_utc().format(&DISPATCH_TIME_FMT).unwrap_or_default(),
                account: account.alias.clone(),
                model: model_resolution.target_model.clone(),
                url: dispatch_url,
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
            send_tui_request(&state.tui_tx, uri.path(), status.as_u16(), start, &model_resolution.target_model);
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
            {
                let mut router = state.dispatch_router.lock().unwrap();
                router.record_result(&dispatch_key, &dispatch_meta, Some(account.id), true);
            }
            send_tui_request(&state.tui_tx, "/v1beta/models/...", status.as_u16(), start, &model_resolution.target_model);
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
        send_tui_request(&state.tui_tx, "/v1beta/models/...", 200, start, &model_resolution.target_model);
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
    send_tui_request(&state.tui_tx, "/v1beta/models/...", 502, start, &model_resolution.target_model);
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
    let prompt = meta["promptTokenCount"].as_i64().unwrap_or_default();
    let candidates = meta["candidatesTokenCount"].as_i64();
    let thoughts = meta["thoughtsTokenCount"].as_i64().unwrap_or_default();
    let completion = candidates.unwrap_or_else(|| {
        meta["totalTokenCount"]
            .as_i64()
            .map(|t| t.saturating_sub(prompt).saturating_sub(thoughts))
            .unwrap_or_default()
    });
    (prompt, completion)
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
        "⚡ streaming {} → {}",
        provider_id,
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
