use std::convert::Infallible;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use futures_util::StreamExt;
use llmux_core::repo;
use llmux_core::repo::ActivityEntry;
use llmux_core::usage::UsageService;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_stream::wrappers::IntervalStream;
use utoipa::ToSchema;

use crate::app::AppState;

#[derive(Debug, Deserialize, Default, ToSchema)]
#[serde(default)]
pub struct ActivityQuery {
    pub limit: Option<i64>,
}

/// Simple activity feed for the dashboard — recent requests without token details.
#[utoipa::path(
    get,
    path = "/api/activity",
    responses(
        (status = 200, description = "最近活动列表（entries + totalRequests + successCount）", body = crate::api_schemas::ActivityResponse)
    )
)]
pub async fn get_activity(
    Extension(state): Extension<AppState>,
    Query(params): Query<ActivityQuery>,
) -> Response {
    let limit = params.limit.unwrap_or(50).min(200);

    let logs = match repo::list_recent_activity(&state.pool, limit).await {
        Ok(rows) => rows,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to fetch activity: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // 复用 SSE 用的同一份映射，避免两处维护字段形状
    let entries: Vec<Value> = logs.iter().map(activity_entry_json).collect();

    // totalRequests/successCount 用全表统计（entries 只是最近 N 条窗口，非真实总量）
    let summary = match UsageService::new(state.pool.clone())
        .get_summary(None, None)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to compute usage summary: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    Json(json!({
        "entries": entries,
        "totalRequests": summary.total_requests,
        "successCount": summary.success_requests,
    }))
    .into_response()
}

/// 活动条目 → JSON（字段形状与 /api/activity 一致）
fn activity_entry_json(log: &ActivityEntry) -> Value {
    json!({
        "id": log.id,
        "timestamp": log.ts,
        "model": log.model.clone().unwrap_or_default(),
        "success": log.success,
        "latency_ms": log.latency_ms,
        "error_message": log.error_message.clone(),
        "account_name": log.account_name.clone(),
    })
}

/// 最近动态 SSE 流：每 1.5s 查询 usage_logs 增量推送（data: {"entries": [...]}）。
/// 连接时把游标初始化为当前最大 id——历史数据由 /api/activity 接口提供，SSE 只推真新增；
/// 按 id 游标增量续拉（ORDER BY id ASC），突发新增超过单批上限时下个 tick 从游标继续，不丢条。
/// 对写入路径零侵入，无需广播。
#[utoipa::path(
    get,
    path = "/api/activity/stream",
    responses(
        (status = 200, description = "SSE 流：data 为新增活动数组", content_type = "text/event-stream")
    )
)]
pub async fn stream_activity(
    Extension(state): Extension<AppState>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let pool = state.pool.clone();
    // 连接时游标初始化为当前最大 id：历史由 /api/activity 接口拉取，这里只推真新增。
    // 查询瞬断时兜底到 i64::MAX（宁可少推，绝不重放整段历史）
    let initial = repo::max_activity_id(&pool).await.unwrap_or(i64::MAX);
    let last_id = Arc::new(AtomicI64::new(initial));

    let stream = IntervalStream::new(tokio::time::interval(Duration::from_millis(1500)))
        .then(move |_| {
            let pool = pool.clone();
            let last_id = last_id.clone();
            async move {
                // 增量：id > 游标按升序续拉；突发超过单批上限时下个 tick 从游标继续，不丢条
                let last = last_id.load(Ordering::Relaxed);
                let items = match repo::list_activity_since(&pool, last, 100).await {
                    Ok(rows) => rows,
                    Err(_) => Vec::new(),
                };
                if items.is_empty() {
                    // 无新增：发注释心跳（EventSource 忽略注释行，不触发 onmessage）
                    return Ok(Event::default().comment("hb"));
                }
                last_id.store(
                    items.last().map(|l| l.id).unwrap_or(last),
                    Ordering::Relaxed,
                );
                let entries: Vec<Value> = items.iter().map(activity_entry_json).collect();
                let payload =
                    serde_json::to_string(&json!({ "entries": entries })).unwrap_or_default();
                Ok(Event::default().data(payload))
            }
        });

    Sse::new(stream)
}
