use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde_json::{json, Value};
use sqlx::Row;

use crate::app::AppState;

pub async fn get_health_status(Extension(state): Extension<AppState>) -> Response {
    // Single GROUP BY — was N serial queries (one per account).
    let rows = match sqlx::query(
        "SELECT a.id, a.alias, \
                COALESCE(s.total, 0) AS total, \
                COALESCE(s.success, 0) AS success \
         FROM accounts a \
         LEFT JOIN ( \
           SELECT account_id, COUNT(*) AS total, \
                  SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) AS success \
           FROM usage_logs GROUP BY account_id \
         ) s ON s.account_id = a.id \
         ORDER BY a.id",
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to query accounts: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    tracing::info!("💚 Health check for {} accounts (single query)", rows.len());

    let health_data: Vec<Value> = rows
        .iter()
        .map(|row| {
            let acc_id: i64 = row.try_get("id").unwrap_or_default();
            let alias: String = row.try_get("alias").unwrap_or_default();
            let total: i64 = row.try_get("total").unwrap_or_default();
            let success_count: i64 = row.try_get("success").unwrap_or_default();
            let status = if total > 0 {
                let rate = success_count as f64 / total as f64;
                if rate > 0.9 {
                    "healthy"
                } else if rate > 0.5 {
                    "degraded"
                } else {
                    "down"
                }
            } else {
                "unknown"
            };
            json!({
                "id": format!("acc_{acc_id}"),
                "name": alias,
                "status": status,
                // 曾经叫 `lastSuccess`，但装的其实是成功**次数**，不是时间戳。
                // 读的人一定会误读成「最后一次成功距今多久」（本轮就被坑过一次：
                // 看到 free 的 40 以为是 40 秒前刚成功过）。UI 当时没读这个字段，
                // 所以没造成可见故障，但它是个等着坑下一个人的陷阱。
                // 改名而不是补一个真时间戳：真正需要「距今多久」的地方
                // （模型健康、请求日志）已经从 usage_logs 的 timestamp 单独查了，
                // 这个接口的职责就是给出总调用量与成功量，供算成功率。
                "successCount": success_count,
                "totalChecks": total,
            })
        })
        .collect();

    Json(Value::Array(health_data)).into_response()
}
