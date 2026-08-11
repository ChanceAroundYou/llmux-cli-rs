use axum::{
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use llmux_core::repo;
use serde::Serialize;
use serde_json::{json, Value};
use utoipa::ToSchema;

use crate::app::AppState;

/// 绑定账户条目：含账户名 + 厂商信息 + 首选标记（前端直接渲染，无需再查）。
#[derive(Serialize, ToSchema)]
pub struct AliasAccountSummary {
    pub id: i64,
    pub name: String,
    pub vendor_id: String,
    pub vendor_name: String,
    pub protocol: String,
    pub is_preferred: bool,
}

/// alias 列表条目：绑定账户数组 accounts 替代 account_ids 数字，
/// 每个账户带完整信息与首选标记。
#[derive(Serialize, ToSchema)]
pub struct AliasResponse {
    pub id: i64,
    pub alias: String,
    pub target_model: String,
    pub vendor_id: Option<String>,
    pub created_at: Option<String>,
    pub accounts: Vec<AliasAccountSummary>,
    pub preferred_account_id: Option<i64>,
}

#[utoipa::path(
    get,
    path = "/api/models/aliases",
    responses(
        (status = 200, description = "别名列表（含厂商聚合）", body = [AliasResponse])
    )
)]
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

    // 一次批量查询所有 alias 的绑定 + 厂商信息（消除 N+1）
    let alias_ids: Vec<i64> = rows.iter().map(|a| a.id.unwrap_or_default()).collect();
    let bindings_map = repo::list_alias_bindings_with_vendors(&state.pool, &alias_ids)
        .await
        .unwrap_or_default();

    let mut aliases = Vec::with_capacity(rows.len());
    for alias in &rows {
        let id = alias.id.unwrap_or_default();
        let bindings = bindings_map.get(&id).cloned().unwrap_or_default();

        // 绑定账户数组（含账户名 + 厂商信息 + 首选标记）
        let accounts: Vec<AliasAccountSummary> = bindings
            .iter()
            .map(|b| AliasAccountSummary {
                id: b.account_id,
                name: b.account_name.clone(),
                vendor_id: b.vendor_id.clone(),
                vendor_name: b.vendor_name.clone(),
                protocol: b.protocol.clone(),
                is_preferred: b.is_preferred == 1,
            })
            .collect();
        // preferred_account_id = is_preferred 标记的账户
        let preferred_account_id = bindings
            .iter()
            .find(|b| b.is_preferred == 1)
            .map(|b| b.account_id);

        aliases.push(AliasResponse {
            id,
            alias: alias.alias.clone(),
            target_model: alias.target_model.clone(),
            vendor_id: alias.vendor_id.clone(),
            created_at: alias.created_at.clone(),
            accounts,
            preferred_account_id,
        });
    }

    Json(aliases).into_response()
}

#[utoipa::path(
    post,
    path = "/api/models/aliases",
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "设置（创建/更新）别名成功", body = crate::api_schemas::MessageResponse)
    )
)]
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

#[utoipa::path(
    delete,
    path = "/api/models/aliases/{id}",
    params(("id" = i64, Path, description = "别名 ID")),
    responses(
        (status = 200, description = "删除别名成功", body = crate::api_schemas::MessageResponse),
        (status = 404, description = "别名不存在", body = crate::api_schemas::ErrorResponse)
    )
)]
pub async fn delete_model_alias(
    Extension(state): Extension<AppState>,
    Path(id): Path<i64>,
) -> Response {
    // 确认 alias 存在（不存在返回 404）
    match repo::get_alias_name_by_id(&state.pool, id).await {
        Ok(Some(_)) => {}
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

    // 不再清理 key 白名单里与 alias 同名的条目：白名单条目可能是真实 model 名，
    // 误删会清空受限 key 的白名单 → 静默升级为全模型可用（安全漏洞）。
    // 残留条目只会让该 key 对 alias 名的请求走前缀回退透传到上游（上游拒绝未知模型），
    // 不会扩大 key 的真实权限，属可接受的悬挂引用。

    // Invalidate models cache
    if let Ok(mut cache) = state.models_cache.lock() {
        *cache = None;
    }

    Json(json!({ "success": true, "message": "Alias deleted successfully" })).into_response()
}
