use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use llmux_core::adapters::Account;
use llmux_core::crypto::encrypt_api_key;
use llmux_core::dispatcher::resolve_provider_type;
use llmux_core::models::AccountPublic;
use serde_json::{json, Value};

use crate::app::AppState;
use crate::routes::models::fetch_provider_models;

pub async fn list_accounts(Extension(state): Extension<AppState>) -> Response {
    match sqlx::query_as::<_, AccountPublic>(
        "SELECT id, alias, provider_id, base_url, anthropic_base_url, CAST(is_active AS INTEGER) as is_active, weight, notes, openai_compatible, created_at, chat_endpoint, responses_endpoint, messages_endpoint, default_protocol FROM accounts ORDER BY id DESC",
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(accounts) => Json(serde_json::to_value(accounts).unwrap_or(Value::Array(vec![])))
            .into_response(),
        Err(e) => crate::error::simple_error(
            format!("Failed to list accounts: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

pub async fn create_account(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let missing = body.get("alias").is_none()
        || body.get("provider_id").is_none()
        || body.get("api_key").is_none();
    if missing {
        return crate::error::simple_error(
            "Missing required fields: alias, provider_id, api_key",
            StatusCode::BAD_REQUEST,
        );
    }

    let alias = body["alias"].as_str().unwrap_or_default().to_string();
    let provider_id = body["provider_id"].as_str().unwrap_or_default().to_string();
    let api_key_plain = body["api_key"].as_str().unwrap_or_default().to_string();
    let base_url = body["base_url"].as_str().map(|s| s.to_string());
    let anthropic_base_url = body["anthropic_base_url"].as_str().map(|s| s.to_string());
    let is_active = body["is_active"].as_i64().unwrap_or(1);
    let weight = body["weight"].as_i64().unwrap_or(1);
    let notes = body["notes"].as_str().map(|s| s.to_string());
    let openai_compatible = body["openai_compatible"].as_i64().unwrap_or(0);
    let skip_validation = body["skip_validation"].as_bool().unwrap_or(false);

    if alias.is_empty() || provider_id.is_empty() || api_key_plain.is_empty() {
        return crate::error::simple_error(
            "alias, provider_id, and api_key must not be empty",
            StatusCode::BAD_REQUEST,
        );
    }

    // Always try to validate — but only reject on failure if skip_validation is false.
    let test_account = Account {
        id: 0,
        alias: alias.clone(),
        provider_id: provider_id.clone(),
        api_key: api_key_plain.clone(),
        base_url: base_url.clone(),
        anthropic_base_url: anthropic_base_url.clone(),
        is_active,
        weight,
        openai_compatible,
        chat_endpoint: base_url.clone(),
        responses_endpoint: None,
        messages_endpoint: anthropic_base_url.clone(),
        default_protocol: Some("chat".to_string()),
    };

    let provider_type = {
        let pt = sqlx::query_scalar::<_, Option<String>>(
            "SELECT type FROM providers WHERE id = ?",
        )
        .bind(&provider_id)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten()
        .flatten();
        resolve_provider_type(pt.as_deref(), &provider_id)
    };

    let (models, _) = fetch_provider_models(&test_account, &provider_type).await;
    if models.is_empty() && !skip_validation {
        return crate::error::simple_error(
            "accounts.validationFailed",
            StatusCode::BAD_REQUEST,
        );
    }
    let models_fetched = models.len();

    let encrypted_key = match encrypt_api_key(&api_key_plain, &state.master_key) {
        Ok(key) => key,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to encrypt API key: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    match sqlx::query(
        "INSERT INTO accounts (alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight, notes, openai_compatible)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&alias)
    .bind(&provider_id)
    .bind(&encrypted_key)
    .bind(&base_url)
    .bind(&anthropic_base_url)
    .bind(is_active)
    .bind(weight)
    .bind(&notes)
    .bind(openai_compatible)
    .execute(&state.pool)
    .await
    {
        Ok(result) => {
            let id = result.last_insert_rowid();
            Json(json!({
                "success": true,
                "id": id,
                "message": if skip_validation { "Account created (skipped validation)" } else { "Account verified and created successfully" },
                "modelCount": models_fetched,
                "skippedValidation": skip_validation,
            }))
            .into_response()
        }
        Err(e) => crate::error::simple_error(
            format!("Failed to create account: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

pub async fn update_account(
    Extension(state): Extension<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Response {
    // Verify the account exists.
    let existing = sqlx::query_as::<_, llmux_core::models::Account>(
        "SELECT id, alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight, notes, limits_cache, limits_cache_updated_at, openai_compatible, created_at, chat_endpoint, responses_endpoint, messages_endpoint, default_protocol FROM accounts WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await;

    let existing = match existing {
        Ok(Some(acct)) => acct,
        Ok(None) => {
            return crate::error::simple_error(
                format!("Account with id {id} not found"),
                StatusCode::NOT_FOUND,
            );
        }
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to look up account: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Merge: use body values when present, otherwise keep existing.
    let alias = body
        .get("alias")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or(existing.alias);
    let provider_id = body
        .get("provider_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or(existing.provider_id);
    let base_url = body
        .get("base_url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or(existing.base_url);
    let anthropic_base_url = body
        .get("anthropic_base_url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or(existing.anthropic_base_url);
    let is_active = body
        .get("is_active")
        .and_then(|v| v.as_i64())
        .unwrap_or(existing.is_active);
    let weight = body
        .get("weight")
        .and_then(|v| v.as_i64())
        .unwrap_or(existing.weight);
    let notes = body
        .get("notes")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or(existing.notes);
    let openai_compatible = body
        .get("openai_compatible")
        .and_then(|v| v.as_i64())
        .unwrap_or(existing.openai_compatible.unwrap_or(0));

    // Handle API key: if a new one is provided, encrypt it; otherwise keep the old ciphertext.
    // Bun only re-validates when api_key !== "********" or base_url is present.
    let api_key_changed = body
        .get("api_key")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty() && s != "********");
    let base_url_changed = body.get("base_url").is_some();
    let skip_validation = body["skip_validation"].as_bool().unwrap_or(false);

    let api_key_ciphertext = if api_key_changed {
        let new_key = body["api_key"].as_str().unwrap_or_default();

        if api_key_changed || base_url_changed {
            let test_account = Account {
                id,
                alias: alias.clone(),
                provider_id: provider_id.clone(),
                api_key: new_key.to_string(),
                base_url: base_url.clone(),
                anthropic_base_url: anthropic_base_url.clone(),
                is_active,
                weight,
                openai_compatible,
                chat_endpoint: base_url.clone(),
                responses_endpoint: None,
                messages_endpoint: anthropic_base_url.clone(),
                default_protocol: Some("chat".to_string()),
            };

            let provider_type = {
                let pt = sqlx::query_scalar::<_, Option<String>>(
                    "SELECT type FROM providers WHERE id = ?",
                )
                .bind(&test_account.provider_id)
                .fetch_optional(&state.pool)
                .await
                .ok()
                .flatten()
                .flatten();
                resolve_provider_type(pt.as_deref(), &test_account.provider_id)
            };

            let (models, _) = fetch_provider_models(&test_account, &provider_type).await;
            if models.is_empty() && !skip_validation {
                return crate::error::simple_error(
                    "accounts.validationFailed",
                    StatusCode::BAD_REQUEST,
                );
            }
        }

        match encrypt_api_key(new_key, &state.master_key) {
            Ok(key) => key,
            Err(e) => {
                return crate::error::simple_error(
                    format!("Failed to encrypt API key: {e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        }
    } else {
        existing.api_key
    };

    let update_res = sqlx::query(
        "UPDATE accounts SET alias = ?, provider_id = ?, api_key = ?, base_url = ?, anthropic_base_url = ?, is_active = ?, weight = ?, notes = ?, openai_compatible = ? WHERE id = ?",
    )
    .bind(&alias)
    .bind(&provider_id)
    .bind(&api_key_ciphertext)
    .bind(&base_url)
    .bind(&anthropic_base_url)
    .bind(is_active)
    .bind(weight)
    .bind(&notes)
    .bind(openai_compatible)
    .bind(id)
    .execute(&state.pool)
    .await;
    match update_res {
        Ok(_) => {
            if is_active == 0 {
                let _ = sqlx::query("DELETE FROM account_model_cache WHERE account_id = ?")
                    .bind(id)
                    .execute(&state.pool)
                    .await;
            }
            Json(json!({ "success": true, "message": "Account updated successfully" })).into_response()
        }
        Err(e) => crate::error::simple_error(
            format!("Failed to update account: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

pub async fn delete_account(
    Extension(state): Extension<AppState>,
    Path(id): Path<i64>,
) -> Response {
    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to start transaction: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Delete usage_logs and model cache for this account first.
    if let Err(e) = sqlx::query("DELETE FROM usage_logs WHERE account_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await
    {
        return crate::error::simple_error(
            format!("Failed to delete usage logs: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }
    let _ = sqlx::query("DELETE FROM account_model_cache WHERE account_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await;

    let result = sqlx::query("DELETE FROM accounts WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await;

    match result {
        Ok(query_result) => {
            if query_result.rows_affected() == 0 {
                return crate::error::simple_error(
                    format!("Account with id {id} not found"),
                    StatusCode::NOT_FOUND,
                );
            }
            if let Err(e) = tx.commit().await {
                return crate::error::simple_error(
                    format!("Failed to commit transaction: {e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
            Json(json!({
                "success": true,
                "message": "Account and all associated history deleted successfully"
            }))
            .into_response()
        }
        Err(e) => crate::error::simple_error(
            format!("Failed to delete account: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

pub async fn export_account_usage(
    Extension(state): Extension<AppState>,
    Path(id): Path<i64>,
) -> Response {
    let rows = sqlx::query_as::<_, (i64, Option<String>, i64, i64, i64, i64)>(
        "SELECT timestamp, model, input_tokens, output_tokens, latency_ms, success FROM usage_logs WHERE account_id = ? ORDER BY timestamp DESC",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await;

    let rows = match rows {
        Ok(rows) => rows,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to query usage logs: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let mut csv = String::from("Timestamp,Model,Input Tokens,Output Tokens,Latency (ms),Status\n");
    for (timestamp, model, input, output, latency, success) in &rows {
        let status = if *success != 0 { "Success" } else { "Failed" };
        let model = model.as_deref().unwrap_or("unknown");
        csv.push_str(&format!(
            "{timestamp},{model},{input},{output},{latency},{status}\n"
        ));
    }

    let mut response = (StatusCode::OK, csv).into_response();
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    let disposition = format!("attachment; filename=\"usage_history_account_{id}.csv\"");
    if let Ok(value) = axum::http::HeaderValue::from_str(&disposition) {
        response
            .headers_mut()
            .insert(axum::http::header::CONTENT_DISPOSITION, value);
    }
    response
}
