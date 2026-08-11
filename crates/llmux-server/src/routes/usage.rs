use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use llmux_core::repo;
use llmux_core::usage::UsageService;
use serde::Deserialize;
use serde_json::{json, Value};
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

    let entries: Vec<Value> = logs
        .iter()
        .map(|log| {
            json!({
                "id": log.id,
                "timestamp": log.ts,
                "model": log.model.clone().unwrap_or_default(),
                "success": log.success,
                "latency_ms": log.latency_ms,
                "error_message": log.error_message.clone(),
                "account_name": log.account_name.clone(),
            })
        })
        .collect();

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
