use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use llmux_core::repo;
use serde_json::{json, Value};

use crate::app::AppState;

const VALID_PROTOCOLS: [&str; 4] = ["openai", "anthropic", "gemini", "custom"];

pub async fn list_vendors(Extension(state): Extension<AppState>) -> Response {
    match repo::list_vendors(&state.pool).await {
        Ok(vendors) => Json(serde_json::to_value(vendors).unwrap_or(Value::Array(vec![])))
            .into_response(),
        Err(e) => crate::error::simple_error(
            format!("Failed to list vendors: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

pub async fn create_vendor(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let Some(id) = body.get("id").and_then(Value::as_str) else {
        return crate::error::simple_error(
            "Missing required fields: id, name, protocol",
            StatusCode::BAD_REQUEST,
        );
    };
    let Some(name) = body.get("name").and_then(Value::as_str) else {
        return crate::error::simple_error(
            "Missing required fields: id, name, protocol",
            StatusCode::BAD_REQUEST,
        );
    };
    let protocol = body.get("protocol").and_then(Value::as_str).unwrap_or("openai");
    if !VALID_PROTOCOLS.contains(&protocol) {
        return crate::error::simple_error(
            format!("Invalid protocol: {protocol} (must be one of {VALID_PROTOCOLS:?})"),
            StatusCode::BAD_REQUEST,
        );
    }
    let default_base_url = body.get("default_base_url").and_then(Value::as_str);
    let default_anthropic_url = body.get("default_anthropic_url").and_then(Value::as_str);

    match repo::create_vendor(&state.pool, id, name, protocol, default_base_url, default_anthropic_url).await
    {
        Ok(_) => Json(json!({ "success": true, "message": "Vendor created" })).into_response(),
        Err(e) => crate::error::simple_error(
            format!("Failed to create vendor: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

pub async fn update_vendor(
    Extension(state): Extension<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let protocol = body.get("protocol").and_then(Value::as_str).unwrap_or("openai");
    if !VALID_PROTOCOLS.contains(&protocol) {
        return crate::error::simple_error(
            format!("Invalid protocol: {protocol}"),
            StatusCode::BAD_REQUEST,
        );
    }
    let name = body.get("name").and_then(Value::as_str).unwrap_or(&id);
    let default_base_url = body.get("default_base_url").and_then(Value::as_str);
    let default_anthropic_url = body.get("default_anthropic_url").and_then(Value::as_str);

    match repo::update_vendor(&state.pool, &id, name, protocol, default_base_url, default_anthropic_url).await
    {
        Ok(r) if r == 0 => crate::error::simple_error(
            format!("Vendor not found: {id}"),
            StatusCode::NOT_FOUND,
        ),
        Ok(_) => Json(json!({ "success": true, "message": "Vendor updated" })).into_response(),
        Err(e) => crate::error::simple_error(
            format!("Failed to update vendor: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

pub async fn delete_vendor(
    Extension(state): Extension<AppState>,
    Path(id): Path<String>,
) -> Response {
    match repo::delete_vendor(&state.pool, &id).await {
        Ok(r) if r == 0 => crate::error::simple_error(
            format!("Vendor not found: {id}"),
            StatusCode::NOT_FOUND,
        ),
        Ok(_) => Json(json!({ "success": true, "message": "Vendor deleted" })).into_response(),
        // 外键挡：还有账户引用该厂商 → 提示先处理账户
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("FOREIGN KEY") {
                crate::error::simple_error(
                    format!("Cannot delete vendor '{id}': still referenced by accounts. Remove or move those accounts first."),
                    StatusCode::CONFLICT,
                )
            } else {
                crate::error::simple_error(
                    format!("Failed to delete vendor: {e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                )
            }
        }
    }
}
