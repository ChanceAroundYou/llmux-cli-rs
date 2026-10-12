use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde_json::{json, Value};

use crate::app::AppState;

/// 手动禁用/启用某个账户下的某个模型。
///
/// 禁用后该模型从 `/api/models/available` 的卡片列表里消失（不再被拨测、
/// 不再出现在快速关联的目标下拉里），但**不删别名** —— 别名仍指向它，
/// 只是上游不再把它当可用模型暴露。重新启用即恢复。
///
/// 存储是 `disabled_models(account_id, model)` 一张小表（迁移 0030），
/// 与 `model_protocol_cache` 一样是「事实记录」，不参与路由决策 ——
/// 路由仍按别名走，禁用只影响「这个账户的模型列表里要不要展示它」。
pub async fn set_model_disabled(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let account_id = match body.get("account_id").and_then(Value::as_i64) {
        Some(id) if id > 0 => id,
        _ => return crate::error::simple_error("Missing required field: account_id", StatusCode::BAD_REQUEST),
    };
    let model = match body.get("model").and_then(Value::as_str) {
        Some(m) if !m.trim().is_empty() => m.trim().to_string(),
        _ => return crate::error::simple_error("Missing required field: model", StatusCode::BAD_REQUEST),
    };
    let disabled = match body.get("disabled").and_then(Value::as_bool) {
        Some(d) => d,
        None => return crate::error::simple_error("Missing required field: disabled", StatusCode::BAD_REQUEST),
    };

    // 确认账户存在（避免给不存在的账户写禁用记录）
    let exists: Option<i64> = match sqlx::query_scalar("SELECT id FROM accounts WHERE id = ?")
        .bind(account_id)
        .fetch_optional(&state.pool)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to lookup account: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        }
    };
    if exists.is_none() {
        return crate::error::simple_error("Account not found", StatusCode::NOT_FOUND);
    }

    if disabled {
        if let Err(e) = sqlx::query("INSERT OR IGNORE INTO disabled_models (account_id, model) VALUES (?, ?)")
            .bind(account_id)
            .bind(&model)
            .execute(&state.pool)
            .await
        {
            return crate::error::simple_error(
                format!("Failed to disable model: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    } else if let Err(e) = sqlx::query("DELETE FROM disabled_models WHERE account_id = ? AND model = ?")
        .bind(account_id)
        .bind(&model)
        .execute(&state.pool)
        .await
    {
        return crate::error::simple_error(
            format!("Failed to enable model: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    // 失效模型缓存，让卡片列表立刻反映
    if let Ok(mut cache) = state.models_cache.lock() {
        *cache = None;
    }
    tracing::info!(
        "🚫 [account {}] model {} {}",
        account_id,
        model,
        if disabled { "disabled" } else { "enabled" }
    );
    Json(json!({ "success": true, "account_id": account_id, "model": model, "disabled": disabled })).into_response()
}
