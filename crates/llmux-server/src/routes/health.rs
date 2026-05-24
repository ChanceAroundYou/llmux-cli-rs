use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde_json::{json, Value};
use sqlx::Row;

use crate::app::AppState;

pub async fn get_health_status(Extension(state): Extension<AppState>) -> Response {
    let accounts = match sqlx::query("SELECT id, alias FROM accounts ORDER BY id")
        .fetch_all(&state.pool)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to query accounts: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    tracing::info!("💚 Starting health check for {} accounts...", accounts.len());

    let mut health_data: Vec<Value> = Vec::new();

    for account in &accounts {
        let acc_id: i64 = account.try_get("id").unwrap_or_default();
        let alias: String = account.try_get("alias").unwrap_or_default();

        // Query the last 50 usage logs for this account
        let stats = sqlx::query(
            "SELECT COUNT(*) as total,
                    SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) as success
             FROM usage_logs
             WHERE account_id = ?
             ORDER BY timestamp DESC
             LIMIT 50",
        )
        .bind(acc_id)
        .fetch_optional(&state.pool)
        .await;

        let (total, success_count) = match stats {
            Ok(Some(row)) => {
                let t: i64 = row.try_get("total").unwrap_or_default();
                let s: i64 = row.try_get("success").unwrap_or_default();
                (t, s)
            }
            _ => (0, 0),
        };

        let status = if total > 0 {
            let rate = success_count as f64 / total as f64;
            if rate > 0.9 {
                "healthy"
            } else if rate > 0.5 {
                "degraded"
            } else {
                "down"
            }
        } else {
            "unknown"
        };

        tracing::info!(
            "💚 Account {} ({}): {}",
            acc_id,
            alias,
            status.to_uppercase()
        );

        health_data.push(json!({
            "id": format!("acc_{acc_id}"),
            "name": alias,
            "status": status,
            "lastSuccess": success_count,
            "totalChecks": total,
        }));
    }

    Json(Value::Array(health_data)).into_response()
}
