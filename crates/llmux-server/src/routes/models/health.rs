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

    let tests = llmux_core::probe::load_test_results(&state.pool).await;
    // 连续失败暂停状态（方案 A）。UI 据此把卡片打灰并显示还剩多久，
    // 而不是让一堆已下架的模型永远挂着红点。
    let suspensions = llmux_core::probe::load_suspensions(&state.pool).await;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;

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
            // 拨测记录（含协议集合）。由 usage_logs 之外独立提供 —— 见 0019/0020。
            let test = tests.get(&(account_id, model.clone()));
            // 展示层面合并「最近一次状态」：拨测（限手工，`manual`）与真实流量
            // 各带时间戳，谁更近谁显示。后台聚合探活/别名校验不在这里抢 ——
            // 它们每 300s 一轮，会把用户手动拨测的结果一遍遍刷掉。
            let (success, latency, error, last_checked) = match test.filter(|t| t.is_manual() && t.checked_at > traffic_at) {
                Some(t) => (t.success, t.latency_ms, t.error_message.clone(), t.checked_at),
                None => (
                    row.try_get::<Option<i64>, _>("traffic_ok").ok().flatten().unwrap_or_default(),
                    row.try_get::<Option<i64>, _>("traffic_latency").ok().flatten().unwrap_or_default(),
                    row.try_get::<Option<String>, _>("traffic_err").ok().flatten(),
                    traffic_at,
                )
            };
            // 可用协议 = 该 (账户, 模型) 最近一次探测记下的集合（角标用），
            // 三个来源通用 —— 角标只看事实，不涉及「谁抢谁」。
            let supported: Vec<String> = test
                .map(|t| t.protocols().iter().map(|p| p.as_str().to_string()).collect())
                .unwrap_or_default();
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
                "test": test.map(|t| json!({
                    "success": t.success, "latency": t.latency_ms,
                    "error": t.error_message, "checked_at": t.checked_at,
                    "via": t.via, "source": t.source,
                })),
                // 自动拨测是否被暂停（连续失败 N 次触发）。`suspended` 为当前
                // 是否仍在冷却期内，`failures` 用于展示原因。
                "suspension": suspensions.get(&(account_id, model.clone())).map(|s| json!({
                    "suspended": s.is_suspended(now_ms),
                    "failures": s.consecutive_failures,
                    "remaining_secs": s.remaining_secs(now_ms),
                    "last_error": s.last_error,
                })),
            })
        })
        .collect();

    Json(Value::Array(health)).into_response()
}
