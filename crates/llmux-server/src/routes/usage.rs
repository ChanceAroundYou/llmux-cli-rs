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
                l.error_message, l.input_tokens, l.output_tokens, l.cache_read_input_tokens, l.cache_creation_input_tokens, l.ttft_ms, l.is_stream,
                a.alias AS account_name, a.provider_id
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
            let cache = row.try_get::<i64, _>("cache_read_input_tokens").unwrap_or_default()
                + row.try_get::<i64, _>("cache_creation_input_tokens").unwrap_or_default();
            json!({
                "id": row.try_get::<i64, _>("id").unwrap_or_default(),
                "timestamp": row.try_get::<i64, _>("timestamp").unwrap_or_default(),
                "model": row.try_get::<String, _>("model").unwrap_or_default(),
                "success": row.try_get::<i64, _>("success").unwrap_or_default(),
                "latency_ms": row.try_get::<i64, _>("latency_ms").unwrap_or_default(),
                "input_tokens": row.try_get::<i64, _>("input_tokens").unwrap_or_default(),
                "output_tokens": row.try_get::<i64, _>("output_tokens").unwrap_or_default(),
                "cache_tokens": cache,
                "ttft_ms": row.try_get::<Option<i64>, _>("ttft_ms").unwrap_or_default(),
                "is_stream": row.try_get::<i64, _>("is_stream").unwrap_or_default(),
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

/// Log detail: request/response bodies captured at dispatch time (nullable
/// for old rows or paths without a capture point).
pub async fn get_activity_detail(
    Extension(state): Extension<AppState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Response {
    let row = match sqlx::query(
        "SELECT l.id, l.timestamp, l.model, l.success, l.latency_ms,
                l.error_message, l.input_tokens, l.output_tokens, l.cache_read_input_tokens, l.cache_creation_input_tokens, l.ttft_ms, l.is_stream,
                l.request_body, l.response_body, l.client_ip,
                a.alias AS account_name, a.provider_id
         FROM usage_logs l
         LEFT JOIN accounts a ON l.account_id = a.id
         WHERE l.id = ? AND l.is_test = 0",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            return crate::error::simple_error("Activity not found", StatusCode::NOT_FOUND);
        }
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to fetch activity detail: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let cache_tokens = row.try_get::<i64, _>("cache_read_input_tokens").unwrap_or_default();
    Json(json!({
        "id": row.try_get::<i64, _>("id").unwrap_or_default(),
        "timestamp": row.try_get::<i64, _>("timestamp").unwrap_or_default(),
        "model": row.try_get::<String, _>("model").unwrap_or_default(),
        "success": row.try_get::<i64, _>("success").unwrap_or_default(),
        "latency_ms": row.try_get::<i64, _>("latency_ms").unwrap_or_default(),
        "input_tokens": row.try_get::<i64, _>("input_tokens").unwrap_or_default(),
        "output_tokens": row.try_get::<i64, _>("output_tokens").unwrap_or_default(),
        "cache_tokens": cache_tokens,
        "ttft_ms": row.try_get::<Option<i64>, _>("ttft_ms").unwrap_or_default(),
        "is_stream": row.try_get::<i64, _>("is_stream").unwrap_or_default(),
        "error_message": row.try_get::<Option<String>, _>("error_message").unwrap_or_default(),
        "account_name": row.try_get::<String, _>("account_name").unwrap_or_default(),
        "provider_id": row.try_get::<String, _>("provider_id").unwrap_or_default(),
        "request_body": row.try_get::<Option<String>, _>("request_body").unwrap_or_default(),
        "response_body": row.try_get::<Option<String>, _>("response_body").unwrap_or_default(),
        "client_ip": row.try_get::<Option<String>, _>("client_ip").unwrap_or_default(),
    }))
    .into_response()
}
