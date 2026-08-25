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
    pub granularity: Option<i64>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct LogsQuery {
    pub start: Option<i64>,
    pub end: Option<i64>,
    pub model: Option<String>,
    pub provider: Option<String>,
    /// Accepts 1/0 or true/false (the UI sends 1/0; serde bool would 400).
    pub success: Option<String>,
    /// Accepts 1/0 or true/false (the UI sends 1/0; serde bool would 400).
    pub is_stream: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

fn parse_flag(v: &Option<String>) -> Option<bool> {
    match v.as_deref() {
        Some("1") | Some("true") => Some(true),
        Some("0") | Some("false") => Some(false),
        _ => None,
    }
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

    let granularity = params
        .granularity
        .unwrap_or_else(|| auto_granularity(start, end));

    let svc = UsageService::new(state.pool.clone());
    let (summary, by_model, by_account, by_provider, timeseries) = tokio::join!(
        svc.get_summary(Some(start), Some(end)),
        svc.get_breakdown_by_model(Some(start), Some(end)),
        svc.get_breakdown_by_account(Some(start), Some(end)),
        svc.get_breakdown_by_provider(Some(start), Some(end)),
        svc.get_timeseries(Some(start), Some(end), granularity),
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
    let timeseries = match timeseries {
        Ok(v) => v,
        Err(e) => return simple_error(format!("Failed to get timeseries: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };

    Json(json!({
        "summary": summary,
        "byModel": by_model,
        "byAccount": by_account,
        "byProvider": by_provider,
        "timeseries": timeseries,
        "granularityMs": granularity,
    }))
    .into_response()
}

fn auto_granularity(start: i64, end: i64) -> i64 {
    let span = (end - start).max(0);
    if span <= 2 * 60 * 60 * 1000 { 5 * 60 * 1000 }        // ≤2h → 5m
    else if span <= 48 * 60 * 60 * 1000 { 60 * 60 * 1000 } // ≤48h → 1h
    else if span <= 14 * 24 * 60 * 60 * 1000 { 6 * 60 * 60 * 1000 } // ≤14d → 6h
    else { 24 * 60 * 60 * 1000 }                           // else → 1d
}

/// Paginated detailed usage logs with optional filters.
pub async fn get_stats_logs(
    Extension(state): Extension<AppState>,
    Query(params): Query<LogsQuery>,
) -> Response {
    let limit = params.limit.unwrap_or(50).clamp(0, 200);
    let offset = params.offset.unwrap_or(0).max(0);
    let success_flag = parse_flag(&params.success);
    let stream_flag = parse_flag(&params.is_stream);

    // Count first (params fields are consumed by the logs query below).
    let total = match UsageService::new(state.pool.clone())
        .count_detailed_logs(llmux_core::usage::DetailedLogQuery {
            start_time: params.start,
            end_time: params.end,
            model: params.model.clone(),
            provider: params.provider.clone(),
            success: success_flag,
            is_stream: stream_flag,
            limit: None,
            offset: None,
        })
        .await
    {
        Ok(v) => v,
        Err(e) => return simple_error(format!("Failed to count logs: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };

    let logs = match UsageService::new(state.pool.clone())
        .get_detailed_logs(llmux_core::usage::DetailedLogQuery {
            start_time: params.start,
            end_time: params.end,
            model: params.model,
            provider: params.provider,
            success: success_flag,
            is_stream: stream_flag,
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
            // t/s：流式按生成段计时，非流式/无 TTFT 按总耗时。
            let gen_ms = match r.ttft_ms {
                Some(tt) if r.is_stream && tt < r.latency_ms => (r.latency_ms - tt).max(1),
                _ => r.latency_ms.max(1),
            };
            let tps = (r.output_tokens * 1000) as f64 / gen_ms as f64;
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
                "ttftMs": r.ttft_ms,
                "isStream": r.is_stream,
                "tps": tps,
                "success": r.success,
                "errorMessage": r.error_message,
                "isTest": r.is_test,
                "accountName": r.account_name,
                "clientIp": r.client_ip,
            })
        })
        .collect();

    Json(json!({ "logs": entries, "total": total })).into_response()
}
