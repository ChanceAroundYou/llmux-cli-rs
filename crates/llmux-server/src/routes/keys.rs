use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use llmux_core::models::ApiKey;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;

/// Normalize allowed_models from JSON body to storage format.
/// Bun stores as `"*"` or a JSON array string like `["gpt-4","claude-3"]`.
fn normalize_allowed_models(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "*".to_string(),
        Some(Value::String(s)) if s == "*" => "*".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(_)) => {
            serde_json::to_string(value.unwrap()).unwrap_or_else(|_| "*".to_string())
        }
        _ => "*".to_string(),
    }
}

pub async fn list_api_keys(Extension(state): Extension<AppState>) -> Response {
    match sqlx::query_as::<_, ApiKey>(
        "SELECT id, name, key, allowed_models, created_at FROM api_keys ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(keys) => Json(serde_json::to_value(keys).unwrap_or(Value::Array(vec![]))).into_response(),
        Err(e) => crate::error::simple_error(
            format!("Database error: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

pub async fn create_api_key(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Unnamed Key");
    let allowed_models = normalize_allowed_models(body.get("allowed_models"));
    let key = format!("sk-llmux-{}", Uuid::new_v4().simple());

    match sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind(name)
        .bind(&key)
        .bind(&allowed_models)
        .execute(&state.pool)
        .await
    {
        Ok(_) => { state.clear_auth_cache(); Json(json!({
            "success": true,
            "key": key,
        }))
        .into_response() },
        Err(e) => crate::error::simple_error(
            format!("Failed to create API key: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

pub async fn update_api_key(
    Extension(state): Extension<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Response {
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Untitled Key");

    // Read existing allowed_models so we can fall back to it when the body
    // doesn't include the field.  Bun silently succeeds even when the key
    // doesn't exist, so we don't verify existence beforehand.
    let existing_models: Option<String> =
        sqlx::query_scalar("SELECT allowed_models FROM api_keys WHERE id = ?")
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten();

    let allowed_models = if body.get("allowed_models").is_some() {
        normalize_allowed_models(body.get("allowed_models"))
    } else if let Some(existing) = existing_models {
        existing
    } else {
        "*".to_string()
    };

    match sqlx::query("UPDATE api_keys SET name = ?, allowed_models = ? WHERE id = ?")
        .bind(name)
        .bind(&allowed_models)
        .bind(id)
        .execute(&state.pool)
        .await
    {
        Ok(_) => { state.clear_auth_cache(); Json(json!({ "success": true })).into_response() },
        Err(e) => crate::error::simple_error(
            format!("Failed to update API key: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

pub async fn delete_api_key(
    Extension(state): Extension<AppState>,
    Path(id): Path<i64>,
) -> Response {
    // Bun silently succeeds even when the key doesn't exist — match that.
    match sqlx::query("DELETE FROM api_keys WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await
    {
        Ok(_) => { state.clear_auth_cache(); Json(json!({ "success": true })).into_response() },
        Err(e) => crate::error::simple_error(
            format!("Failed to delete API key: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}
