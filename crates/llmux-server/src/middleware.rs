use axum::{
    extract::Request,
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde_json::json;
use sqlx::Row;

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
        Some(k) => {
            // Hot cache (60s TTL) — avoids per-request DB RTT.
            let cached = {
                let guard = state.auth_cache.lock().unwrap();
                guard.get(&k).filter(|c| c.expires > std::time::Instant::now()).map(|c| c.ctx.clone())
            };
            let ctx = if let Some(c) = cached {
                Ok(c)
            } else {
                let res = validate_api_key(&state.pool, &k).await;
                if let Ok(ref c) = res {
                    let mut guard = state.auth_cache.lock().unwrap();
                    guard.insert(k.clone(), crate::app::CachedAuth { ctx: c.clone(), expires: std::time::Instant::now() + crate::app::HOT_CACHE_TTL });
                    if guard.len() > 512 { guard.retain(|_, v| v.expires > std::time::Instant::now()); }
                }
                res
            };
            match ctx {
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
            }
        }
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

async fn validate_api_key(
    pool: &sqlx::SqlitePool,
    key: &str,
) -> anyhow::Result<AuthContext> {
    let row = sqlx::query("SELECT name, allowed_models FROM api_keys WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await?;

    match row {
        Some(row) => {
            let name: String = row.try_get("name")?;
            let allowed_models: String = row.try_get("allowed_models")?;
            Ok(AuthContext {
                key_name: name,
                allowed_models,
            })
        }
        None => Err(anyhow::anyhow!("API key not found")),
    }
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
