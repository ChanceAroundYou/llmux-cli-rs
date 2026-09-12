use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde_json::{json, Value};
use sqlx::{sqlite::SqliteRow, Row};

use crate::app::AppState;

pub async fn get_models_health(Extension(state): Extension<AppState>) -> Response {
    // Match Bun backend: for each (account_id, model) group, return the LATEST
    // usage_log row's success, latency_ms as latency, error_message as error,
    // timestamp as last_checked. Also include limits_cache (JSON-parsed),
    // limits_cache_updated_at, and account alias/provider from accounts table.
    // 键集合 = 真实流量 ∪ 拨测结果。必须取并集：只拨测过、没被真实调用过的
    // (账户, 模型) 在旧写法（FROM usage_logs）下会整个消失。
    let rows: Vec<SqliteRow> = match sqlx::query(
        "SELECT k.account_id, k.model, a.provider_id, a.limits_cache, a.limits_cache_updated_at, \
                a.alias AS account_name, \
                t.timestamp AS traffic_at, t.success AS traffic_ok, \
                t.latency_ms AS traffic_latency, t.error_message AS traffic_err \
         FROM ( \
           SELECT account_id, model FROM usage_logs \
           UNION \
           SELECT account_id, model FROM model_test_results \
         ) k \
         JOIN accounts a ON a.id = k.account_id \
         LEFT JOIN usage_logs t ON t.id = ( \
           SELECT id FROM usage_logs \
           WHERE account_id = k.account_id AND model = k.model \
           ORDER BY id DESC LIMIT 1 \
         )",
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to get model health: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let protocols = llmux_core::probe::load_protocol_map(&state.pool).await;

    // 拨测结果（独立表）按 (account_id, model) 取出，用于与真实流量合并展示：
    // 两边各带自己的时间戳，谁更新都不会抹掉对方。
    let test_rows: Vec<(i64, String, i64, i64, Option<String>, Option<String>, Option<String>, i64)> =
        sqlx::query_as(
            "SELECT account_id, model, success, latency_ms, error_message, via, supported, checked_at \
             FROM model_test_results",
        )
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default();
    let tests: std::collections::HashMap<(i64, String), (i64, i64, Option<String>, Option<String>, Option<String>, i64)> =
        test_rows
            .into_iter()
            .map(|(a, m, ok, lat, err, via, sup, at)| ((a, m), (ok, lat, err, via, sup, at)))
            .collect();

    let health: Vec<Value> = rows
        .iter()
        .map(|row: &SqliteRow| {
            let limits_cache_str: Option<String> =
                row.try_get("limits_cache").unwrap_or_default();
            let limits_cache: Value = limits_cache_str
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(Value::Null);
            let account_id = row.try_get::<i64, _>("account_id").unwrap_or_default();
            let model = row.try_get::<String, _>("model").unwrap_or_default();
            let traffic_at = row.try_get::<Option<i64>, _>("traffic_at").ok().flatten().unwrap_or(0);
            let test = tests.get(&(account_id, model.clone()));
            // 展示层面合并「最近一次状态」：以时间戳更近的一方为准 —— 报错文案
            // 允许互相覆盖（用户要求），但两张表各自的记录始终保留、互不抹除。
            let test_at = test.map(|t| t.5).unwrap_or(0);
            let (success, latency, error, last_checked) = match test {
                Some((ok, lat, err, _, _, _)) if test_at > traffic_at => (*ok, *lat, err.clone(), test_at),
                _ => (
                    row.try_get::<Option<i64>, _>("traffic_ok").ok().flatten().unwrap_or_default(),
                    row.try_get::<Option<i64>, _>("traffic_latency").ok().flatten().unwrap_or_default(),
                    row.try_get::<Option<String>, _>("traffic_err").ok().flatten(),
                    traffic_at,
                ),
            };
            // 可用协议优先用拨测记下的（更全），否则退回协议缓存，供角标用。
            let supported: Vec<String> = match test {
                Some((_, _, _, _, Some(sup), _)) => {
                    sup.split(',').filter(|x| !x.is_empty()).map(String::from).collect()
                }
                _ => protocols
                    .get(&(account_id, model.clone()))
                    .map(|v| v.iter().map(|p| p.as_str().to_string()).collect())
                    .unwrap_or_default(),
            };
            json!({
                "account_id": account_id,
                "provider_id": row.try_get::<String, _>("provider_id").unwrap_or_default(),
                "model": model,
                "last_checked": last_checked,
                "success": success,
                "latency": latency,
                "error": error,
                "limits_cache": limits_cache,
                "limits_cache_updated_at": row.try_get::<Option<String>, _>("limits_cache_updated_at").unwrap_or_default(),
                "account_name": row.try_get::<String, _>("account_name").unwrap_or_default(),
                "supported": supported,
                // 拨测结果单独回传，UI 可区分「拨测」与「真实流量」
                "test": test.map(|(ok, lat, err, _, _, at)| json!({
                    "success": ok, "latency": lat, "error": err, "checked_at": at,
                })),
            })
        })
        .collect();

    Json(Value::Array(health)).into_response()
}
