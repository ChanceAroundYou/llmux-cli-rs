//! 价目管理接口：列出 / 手动刷新 / 手工定价。
//!
//! 手工定价走 `PUT /api/model-prices` 的 JSON body（不是路径参数）：模型名里
//! 可能带 `/`（如 `stealth/space-bunny-alpha`），放进路径参数会被 axum 拆错。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use llmux_core::models::ModelPrice;
use serde_json::{json, Value};

use crate::app::AppState;
use crate::error::simple_error;
use crate::model_prices;

/// 列出全部价目，并附上「有流量却没价目」的模型名 —— 让估算偏低可见。
pub async fn list_model_prices(Extension(state): Extension<AppState>) -> Response {
    let prices = sqlx::query_as::<_, ModelPrice>(
        "SELECT model_id, vendor, input_price, output_price, cache_read_price,
                cache_write_price, source, source_model_id, updated_at
         FROM model_prices ORDER BY model_id",
    )
    .fetch_all(&state.pool)
    .await;
    let prices = match prices {
        Ok(p) => p,
        Err(e) => {
            return simple_error(
                format!("Database error: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        }
    };

    let unpriced: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT l.model FROM usage_logs l
         LEFT JOIN model_prices p ON p.model_id = l.model
         WHERE l.is_test = 0 AND l.model IS NOT NULL AND l.model != '' AND p.model_id IS NULL
         ORDER BY l.model",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    Json(json!({ "prices": prices, "unpriced": unpriced })).into_response()
}

/// 立即拉一次 OpenRouter 价目（人工/免费 0 价行不受影响）。
pub async fn refresh_model_prices(Extension(state): Extension<AppState>) -> Response {
    match model_prices::refresh(&state.pool).await {
        Ok(report) => Json(json!({ "success": true, "report": report })).into_response(),
        Err(e) => simple_error(format!("Refresh failed: {e}"), StatusCode::BAD_GATEWAY),
    }
}

/// 手工定价。写入即 `source='manual'`，从此 6h 刷新不再覆盖这一行。
pub async fn set_model_price(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let Some(model_id) = body
        .get("modelId")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return simple_error("modelId is required", StatusCode::BAD_REQUEST);
    };

    let num = |key: &str| -> Option<f64> {
        body.get(key).and_then(|v| {
            if v.is_null() {
                None
            } else if let Some(n) = v.as_f64() {
                Some(n)
            } else {
                v.as_str()?.trim().parse().ok()
            }
        })
    };
    let (Some(input), Some(output)) = (num("inputPrice"), num("outputPrice")) else {
        return simple_error(
            "inputPrice and outputPrice are required",
            StatusCode::BAD_REQUEST,
        );
    };

    let vendor = body.get("vendor").and_then(|v| v.as_str());
    match model_prices::set_manual_price(
        &state.pool,
        model_id,
        vendor,
        input,
        output,
        num("cacheReadPrice"),
        num("cacheWritePrice"),
    )
    .await
    {
        Ok(()) => Json(json!({ "success": true })).into_response(),
        Err(e) => simple_error(
            format!("Database error: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    }
}
