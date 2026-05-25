use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde_json::{json, Value};
use sqlx::{sqlite::SqliteRow, Row};

use crate::app::AppState;

pub async fn get_models_health(Extension(state): Extension<AppState>) -> Response {
    // Match Bun backend: for each (account_id, model) group, return the LATEST
    // usage_log row's success, latency_ms as latency, error_message as error,
    // timestamp as last_checked. Also include limits_cache (JSON-parsed),
    // limits_cache_updated_at, and account alias/provider from accounts table.
    let rows: Vec<SqliteRow> = match sqlx::query(
        "SELECT u.account_id, a.provider_id, u.model, u.timestamp AS last_checked, \
                u.success, u.latency_ms AS latency, u.error_message AS error, \
                a.limits_cache, a.limits_cache_updated_at, a.alias AS account_name \
         FROM usage_logs u \
         JOIN accounts a ON u.account_id = a.id \
         WHERE u.id IN ( \
           SELECT MAX(id) FROM usage_logs GROUP BY account_id, model \
         )",
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to get model health: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let health: Vec<Value> = rows
        .iter()
        .map(|row: &SqliteRow| {
            let limits_cache_str: Option<String> =
                row.try_get("limits_cache").unwrap_or_default();
            let limits_cache: Value = limits_cache_str
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(Value::Null);
            json!({
                "account_id": row.try_get::<i64, _>("account_id").unwrap_or_default(),
                "provider_id": row.try_get::<String, _>("provider_id").unwrap_or_default(),
                "model": row.try_get::<String, _>("model").unwrap_or_default(),
                "last_checked": row.try_get::<i64, _>("last_checked").unwrap_or_default(),
                "success": row.try_get::<i64, _>("success").unwrap_or_default(),
                "latency": row.try_get::<i64, _>("latency").unwrap_or_default(),
                "error": row.try_get::<Option<String>, _>("error").unwrap_or_default(),
                "limits_cache": limits_cache,
                "limits_cache_updated_at": row.try_get::<Option<String>, _>("limits_cache_updated_at").unwrap_or_default(),
                "account_name": row.try_get::<String, _>("account_name").unwrap_or_default(),
            })
        })
        .collect();

    Json(Value::Array(health)).into_response()
}
