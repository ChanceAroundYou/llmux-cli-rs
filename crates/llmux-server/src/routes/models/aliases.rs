use axum::{
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use llmux_core::repo;
use serde_json::{json, Value};

use crate::app::AppState;

/// 读取一个 alias 的绑定账户集：返回 (account_ids, preferred_account_id)。
async fn load_bindings(state: &AppState, alias_id: i64) -> (Vec<i64>, Option<i64>) {
    let rows = repo::list_alias_bindings(&state.pool, alias_id)
        .await
        .unwrap_or_default();
    let account_ids = rows.iter().map(|(id, _)| *id).collect();
    let preferred = rows.iter().find(|(_, p)| *p == 1).map(|(id, _)| *id);
    (account_ids, preferred)
}

pub async fn get_model_aliases(Extension(state): Extension<AppState>) -> Response {
    let rows = match repo::list_aliases(&state.pool).await {
        Ok(rows) => rows,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to list aliases: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let mut aliases = Vec::with_capacity(rows.len());
    for alias in &rows {
        let id = alias.id.unwrap_or_default();
        let (account_ids, preferred_account_id) = load_bindings(&state, id).await;
        aliases.push(json!({
            "id": id,
            "alias": alias.alias,
            "target_model": alias.target_model,
            "vendor_id": alias.vendor_id,
            "created_at": alias.created_at,
            "account_ids": account_ids,
            "preferred_account_id": preferred_account_id,
        }));
    }

    Json(Value::Array(aliases)).into_response()
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
    let vendor_id = body.get("vendor_id").and_then(Value::as_str);

    // 绑定账户集：[1,5] 或逗号串 "1,5"；preferred_account_id 为其中首选
    let account_ids: Vec<i64> = match body.get("account_ids") {
        Some(Value::Array(arr)) => arr.iter().filter_map(|v| v.as_i64()).collect(),
        Some(Value::String(s)) => s
            .split(',')
            .filter_map(|p| p.trim().parse::<i64>().ok())
            .collect(),
        _ => vec![],
    };
    let preferred_account_id = body
        .get("preferred_account_id")
        .and_then(|v| v.as_i64());

    let alias_id = match repo::upsert_alias(&state.pool, alias, target_model, vendor_id).await {
        Ok(id) => id,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to set alias: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // 替换绑定集（repo 内部事务保证 清空+写入 原子性）
    if let Err(e) = repo::replace_alias_bindings(&state.pool, alias_id, &account_ids, preferred_account_id).await {
        return crate::error::simple_error(
            format!("Failed to reset bindings: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    // Invalidate models cache so custom models appear immediately
    if let Ok(mut cache) = state.models_cache.lock() {
        *cache = None;
    }
    tracing::info!(
        "🏷️ Set alias {} -> {} (vendor: {:?}, {} bindings), cache invalidated",
        alias,
        target_model,
        vendor_id,
        account_ids.len()
    );
    Json(json!({ "success": true, "message": "Alias set successfully" })).into_response()
}

pub async fn delete_model_alias(
    Extension(state): Extension<AppState>,
    Path(id): Path<i64>,
) -> Response {
    let alias_name = match repo::get_alias_name_by_id(&state.pool, id).await {
        Ok(Some(name)) => name,
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

    // 删 alias → 绑定行 CASCADE 清空
    if let Err(e) = repo::delete_alias(&state.pool, id).await {
        return crate::error::simple_error(
            format!("Failed to delete alias: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    // 顺带清理 key 白名单里指向该 alias 的条目（避免悬挂引用）
    let _ = repo::delete_key_model_by_name(&state.pool, &alias_name).await;

    // Invalidate models cache
    if let Ok(mut cache) = state.models_cache.lock() {
        *cache = None;
    }

    Json(json!({ "success": true, "message": "Alias deleted successfully" })).into_response()
}
