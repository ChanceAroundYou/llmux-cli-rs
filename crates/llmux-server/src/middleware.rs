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

pub const SESSION_COOKIE: &str = "llmux_session";
pub const SESSION_TTL_SECS: u64 = 7 * 24 * 3600;

use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

pub fn sign_jwt(sub: &str, secret: &str, ttl_secs: u64) -> String {
    let header_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","typ":"JWT"}"#);
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let exp = now + ttl_secs as i64;
    let payload_json = serde_json::json!({"sub": sub, "iat": now, "exp": exp}).to_string();
    let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload_json.as_bytes());
    let signing_input = format!("{header_b64}.{payload_b64}");
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(signing_input.as_bytes());
    let sig = mac.finalize().into_bytes();
    let sig_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sig);
    format!("{signing_input}.{sig_b64}")
}

pub fn verify_jwt(token: &str, secret: &str) -> Option<String> {
    let mut parts = token.splitn(3, '.');
    let h = parts.next()?;
    let p = parts.next()?;
    let s = parts.next()?;
    if parts.next().is_some() { return None; }
    let signing_input = format!("{h}.{p}");
    let sig_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s).ok()?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(signing_input.as_bytes());
    let expected = mac.finalize().into_bytes();
    if expected.as_slice() != sig_bytes.as_slice() { return None; }
    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(p).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&payload_bytes).ok()?;
    let sub = v.get("sub")?.as_str()?.to_string();
    let exp = v.get("exp")?.as_i64()?;
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    if exp <= now { return None; }
    Some(sub)
}

fn extract_session_token(headers: &HeaderMap) -> Option<String> {
    let cookie = headers.get("cookie")?.to_str().ok()?;
    for part in cookie.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix(&format!("{}=", SESSION_COOKIE)) {
            let v = v.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn is_whitelisted(effective: &str) -> bool {
    matches!(effective, "/api/auth/login" | "/api/health")
}

pub async fn ui_auth_middleware(
    Extension(state): Extension<AppState>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    let method_is_options = request.method() == axum::http::Method::OPTIONS;
    if method_is_options {
        return next.run(request).await;
    }
    let path = request.uri().path().to_string();
    let base = state.base_path.clone();
    let effective = if base.is_empty() {
        path.clone()
    } else if path == base {
        "/".to_string()
    } else if let Some(s) = path.strip_prefix(base.as_str()) {
        if s.is_empty() { "/".to_string() } else { s.to_string() }
    } else {
        path.clone()
    };

    // Only protect /api/* ; let /v1/* and static/SPA through (v1 has its own middleware)
    if !effective.starts_with("/api/") {
        return next.run(request).await;
    }
    if is_whitelisted(&effective) {
        return next.run(request).await;
    }

    if let Some(ref token) = extract_session_token(&headers) {
        // denylist check for JWT logout (sessions reused as denylist)
        let denied = {
            let guard = state.sessions.lock().unwrap();
            guard.get(token).map(|exp| *exp > std::time::Instant::now()).unwrap_or(false) && token.contains('.')
        };
        if denied {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "Unauthorized"})),
            ).into_response();
        }
        if let Some(sub) = verify_jwt(token, &state.master_key) {
            let refreshed = sign_jwt(&sub, &state.master_key, SESSION_TTL_SECS);
            let mut res = next.run(request).await;
            let cookie_val = format!("{}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}", SESSION_COOKIE, refreshed, SESSION_TTL_SECS);
            let h = res.headers_mut();
            h.append(axum::http::header::SET_COOKIE, cookie_val.parse().unwrap());
            if !base.is_empty() {
                let bc = format!("{}={}; Path={}; HttpOnly; SameSite=Lax; Max-Age={}", SESSION_COOKIE, refreshed, base, SESSION_TTL_SECS);
                h.append(axum::http::header::SET_COOKIE, bc.parse().unwrap());
            }
            return res;
        }
        // fallback: legacy in-memory token (rolling upgrade — keep sliding window until natural expiry)
        let legacy_valid = {
            let mut guard = state.sessions.lock().unwrap();
            if let Some(exp) = guard.get_mut(token) {
                if *exp > std::time::Instant::now() {
                    *exp = std::time::Instant::now() + std::time::Duration::from_secs(SESSION_TTL_SECS);
                    true
                } else { false }
            } else { false }
        };
        if legacy_valid {
            let mut res = next.run(request).await;
            let cookie_val = format!("{}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}", SESSION_COOKIE, token, SESSION_TTL_SECS);
            let h = res.headers_mut();
            h.append(axum::http::header::SET_COOKIE, cookie_val.parse().unwrap());
            if !base.is_empty() {
                let bc = format!("{}={}; Path={}; HttpOnly; SameSite=Lax; Max-Age={}", SESSION_COOKIE, token, base, SESSION_TTL_SECS);
                h.append(axum::http::header::SET_COOKIE, bc.parse().unwrap());
            }
            return res;
        }
    }

    // ponytail: JWT stateless 7d sliding window — no DB; sessions kept as legacy fallback only
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": "Unauthorized"})),
    ).into_response()
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
