//! 价目管理接口：列出 / 手动刷新 / 手工定价。
//!
//! 手工定价走 `PUT /api/model-prices` 的 JSON body（不是路径参数）：模型名里
//! 可能带 `/`（如 `stealth/space-bunny-alpha`），放进路径参数会被 axum 拆错。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use llmux_core::models::ModelPrice;
use serde_json::{json, Value};
use sqlx::Row;

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
         LEFT JOIN model_prices mp ON mp.model_id = l.model
         LEFT JOIN upstream_prices up ON up.account_id = l.account_id AND up.model_id = l.model
         WHERE l.is_test = 0 AND l.model IS NOT NULL AND l.model != ''
           AND mp.model_id IS NULL AND up.model_id IS NULL
         ORDER BY l.model",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    // 按上游账号的真实价目（go2/go6/go7 的 OpenCode Go、Zen、OpenRouter…）。
    let upstream: Vec<Value> = sqlx::query(
        "SELECT up.account_id AS account_id, up.model_id AS model_id, a.alias AS alias,
                up.input_price, up.output_price, up.cache_read_price, up.cache_write_price,
                up.long_context_threshold, up.long_input_price, up.long_output_price,
                up.source, up.updated_at
         FROM upstream_prices up LEFT JOIN accounts a ON a.id = up.account_id
         ORDER BY up.account_id, up.model_id",
    )
    .fetch_all(&state.pool)
    .await
    .map(|rows| {
        rows.iter()
            .map(|r| {
                json!({
                    "accountId": r.try_get::<i64, _>("account_id").unwrap_or(0),
                    "alias": r.try_get::<Option<String>, _>("alias").unwrap_or(None),
                    "modelId": r.try_get::<String, _>("model_id").unwrap_or_default(),
                    "inputPrice": r.try_get::<Option<f64>, _>("input_price").unwrap_or(None),
                    "outputPrice": r.try_get::<Option<f64>, _>("output_price").unwrap_or(None),
                    "cacheReadPrice": r.try_get::<Option<f64>, _>("cache_read_price").unwrap_or(None),
                    "cacheWritePrice": r.try_get::<Option<f64>, _>("cache_write_price").unwrap_or(None),
                    "longContextThreshold": r.try_get::<Option<i64>, _>("long_context_threshold").unwrap_or(None),
                    "longInputPrice": r.try_get::<Option<f64>, _>("long_input_price").unwrap_or(None),
                    "longOutputPrice": r.try_get::<Option<f64>, _>("long_output_price").unwrap_or(None),
                    "source": r.try_get::<Option<String>, _>("source").unwrap_or(None),
                    "updatedAt": r.try_get::<Option<String>, _>("updated_at").unwrap_or(None),
                })
            })
            .collect()
    })
    .unwrap_or_default();

    Json(json!({ "prices": prices, "upstream": upstream, "unpriced": unpriced })).into_response()
}

/// 立即拉一次价目：OpenRouter 全局目录 + 按上游账号的真实价目。
/// 人工 / free 行不受影响。
pub async fn refresh_model_prices(Extension(state): Extension<AppState>) -> Response {
    let catalog = model_prices::refresh(&state.pool).await;
    let upstream = crate::price_sources::refresh_all(&state.pool).await;
    match (catalog, upstream) {
        (Ok(c), Ok(u)) => {
            Json(json!({ "success": true, "report": c, "upstream": u })).into_response()
        }
        (Err(e), _) => simple_error(
            format!("OpenRouter refresh failed: {e}"),
            StatusCode::BAD_GATEWAY,
        ),
        (_, Err(e)) => simple_error(
            format!("Upstream refresh failed: {e}"),
            StatusCode::BAD_GATEWAY,
        ),
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

    // 带 accountId → 写「账号专属」manual 行；否则写全局目录。
    if let Some(account_id) = body.get("accountId").and_then(|v| v.as_i64()) {
        return match sqlx::query(
            "INSERT INTO upstream_prices
                (account_id, model_id, vendor, input_price, output_price,
                 cache_read_price, cache_write_price, source)
             VALUES (?, ?, ?, ?, ?, ?, ?, 'manual')
             ON CONFLICT(account_id, model_id) DO UPDATE SET
                vendor = COALESCE(excluded.vendor, upstream_prices.vendor),
                input_price = excluded.input_price,
                output_price = excluded.output_price,
                cache_read_price = excluded.cache_read_price,
                cache_write_price = excluded.cache_write_price,
                source = 'manual',
                updated_at = CURRENT_TIMESTAMP",
        )
        .bind(account_id)
        .bind(model_id)
        .bind(vendor)
        .bind(input)
        .bind(output)
        .bind(num("cacheReadPrice"))
        .bind(num("cacheWritePrice"))
        .execute(&state.pool)
        .await
        {
            Ok(_) => Json(json!({ "success": true })).into_response(),
            Err(e) => simple_error(
                format!("Database error: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        };
    }

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
