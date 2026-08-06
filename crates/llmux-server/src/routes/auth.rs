use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use llmux_core::crypto::encrypt_api_key;
use llmux_core::repo;
use serde_json::{json, Value};

use crate::app::AppState;

/// 把 web session 的 provider 名解析成 vendors.id。
/// 优先精确匹配；再按协议猜测；都没有则回退 openai。
async fn resolve_vendor_id(state: &AppState, provider: &str) -> String {
    if let Ok(Some(_)) = repo::get_vendor(&state.pool, provider).await {
        return provider.to_string();
    }
    // 协议猜测：claude/anthropic → anthropic；gemini → gemini
    let guessed = if provider.contains("anthropic") || provider.contains("claude") {
        "anthropic"
    } else if provider.contains("gemini") {
        "gemini"
    } else {
        "openai"
    };
    if let Ok(Some(_)) = repo::get_vendor(&state.pool, guessed).await {
        return guessed.to_string();
    }
    guessed.to_string()
}

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

    let vendor_id = resolve_vendor_id(&state, provider).await;
    let name = alias.clone();

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

    // Check for an existing web-session account for this vendor and name.
    if let Ok(Some(existing_id)) =
        repo::find_account_by_vendor_and_name(&state.pool, &vendor_id, &name).await
    {
        // Update existing web session.
        match repo::set_account_api_key_enc(&state.pool, existing_id, &encrypted_token).await {
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
    match repo::create_account(&state.pool, &vendor_id, &name, &encrypted_token, None, None, 0, 1, 1, None).await {
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
