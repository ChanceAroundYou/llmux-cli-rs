use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use llmux_core::usage::UsageService;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::app::AppState;
use crate::error::simple_error;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct StatsQuery {
    pub start: Option<i64>,
    pub end: Option<i64>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct LogsQuery {
    pub start: Option<i64>,
    pub end: Option<i64>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub success: Option<bool>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// Aggregate stats over a time window: summary + breakdowns by model/account/provider.
pub async fn get_stats(
    Extension(state): Extension<AppState>,
    Query(params): Query<StatsQuery>,
) -> Response {
    let end = params.end.unwrap_or_else(now_ms);
    let start = params.start.unwrap_or_else(|| end.saturating_sub(DAY_MS));
    if start > end {
        return simple_error("start must be <= end", StatusCode::BAD_REQUEST);
    }

    let svc = UsageService::new(state.pool.clone());
    let (summary, by_model, by_account, by_provider) = tokio::join!(
        svc.get_summary(Some(start), Some(end)),
        svc.get_breakdown_by_model(Some(start), Some(end)),
        svc.get_breakdown_by_account(Some(start), Some(end)),
        svc.get_breakdown_by_provider(Some(start), Some(end)),
    );

    let summary = match summary {
        Ok(s) => s,
        Err(e) => return simple_error(format!("Failed to get summary: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };
    let by_model = match by_model {
        Ok(v) => v,
        Err(e) => return simple_error(format!("Failed to get model breakdown: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };
    let by_account = match by_account {
        Ok(v) => v,
        Err(e) => return simple_error(format!("Failed to get account breakdown: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };
    let by_provider = match by_provider {
        Ok(v) => v,
        Err(e) => return simple_error(format!("Failed to get provider breakdown: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };

    Json(json!({
        "summary": summary,
        "byModel": by_model,
        "byAccount": by_account,
        "byProvider": by_provider,
    }))
    .into_response()
}

/// Paginated detailed usage logs with optional filters.
pub async fn get_stats_logs(
    Extension(state): Extension<AppState>,
    Query(params): Query<LogsQuery>,
) -> Response {
    let limit = params.limit.unwrap_or(50).clamp(0, 200);
    let offset = params.offset.unwrap_or(0).max(0);

    let logs = match UsageService::new(state.pool.clone())
        .get_detailed_logs(llmux_core::usage::DetailedLogQuery {
            start_time: params.start,
            end_time: params.end,
            model: params.model,
            provider: params.provider,
            success: params.success,
            limit: Some(limit),
            offset: Some(offset),
        })
        .await
    {
        Ok(v) => v,
        Err(e) => return simple_error(format!("Failed to get logs: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };

    let entries: Vec<Value> = logs
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "timestamp": r.timestamp,
                "accountId": r.account_id,
                "providerId": r.provider_id,
                "model": r.model,
                "inputTokens": r.input_tokens,
                "outputTokens": r.output_tokens,
                "cacheReadInputTokens": r.cache_read_input_tokens,
                "cacheCreationInputTokens": r.cache_creation_input_tokens,
                "latencyMs": r.latency_ms,
                "success": r.success,
                "errorMessage": r.error_message,
                "isTest": r.is_test,
                "accountName": r.account_name,
            })
        })
        .collect();

    Json(json!({ "logs": entries })).into_response()
}
