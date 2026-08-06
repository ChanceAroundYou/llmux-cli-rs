use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use llmux_core::adapters::Account;
use llmux_core::crypto::encrypt_api_key;
use llmux_core::repo;
use serde_json::{json, Value};

use crate::app::AppState;
use crate::routes::models::fetch_provider_models;

#[utoipa::path(
    get,
    path = "/api/accounts",
    responses(
        (status = 200, description = "账户列表（不含 api_key 密文）", body = [llmux_core::models::AccountPublic])
    )
)]
pub async fn list_accounts(Extension(state): Extension<AppState>) -> Response {
    match repo::list_accounts_public(&state.pool).await {
        Ok(accounts) => Json(serde_json::to_value(accounts).unwrap_or(Value::Array(vec![])))
            .into_response(),
        Err(e) => crate::error::simple_error(
            format!("Failed to list accounts: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

#[utoipa::path(
    post,
    path = "/api/accounts",
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "创建账户成功（返回 id / modelCount / skippedValidation）", body = serde_json::Value),
        (status = 400, description = "参数缺失或厂商未知/校验失败")
    )
)]
pub async fn create_account(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let missing = body.get("vendor_id").is_none()
        || body.get("name").is_none()
        || body.get("api_key").is_none();
    if missing {
        return crate::error::simple_error(
            "Missing required fields: vendor_id, name, api_key",
            StatusCode::BAD_REQUEST,
        );
    }

    let vendor_id = body["vendor_id"].as_str().unwrap_or_default().to_string();
    let name = body["name"].as_str().unwrap_or_default().to_string();
    let api_key_plain = body["api_key"].as_str().unwrap_or_default().to_string();
    let base_url = body["base_url"].as_str().map(|s| s.to_string());
    let anthropic_base_url = body["anthropic_base_url"].as_str().map(|s| s.to_string());
    let enabled = body["enabled"].as_i64().unwrap_or(1);
    let weight = body["weight"].as_i64().unwrap_or(1);
    let openai_compatible = body["openai_compatible"].as_i64().unwrap_or(0);
    let notes = body["notes"].as_str().map(|s| s.to_string());
    let skip_validation = body["skip_validation"].as_bool().unwrap_or(false);

    if vendor_id.is_empty() || name.is_empty() || api_key_plain.is_empty() {
        return crate::error::simple_error(
            "vendor_id, name, and api_key must not be empty",
            StatusCode::BAD_REQUEST,
        );
    }

    // 厂商校验：vendor 必须存在（DB 错误同样视为未知厂商，与旧行为一致）
    let Some((protocol, vendor_default_base)) =
        repo::get_vendor(&state.pool, &vendor_id).await.unwrap_or(None)
    else {
        return crate::error::simple_error(
            format!("Unknown vendor: {vendor_id}"),
            StatusCode::BAD_REQUEST,
        );
    };

    let custom_base_url = base_url.as_deref().is_some_and(|u| !u.is_empty());
    // 用有效 base_url 构造测试账户（base_url 为空时用厂商默认值）
    let effective_base = base_url
        .clone()
        .filter(|u| !u.is_empty())
        .or(vendor_default_base);
    let test_account = Account {
        id: 0,
        name: name.clone(),
        vendor_id: vendor_id.clone(),
        protocol: protocol.clone(),
        api_key: api_key_plain.clone(),
        base_url: effective_base,
        anthropic_base_url: anthropic_base_url.clone(),
        custom_base_url,
        custom_anthropic_base_url: anthropic_base_url.as_deref().is_some_and(|u| !u.is_empty()),
        serves_anthropic: protocol == "anthropic",
        openai_compatible,
        openai_responses: true,
        enabled,
        weight,
    };

    let (models, _) = fetch_provider_models(&test_account, &protocol).await;
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

    match repo::create_account(
        &state.pool,
        &vendor_id,
        &name,
        &encrypted_key,
        base_url.as_deref(),
        anthropic_base_url.as_deref(),
        openai_compatible,
        enabled,
        weight,
        notes.as_deref(),
    )
    .await
    {
        Ok(id) => {
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

#[utoipa::path(
    put,
    path = "/api/accounts/{id}",
    params(("id" = i64, Path, description = "账户 ID")),
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "更新账户成功", body = serde_json::Value),
        (status = 404, description = "账户不存在")
    )
)]
pub async fn update_account(
    Extension(state): Extension<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Response {
    // Verify the account exists.
    let existing = match repo::get_account(&state.pool, id).await {
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
    let vendor_id = body
        .get("vendor_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or(existing.vendor_id);
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or(existing.name);
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
    let enabled = body
        .get("enabled")
        .and_then(|v| v.as_i64())
        .unwrap_or(existing.enabled);
    let weight = body
        .get("weight")
        .and_then(|v| v.as_i64())
        .unwrap_or(existing.weight);
    let openai_compatible = body
        .get("openai_compatible")
        .and_then(|v| v.as_i64())
        .unwrap_or(existing.openai_compatible);
    let notes = body
        .get("notes")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or(existing.notes);

    let api_key_changed = body
        .get("api_key")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty() && s != "********");
    let base_url_changed = body.get("base_url").is_some();
    let skip_validation = body["skip_validation"].as_bool().unwrap_or(false);

    let api_key_ciphertext = if api_key_changed {
        let new_key = body["api_key"].as_str().unwrap_or_default();

        if api_key_changed || base_url_changed {
            let (protocol, vendor_default_base) =
                match repo::get_vendor(&state.pool, &vendor_id).await.unwrap_or(None) {
                    Some(v) => v,
                    None => {
                        return crate::error::simple_error(
                            format!("Unknown vendor: {vendor_id}"),
                            StatusCode::BAD_REQUEST,
                        );
                    }
                };
            let custom_base_url = base_url.as_deref().is_some_and(|u| !u.is_empty());
            let effective_base = base_url
                .clone()
                .filter(|u| !u.is_empty())
                .or(vendor_default_base);
            let test_account = Account {
                id,
                name: name.clone(),
                vendor_id: vendor_id.clone(),
                protocol: protocol.clone(),
                api_key: new_key.to_string(),
                base_url: effective_base,
                anthropic_base_url: anthropic_base_url.clone(),
                custom_base_url,
                custom_anthropic_base_url: anthropic_base_url
                    .as_deref()
                    .is_some_and(|u| !u.is_empty()),
                serves_anthropic: protocol == "anthropic",
                openai_compatible,
                openai_responses: true,
                enabled,
                weight,
            };

            let (models, _) = fetch_provider_models(&test_account, &protocol).await;
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
        existing.api_key_enc
    };

    match repo::update_account(
        &state.pool,
        id,
        &vendor_id,
        &name,
        &api_key_ciphertext,
        base_url.as_deref(),
        anthropic_base_url.as_deref(),
        openai_compatible,
        enabled,
        weight,
        notes.as_deref(),
    )
    .await
    {
        Ok(_) => Json(json!({ "success": true, "message": "Account updated successfully" }))
            .into_response(),
        Err(e) => crate::error::simple_error(
            format!("Failed to update account: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

#[utoipa::path(
    delete,
    path = "/api/accounts/{id}",
    params(("id" = i64, Path, description = "账户 ID")),
    responses(
        (status = 200, description = "删除账户成功", body = serde_json::Value),
        (status = 404, description = "账户不存在")
    )
)]
pub async fn delete_account(
    Extension(state): Extension<AppState>,
    Path(id): Path<i64>,
) -> Response {
    // 外键开启时：删账户 → 绑定 CASCADE 清空、usage_logs.account_id SET NULL
    //（account_name 快照保留）、dispatch_state 由调度器自然清理。
    match repo::delete_account(&state.pool, id).await {
        Ok(affected) => {
            if affected == 0 {
                return crate::error::simple_error(
                    format!("Account with id {id} not found"),
                    StatusCode::NOT_FOUND,
                );
            }
            Json(json!({
                "success": true,
                "message": "Account deleted; bindings removed, history retained (snapshot kept)"
            }))
            .into_response()
        }
        Err(e) => crate::error::simple_error(
            format!("Failed to delete account: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

/// CSV 字段转义（RFC 4180）：含逗号/引号/换行的字段用双引号包裹，内部引号翻倍。
fn csv_escape(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

#[utoipa::path(
    get,
    path = "/api/accounts/{id}/export",
    params(("id" = i64, Path, description = "账户 ID")),
    responses(
        (status = 200, description = "账户用量历史 CSV 导出（附件下载）", content_type = "text/csv")
    )
)]
pub async fn export_account_usage(
    Extension(state): Extension<AppState>,
    Path(id): Path<i64>,
) -> Response {
    let rows = match repo::list_account_usage_logs(&state.pool, id).await {
        Ok(rows) => rows,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to query usage logs: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let mut csv = String::from("Timestamp,Model,Latency (ms),Status,Error\n");
    for (ts, model, latency, success, error) in &rows {
        let status = if *success != 0 { "Success" } else { "Failed" };
        let model = model.as_deref().unwrap_or("unknown");
        let error = error.as_deref().unwrap_or("");
        // 字段做 CSV 转义，避免 model/error 含逗号或引号破坏列结构
        csv.push_str(&format!(
            "{},{},{},{},{}\n",
            csv_escape(&ts.to_string()),
            csv_escape(model),
            csv_escape(&latency.to_string()),
            csv_escape(status),
            csv_escape(error),
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
