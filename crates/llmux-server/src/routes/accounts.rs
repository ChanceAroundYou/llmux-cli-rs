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

    // New multi-endpoint fields: explicit null/empty = disabled. If not provided, fall back to legacy base_url for compat.
    let mut chat_endpoint = body
        .get("chat_endpoint")
        .and_then(|v| if v.is_null() { Some(None) } else { v.as_str().map(|s| s.trim().to_string()).map(|s| if s.is_empty() { None } else { Some(s) }) })
        .unwrap_or_else(|| base_url.clone());
    // base_url fallback already handled above; keep as-is if body didn't contain chat_endpoint
    if !body.as_object().map(|m| m.contains_key("chat_endpoint")).unwrap_or(false) {
        chat_endpoint = base_url.clone();
    }
    let responses_endpoint = body
        .get("responses_endpoint")
        .and_then(|v| if v.is_null() { Some(None) } else { v.as_str().map(|s| s.trim().to_string()).map(|s| if s.is_empty() { None } else { Some(s) }) })
        .unwrap_or(None);
    let mut messages_endpoint = body
        .get("messages_endpoint")
        .and_then(|v| if v.is_null() { Some(None) } else { v.as_str().map(|s| s.trim().to_string()).map(|s| if s.is_empty() { None } else { Some(s) }) })
        .unwrap_or_else(|| anthropic_base_url.clone());
    if !body.as_object().map(|m| m.contains_key("messages_endpoint")).unwrap_or(false) {
        messages_endpoint = anthropic_base_url.clone();
    }
    let mut default_protocol = body
        .get("default_protocol")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_lowercase())
        .unwrap_or_else(|| "chat".to_string());

    // Normalize default_protocol
    if !["chat", "responses", "messages"].contains(&default_protocol.as_str()) {
        default_protocol = "chat".to_string();
    }

    // Validate at least one endpoint
    let enabled: Vec<&str> = [
        ("chat", chat_endpoint.as_deref()),
        ("responses", responses_endpoint.as_deref()),
        ("messages", messages_endpoint.as_deref()),
    ]
    .iter()
    .filter_map(|(k, v)| v.filter(|s| !s.trim().is_empty()).map(|_| *k))
    .collect();
    if enabled.is_empty() {
        return crate::error::simple_error("At least one endpoint is required", StatusCode::BAD_REQUEST);
    }
    if !enabled.contains(&default_protocol.as_str()) {
        return crate::error::simple_error(
            "default_protocol must be one of the enabled protocols",
            StatusCode::BAD_REQUEST,
        );
    }
    // Validate URLs are parseable
    for ep in [&chat_endpoint, &responses_endpoint, &messages_endpoint].iter().filter_map(|o| o.as_deref()) {
        if url::Url::parse(ep).is_err() {
            return crate::error::simple_error(format!("Invalid endpoint URL: {ep}"), StatusCode::BAD_REQUEST);
        }
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
        chat_endpoint: chat_endpoint.clone(),
        responses_endpoint: responses_endpoint.clone(),
        messages_endpoint: messages_endpoint.clone(),
        default_protocol: Some(default_protocol.clone()),
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
        "INSERT INTO accounts (alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight, notes, openai_compatible, chat_endpoint, responses_endpoint, messages_endpoint, default_protocol)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
    .bind(&chat_endpoint)
    .bind(&responses_endpoint)
    .bind(&messages_endpoint)
    .bind(&default_protocol)
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

    // Merge: use body values when present, otherwise keep existing. Explicit
    // null clears a field, a missing key keeps the stored value.
    let parse_ep = |v: &Value| -> Option<String> {
        if v.is_null() { None } else { v.as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) }
    };
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
    // Explicit null = clear (consistent with the *_endpoint columns below);
    // missing key = keep existing.
    let base_url = if body.as_object().map(|m| m.contains_key("base_url")).unwrap_or(false) {
        body.get("base_url").and_then(parse_ep)
    } else {
        existing.base_url
    };
    let anthropic_base_url = if body.as_object().map(|m| m.contains_key("anthropic_base_url")).unwrap_or(false) {
        body.get("anthropic_base_url").and_then(parse_ep)
    } else {
        existing.anthropic_base_url
    };
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

    // New multi-endpoint fields: explicit null/empty = disabled; missing = keep existing; legacy fallback only for create.
    let has_chat_key = body.as_object().map(|m| m.contains_key("chat_endpoint")).unwrap_or(false);
    let has_resp_key = body.as_object().map(|m| m.contains_key("responses_endpoint")).unwrap_or(false);
    let has_msg_key = body.as_object().map(|m| m.contains_key("messages_endpoint")).unwrap_or(false);
    let has_default_key = body.as_object().map(|m| m.contains_key("default_protocol")).unwrap_or(false);

    let chat_endpoint = if has_chat_key {
        body.get("chat_endpoint").and_then(parse_ep)
    } else {
        existing.chat_endpoint.clone()
    };
    let responses_endpoint = if has_resp_key {
        body.get("responses_endpoint").and_then(parse_ep)
    } else {
        existing.responses_endpoint.clone()
    };
    let messages_endpoint = if has_msg_key {
        body.get("messages_endpoint").and_then(parse_ep)
    } else {
        existing.messages_endpoint.clone()
    };
    let mut default_protocol = if has_default_key {
        body.get("default_protocol").and_then(|v| v.as_str()).map(|s| s.trim().to_lowercase()).unwrap_or_else(|| "chat".to_string())
    } else {
        existing.default_protocol.clone().unwrap_or_else(|| "chat".to_string())
    };
    if !["chat", "responses", "messages"].contains(&default_protocol.as_str()) {
        default_protocol = "chat".to_string();
    }

    // Validate at least one endpoint and default in enabled set
    let enabled: Vec<&str> = [
        ("chat", chat_endpoint.as_deref()),
        ("responses", responses_endpoint.as_deref()),
        ("messages", messages_endpoint.as_deref()),
    ].iter().filter_map(|(k, v)| v.filter(|s| !s.trim().is_empty()).map(|_| *k)).collect();
    if enabled.is_empty() {
        return crate::error::simple_error("At least one endpoint is required", StatusCode::BAD_REQUEST);
    }
    // Auto-fix default_protocol if it was removed
    if !enabled.contains(&default_protocol.as_str()) {
        // pick fallback chat > messages > responses among remaining
        default_protocol = if enabled.contains(&"chat") { "chat".to_string() } else if enabled.contains(&"messages") { "messages".to_string() } else { "responses".to_string() };
    }
    for ep in [&chat_endpoint, &responses_endpoint, &messages_endpoint].iter().filter_map(|o| o.as_deref()) {
        if url::Url::parse(ep).is_err() {
            return crate::error::simple_error(format!("Invalid endpoint URL: {ep}"), StatusCode::BAD_REQUEST);
        }
    }

    // Detect removed protocols for bulk alias fallback
    let was_enabled = |opt: &Option<String>| opt.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false);
    let mut removed: Vec<String> = Vec::new();
    if was_enabled(&existing.chat_endpoint) && chat_endpoint.is_none() { removed.push("chat".to_string()); }
    if was_enabled(&existing.responses_endpoint) && responses_endpoint.is_none() { removed.push("responses".to_string()); }
    if was_enabled(&existing.messages_endpoint) && messages_endpoint.is_none() { removed.push("messages".to_string()); }

    // Handle API key: if a new one is provided, encrypt it; otherwise keep the old ciphertext.
    // Bun only re-validates when api_key !== "********" or base_url is present.
    let api_key_changed = body
        .get("api_key")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty() && s != "********");
    let base_url_changed = body.get("base_url").is_some() || has_chat_key || has_msg_key;
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
                chat_endpoint: chat_endpoint.clone(),
                responses_endpoint: responses_endpoint.clone(),
                messages_endpoint: messages_endpoint.clone(),
                default_protocol: Some(default_protocol.clone()),
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
        "UPDATE accounts SET alias = ?, provider_id = ?, api_key = ?, base_url = ?, anthropic_base_url = ?, is_active = ?, weight = ?, notes = ?, openai_compatible = ?, chat_endpoint = ?, responses_endpoint = ?, messages_endpoint = ?, default_protocol = ? WHERE id = ?",
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
    .bind(&chat_endpoint)
    .bind(&responses_endpoint)
    .bind(&messages_endpoint)
    .bind(&default_protocol)
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
            // Bulk fallback aliases that forced the removed protocol
            let mut affected_ordinary: Vec<String> = Vec::new();
            let mut affected_aggregate: Vec<String> = Vec::new();
            for proto in &removed {
                let rows = sqlx::query_scalar::<_, String>("SELECT alias FROM model_aliases WHERE upstream_api = ? AND (account_ids LIKE ? OR (account_ids IS NULL AND provider_id = (SELECT provider_id FROM accounts WHERE id = ?)))")
                    .bind(proto).bind(format!("%{}%", id)).bind(id).fetch_all(&state.pool).await.unwrap_or_default();
                affected_ordinary.extend(rows);
                sqlx::query("UPDATE model_aliases SET upstream_api='default' WHERE upstream_api = ? AND (account_ids LIKE ? OR (account_ids IS NULL AND provider_id = (SELECT provider_id FROM accounts WHERE id = ?)))")
                    .bind(proto).bind(format!("%{}%", id)).bind(id).execute(&state.pool).await.ok();
                let agg_rows = sqlx::query_scalar::<_, String>("SELECT alias FROM aggregate_aliases WHERE upstream_api = ? AND EXISTS (SELECT 1 FROM json_each(candidates) WHERE json_extract(value,'$.account_id') = ?)")
                    .bind(proto).bind(id).fetch_all(&state.pool).await.unwrap_or_default();
                affected_aggregate.extend(agg_rows);
                sqlx::query("UPDATE aggregate_aliases SET upstream_api='default' WHERE upstream_api = ? AND EXISTS (SELECT 1 FROM json_each(candidates) WHERE json_extract(value,'$.account_id') = ?)")
                    .bind(proto).bind(id).execute(&state.pool).await.ok();
            }
            // Mode changed in DB — bust the hot resolution cache so the new
            // default/chat/responses/messages semantics apply immediately.
            for a in &affected_ordinary { state.invalidate_model_cache(a); }
            for a in &affected_aggregate { state.invalidate_aggregate_cache(a); }
            if !affected_ordinary.is_empty() || !affected_aggregate.is_empty() {
                Json(json!({ "success": true, "message": "Account updated successfully", "affectedAliases": { "ordinary": affected_ordinary, "aggregate": affected_aggregate }, "newDefaultProtocol": default_protocol })).into_response()
            } else {
                Json(json!({ "success": true, "message": "Account updated successfully" })).into_response()
            }
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
