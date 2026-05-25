use axum::{
    http::HeaderMap,
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde_json::Value;

use crate::app::AppState;
use crate::middleware;

use super::helpers::iso8601_now;

// ---------------------------------------------------------------------------
// /v1/models
// ---------------------------------------------------------------------------

pub async fn models(Extension(state): Extension<AppState>, headers: HeaderMap) -> Response {
    tracing::info!("🤖 Request received");
    let is_anthropic =
        headers.contains_key("x-api-key") || headers.contains_key("anthropic-version");

    let alias_names: Vec<String> = match sqlx::query_scalar::<_, String>(
        "SELECT alias FROM model_aliases",
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            return middleware::send_error(
                &format!("Failed to load models: {e}"),
                "server_error",
                StatusCode::INTERNAL_SERVER_ERROR,
                is_anthropic,
            );
        }
    };

    if is_anthropic {
        let created_at = iso8601_now();
        let data: Vec<Value> = alias_names
            .iter()
            .map(|alias| {
                serde_json::json!({
                    "type": "model",
                    "id": alias,
                    "display_name": alias,
                    "created_at": created_at,
                })
            })
            .collect();
        let first_id = data
            .first()
            .and_then(|m| m["id"].as_str().map(str::to_string));
        let last_id = data
            .last()
            .and_then(|m| m["id"].as_str().map(str::to_string));
        return Json(serde_json::json!({
            "data": data,
            "has_more": false,
            "first_id": first_id,
            "last_id": last_id,
        }))
        .into_response();
    }

    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let data: Vec<Value> = alias_names
        .into_iter()
        .map(|alias| {
            serde_json::json!({
                "id": alias,
                "object": "model",
                "created": created,
                "owned_by": "llmux",
            })
        })
        .collect();

    Json(serde_json::json!({
        "object": "list",
        "data": data
    }))
    .into_response()
}
