use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use llmux_core::repo;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;

/// 白名单解析：`"*"` = 不限制；JSON 数组（或数组字符串）→ 多个模型；裸字符串 → 单个模型名。
/// 注意：裸字符串必须按「限制为这一个模型」处理，否则调用方以为限制了、实际却放行全部（安全）。
fn parse_allowed_models(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) if s == "*" => vec![],
        Value::String(s) => {
            // 兼容 JSON 数组字符串 "[\"a\",\"b\"]"
            serde_json::from_str::<Vec<String>>(s)
                .unwrap_or_else(|_| vec![s.to_string()])
        }
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => vec![],
    }
}

#[utoipa::path(
    get,
    path = "/api/keys",
    responses(
        (status = 200, description = "网关 key 列表（含 allowed_models 白名单）", body = [llmux_core::models::ApiKey])
    )
)]
pub async fn list_api_keys(Extension(state): Extension<AppState>) -> Response {
    let rows = match repo::list_api_keys(&state.pool).await {
        Ok(rows) => rows,
        Err(e) => {
            return crate::error::simple_error(
                format!("Database error: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let mut result = Vec::with_capacity(rows.len());
    for key in rows {
        let models: Vec<String> = repo::list_key_models(&state.pool, key.id.unwrap_or_default())
            .await
            .unwrap_or_default();
        let allowed_models = if models.is_empty() {
            Value::String("*".to_string())
        } else {
            Value::Array(models.into_iter().map(Value::String).collect())
        };
        result.push(json!({
            "id": key.id,
            "name": key.name,
            "key": key.key,
            "enabled": key.enabled,
            "last_used_at": key.last_used_at,
            "created_at": key.created_at,
            "allowed_models": allowed_models,
        }));
    }

    Json(Value::Array(result)).into_response()
}

#[utoipa::path(
    post,
    path = "/api/keys",
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "创建网关 key 成功（返回明文 key）", body = serde_json::Value)
    )
)]
pub async fn create_api_key(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Unnamed Key");
    let allowed_models = parse_allowed_models(
        body.get("allowed_models").unwrap_or(&Value::String("*".into())),
    );

    // 网关 key 明文存储（厂商 key 已单独加密，见 schema 注释），可随时回读用于一键配置
    let key = format!("sk-llmux-{}", Uuid::new_v4().simple());

    let key_id = match repo::create_api_key(&state.pool, name, &key).await {
        Ok(id) => id,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to create API key: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    if let Err(e) = repo::replace_key_models(&state.pool, key_id, &allowed_models).await {
        return crate::error::simple_error(
            format!("Failed to store allowed model: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    Json(json!({
        "success": true,
        "id": key_id,
        "key": key,
    }))
    .into_response()
}

#[utoipa::path(
    put,
    path = "/api/keys/{id}",
    params(("id" = i64, Path, description = "网关 key ID")),
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "更新网关 key 成功", body = serde_json::Value)
    )
)]
pub async fn update_api_key(
    Extension(state): Extension<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> Response {
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Untitled Key");

    // Bun 在 key 不存在时也静默成功，保持一致。
    if let Err(e) = repo::update_api_key_name(&state.pool, id, name).await {
        return crate::error::simple_error(
            format!("Failed to update API key: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    // 白名单替换
    if let Some(rule) = body.get("allowed_models") {
        let allowed_models = parse_allowed_models(rule);
        if let Err(e) = repo::replace_key_models(&state.pool, id, &allowed_models).await {
            return crate::error::simple_error(
                format!("Failed to reset allowed models: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    }

    Json(json!({ "success": true })).into_response()
}

#[utoipa::path(
    delete,
    path = "/api/keys/{id}",
    params(("id" = i64, Path, description = "网关 key ID")),
    responses(
        (status = 200, description = "删除网关 key 成功", body = serde_json::Value)
    )
)]
pub async fn delete_api_key(
    Extension(state): Extension<AppState>,
    Path(id): Path<i64>,
) -> Response {
    // Bun 在 key 不存在时也静默成功 —— 保持一致。
    // 外键开启时 api_key_models 白名单自动 CASCADE 清空。
    match repo::delete_api_key(&state.pool, id).await {
        Ok(_) => Json(json!({ "success": true })).into_response(),
        Err(e) => crate::error::simple_error(
            format!("Failed to delete API key: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}
