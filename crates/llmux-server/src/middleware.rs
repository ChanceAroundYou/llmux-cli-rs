use axum::{
    extract::Request,
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Extension, Json,
};
use llmux_core::repo;
use serde_json::json;

use crate::app::AppState;

/// Context injected into request extensions by the auth middleware so
/// downstream handlers can check allowed_models.
#[derive(Debug, Clone)]
pub struct AuthContext {
    pub key_name: String,
    pub allowed_models: String,
}

pub async fn v1_auth_middleware(
    Extension(state): Extension<AppState>,
    headers: HeaderMap,
    mut request: Request,
    next: Next,
) -> Response {
    let key = extract_api_key(&headers);
    let has_x_api_key = headers.contains_key("x-api-key");

    match key {
        Some(k) => match validate_api_key(&state.pool, &k).await {
            Ok(ctx) => {
                request.extensions_mut().insert(ctx);
                next.run(request).await
            }
            Err(_) => send_error(
                "Invalid API Key",
                "authentication_error",
                StatusCode::UNAUTHORIZED,
                has_x_api_key,
            ),
        },
        None => send_error(
            "Missing API Key. Gateway is locked.",
            "authentication_error",
            StatusCode::UNAUTHORIZED,
            has_x_api_key,
        ),
    }
}

fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    // x-api-key takes precedence for Anthropic clients
    if let Some(value) = headers.get("x-api-key") {
        return value.to_str().ok().map(str::to_string);
    }
    // x-goog-api-key for Gemini clients
    if let Some(value) = headers.get("x-goog-api-key") {
        return value.to_str().ok().map(str::to_string);
    }
    if let Some(auth) = headers.get("authorization") {
        return auth
            .to_str()
            .ok()
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::to_string);
    }
    None
}

/// 校验网关 key：明文比对（网关 key 明文存储，见 schema 注释）。
async fn validate_api_key(
    pool: &sqlx::SqlitePool,
    key: &str,
) -> anyhow::Result<AuthContext> {
    // 单条 LEFT JOIN 一次取回 key + 白名单，避免热路径两次 DB 往返
    let Some((id, name, models)) = repo::find_api_key_with_models(pool, key).await? else {
        return Err(anyhow::anyhow!("API key not found"));
    };

    let _ = repo::update_api_key_last_used(pool, id).await;
    // 白名单来自 api_key_models（空表 = 不限制）
    let allowed_models = if models.is_empty() {
        "*".to_string()
    } else {
        serde_json::to_string(&models).unwrap_or_else(|_| "*".to_string())
    };
    Ok(AuthContext {
        key_name: name,
        allowed_models,
    })
}

/// Returns a dual-format error response.
///
/// When `is_anthropic` is true (request had an `x-api-key` header) the
/// response follows the Anthropic error schema:
///   `{ "type": "error", "error": { "type": "...", "message": "..." } }`
///
/// Otherwise it follows the OpenAI error schema:
///   `{ "error": { "message": "...", "type": "...", "code": "..." } }`
pub fn send_error(
    message: &str,
    error_type: &str,
    status: StatusCode,
    is_anthropic: bool,
) -> Response {
    if is_anthropic {
        (
            status,
            Json(json!({
                "type": "error",
                "error": { "type": error_type, "message": message }
            })),
        )
            .into_response()
    } else {
        (
            status,
            Json(json!({
                "error": {
                    "message": message,
                    "type": error_type,
                    "code": status.as_u16().to_string()
                }
            })),
        )
            .into_response()
    }
}

/// Check whether a requested model is permitted by the allowed_models policy.
/// Bun stores allowed_models as either `"*"` or a JSON array string like
/// `["gpt-4","claude-3"]`.
pub fn is_model_allowed(allowed_models: &str, model: &str) -> bool {
    if allowed_models == "*" || allowed_models == "\"*\"" {
        return true;
    }
    serde_json::from_str::<Vec<String>>(allowed_models)
        .map(|models| models.iter().any(|m| m == model))
        .unwrap_or(false)
}
