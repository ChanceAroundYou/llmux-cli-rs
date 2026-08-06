use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use llmux_core::repo;
use serde_json::{json, Value};

use crate::app::AppState;

const VALID_PROTOCOLS: [&str; 4] = ["openai", "anthropic", "gemini", "custom"];

#[utoipa::path(
    get,
    path = "/api/vendors",
    responses(
        (status = 200, description = "厂商列表", body = [llmux_core::models::Vendor])
    )
)]
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

/// 解析请求体的 protocols（JSON 数组或逗号串）；缺省为 [主协议]。
/// 只保留合法协议，并确保主协议在列表中（主协议优先）。
fn parse_protocols(body: &Value, primary: &str) -> Vec<String> {
    let mut list: Vec<String> = match body.get("protocols") {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect(),
        Some(Value::String(s)) => s
            .split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect(),
        _ => vec![],
    };
    list.retain(|p| VALID_PROTOCOLS.contains(&p.as_str()));
    if !list.iter().any(|p| p == primary) {
        list.insert(0, primary.to_string());
    }
    list
}

#[utoipa::path(
    post,
    path = "/api/vendors",
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "创建厂商成功", body = serde_json::Value),
        (status = 400, description = "参数缺失或 protocol 非法")
    )
)]
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

    // protocols：JSON 数组或逗号串，缺省为 [主协议]；确保主协议在列表中
    let protocols = parse_protocols(&body, protocol);
    let openai_responses = body.get("openai_responses").and_then(Value::as_bool).unwrap_or(true);

    match repo::create_vendor(&state.pool, id, name, protocol, &protocols, openai_responses, default_base_url, default_anthropic_url).await
    {
        Ok(_) => Json(json!({ "success": true, "message": "Vendor created" })).into_response(),
        Err(e) => crate::error::simple_error(
            format!("Failed to create vendor: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

#[utoipa::path(
    put,
    path = "/api/vendors/{id}",
    params(("id" = String, Path, description = "厂商 ID")),
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "更新厂商成功", body = serde_json::Value),
        (status = 404, description = "厂商不存在")
    )
)]
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

    // protocols：JSON 数组或逗号串，缺省为 [主协议]；确保主协议在列表中
    let protocols = parse_protocols(&body, protocol);
    let openai_responses = body.get("openai_responses").and_then(Value::as_bool).unwrap_or(true);

    match repo::update_vendor(&state.pool, &id, name, protocol, &protocols, openai_responses, default_base_url, default_anthropic_url).await
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

#[utoipa::path(
    delete,
    path = "/api/vendors/{id}",
    params(("id" = String, Path, description = "厂商 ID")),
    responses(
        (status = 200, description = "删除厂商成功", body = serde_json::Value),
        (status = 404, description = "厂商不存在"),
        (status = 409, description = "仍有账户引用该厂商")
    )
)]
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
