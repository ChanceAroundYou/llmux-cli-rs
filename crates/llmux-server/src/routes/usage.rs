use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use crate::app::AppState;

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct ActivityQuery {
    pub limit: Option<i64>,
}

/// Simple activity feed for the dashboard — recent requests without token details.
pub async fn get_activity(
    Extension(state): Extension<AppState>,
    Query(params): Query<ActivityQuery>,
) -> Response {
    let limit = params.limit.unwrap_or(50).min(200);

    let logs = match sqlx::query(
        "SELECT l.id, l.timestamp, l.model, l.success, l.latency_ms,
                l.error_message, a.alias AS account_name, a.provider_id
         FROM usage_logs l
         LEFT JOIN accounts a ON l.account_id = a.id
         WHERE l.is_test = 0
         ORDER BY l.timestamp DESC
         LIMIT ?",
    )
    .bind(limit)
    .fetch_all(&state.pool)
    .await
    {
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
        .map(|row| {
            json!({
                "id": row.try_get::<i64, _>("id").unwrap_or_default(),
                "timestamp": row.try_get::<i64, _>("timestamp").unwrap_or_default(),
                "model": row.try_get::<String, _>("model").unwrap_or_default(),
                "success": row.try_get::<i64, _>("success").unwrap_or_default(),
                "latency_ms": row.try_get::<i64, _>("latency_ms").unwrap_or_default(),
                "error_message": row.try_get::<Option<String>, _>("error_message").unwrap_or_default(),
                "account_name": row.try_get::<String, _>("account_name").unwrap_or_default(),
                "provider_id": row.try_get::<String, _>("provider_id").unwrap_or_default(),
            })
        })
        .collect();

    let total_requests: i64 = entries.len() as i64;
    let success_count: i64 = entries.iter().filter(|e| e["success"] == 1).count() as i64;

    Json(json!({
        "entries": entries,
        "totalRequests": total_requests,
        "successCount": success_count,
    }))
    .into_response()
}
