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
