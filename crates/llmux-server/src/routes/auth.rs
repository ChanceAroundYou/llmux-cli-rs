use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use llmux_core::crypto::encrypt_api_key;
use serde_json::{json, Value};

use crate::app::AppState;

pub async fn handle_web_session(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let token = body
        .get("token")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let provider = body
        .get("provider")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());

    let Some(provider) = provider else {
        return crate::error::simple_error("Missing token or provider", StatusCode::BAD_REQUEST);
    };
    if token.is_none() {
        return crate::error::simple_error("Missing token or provider", StatusCode::BAD_REQUEST);
    }

    let token = token.unwrap();
    let alias = body
        .get("alias")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("{provider}-web"));

    let provider_id = format!("{provider}-web");

    // Encrypt the token before storing.
    let encrypted_token = match encrypt_api_key(token, &state.master_key) {
        Ok(key) => key,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to encrypt web session token: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Check for an existing web-session account for this provider and alias.
    if let Ok(Some(existing_id)) = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM accounts WHERE provider_id = ? AND alias = ?",
    )
    .bind(&provider_id)
    .bind(&alias)
    .fetch_optional(&state.pool)
    .await
    {
        // Update existing web session.
        match sqlx::query("UPDATE accounts SET api_key = ? WHERE id = ?")
            .bind(&encrypted_token)
            .bind(existing_id)
            .execute(&state.pool)
            .await
        {
            Ok(_) => {
                tracing::info!("🔐 Successfully updated Web Session for {provider}");
                return Json(json!({
                    "success": true,
                    "message": format!("Web Session for {provider} updated successfully as {alias}")
                }))
                .into_response();
            }
            Err(e) => {
                return crate::error::simple_error(
                    format!("Failed to update web session: {e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        }
    }

    // Insert new web session account.
    match sqlx::query(
        "INSERT INTO accounts (alias, provider_id, api_key, is_active, weight)
         VALUES (?, ?, ?, 1, 1)",
    )
    .bind(&alias)
    .bind(&provider_id)
    .bind(&encrypted_token)
    .execute(&state.pool)
    .await
    {
        Ok(_) => {
            tracing::info!("🔐 Successfully imported Web Session for {provider}");
            Json(json!({
                "success": true,
                "message": format!("Web Session for {provider} imported successfully as {alias}")
            }))
            .into_response()
        }
        Err(e) => crate::error::simple_error(
            format!("Failed to store web session: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

use axum::http::{HeaderMap, header};

use crate::middleware::{SESSION_COOKIE, SESSION_TTL_SECS};

fn admin_credentials() -> (String, String) {
    let user = std::env::var("ADMIN_USERNAME").unwrap_or_else(|_| "xiaokubao".to_string());
    let pass = std::env::var("ADMIN_PASSWORD").unwrap_or_else(|_| "Xkb111717!".to_string());
    (user, pass)
}

fn extract_session_from_headers(headers: &HeaderMap) -> Option<String> {
    let cookie = headers.get("cookie")?.to_str().ok()?;
    for part in cookie.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix(&format!("{}=", SESSION_COOKIE)) {
            let v = v.trim().to_string();
            if !v.is_empty() { return Some(v); }
        }
    }
    None
}

fn is_session_valid(state: &AppState, token: &str) -> bool {
    // denylisted JWT (logout) must not be considered valid
    let denied = token.contains('.')
        && state.sessions.lock().unwrap().get(token).map(|exp| *exp > std::time::Instant::now()).unwrap_or(false);
    if denied { return false; }
    if crate::middleware::verify_jwt(token, &state.master_key).is_some() { return true; }
    state.sessions.lock().unwrap().get(token).map(|exp| *exp > std::time::Instant::now()).unwrap_or(false)
}

pub async fn handle_login(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let username = body.get("username").and_then(Value::as_str).unwrap_or("").trim().to_string();
    let password = body.get("password").and_then(Value::as_str).unwrap_or("").to_string();
    let (exp_user, exp_pass) = admin_credentials();
    if username != exp_user || password != exp_pass {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Invalid credentials"}))).into_response();
    }
    let token = crate::middleware::sign_jwt(&username, &state.master_key, SESSION_TTL_SECS);
    let cookie_val = format!("{}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}", SESSION_COOKIE, token, SESSION_TTL_SECS);
    let base_cookie = if state.base_path.is_empty() { None } else {
        Some(format!("{}={}; Path={}; HttpOnly; SameSite=Lax; Max-Age={}", SESSION_COOKIE, token, state.base_path, SESSION_TTL_SECS))
    };
    let mut res = Json(json!({"success": true})).into_response();
    let headers = res.headers_mut();
    headers.insert(header::SET_COOKIE, cookie_val.parse().unwrap());
    if let Some(bc) = base_cookie {
        headers.append(header::SET_COOKIE, bc.parse().unwrap());
    }
    res
}

pub async fn handle_logout(
    Extension(state): Extension<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Some(token) = extract_session_from_headers(&headers) {
        let mut guard = state.sessions.lock().unwrap();
        // legacy token: drop it; JWT: denylist until natural expiry so stolen token dies on logout
        if token.contains('.') {
            guard.insert(token.clone(), std::time::Instant::now() + std::time::Duration::from_secs(SESSION_TTL_SECS));
            if guard.len() > 512 { guard.retain(|_, v| *v > std::time::Instant::now()); }
        } else {
            guard.remove(&token);
        }
    }
    let clear = format!("{}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0", SESSION_COOKIE);
    let mut res = Json(json!({"success": true})).into_response();
    let headers_mut = res.headers_mut();
    headers_mut.insert(header::SET_COOKIE, clear.parse().unwrap());
    if !state.base_path.is_empty() {
        let clear2 = format!("{}=; Path={}; HttpOnly; SameSite=Lax; Max-Age=0", SESSION_COOKIE, state.base_path);
        headers_mut.append(header::SET_COOKIE, clear2.parse().unwrap());
    }
    res
}

pub async fn handle_me(
    Extension(state): Extension<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Some(token) = extract_session_from_headers(&headers) {
        if is_session_valid(&state, &token) {
            let (user, _) = admin_credentials();
            return Json(json!({"authenticated": true, "username": user})).into_response();
        }
    }
    (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response()
}

