use crate::models::UsageLogParams;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct UsageService {
    pool: SqlitePool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageSummary {
    pub total_input: i64,
    pub total_output: i64,
    pub total_cache_read: i64,
    pub total_cache_create: i64,
    pub cache_hit_rate: f64,
    pub avg_latency: f64,
    pub total_requests: i64,
    pub success_requests: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageLogRecord {
    pub id: i64,
    pub timestamp: i64,
    pub account_id: Option<i64>,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub latency_ms: i64,
    pub success: bool,
    pub error_message: Option<String>,
    pub is_test: bool,
    pub account_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderBreakdown {
    pub id: Option<String>,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_create: i64,
    pub cache_hit_rate: f64,
    pub total_tokens: i64,
    pub requests: i64,
    pub success_count: i64,
    pub avg_latency: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelBreakdown {
    pub model: Option<String>,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_create: i64,
    pub cache_hit_rate: f64,
    pub requests: i64,
    pub success_count: i64,
    pub avg_latency: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccountBreakdown {
    pub id: i64,
    pub name: String,
    pub provider: String,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_create: i64,
    pub cache_hit_rate: f64,
    pub total_tokens: i64,
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
    pub provider: Option<String>,
    pub success: Option<bool>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

impl UsageService {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn log_usage(&self, params: UsageLogParams) -> Result<()> {
        let timestamp = params.timestamp.unwrap_or_else(now_millis);
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            "INSERT INTO usage_logs (
                timestamp, account_id, provider_id, model, input_tokens, output_tokens,
                cache_read_input_tokens, cache_creation_input_tokens,
                latency_ms, success, error_message, is_test
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(timestamp)
        .bind(params.account_id)
        .bind(params.provider_id)
        .bind(params.model)
        .bind(params.input_tokens)
        .bind(params.output_tokens)
        .bind(params.cache_read_input_tokens)
        .bind(params.cache_creation_input_tokens)
        .bind(params.latency_ms)
        .bind(bool_to_i64(params.success))
        .bind(params.error_message)
        .bind(bool_to_i64(params.is_test))
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
        let mut sql = base_log_select("WHERE l.is_test = 0");
        append_time_filter(&mut sql, "l", start_time, end_time);
        sql.push_str(" ORDER BY l.timestamp DESC LIMIT ?");

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
                IFNULL(SUM(input_tokens), 0) AS total_input,
                IFNULL(SUM(output_tokens), 0) AS total_output,
                IFNULL(SUM(cache_read_input_tokens), 0) AS total_cache_read,
                IFNULL(SUM(cache_creation_input_tokens), 0) AS total_cache_create,
                CAST(IFNULL(AVG(latency_ms), 0) AS REAL) AS avg_latency,
                COUNT(*) AS total_requests,
                IFNULL(SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END), 0) AS success_requests
             FROM usage_logs
             WHERE is_test = 0",
        );
        append_time_filter(&mut sql, "", start_time, end_time);
        let mut query = sqlx::query(&sql);
        query = bind_time_filter(query, start_time, end_time);
        let row = query.fetch_one(&self.pool).await?;
        let total_input: i64 = row.try_get("total_input")?;
        let total_cache_read: i64 = row.try_get("total_cache_read")?;
        let total_cache_create: i64 = row.try_get("total_cache_create")?;
        let denom = (total_input + total_cache_read + total_cache_create) as f64;
        let cache_hit_rate = if denom > 0.0 {
            (total_cache_read as f64 / denom) * 100.0
        } else {
            0.0
        };
        Ok(UsageSummary {
            total_input,
            total_output: row.try_get("total_output")?,
            total_cache_read,
            total_cache_create,
            cache_hit_rate,
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
             WHERE is_test = 0 AND success = 0",
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

    pub async fn get_breakdown_by_provider(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
    ) -> Result<Vec<ProviderBreakdown>> {
        let mut sql = String::from(
            "SELECT provider_id AS id,
                    IFNULL(SUM(input_tokens), 0) AS input,
                    IFNULL(SUM(output_tokens), 0) AS output,
                    IFNULL(SUM(cache_read_input_tokens), 0) AS cache_read,
                    IFNULL(SUM(cache_creation_input_tokens), 0) AS cache_create,
                    IFNULL(SUM(input_tokens + output_tokens), 0) AS total_tokens,
                    COUNT(*) AS requests,
                    IFNULL(SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END), 0) AS success_count,
                    CAST(IFNULL(AVG(latency_ms), 0) AS REAL) AS avg_latency
             FROM usage_logs
             WHERE is_test = 0",
        );
        append_time_filter(&mut sql, "", start_time, end_time);
        sql.push_str(" GROUP BY provider_id");
        let mut query = sqlx::query(&sql);
        query = bind_time_filter(query, start_time, end_time);
        let rows = query.fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                let input: i64 = row.try_get("input")?;
                let cache_read: i64 = row.try_get("cache_read")?;
                let cache_create: i64 = row.try_get("cache_create")?;
                let denom = (input + cache_read + cache_create) as f64;
                let cache_hit_rate = if denom > 0.0 { (cache_read as f64 / denom) * 100.0 } else { 0.0 };
                Ok(ProviderBreakdown {
                    id: row.try_get("id")?,
                    input,
                    output: row.try_get("output")?,
                    cache_read,
                    cache_create,
                    cache_hit_rate,
                    total_tokens: row.try_get("total_tokens")?,
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
                    IFNULL(SUM(input_tokens), 0) AS input,
                    IFNULL(SUM(output_tokens), 0) AS output,
                    IFNULL(SUM(cache_read_input_tokens), 0) AS cache_read,
                    IFNULL(SUM(cache_creation_input_tokens), 0) AS cache_create,
                    COUNT(*) AS requests,
                    IFNULL(SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END), 0) AS success_count,
                    CAST(IFNULL(AVG(latency_ms), 0) AS REAL) AS avg_latency
             FROM usage_logs
             WHERE is_test = 0",
        );
        append_time_filter(&mut sql, "", start_time, end_time);
        sql.push_str(" GROUP BY model ORDER BY (input + output) DESC");
        let mut query = sqlx::query(&sql);
        query = bind_time_filter(query, start_time, end_time);
        let rows = query.fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                let input: i64 = row.try_get("input")?;
                let cache_read: i64 = row.try_get("cache_read")?;
                let cache_create: i64 = row.try_get("cache_create")?;
                let denom = (input + cache_read + cache_create) as f64;
                let cache_hit_rate = if denom > 0.0 { (cache_read as f64 / denom) * 100.0 } else { 0.0 };
                Ok(ModelBreakdown {
                    model: row.try_get("model")?,
                    input,
                    output: row.try_get("output")?,
                    cache_read,
                    cache_create,
                    cache_hit_rate,
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
                    a.alias AS name,
                    a.provider_id AS provider,
                    IFNULL(SUM(l.input_tokens), 0) AS input,
                    IFNULL(SUM(l.output_tokens), 0) AS output,
                    IFNULL(SUM(l.cache_read_input_tokens), 0) AS cache_read,
                    IFNULL(SUM(l.cache_creation_input_tokens), 0) AS cache_create,
                    IFNULL(SUM(l.input_tokens + l.output_tokens), 0) AS total_tokens,
                    COUNT(*) AS requests,
                    IFNULL(SUM(CASE WHEN l.success = 1 THEN 1 ELSE 0 END), 0) AS success_count,
                    CAST(IFNULL(AVG(l.latency_ms), 0) AS REAL) AS avg_latency
             FROM usage_logs l
             JOIN accounts a ON l.account_id = a.id
             WHERE l.is_test = 0",
        );
        append_time_filter(&mut sql, "l", start_time, end_time);
        sql.push_str(" GROUP BY a.id, a.alias");
        let mut query = sqlx::query(&sql);
        query = bind_time_filter(query, start_time, end_time);
        let rows = query.fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                let input: i64 = row.try_get("input")?;
                let cache_read: i64 = row.try_get("cache_read")?;
                let cache_create: i64 = row.try_get("cache_create")?;
                let denom = (input + cache_read + cache_create) as f64;
                let cache_hit_rate = if denom > 0.0 { (cache_read as f64 / denom) * 100.0 } else { 0.0 };
                Ok(AccountBreakdown {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    provider: row.try_get("provider")?,
                    input,
                    output: row.try_get("output")?,
                    cache_read,
                    cache_create,
                    cache_hit_rate,
                    total_tokens: row.try_get("total_tokens")?,
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
            sql.push_str(" AND l.timestamp >= ?");
        }
        if options.end_time.is_some() {
            sql.push_str(" AND l.timestamp <= ?");
        }
        if options.model.is_some() {
            sql.push_str(" AND l.model LIKE ?");
        }
        if options.provider.is_some() {
            sql.push_str(" AND l.provider_id = ?");
        }
        if options.success.is_some() {
            sql.push_str(" AND l.success = ?");
        }
        sql.push_str(" ORDER BY l.timestamp DESC");
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
        if let Some(provider) = options.provider {
            query = query.bind(provider);
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
        "SELECT l.id, l.timestamp, l.account_id, l.provider_id, l.model,
                l.input_tokens, l.output_tokens, l.cache_read_input_tokens,
                l.cache_creation_input_tokens, l.latency_ms, l.success,
                l.error_message, l.is_test, a.alias AS account_name
         FROM usage_logs l
         LEFT JOIN accounts a ON l.account_id = a.id
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
        "timestamp".to_string()
    } else {
        format!("{table_alias}.timestamp")
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
                timestamp: row.try_get("timestamp")?,
                account_id: row.try_get("account_id")?,
                provider_id: row.try_get("provider_id")?,
                model: row.try_get("model")?,
                input_tokens: row.try_get("input_tokens")?,
                output_tokens: row.try_get("output_tokens")?,
                cache_read_input_tokens: row.try_get("cache_read_input_tokens")?,
                cache_creation_input_tokens: row.try_get("cache_creation_input_tokens")?,
                latency_ms: row.try_get("latency_ms")?,
                success: row.try_get::<i64, _>("success")? != 0,
                error_message: row.try_get("error_message")?,
                is_test: row.try_get::<i64, _>("is_test")? != 0,
                account_name: row.try_get("account_name")?,
            })
        })
        .collect()
}
