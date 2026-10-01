use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde_json::{json, Value};
use sqlx::Row;

use crate::app::AppState;

/// 成功率统计的时间窗（天）。
///
/// 此前 `usage_logs GROUP BY account_id` **没有时间窗**，扫全表。而
/// `usage_logs` 的行永不删除（body 被置 NULL，行与统计保留 —— 设计决定），
/// 2026-10 已达 10.7 万行 / 103 MiB，且只涨不跌：这个查询挂在 `/api/health`
/// 与 `/api/dashboard` 上，也就是每次打开首页都要付一次全表扫描。
///
/// 口径取**近 30 天**而不是全历史，理由：成功率要回答的是「这个上游现在还好用吗」，
/// 全历史会把早已修好的问题永久稀释进去（一个两年前炸过一次的账户永远升不回
/// healthy），也会让新账户的样本被老数据压平。
///
/// ponytail: 常量，30 天是拍的。真要可调就往 settings 表加 key 读 here ——
/// 现在没有 UI 要它，先别加。
const HEALTH_WINDOW_DAYS: i64 = 30;

/// 每个账户的 (id, alias, 成功次数, 总次数)，带时间窗。
///
/// 窗口内**一条流量都没有**的账户回退到全历史，而不是报 `unknown` —— 后者会
/// 让「上个月才配好、这个月没流量」的账户显示成无数据，比给个历史成功率更没用。
/// 回退查询只针对这些账户，且走 `account_id` 索引，不是又一次全表扫。
pub async fn fetch_health_rows(pool: &sqlx::SqlitePool) -> Result<Vec<HealthRow>, sqlx::Error> {
    let cutoff = now_ms() - HEALTH_WINDOW_DAYS * 86_400_000;
    let rows = sqlx::query(
        "SELECT a.id, a.alias, \
                COALESCE(w.total, 0) AS total, \
                COALESCE(w.success, 0) AS success \
         FROM accounts a \
         LEFT JOIN ( \
           SELECT account_id, COUNT(*) AS total, \
                  SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) AS success \
           FROM usage_logs WHERE timestamp >= ? GROUP BY account_id \
         ) w ON w.account_id = a.id \
         ORDER BY a.id",
    )
    .bind(cutoff)
    .fetch_all(pool)
    .await?;

    // 窗口内零流量的账户 → 回退全历史
    let idle: Vec<i64> = rows
        .iter()
        .filter(|r| r.try_get::<i64, _>("total").unwrap_or(0) == 0)
        .filter_map(|r| r.try_get::<i64, _>("id").ok())
        .collect();

    // 窗口内零流量的账户 → 回退全历史。只针对这些账户，且走 account_id 索引，
    // 不是又一次全表扫。`hist` 为空即「人人都在窗口内有流量」，无需额外查询。
    let mut hist: std::collections::HashMap<i64, (i64, i64)> = Default::default();
    if !idle.is_empty() {
        let mut sql = String::from(
            "SELECT account_id, COUNT(*) AS total, \
                    SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) AS success \
             FROM usage_logs WHERE account_id IN (",
        );
        sql.push_str(&vec!["?"; idle.len()].join(","));
        sql.push_str(") GROUP BY account_id");
        let mut q = sqlx::query(&sql);
        for id in &idle {
            q = q.bind(id);
        }
        hist = q
            .fetch_all(pool)
            .await?
            .iter()
            .filter_map(|r| {
                Some((
                    r.try_get::<i64, _>("account_id").ok()?,
                    (
                        r.try_get::<i64, _>("total").unwrap_or(0),
                        r.try_get::<i64, _>("success").unwrap_or(0),
                    ),
                ))
            })
            .collect();
    }

    Ok(rows
        .iter()
        .map(|r| {
            let id: i64 = r.try_get("id").unwrap_or_default();
            let alias: String = r.try_get("alias").unwrap_or_default();
            let (total, success) = match hist.get(&id) {
                Some(&(t, s)) => (t, s),
                None => (
                    r.try_get::<i64, _>("total").unwrap_or(0),
                    r.try_get::<i64, _>("success").unwrap_or(0),
                ),
            };
            HealthRow { id, alias, total, success }
        })
        .collect())
}

/// 每个账户的一行。刻意不用 `SqliteRow`：那个不可变，而这里需要把回退的全历史值
/// 合并进去。
pub struct HealthRow {
    pub id: i64,
    pub alias: String,
    pub total: i64,
    pub success: i64,
}

/// 成功次数 / 总次数 → healthy / degraded / down。窗口与回退逻辑在
/// `fetch_health_rows` 里，这里只管分档，dashboard 与 /api/health 共用。
pub fn status_for(total: i64, success: i64) -> &'static str {
    if total <= 0 {
        return "unknown";
    }
    let rate = success as f64 / total as f64;
    if rate > 0.9 {
        "healthy"
    } else if rate > 0.5 {
        "degraded"
    } else {
        "down"
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub async fn get_health_status(Extension(state): Extension<AppState>) -> Response {
    let rows = match fetch_health_rows(&state.pool).await {
        Ok(r) => r,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to query accounts: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    tracing::info!("💚 Health check for {} accounts", rows.len());

    let health_data: Vec<Value> = rows
        .iter()
        .map(|row| {
            let status = status_for(row.total, row.success);
            json!({
                "id": format!("acc_{}", row.id),
                "name": row.alias,
                "status": status,
                // 曾经叫 `lastSuccess`，但装的其实是成功**次数**，不是时间戳。
                // 读的人一定会误读成「最后一次成功距今多久」（本轮就被坑过一次：
                // 看到 free 的 40 以为是 40 秒前刚成功过）。UI 当时没读这个字段，
                // 所以没造成可见故障，但它是个等着坑下一个人的陷阱。
                // 改名而不是补一个真时间戳：真正需要「距今多久」的地方
                // （模型健康、请求日志）已经从 usage_logs 的 timestamp 单独查了，
                // 这个接口的职责就是给出总调用量与成功量，供算成功率。
                "successCount": row.success,
                "totalChecks": row.total,
            })
        })
        .collect();

    Json(Value::Array(health_data)).into_response()
}
#[cfg(test)]
mod tests {
    use super::*;

    async fn seeded() -> (sqlx::SqlitePool, i64) {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        llmux_core::db::init_db(&pool).await.unwrap();
        let now = now_ms();
        let day = 86_400_000i64;
        (pool, now / day)
    }

    async fn add(pool: &sqlx::SqlitePool, id: i64, alias: &str, days_ago: i64, ok: i64) {
        let ts = now_ms() - days_ago * 86_400_000;
        sqlx::query(
            "INSERT INTO accounts (id, alias, provider_id, api_key, is_active) \
             VALUES (?, ?, 'p', 'k', 1)",
        )
        .bind(id)
        .bind(alias)
        .execute(pool)
        .await
        .ok();
        sqlx::query(
            "INSERT INTO usage_logs (timestamp, account_id, provider_id, model, \
               input_tokens, output_tokens, latency_ms, success, is_test) \
             VALUES (?, ?, 'p', 'm', 1, 1, 5, ?, 0)",
        )
        .bind(ts)
        .bind(id)
        .bind(ok)
        .execute(pool)
        .await
        .unwrap();
    }

    /// 核心：窗口外的老数据**不算进**成功率。
    ///
    /// 没有这条约束，窗口参数写错（比如单位搞错成秒、或 cutoff 算成未来时间）
    /// 都不会有任何症状 —— 只会安静地退回全表扫，正是本条改动要消灭的东西。
    #[tokio::test]
    async fn rows_older_than_the_window_are_excluded() {
        let (pool, _) = seeded().await;
        // 3 条在窗口内（2 成功 1 失败 = 66.7% degraded）
        add(&pool, 1, "recent", 1, 1).await;
        add(&pool, 1, "recent", 2, 1).await;
        add(&pool, 1, "recent", 3, 0).await;
        // 100 条窗口外且全部失败 —— 若被计入，状态会掉到 down
        for i in 0..100 {
            add(&pool, 1, "recent", 60 + i, 0).await;
        }

        let rows = fetch_health_rows(&pool).await.unwrap();
        let r = rows.iter().find(|r| r.id == 1).unwrap();
        assert_eq!(
            (r.total, r.success),
            (3, 2),
            "只应统计窗口内的 3 条，窗口外的 100 条失败必须被排除"
        );
        assert_eq!(status_for(r.total, r.success), "degraded");
    }

    /// 窗口内零流量但历史有数据的账户 → **回退全历史**，而不是 unknown。
    /// 否则「上个月才配好、这个月没用」的账户会显示成无数据，比给个历史值更没用。
    #[tokio::test]
    async fn falls_back_to_full_history_when_no_recent_traffic() {
        let (pool, _) = seeded().await;
        for i in 0..5 {
            add(&pool, 2, "quiet", 90 + i, 1).await; // 90 天前，窗口外
        }
        let rows = fetch_health_rows(&pool).await.unwrap();
        let r = rows.iter().find(|r| r.id == 2).unwrap();
        assert_eq!(
            (r.total, r.success),
            (5, 5),
            "窗口内无流量时应回退到全历史，而不是报 0/unknown"
        );
        assert_eq!(status_for(r.total, r.success), "healthy");
    }

    /// 回退**不能**覆盖窗口内有流量的账户：窗口内 1 条成功 + 窗口外 99 条失败，
    /// 结果必须是 healthy（1/1），不能因为回退被污染成 down。
    #[tokio::test]
    async fn fallback_does_not_pollute_accounts_with_recent_traffic() {
        let (pool, _) = seeded().await;
        add(&pool, 3, "mixed", 1, 1).await; // 窗口内，1 成功
        for i in 0..99 {
            add(&pool, 3, "mixed", 60 + i, 0).await; // 窗口外，全失败
        }
        let rows = fetch_health_rows(&pool).await.unwrap();
        let r = rows.iter().find(|r| r.id == 3).unwrap();
        assert_eq!(
            (r.total, r.success),
            (1, 1),
            "有近期流量的账户绝不能被全历史回退污染"
        );
        assert_eq!(status_for(r.total, r.success), "healthy");
    }

    /// 从无流量的账户 → unknown，且不该被回退查询凭空造出数据。
    #[tokio::test]
    async fn account_with_no_traffic_at_all_is_unknown() {
        let (pool, _) = seeded().await;
        sqlx::query(
            "INSERT INTO accounts (id, alias, provider_id, api_key, is_active) \
             VALUES (7, 'fresh', 'p', 'k', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let rows = fetch_health_rows(&pool).await.unwrap();
        let r = rows.iter().find(|r| r.id == 7).unwrap();
        assert_eq!((r.total, r.success), (0, 0));
        assert_eq!(status_for(r.total, r.success), "unknown");
    }

    /// 分档边界：>0.9 / >0.5 / 其余。别让重构顺手改了阈值。
    #[test]
    fn status_thresholds_are_unchanged() {
        assert_eq!(status_for(0, 0), "unknown");
        assert_eq!(status_for(10, 10), "healthy");
        assert_eq!(status_for(10, 9), "degraded"); // 90% 不算 healthy
        assert_eq!(status_for(10, 6), "degraded");
        assert_eq!(status_for(10, 5), "down"); // 50% 不算 degraded
        assert_eq!(status_for(10, 0), "down");
    }
}
