use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use llmux_core::repo;
use serde_json::{json, Value};

use crate::app::AppState;

#[utoipa::path(
    get,
    path = "/api/health",
    responses(
        (status = 200, description = "网关健康状态（账户/模型/上游在线情况）", body = serde_json::Value)
    )
)]
pub async fn get_health_status(Extension(state): Extension<AppState>) -> Response {
    let accounts = match repo::list_account_id_name(&state.pool).await {
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

    for (acc_id, alias) in &accounts {
        let acc_id = *acc_id;

        // Query the last 50 usage logs for this account
        let stats = repo::get_account_usage_stats(&state.pool, acc_id).await;

        let (total, success_count) = match stats {
            Ok(Some((t, s))) => (t, s),
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
