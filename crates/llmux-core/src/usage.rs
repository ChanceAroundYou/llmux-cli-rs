use crate::models::UsageLogParams;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use std::time::{SystemTime, UNIX_EPOCH};

/// 最小化监控服务：仅成功率 / 延迟 / 最近活动（spec §3.3），无 token 列。
#[derive(Debug, Clone)]
pub struct UsageService {
    pool: SqlitePool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageSummary {
    pub avg_latency: f64,
    pub total_requests: i64,
    pub success_requests: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageLogRecord {
    pub id: i64,
    pub ts: i64,
    pub account_id: Option<i64>,
    pub account_name: Option<String>,
    pub model: Option<String>,
    pub latency_ms: i64,
    pub success: bool,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelBreakdown {
    pub model: Option<String>,
    pub requests: i64,
    pub success_count: i64,
    pub avg_latency: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccountBreakdown {
    pub id: i64,
    pub name: String,
    pub vendor: String,
    pub requests: i64,
    pub success_count: i64,
    pub avg_latency: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VendorBreakdown {
    pub id: Option<String>,
    pub requests: i64,
    pub success_count: i64,
    pub avg_latency: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FailoverStats {
    pub failover_triggers: i64,
    pub recovered_requests: i64,
    pub failover_success_rate: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DetailedLogQuery {
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    pub model: Option<String>,
    pub success: Option<bool>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

impl UsageService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn log_usage(&self, params: UsageLogParams) -> Result<()> {
        let ts = params.timestamp.unwrap_or_else(now_millis);
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            "INSERT INTO usage_logs (ts, account_id, account_name, model, latency_ms, success, error_message)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(ts)
        .bind(params.account_id)
        .bind(params.account_name)
        .bind(params.model)
        .bind(params.latency_ms)
        .bind(bool_to_i64(params.success))
        .bind(params.error_message)
        .execute(&mut *tx)
        .await?;

        if let Some(limit_cache) = params.limit_cache {
            sqlx::query(
                "UPDATE accounts
                 SET limits_cache = ?, limits_cache_updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?",
            )
            .bind(serde_json::to_string(&limit_cache)?)
            .bind(params.account_id)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    pub async fn get_recent_logs(
        &self,
        limit: i64,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> Result<Vec<UsageLogRecord>> {
        let mut sql = base_log_select("WHERE 1=1");
        append_time_filter(&mut sql, "l", start_time, end_time);
        sql.push_str(" ORDER BY l.ts DESC LIMIT ?");

        let mut query = sqlx::query(&sql);
        query = bind_time_filter(query, start_time, end_time);
        query = query.bind(limit);
        rows_to_logs(query.fetch_all(&self.pool).await?)
    }

    pub async fn get_summary(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> Result<UsageSummary> {
        let mut sql = String::from(
            "SELECT
                CAST(IFNULL(AVG(latency_ms), 0) AS REAL) AS avg_latency,
                COUNT(*) AS total_requests,
                IFNULL(SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END), 0) AS success_requests
             FROM usage_logs",
        );
        append_time_filter(&mut sql, "", start_time, end_time);
        let mut query = sqlx::query(&sql);
        query = bind_time_filter(query, start_time, end_time);
        let row = query.fetch_one(&self.pool).await?;
        Ok(UsageSummary {
            avg_latency: row.try_get("avg_latency")?,
            total_requests: row.try_get("total_requests")?,
            success_requests: row.try_get("success_requests")?,
        })
    }

    pub async fn get_failover_stats(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> Result<FailoverStats> {
        let mut failure_sql = String::from(
            "SELECT COUNT(*) AS failed_requests,
                    IFNULL(SUM(CASE WHEN error_message LIKE '%429%' OR error_message LIKE '%401%' OR error_message LIKE '%403%' THEN 1 ELSE 0 END), 0) AS failover_triggers
             FROM usage_logs
             WHERE success = 0",
        );
        append_time_filter(&mut failure_sql, "", start_time, end_time);
        let mut failure_query = sqlx::query(&failure_sql);
        failure_query = bind_time_filter(failure_query, start_time, end_time);
        let failure_row = failure_query.fetch_one(&self.pool).await?;
        let failed_requests: i64 = failure_row.try_get("failed_requests")?;
        let failover_triggers: i64 = failure_row.try_get("failover_triggers")?;

        let summary = self.get_summary(start_time, end_time).await?;
        let expected_success = summary.total_requests - failed_requests;
        let recovered_requests = (summary.success_requests - expected_success).max(0);
        let failover_success_rate = if failover_triggers > 0 {
            ((recovered_requests as f64 / failover_triggers as f64) * 100.0).min(100.0)
        } else {
            0.0
        };

        Ok(FailoverStats {
            failover_triggers,
            recovered_requests,
            failover_success_rate,
        })
    }

    pub async fn get_breakdown_by_vendor(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> Result<Vec<VendorBreakdown>> {
        let mut sql = String::from(
            "SELECT v.id AS id,
                    COUNT(l.id) AS requests,
                    IFNULL(SUM(CASE WHEN l.success = 1 THEN 1 ELSE 0 END), 0) AS success_count,
                    CAST(IFNULL(AVG(l.latency_ms), 0) AS REAL) AS avg_latency
             FROM usage_logs l
             LEFT JOIN accounts a ON l.account_id = a.id
             LEFT JOIN vendors v ON a.vendor_id = v.id",
        );
        append_time_filter(&mut sql, "l", start_time, end_time);
        sql.push_str(" GROUP BY v.id");
        let mut query = sqlx::query(&sql);
        query = bind_time_filter(query, start_time, end_time);
        let rows = query.fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                Ok(VendorBreakdown {
                    id: row.try_get("id")?,
                    requests: row.try_get("requests")?,
                    success_count: row.try_get("success_count")?,
                    avg_latency: row.try_get("avg_latency")?,
                })
            })
            .collect()
    }

    pub async fn get_breakdown_by_model(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> Result<Vec<ModelBreakdown>> {
        let mut sql = String::from(
            "SELECT model,
                    COUNT(*) AS requests,
                    IFNULL(SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END), 0) AS success_count,
                    CAST(IFNULL(AVG(latency_ms), 0) AS REAL) AS avg_latency
             FROM usage_logs",
        );
        append_time_filter(&mut sql, "", start_time, end_time);
        sql.push_str(" GROUP BY model ORDER BY requests DESC");
        let mut query = sqlx::query(&sql);
        query = bind_time_filter(query, start_time, end_time);
        let rows = query.fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                Ok(ModelBreakdown {
                    model: row.try_get("model")?,
                    requests: row.try_get("requests")?,
                    success_count: row.try_get("success_count")?,
                    avg_latency: row.try_get("avg_latency")?,
                })
            })
            .collect()
    }

    pub async fn get_breakdown_by_account(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> Result<Vec<AccountBreakdown>> {
        let mut sql = String::from(
            "SELECT a.id AS id,
                    a.name AS name,
                    v.id AS vendor,
                    COUNT(l.id) AS requests,
                    IFNULL(SUM(CASE WHEN l.success = 1 THEN 1 ELSE 0 END), 0) AS success_count,
                    CAST(IFNULL(AVG(l.latency_ms), 0) AS REAL) AS avg_latency
             FROM usage_logs l
             JOIN accounts a ON l.account_id = a.id
             JOIN vendors v ON a.vendor_id = v.id",
        );
        append_time_filter(&mut sql, "l", start_time, end_time);
        sql.push_str(" GROUP BY a.id, a.name");
        let mut query = sqlx::query(&sql);
        query = bind_time_filter(query, start_time, end_time);
        let rows = query.fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                Ok(AccountBreakdown {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    vendor: row.try_get("vendor")?,
                    requests: row.try_get("requests")?,
                    success_count: row.try_get("success_count")?,
                    avg_latency: row.try_get("avg_latency")?,
                })
            })
            .collect()
    }

    pub async fn get_detailed_logs(
        &self,
        options: DetailedLogQuery,
    ) -> Result<Vec<UsageLogRecord>> {
        let mut sql = base_log_select("WHERE 1=1");
        if options.start_time.is_some() {
            sql.push_str(" AND l.ts >= ?");
        }
        if options.end_time.is_some() {
            sql.push_str(" AND l.ts <= ?");
        }
        if options.model.is_some() {
            sql.push_str(" AND l.model LIKE ?");
        }
        if options.success.is_some() {
            sql.push_str(" AND l.success = ?");
        }
        sql.push_str(" ORDER BY l.ts DESC");
        if options.limit.is_some() {
            sql.push_str(" LIMIT ?");
        }
        if options.offset.is_some() {
            sql.push_str(" OFFSET ?");
        }

        let mut query = sqlx::query(&sql);
        if let Some(start_time) = options.start_time {
            query = query.bind(start_time);
        }
        if let Some(end_time) = options.end_time {
            query = query.bind(end_time);
        }
        if let Some(model) = options.model {
            query = query.bind(format!("%{model}%"));
        }
        if let Some(success) = options.success {
            query = query.bind(bool_to_i64(success));
        }
        if let Some(limit) = options.limit {
            query = query.bind(limit);
        }
        if let Some(offset) = options.offset {
            query = query.bind(offset);
        }

        rows_to_logs(query.fetch_all(&self.pool).await?)
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn bool_to_i64(value: bool) -> i64 {
    if value {
        1
    } else {
        0
    }
}

fn base_log_select(where_clause: &str) -> String {
    format!(
        "SELECT l.id, l.ts, l.account_id, l.account_name, l.model,
                l.latency_ms, l.success, l.error_message
         FROM usage_logs l
         {where_clause}"
    )
}

fn append_time_filter(
    sql: &mut String,
    table_alias: &str,
    start_time: Option<i64>,
    end_time: Option<i64>,
) {
    let prefix = if table_alias.is_empty() {
        "ts".to_string()
    } else {
        format!("{table_alias}.ts")
    };
    if start_time.is_some() && end_time.is_some() {
        sql.push_str(&format!(" AND {prefix} BETWEEN ? AND ?"));
    } else if start_time.is_some() {
        sql.push_str(&format!(" AND {prefix} >= ?"));
    } else if end_time.is_some() {
        sql.push_str(&format!(" AND {prefix} <= ?"));
    }
}

fn bind_time_filter<'q>(
    mut query: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    start_time: Option<i64>,
    end_time: Option<i64>,
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
    if let Some(start_time) = start_time {
        query = query.bind(start_time);
    }
    if let Some(end_time) = end_time {
        query = query.bind(end_time);
    }
    query
}

fn rows_to_logs(rows: Vec<sqlx::sqlite::SqliteRow>) -> Result<Vec<UsageLogRecord>> {
    rows.into_iter()
        .map(|row| {
            Ok(UsageLogRecord {
                id: row.try_get("id")?,
                ts: row.try_get("ts")?,
                account_id: row.try_get("account_id")?,
                account_name: row.try_get("account_name")?,
                model: row.try_get("model")?,
                latency_ms: row.try_get("latency_ms")?,
                success: row.try_get::<i64, _>("success")? != 0,
                error_message: row.try_get("error_message")?,
            })
        })
        .collect()
}
