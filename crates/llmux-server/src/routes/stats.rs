use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use llmux_core::perf;
use llmux_core::usage::{UsageFilter, UsageService};
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
    /// 按网关密钥筛选。缺省 = 全部。
    ///
    /// 只能筛到本列有值的行：usage_logs.api_key_id 是 0026 才加的，
    /// 更早的请求根本没记是哪把密钥，筛老时间窗必然是空。
    pub key_id: Option<i64>,
    /// 绑定账户 id。空 = 全部。
    pub account_id: Option<i64>,
    /// 客户端请求的别名（ordinary 按 requested_model，aggregate 按 aggregate_alias）。
    pub alias: Option<String>,
    /// ordinary | aggregate，由前端显式传；不按名字猜。
    pub alias_kind: Option<String>,
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
    /// 按网关密钥筛选，见 StatsQuery::key_id。
    pub key_id: Option<i64>,
    /// 账户 / 别名归属筛选，与 /api/stats 同口径。
    pub account_id: Option<i64>,
    pub alias: Option<String>,
    pub alias_kind: Option<String>,
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
    let filter = UsageFilter {
        account_id: params.account_id,
        alias: params.alias.clone(),
        alias_kind: params.alias_kind.clone(),
    };
    let (summary, by_model, by_account, by_provider, timeseries) = tokio::join!(
        svc.get_summary(Some(start), Some(end), params.key_id, &filter),
        svc.get_breakdown_by_model(Some(start), Some(end), params.key_id, &filter),
        svc.get_breakdown_by_account(Some(start), Some(end), params.key_id, &filter),
        svc.get_breakdown_by_provider(Some(start), Some(end), params.key_id, &filter),
        svc.get_timeseries(Some(start), Some(end), granularity, params.key_id, &filter),
    );

    // 只在选中别名时扫候选维度，避免无筛选页面白做一次 account×model 聚合。
    let by_candidate = match &params.alias {
        Some(alias) => match svc
            .get_breakdown_by_candidate(
                Some(start),
                Some(end),
                params.key_id,
                params.account_id,
                alias,
                params.alias_kind.as_deref().unwrap_or("ordinary"),
            )
            .await
        {
            Ok(rows) => rows,
            Err(e) => return simple_error(format!("Failed to get candidate breakdown: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
        },
        None => Vec::new(),
    };

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
        "byCandidate": by_candidate,
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
            api_key_id: params.key_id,
            account_id: params.account_id,
            alias: params.alias.clone(),
            alias_kind: params.alias_kind.clone(),
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
            api_key_id: params.key_id,
            account_id: params.account_id,
            alias: params.alias,
            alias_kind: params.alias_kind,
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
                "requestedModel": r.requested_model,
                "aggregateAlias": r.aggregate_alias,
                "candidateIdx": r.candidate_idx,
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

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct PerformanceQuery {
    pub start: Option<i64>,
    pub end: Option<i64>,
    pub granularity: Option<i64>,
    pub key_id: Option<i64>,
    pub account_id: Option<i64>,
    pub alias: Option<String>,
    pub alias_kind: Option<String>,
    pub model: Option<String>,
    /// model | account_model；缺省时按筛选条件推断。
    pub group_by: Option<String>,
}

/// 流式性能历史：当前/前一窗口的行级指标 + PerfRegistry 实时快照。
pub async fn get_performance(
    Extension(state): Extension<AppState>,
    Query(params): Query<PerformanceQuery>,
) -> Response {
    let end = params.end.unwrap_or_else(now_ms);
    let start = params.start.unwrap_or_else(|| end.saturating_sub(DAY_MS));
    if start > end {
        return simple_error("start must be <= end", StatusCode::BAD_REQUEST);
    }
    let group_by = params.group_by.unwrap_or_else(|| {
        if params.account_id.is_some() || params.alias_kind.as_deref() == Some("aggregate") {
            "account_model".to_string()
        } else {
            "model".to_string()
        }
    });
    let granularity = params.granularity.unwrap_or_else(|| auto_granularity(start, end));
    let filter = UsageFilter {
        account_id: params.account_id,
        alias: params.alias.clone(),
        alias_kind: params.alias_kind.clone(),
    };
    let data = match UsageService::new(state.pool.clone())
        .get_performance(
            Some(start),
            Some(end),
            params.key_id,
            &filter,
            params.model.as_deref(),
            &group_by,
            granularity,
        )
        .await
    {
        Ok(v) => v,
        Err(e) => return simple_error(format!("Failed to get performance: {e}"), StatusCode::INTERNAL_SERVER_ERROR),
    };
    let live = match filtered_live(
        &state,
        PerfQuery {
            account_id: params.account_id,
            alias: params.alias,
            alias_kind: params.alias_kind,
            model: params.model,
        },
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return simple_error(e, StatusCode::INTERNAL_SERVER_ERROR),
    };
    Json(json!({
        "granularityMs": data.granularity_ms,
        "groupBy": data.group_by,
        "rows": data.rows,
        "series": data.series,
        "live": live,
    }))
    .into_response()
}

/// 实时性能快照（进程内存，重启清零）。筛选口径与 /api/stats 一致：
/// 别名过滤先从 usage_logs 近 24h 的归属行取 (account_id, model) 集合，
/// 再拿它裁剪 registry 快照 —— 不把无关节点的 severity 混进别名卡片。
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct PerfQuery {
    pub account_id: Option<i64>,
    pub alias: Option<String>,
    pub alias_kind: Option<String>,
    pub model: Option<String>,
}

pub async fn get_live_perf(
    Extension(state): Extension<AppState>,
    Query(params): Query<PerfQuery>,
) -> Response {
    match filtered_live(&state, params).await {
        Ok(live) => Json(json!({ "live": live })).into_response(),
        Err(e) => simple_error(e, StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn filtered_live(
    state: &AppState,
    params: PerfQuery,
) -> std::result::Result<Vec<perf::PerfNodeView>, String> {
    let mut live = perf::global()
        .lock()
        .unwrap()
        .snapshot_all(std::time::Instant::now());

    if let Some(account_id) = params.account_id {
        live.retain(|n| n.account_id == account_id);
    }
    if let Some(model) = &params.model {
        live.retain(|n| &n.model == model);
    }
    if let Some(alias) = &params.alias {
        let col = if params.alias_kind.as_deref() == Some("aggregate") {
            "aggregate_alias"
        } else {
            "requested_model"
        };
        let cutoff = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
            - 24 * 60 * 60 * 1000;
        let sql = format!(
            "SELECT DISTINCT account_id, model FROM usage_logs WHERE {col} = ? AND timestamp >= ?"
        );
        let rows: Vec<(Option<i64>, Option<String>)> = sqlx::query_as(&sql)
            .bind(alias)
            .bind(cutoff)
            .fetch_all(&state.pool)
            .await
            .map_err(|e| format!("Failed to resolve alias for live perf: {e}"))?;
        let allowed: Vec<(i64, String)> = rows
            .into_iter()
            .filter_map(|(a, m)| a.zip(m))
            .collect();
        live.retain(|n| allowed.iter().any(|(a, m)| *a == n.account_id && m == &n.model));
    }
    Ok(live)
}
