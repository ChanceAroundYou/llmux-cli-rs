use axum::{
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde_json::{json, Value};

use llmux_core::models::ModelAlias;

use crate::app::AppState;

pub async fn get_model_aliases(Extension(state): Extension<AppState>) -> Response {
    match sqlx::query_as::<_, ModelAlias>(
        "SELECT id, alias, target_model, provider_id, account_ids FROM model_aliases ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(aliases) => Json(serde_json::to_value(aliases).unwrap_or(Value::Array(vec![])))
            .into_response(),
        Err(e) => crate::error::simple_error(
            format!("Failed to list aliases: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

pub async fn set_model_alias(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let Some(alias) = body.get("alias").and_then(Value::as_str) else {
        return crate::error::simple_error(
            "Missing required fields: alias, target_model",
            StatusCode::BAD_REQUEST,
        );
    };
    let Some(target_model) = body.get("target_model").and_then(Value::as_str) else {
        return crate::error::simple_error(
            "Missing required fields: alias, target_model",
            StatusCode::BAD_REQUEST,
        );
    };
    let provider_id = body.get("provider_id").and_then(Value::as_str);

    // Parse account_ids: JSON array like [1,5] or comma-separated "1,5"
    let account_ids = body.get("account_ids").and_then(|v| {
        if v.is_array() {
            Some(serde_json::to_string(v).unwrap_or_default())
        } else {
            v.as_str().map(|s| s.to_string())
        }
    });

    match sqlx::query(
        "INSERT OR REPLACE INTO model_aliases (alias, target_model, provider_id, account_ids) VALUES (?, ?, ?, ?)",
    )
    .bind(alias)
    .bind(target_model)
    .bind(provider_id)
    .bind(&account_ids)
    .execute(&state.pool)
    .await
    {
        Ok(_) => {
            // Invalidate models cache so custom models appear immediately
            if let Ok(mut cache) = state.models_cache.lock() {
                *cache = None;
            }
            tracing::info!("🏷️ Set alias {} -> {} (provider: {:?}), cache invalidated", alias, target_model, provider_id);
            Json(json!({ "success": true, "message": "Alias set successfully" })).into_response()
        },
        Err(e) => crate::error::simple_error(
            format!("Failed to set alias: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

pub async fn delete_model_alias(
    Extension(state): Extension<AppState>,
    Path(id): Path<String>,
) -> Response {
    let alias_row = match sqlx::query_as::<_, ModelAlias>(
        "SELECT id, alias, target_model, provider_id, account_ids FROM model_aliases WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            return crate::error::simple_error("Alias not found", StatusCode::NOT_FOUND);
        }
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to lookup alias: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Delete the alias
    if let Err(e) = sqlx::query("DELETE FROM model_aliases WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await
    {
        return crate::error::simple_error(
            format!("Failed to delete alias: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    // Cascade-clean API keys: remove the alias name from allowed_models.
    // Bun stores allowed_models as JSON arrays like ["gpt-4","claude-3"].
    let api_keys: Vec<(i64, String)> =
        match sqlx::query_as("SELECT id, allowed_models FROM api_keys")
            .fetch_all(&state.pool)
            .await
        {
            Ok(rows) => rows,
            Err(_) => {
                return Json(
                    json!({ "success": true, "message": "Alias deleted and API Keys synced successfully" }),
                )
                .into_response();
            }
        };

    for (key_id, allowed_models) in &api_keys {
        if allowed_models == "*" {
            continue;
        }
        if let Ok(mut models) = serde_json::from_str::<Vec<String>>(allowed_models) {
            if models.contains(&alias_row.alias) {
                models.retain(|m| m != &alias_row.alias);
                let updated = if models.is_empty() {
                    "*".to_string()
                } else {
                    serde_json::to_string(&models).unwrap_or_else(|_| "*".to_string())
                };
                let _ = sqlx::query("UPDATE api_keys SET allowed_models = ? WHERE id = ?")
                    .bind(&updated)
                    .bind(key_id)
                    .execute(&state.pool)
                    .await;
                tracing::info!("🔄 Removed alias {} from API Key ID: {}", alias_row.alias, key_id);
            }
        }
    }

    // Invalidate models cache
    if let Ok(mut cache) = state.models_cache.lock() {
        *cache = None;
    }

    Json(json!({ "success": true, "message": "Alias deleted and API Keys synced successfully" })).into_response()
}
