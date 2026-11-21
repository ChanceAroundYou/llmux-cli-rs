use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde_json::{json, Value};

use crate::app::AppState;
use crate::codex_update;

pub async fn get_codex_update(Extension(state): Extension<AppState>) -> Response {
    Json(codex_update::status(&state).await).into_response()
}

/// 只发现最新版本并写状态，不应用。后台循环和 UI 的「立即检查」都走这里。
pub async fn check_codex_update(Extension(state): Extension<AppState>) -> Response {
    match codex_update::check(&state, false).await {
        Ok(value) => Json(value).into_response(),
        Err(e) => crate::error::simple_error(e, StatusCode::BAD_GATEWAY),
    }
}

/// `{ "version"?: "0.171.0", "pin"?: bool, "force"?: bool }`
pub async fn apply_codex_update(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let version = body
        .get("version")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string());
    let pin = body.get("pin").and_then(Value::as_bool).unwrap_or(false);
    let force = body.get("force").and_then(Value::as_bool).unwrap_or(false);

    match codex_update::apply_version(&state, version, pin, force).await {
        Ok(value) => Json(value).into_response(),
        Err(e) => crate::error::simple_error(
            format!("Codex 版本应用失败: {e}"),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    }
}

/// `{ "slugs": ["gpt-6.2-sol"] }` — 对状态页里勾选的新模型手动建别名（幂等）。
pub async fn create_codex_aliases(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let slugs: Vec<String> = body
        .get("slugs")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    if slugs.is_empty() {
        return crate::error::simple_error(
            "slugs must be a non-empty array",
            StatusCode::BAD_REQUEST,
        );
    }

    match codex_update::create_aliases_for_slugs(&state, &slugs).await {
        Ok(created) => Json(json!({ "success": true, "created": created })).into_response(),
        Err(e) => crate::error::simple_error(e, StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// 清空手动 pin，回到「自动/兜底」。`last_good_version` 保留。
pub async fn reset_codex_version(Extension(state): Extension<AppState>) -> Response {
    match llmux_core::settings::SettingsService::new(state.pool.clone())
        .set(codex_update::KEY_CLIENT_VERSION, Value::String(String::new()))
        .await
    {
        Ok(()) => {
            codex_update::invalidate_version_cache();
            Json(json!({ "success": true, "status": codex_update::status(&state).await }))
                .into_response()
        }
        Err(e) => crate::error::simple_error(
            format!("Failed to reset codex version: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}
