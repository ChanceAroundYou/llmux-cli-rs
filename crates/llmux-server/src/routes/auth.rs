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

/// 没有任何配置时的兜底账号。**只应作为「开箱即用」的初始值** —— 默认密码
/// 是公开知识，登录后第一件事就该去设置页改掉。
const DEFAULT_ADMIN_USERNAME: &str = "admin";
const DEFAULT_ADMIN_PASSWORD: &str = "admin";

/// 当前生效的管理员凭据（用户名 + 密码哈希，哈希可能为空 = 尚未落库）。
///
/// 优先级：DB（UI 改过就以它为准）→ env → 默认 admin/admin。
/// DB 一旦有记录就不再读 env —— 否则运维设了 env，用户在 UI 里改了却不生效。
async fn admin_credentials(state: &AppState) -> (String, String) {
    if let Ok(Some(row)) = sqlx::query_as::<_, (String, String)>(
        "SELECT username, password_hash FROM admin_credentials WHERE id = 1",
    )
    .fetch_optional(&state.pool)
    .await
    {
        return row;
    }
    let user = std::env::var("ADMIN_USERNAME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_ADMIN_USERNAME.to_string());
    // 第二个槽位**只放哈希**，没有就返回空串。
    //
    // 曾经这里返回的是 env 的明文密码，而 `verify_admin` 把「非空」当作「有哈希」，
    // 于是去 `verify_password` 里解析 `vXkb111717!` 这种根本不是 `v1:salt:hash`
    // 的东西，必然 parse 失败 → 登录恒 401。只在**设了 env** 时复现，而测试环境
    // 不设 env，所以单测全绿、生产全挂。env 明文的比对放在 verify_admin 里做。
    (user, String::new())
}

/// 校验登录。`stored_hash` 为空表示 DB 里还没有记录，此时比对 env/默认明文。
async fn verify_admin(state: &AppState, username: &str, password: &str) -> bool {
    let (expected_user, stored_hash) = admin_credentials(state).await;
    if username != expected_user {
        return false;
    }
    if stored_hash.is_empty() {
        let expected_pass = std::env::var("ADMIN_PASSWORD")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_ADMIN_PASSWORD.to_string());
        // 明文兜底路径也要定时安全比较，避免按字符提前返回。
        return constant_time_eq(password.as_bytes(), expected_pass.as_bytes());
    }
    llmux_core::crypto::verify_password(password, &stored_hash)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
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
    if !verify_admin(&state, &username, &password).await {
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
            let (user, _) = admin_credentials(&state).await;
            return Json(json!({"authenticated": true, "username": user})).into_response();
        }
    }
    (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response()
}

/// 修改管理员用户名/密码。**要求携带当前密码** —— 会话 Cookie 会被 XSS/嗅探
/// 顺手带走，仅凭会话就能改掉账号密码的话，一次 XSS 就永久接管了。
///
/// 密码落库为 scrypt 哈希（见 `llmux_core::crypto::hash_password`），不存明文。
pub async fn handle_update_credentials(
    Extension(state): Extension<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    // 必须是已登录会话
    let authed = extract_session_from_headers(&headers)
        .map(|t| is_session_valid(&state, &t))
        .unwrap_or(false);
    if !authed {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
    }

    let current = body.get("current_password").and_then(Value::as_str).unwrap_or("");
    let new_username = body
        .get("username")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let new_password = body
        .get("new_password")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty());

    // 当前密码必须对：用当前会话的用户名去验，防止拿别人的会话改。
    let (current_user, _) = admin_credentials(&state).await;
    if !verify_admin(&state, &current_user, current).await {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Current password is incorrect"})))
            .into_response();
    }

    if new_username.is_none() && new_password.is_none() {
        return crate::error::simple_error(
            "Nothing to update: provide username and/or new_password",
            StatusCode::BAD_REQUEST,
        );
    }
    if let Some(p) = new_password {
        if p.chars().count() < 4 {
            return crate::error::simple_error(
                "New password must be at least 4 characters",
                StatusCode::BAD_REQUEST,
            );
        }
    }

    let username = new_username.unwrap_or(&current_user).to_string();
    // 只改用户名时沿用原密码哈希：DB 无哈希（还在用 env/默认）则把当前密码落成哈希。
    let hash = match new_password {
        Some(p) => match llmux_core::crypto::hash_password(p) {
            Ok(h) => h,
            Err(e) => {
                return crate::error::simple_error(
                    format!("Failed to hash password: {e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        },
        None => match sqlx::query_scalar::<_, String>(
            "SELECT password_hash FROM admin_credentials WHERE id = 1",
        )
        .fetch_optional(&state.pool)
        .await
        {
            Ok(Some(h)) => h,
            _ => match llmux_core::crypto::hash_password(current) {
                Ok(h) => h,
                Err(e) => {
                    return crate::error::simple_error(
                        format!("Failed to hash password: {e}"),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    );
                }
            },
        },
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    match sqlx::query(
        "INSERT INTO admin_credentials (id, username, password_hash, updated_at) VALUES (1, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET username = excluded.username, \
           password_hash = excluded.password_hash, updated_at = excluded.updated_at",
    )
    .bind(&username)
    .bind(&hash)
    .bind(now)
    .execute(&state.pool)
    .await
    {
        Ok(_) => {
            tracing::info!("🔐 Admin credentials updated (username: {})", username);
            Json(json!({"success": true, "username": username})).into_response()
        }
        Err(e) => crate::error::simple_error(
            format!("Failed to update credentials: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

