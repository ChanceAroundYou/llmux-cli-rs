use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use llmux_core::export_import::{import_config as core_import_config, ConfigExport};
use llmux_core::settings::SettingsService;
use serde_json::{json, Value};

use crate::app::AppState;

#[utoipa::path(
    get,
    path = "/api/settings",
    responses(
        (status = 200, description = "应用设置键值对", body = serde_json::Value)
    )
)]
pub async fn get_settings(Extension(state): Extension<AppState>) -> Response {
    match SettingsService::new(state.pool.clone()).get_all().await {
        Ok(settings) => Json(Value::Object(settings)).into_response(),
        Err(e) => crate::error::simple_error(
            format!("Failed to load settings: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

#[utoipa::path(
    put,
    path = "/api/settings",
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "批量更新设置成功", body = serde_json::Value)
    )
)]
pub async fn update_settings(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let settings_map = match body {
        Value::Object(map) => map,
        _ => {
            return crate::error::simple_error(
                "Request body must be a JSON object",
                StatusCode::BAD_REQUEST,
            );
        }
    };

    if settings_map.is_empty() {
        return Json(json!({ "success": true })).into_response();
    }

    match SettingsService::new(state.pool.clone())
        .batch_set(&settings_map)
        .await
    {
        Ok(()) => Json(json!({ "success": true })).into_response(),
        Err(e) => crate::error::simple_error(
            format!("Failed to update settings: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

#[utoipa::path(
    post,
    path = "/api/settings/reset",
    responses(
        (status = 200, description = "清空运行数据（保留 vendors/dispatch_state/app_settings/gateway_key）", body = serde_json::Value)
    )
)]
pub async fn purge_database(Extension(state): Extension<AppState>) -> Response {
    let mut tx = match state.pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to start transaction: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    tracing::info!("⚙️ Purging database...");

    // 依赖顺序删除运行数据；vendors 目录保留（内置种子 + 用户自建厂商），
    // dispatch_state 保留，app_settings（含 gateway_key）保留 —— 清库后客户端 key 不失效。
    for table in &[
        "usage_logs",
        "model_alias_accounts",
        "api_key_models",
        "api_keys",
        "model_aliases",
        "accounts",
    ] {
        if let Err(e) = sqlx::query(&format!("DELETE FROM {table}"))
            .execute(&mut *tx)
            .await
        {
            tracing::error!("⚙️ Failed to purge table {table}: {e}");
            return crate::error::simple_error(
                format!("Failed to purge {table}: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
        tracing::info!("⚙️ Purged table: {table}");
    }

    if let Err(e) = tx.commit().await {
        return crate::error::simple_error(
            format!("Failed to commit purge transaction: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    // VACUUM to reclaim disk space (must run outside a transaction).
    if let Err(e) = sqlx::query("VACUUM").execute(&state.pool).await {
        return crate::error::simple_error(
            format!("Failed to vacuum database: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    tracing::info!("⚙️ Database purged and vacuumed successfully");
    Json(json!({ "success": true, "message": "Database purged successfully" })).into_response()
}

#[utoipa::path(
    get,
    path = "/api/export",
    responses(
        (status = 200, description = "导出全量配置 JSON（application/json 附件下载）")
    )
)]
pub async fn export_config(Extension(state): Extension<AppState>) -> Response {
    match llmux_core::export_import::export_config(&state.pool, &state.master_key).await {
        Ok(config) => {
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let filename = format!("llmux-config-{}.json", timestamp);
            let body = serde_json::to_string(&config).unwrap_or_default();

            let mut response = (StatusCode::OK, body).into_response();
            response.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("application/json"),
            );
            if let Ok(value) = axum::http::HeaderValue::from_str(&format!(
                "attachment; filename=\"{}\"",
                filename
            )) {
                response
                    .headers_mut()
                    .insert(axum::http::header::CONTENT_DISPOSITION, value);
            }
            response
        }
        Err(e) => crate::error::simple_error(
            format!("Failed to export config: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}

#[utoipa::path(
    post,
    path = "/api/import",
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "导入配置结果（imported 含 accounts/aliases/keys 计数）", body = serde_json::Value),
        (status = 400, description = "配置格式无效")
    )
)]
pub async fn import_config(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    match serde_json::from_value::<ConfigExport>(body) {
        Ok(config) => match core_import_config(&state.pool, config, &state.master_key).await {
            Ok(counts) => Json(json!({
                "success": true,
                "imported": {
                    "accounts": counts.accounts,
                    "aliases": counts.aliases,
                    "keys": counts.keys,
                },
            }))
            .into_response(),
            Err(e) => crate::error::simple_error(
                format!("Import failed: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        },
        Err(e) => crate::error::simple_error(
            format!("Invalid config format: {e}"),
            StatusCode::BAD_REQUEST,
        ),
    }
}
