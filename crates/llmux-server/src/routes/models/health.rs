use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use llmux_core::repo;
use serde_json::{json, Value};

use crate::app::AppState;

#[utoipa::path(
    get,
    path = "/api/models/health",
    responses(
        (status = 200, description = "模型健康状态列表（含 limits_cache）", body = [crate::api_schemas::ModelHealthItem])
    )
)]
pub async fn get_models_health(Extension(state): Extension<AppState>) -> Response {
    let rows = match repo::get_model_health(&state.pool).await {
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
        .map(|row| {
            let limits_cache: Value = row
                .limits_cache
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(Value::Null);
            json!({
                "account_id": row.account_id.unwrap_or_default(),
                "vendor_id": row.vendor_id,
                "model": row.model.clone().unwrap_or_default(),
                "last_checked": row.last_checked.unwrap_or_default(),
                "success": row.success.unwrap_or_default(),
                "latency": row.latency.unwrap_or_default(),
                "error": row.error,
                "limits_cache": limits_cache,
                "limits_cache_updated_at": row.limits_cache_updated_at,
                "account_name": row.account_name,
            })
        })
        .collect();

    Json(Value::Array(health)).into_response()
}
