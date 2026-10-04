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
    pub p50_latency: f64,
    pub p95_latency: f64,
    pub avg_ttft: f64,
    pub p50_ttft: f64,
    pub p95_ttft: f64,
    pub avg_tps: f64,
    pub total_requests: i64,
    pub success_requests: i64,
    /// 折算成本（美元）。按 token 数 × model_prices 单价求和，未定价模型计 0。
    pub est_cost: f64,
    /// 窗口内「有 token 却没价目」的模型数 —— 让估算偏低可见，而不是静默。
    pub unpriced_models: i64,
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
    pub ttft_ms: Option<i64>,
    pub is_stream: bool,
    pub account_name: Option<String>,
    pub client_ip: Option<String>,
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
    pub avg_ttft: f64,
    pub p95_ttft: f64,
    pub avg_tps: f64,
    /// 折算成本（美元），见 UsageSummary::est_cost。
    pub est_cost: f64,
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
    pub avg_ttft: f64,
    pub p95_ttft: f64,
    pub avg_tps: f64,
    /// 折算成本（美元），见 UsageSummary::est_cost。
    pub est_cost: f64,
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
    pub avg_ttft: f64,
    pub p95_ttft: f64,
    pub avg_tps: f64,
    /// 折算成本（美元），见 UsageSummary::est_cost。
    pub est_cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TimeseriesPoint {
    pub bucket: i64,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_create: i64,
    pub requests: i64,
    pub avg_latency: f64,
    pub p95_latency: f64,
    pub avg_ttft: f64,
    pub p95_ttft: f64,
    pub avg_tps: f64,
    /// 折算成本（美元），见 UsageSummary::est_cost。
    pub est_cost: f64,
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
    pub is_stream: Option<bool>,
    pub api_key_id: Option<i64>,
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
        api_key_id: Option<i64>,
    ) -> Result<UsageSummary> {
        let mut sql = format!(
            "SELECT
                IFNULL(SUM(usage_logs.input_tokens), 0) AS total_input,
                IFNULL(SUM(usage_logs.output_tokens), 0) AS total_output,
                IFNULL(SUM(usage_logs.cache_read_input_tokens), 0) AS total_cache_read,
                IFNULL(SUM(usage_logs.cache_creation_input_tokens), 0) AS total_cache_create,
                CAST(IFNULL(AVG(usage_logs.latency_ms), 0) AS REAL) AS avg_latency,
                COUNT(*) AS total_requests,
                IFNULL(SUM(CASE WHEN usage_logs.success = 1 THEN 1 ELSE 0 END), 0) AS success_requests,
                IFNULL(SUM(
                    {}
                ), 0.0) AS est_cost,
                COUNT(DISTINCT CASE WHEN up.model_id IS NULL AND mp.model_id IS NULL
                      AND (IFNULL(usage_logs.input_tokens, 0) + IFNULL(usage_logs.output_tokens, 0)
                           + IFNULL(usage_logs.cache_read_input_tokens, 0)
                           + IFNULL(usage_logs.cache_creation_input_tokens, 0)) > 0
                      THEN usage_logs.model END) AS unpriced_models
             FROM usage_logs
             LEFT JOIN upstream_prices up
                    ON up.account_id = usage_logs.account_id AND up.model_id = usage_logs.model
             LEFT JOIN model_prices mp ON mp.model_id = usage_logs.model
             WHERE usage_logs.is_test = 0",
            price_expr("")
        );
        append_time_filter(&mut sql, "", start_time, end_time);
        append_key_filter(&mut sql, "", api_key_id);
        let mut query = sqlx::query(&sql);
        query = bind_time_filter(query, start_time, end_time);
        query = bind_key_filter(query, api_key_id);
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
        let avg_latency: f64 = row.try_get("avg_latency")?;
        let total_requests: i64 = row.try_get("total_requests")?;
        let success_requests: i64 = row.try_get("success_requests")?;
        let total_output: i64 = row.try_get("total_output")?;
        let est_cost: f64 = row.try_get("est_cost")?;
        let unpriced_models: i64 = row.try_get("unpriced_models")?;

        // Percentiles + tps require raw per-row values.
        let mut raw_sql = String::from(
            "SELECT latency_ms, ttft_ms, output_tokens
             FROM usage_logs
             WHERE is_test = 0",
        );
        append_time_filter(&mut raw_sql, "", start_time, end_time);
        append_key_filter(&mut raw_sql, "", api_key_id);
        let mut raw_query = sqlx::query(&raw_sql);
        raw_query = bind_time_filter(raw_query, start_time, end_time);
        raw_query = bind_key_filter(raw_query, api_key_id);
        let raw_rows = raw_query.fetch_all(&self.pool).await?;
        let mut latencies: Vec<i64> = Vec::with_capacity(raw_rows.len());
        let mut ttfts: Vec<i64> = Vec::new();
        let mut out_total = 0i64;
        let mut lat_total = 0i64;
        for r in &raw_rows {
            let lat: i64 = r.try_get("latency_ms")?;
            let ttft: Option<i64> = r.try_get("ttft_ms")?;
            let out: i64 = r.try_get("output_tokens")?;
            latencies.push(lat);
            lat_total += lat;
            out_total += out;
            if let Some(t) = ttft {
                ttfts.push(t);
            }
        }
        let p50_latency = percentile(&mut latencies, 50.0);
        let p95_latency = percentile(&mut latencies, 95.0);
        let avg_ttft = if ttfts.is_empty() {
            0.0
        } else {
            ttfts.iter().sum::<i64>() as f64 / ttfts.len() as f64
        };
        let p50_ttft = percentile(&mut ttfts, 50.0);
        let p95_tt = percentile(&mut ttfts, 95.0);
        let avg_tps = if lat_total > 0 {
            out_total as f64 / (lat_total as f64 / 1000.0)
        } else {
            0.0
        };

        Ok(UsageSummary {
            total_input,
            total_output,
            total_cache_read,
            total_cache_create,
            cache_hit_rate,
            avg_latency,
            p50_latency,
            p95_latency,
            avg_ttft,
            p50_ttft,
            p95_ttft: p95_tt,
            avg_tps,
            total_requests,
            success_requests,
            est_cost,
            unpriced_models,
        })
    }

    pub async fn get_failover_stats(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        api_key_id: Option<i64>,
    ) -> Result<FailoverStats> {
        let mut failure_sql = String::from(
            "SELECT COUNT(*) AS failed_requests,
                    IFNULL(SUM(CASE WHEN error_message LIKE '%429%' OR error_message LIKE '%401%' OR error_message LIKE '%403%' THEN 1 ELSE 0 END), 0) AS failover_triggers
             FROM usage_logs
             WHERE is_test = 0 AND success = 0",
        );
        append_time_filter(&mut failure_sql, "", start_time, end_time);
        append_key_filter(&mut failure_sql, "", api_key_id);
        let mut failure_query = sqlx::query(&failure_sql);
        failure_query = bind_time_filter(failure_query, start_time, end_time);
        failure_query = bind_key_filter(failure_query, api_key_id);
        let failure_row = failure_query.fetch_one(&self.pool).await?;
        let failed_requests: i64 = failure_row.try_get("failed_requests")?;
        let failover_triggers: i64 = failure_row.try_get("failover_triggers")?;

        let summary = self.get_summary(start_time, end_time, api_key_id).await?;
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
        api_key_id: Option<i64>,
    ) -> Result<Vec<ProviderBreakdown>> {
        let mut sql = format!(
            "SELECT provider_id AS id,
                    IFNULL(input_tokens, 0) AS input_tokens,
                    IFNULL(output_tokens, 0) AS output_tokens,
                    IFNULL(cache_read_input_tokens, 0) AS cache_read,
                    IFNULL(cache_creation_input_tokens, 0) AS cache_create,
                    latency_ms, ttft_ms,
                    CASE WHEN success = 1 THEN 1 ELSE 0 END AS success,
                    {} AS est_cost
             FROM usage_logs
             LEFT JOIN upstream_prices up
                    ON up.account_id = usage_logs.account_id AND up.model_id = usage_logs.model
             LEFT JOIN model_prices mp ON mp.model_id = usage_logs.model
             WHERE is_test = 0",
            price_expr("")
        );
        append_time_filter(&mut sql, "", start_time, end_time);
        append_key_filter(&mut sql, "", api_key_id);
        let mut query = sqlx::query(&sql);
        query = bind_time_filter(query, start_time, end_time);
        query = bind_key_filter(query, api_key_id);
        let rows = query.fetch_all(&self.pool).await?;

        use std::collections::HashMap;
        let mut groups: HashMap<Option<String>, GroupAcc> = HashMap::new();
        for row in rows {
            let id: Option<String> = row.try_get("id")?;
            let acc = groups.entry(id.clone()).or_default();
            acc.input += row.try_get::<i64, _>("input_tokens")?;
            acc.output += row.try_get::<i64, _>("output_tokens")?;
            acc.cache_read += row.try_get::<i64, _>("cache_read")?;
            acc.cache_create += row.try_get::<i64, _>("cache_create")?;
            acc.est_cost += row.try_get::<f64, _>("est_cost")?;
            acc.requests += 1;
            acc.success_count += row.try_get::<i64, _>("success")?;
            acc.latencies.push(row.try_get::<i64, _>("latency_ms")?);
            if let Some(t) = row.try_get::<Option<i64>, _>("ttft_ms")? {
                acc.ttfts.push(t);
            }
        }

        let mut out: Vec<ProviderBreakdown> = groups
            .into_iter()
            .map(|(id, acc)| {
                let (avg_latency, _p50l, _p95l, avg_ttft, _p50t, p95_ttft, avg_tps, cache_hit_rate) =
                    finalize_group(&acc);
                Ok(ProviderBreakdown {
                    id,
                    input: acc.input,
                    output: acc.output,
                    cache_read: acc.cache_read,
                    cache_create: acc.cache_create,
                    cache_hit_rate,
                    total_tokens: acc.input + acc.output + acc.cache_read + acc.cache_create,
                    requests: acc.requests,
                    success_count: acc.success_count,
                    avg_latency,
                    avg_ttft,
                    p95_ttft,
                    avg_tps,
                    est_cost: acc.est_cost,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        out.sort_by(|a, b| {
            let ta = a.input + a.output + a.cache_read + a.cache_create;
            let tb = b.input + b.output + b.cache_read + b.cache_create;
            tb.cmp(&ta)
        });
        Ok(out)
    }

    pub async fn get_breakdown_by_model(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        api_key_id: Option<i64>,
    ) -> Result<Vec<ModelBreakdown>> {
        let mut sql = format!(
            "SELECT model,
                    IFNULL(input_tokens, 0) AS input_tokens,
                    IFNULL(output_tokens, 0) AS output_tokens,
                    IFNULL(cache_read_input_tokens, 0) AS cache_read,
                    IFNULL(cache_creation_input_tokens, 0) AS cache_create,
                    latency_ms, ttft_ms,
                    CASE WHEN success = 1 THEN 1 ELSE 0 END AS success,
                    {} AS est_cost
             FROM usage_logs
             LEFT JOIN upstream_prices up
                    ON up.account_id = usage_logs.account_id AND up.model_id = usage_logs.model
             LEFT JOIN model_prices mp ON mp.model_id = usage_logs.model
             WHERE is_test = 0",
            price_expr("")
        );
        append_time_filter(&mut sql, "", start_time, end_time);
        append_key_filter(&mut sql, "", api_key_id);
        let mut query = sqlx::query(&sql);
        query = bind_time_filter(query, start_time, end_time);
        query = bind_key_filter(query, api_key_id);
        let rows = query.fetch_all(&self.pool).await?;

        use std::collections::HashMap;
        let mut groups: HashMap<Option<String>, GroupAcc> = HashMap::new();
        for row in rows {
            let model: Option<String> = row.try_get("model")?;
            let acc = groups.entry(model.clone()).or_default();
            acc.input += row.try_get::<i64, _>("input_tokens")?;
            acc.output += row.try_get::<i64, _>("output_tokens")?;
            acc.cache_read += row.try_get::<i64, _>("cache_read")?;
            acc.cache_create += row.try_get::<i64, _>("cache_create")?;
            acc.est_cost += row.try_get::<f64, _>("est_cost")?;
            acc.requests += 1;
            acc.success_count += row.try_get::<i64, _>("success")?;
            acc.latencies.push(row.try_get::<i64, _>("latency_ms")?);
            if let Some(t) = row.try_get::<Option<i64>, _>("ttft_ms")? {
                acc.ttfts.push(t);
            }
        }

        let mut out: Vec<ModelBreakdown> = groups
            .into_iter()
            .map(|(model, acc)| {
                let (avg_latency, _p50l, _p95l, avg_ttft, _p50t, p95_ttft, avg_tps, cache_hit_rate) =
                    finalize_group(&acc);
                Ok(ModelBreakdown {
                    model,
                    input: acc.input,
                    output: acc.output,
                    cache_read: acc.cache_read,
                    cache_create: acc.cache_create,
                    cache_hit_rate,
                    requests: acc.requests,
                    success_count: acc.success_count,
                    avg_latency,
                    avg_ttft,
                    p95_ttft,
                    avg_tps,
                    est_cost: acc.est_cost,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        out.sort_by(|a, b| {
            let ta = a.input + a.output + a.cache_read + a.cache_create;
            let tb = b.input + b.output + b.cache_read + b.cache_create;
            tb.cmp(&ta)
        });
        Ok(out)
    }

    pub async fn get_breakdown_by_account(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        api_key_id: Option<i64>,
    ) -> Result<Vec<AccountBreakdown>> {
        let mut sql = format!(
            "SELECT l.account_id AS id,
                    a.alias AS name,
                    a.provider_id AS provider,
                    IFNULL(l.input_tokens, 0) AS input_tokens,
                    IFNULL(l.output_tokens, 0) AS output_tokens,
                    IFNULL(l.cache_read_input_tokens, 0) AS cache_read,
                    IFNULL(l.cache_creation_input_tokens, 0) AS cache_create,
                    l.latency_ms, l.ttft_ms,
                    CASE WHEN l.success = 1 THEN 1 ELSE 0 END AS success,
                    {} AS est_cost
             FROM usage_logs l
             JOIN accounts a ON l.account_id = a.id
             LEFT JOIN upstream_prices up
                    ON up.account_id = l.account_id AND up.model_id = l.model
             LEFT JOIN model_prices mp ON mp.model_id = l.model
             WHERE l.is_test = 0",
            price_expr("l.")
        );
        append_time_filter(&mut sql, "l", start_time, end_time);
        append_key_filter(&mut sql, "l", api_key_id);
        let mut query = sqlx::query(&sql);
        query = bind_time_filter(query, start_time, end_time);
        query = bind_key_filter(query, api_key_id);
        let rows = query.fetch_all(&self.pool).await?;

        use std::collections::HashMap;
        #[derive(Default)]
        struct AccountGroup {
            name: String,
            provider: String,
            acc: GroupAcc,
        }
        let mut groups: HashMap<i64, AccountGroup> = HashMap::new();
        for row in rows {
            let id: i64 = row.try_get("id")?;
            let entry = groups.entry(id).or_default();
            if entry.name.is_empty() {
                entry.name = row.try_get::<String, _>("name")?;
                entry.provider = row.try_get::<String, _>("provider")?;
            }
            entry.acc.input += row.try_get::<i64, _>("input_tokens")?;
            entry.acc.output += row.try_get::<i64, _>("output_tokens")?;
            entry.acc.cache_read += row.try_get::<i64, _>("cache_read")?;
            entry.acc.cache_create += row.try_get::<i64, _>("cache_create")?;
            entry.acc.est_cost += row.try_get::<f64, _>("est_cost")?;
            entry.acc.requests += 1;
            entry.acc.success_count += row.try_get::<i64, _>("success")?;
            entry.acc.latencies.push(row.try_get::<i64, _>("latency_ms")?);
            if let Some(t) = row.try_get::<Option<i64>, _>("ttft_ms")? {
                entry.acc.ttfts.push(t);
            }
        }

        let mut out: Vec<AccountBreakdown> = groups
            .into_iter()
            .map(|(id, g)| {
                let (avg_latency, _p50l, _p95l, avg_ttft, _p50t, p95_ttft, avg_tps, cache_hit_rate) =
                    finalize_group(&g.acc);
                Ok(AccountBreakdown {
                    id,
                    name: g.name,
                    provider: g.provider,
                    input: g.acc.input,
                    output: g.acc.output,
                    cache_read: g.acc.cache_read,
                    cache_create: g.acc.cache_create,
                    cache_hit_rate,
                    total_tokens: g.acc.input + g.acc.output + g.acc.cache_read + g.acc.cache_create,
                    requests: g.acc.requests,
                    success_count: g.acc.success_count,
                    avg_latency,
                    avg_ttft,
                    p95_ttft,
                    avg_tps,
                    est_cost: g.acc.est_cost,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        out.sort_by(|a, b| {
            let ta = a.input + a.output + a.cache_read + a.cache_create;
            let tb = b.input + b.output + b.cache_read + b.cache_create;
            tb.cmp(&ta)
        });
        Ok(out)
    }

    /// Time-bucketed token timeseries (stacked-line ready).
    /// Buckets are [start, end) sliced by `granularity_ms` — caller chooses granularity based on window.
    pub async fn get_timeseries(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        granularity_ms: i64,
        api_key_id: Option<i64>,
    ) -> Result<Vec<TimeseriesPoint>> {
        let gran = granularity_ms.max(60_000);
        let mut sql = format!(
            "SELECT CAST(timestamp / ? AS INTEGER) * ? AS bucket,
                    IFNULL(input_tokens, 0) AS input_tokens,
                    IFNULL(output_tokens, 0) AS output_tokens,
                    IFNULL(cache_read_input_tokens, 0) AS cache_read,
                    IFNULL(cache_creation_input_tokens, 0) AS cache_create,
                    latency_ms, ttft_ms,
                    {} AS est_cost
             FROM usage_logs
             LEFT JOIN upstream_prices up
                    ON up.account_id = usage_logs.account_id AND up.model_id = usage_logs.model
             LEFT JOIN model_prices mp ON mp.model_id = usage_logs.model
             WHERE is_test = 0",
            price_expr("")
        );
        append_time_filter(&mut sql, "", start_time, end_time);
        append_key_filter(&mut sql, "", api_key_id);
        let mut query = sqlx::query(&sql);
        // 顺序即位置：gran 的两个 ? 在 SELECT 里，时间的在其后，key 的最后。
        // 调整这个顺序不会报错，只会让 gran 与 timestamp 错位、静默返回错分桶。
        query = query.bind(gran).bind(gran);
        query = bind_time_filter(query, start_time, end_time);
        query = bind_key_filter(query, api_key_id);
        let rows = query.fetch_all(&self.pool).await?;

        use std::collections::HashMap;
        let mut groups: HashMap<i64, GroupAcc> = HashMap::new();
        for row in rows {
            let bucket: i64 = row.try_get("bucket")?;
            let acc = groups.entry(bucket).or_default();
            acc.input += row.try_get::<i64, _>("input_tokens")?;
            acc.output += row.try_get::<i64, _>("output_tokens")?;
            acc.cache_read += row.try_get::<i64, _>("cache_read")?;
            acc.cache_create += row.try_get::<i64, _>("cache_create")?;
            acc.est_cost += row.try_get::<f64, _>("est_cost")?;
            acc.requests += 1;
            acc.latencies.push(row.try_get::<i64, _>("latency_ms")?);
            if let Some(t) = row.try_get::<Option<i64>, _>("ttft_ms")? {
                acc.ttfts.push(t);
            }
        }

        let mut out: Vec<TimeseriesPoint> = groups
            .into_iter()
            .map(|(bucket, acc)| {
                let (avg_latency, _p50l, p95_latency, avg_ttft, _p50t, p95_ttft, avg_tps, _chr) =
                    finalize_group(&acc);
                Ok(TimeseriesPoint {
                    bucket,
                    input: acc.input,
                    output: acc.output,
                    cache_read: acc.cache_read,
                    cache_create: acc.cache_create,
                    requests: acc.requests,
                    avg_latency,
                    p95_latency,
                    avg_ttft,
                    p95_ttft,
                    avg_tps,
                    est_cost: acc.est_cost,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        out.sort_by_key(|p| p.bucket);
        Ok(out)
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
        if options.is_stream.is_some() {
            sql.push_str(" AND l.is_stream = ?");
        }
        if options.api_key_id.is_some() {
            sql.push_str(" AND l.api_key_id = ?");
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
        if let Some(is_stream) = options.is_stream {
            query = query.bind(bool_to_i64(is_stream));
        }
        if let Some(api_key_id) = options.api_key_id {
            query = query.bind(api_key_id);
        }
        if let Some(limit) = options.limit {
            query = query.bind(limit);
        }
        if let Some(offset) = options.offset {
            query = query.bind(offset);
        }

        rows_to_logs(query.fetch_all(&self.pool).await?)
    }

    /// Same filters as get_detailed_logs (no LIMIT/OFFSET) — feeds the paginator.
    pub async fn count_detailed_logs(&self, options: DetailedLogQuery) -> Result<i64> {
        let mut sql = String::from("SELECT COUNT(*) FROM usage_logs l WHERE 1=1");
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
        if options.is_stream.is_some() {
            sql.push_str(" AND l.is_stream = ?");
        }
        if options.api_key_id.is_some() {
            sql.push_str(" AND l.api_key_id = ?");
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
        if let Some(is_stream) = options.is_stream {
            query = query.bind(bool_to_i64(is_stream));
        }
        if let Some(api_key_id) = options.api_key_id {
            query = query.bind(api_key_id);
        }
        Ok(query.fetch_one(&self.pool).await?.try_get("COUNT(*)")?)
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

/// Percentile over a sample (linear interpolation between closest ranks).
/// `values` is sorted in place. Returns 0.0 for an empty sample.
fn percentile(values: &mut Vec<i64>, p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_unstable();
    let n = values.len();
    if n == 1 {
        return values[0] as f64;
    }
    let rank = (p / 100.0) * (n as f64 - 1.0);
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    let frac = rank - lo as f64;
    if lo == hi {
        values[lo] as f64
    } else {
        let a = values[lo] as f64;
        let b = values[hi] as f64;
        a + (b - a) * frac
    }
}

/// Running accumulator for a single grouped breakdown / timeseries bucket.
#[derive(Default)]
struct GroupAcc {
    input: i64,
    output: i64,
    cache_read: i64,
    cache_create: i64,
    requests: i64,
    success_count: i64,
    /// 逐行累加的折算成本（美元）。分组维度（厂商/账号/分桶）无法从合计 token
    /// 反推成本 —— 不同模型单价不同，必须按行算完再累加。
    est_cost: f64,
    latencies: Vec<i64>,
    ttfts: Vec<i64>,
}

/// Fold an accumulator into the derived timing/cache metrics.
fn finalize_group(acc: &GroupAcc) -> (f64, f64, f64, f64, f64, f64, f64, f64) {
    // (avg_latency, p50_latency, p95_latency, avg_ttft, p50_ttft, p95_ttft, avg_tps, cache_hit_rate) — all f64
    let mut lat = acc.latencies.clone();
    let mut tt = acc.ttfts.clone();
    let avg_latency = if acc.requests > 0 {
        acc.latencies.iter().sum::<i64>() as f64 / acc.requests as f64
    } else {
        0.0
    };
    let p50_latency = percentile(&mut lat, 50.0);
    let p95_latency = percentile(&mut lat, 95.0);
    let avg_ttft = if tt.is_empty() {
        0.0
    } else {
        tt.iter().sum::<i64>() as f64 / tt.len() as f64
    };
    let p50_ttft = percentile(&mut tt, 50.0);
    let p95_ttft = percentile(&mut tt, 95.0);
    let lat_total: i64 = acc.latencies.iter().sum();
    let avg_tps = if lat_total > 0 {
        acc.output as f64 / (lat_total as f64 / 1000.0)
    } else {
        0.0
    };
    let denom = (acc.input + acc.cache_read + acc.cache_create) as f64;
    let cache_hit_rate = if denom > 0.0 {
        (acc.cache_read as f64 / denom) * 100.0
    } else {
        0.0
    };
    (
        avg_latency,
        p50_latency,
        p95_latency,
        avg_ttft,
        p50_ttft,
        p95_ttft,
        avg_tps,
        cache_hit_rate,
    )
}

/// 每行折算成本（美元）的 SQL 片段（不含 `AS est_cost`）。
///
/// 单价优先级：`upstream_prices`（该**账号 × 模型**）→ `model_prices`（全局兜底，
/// OpenRouter 目录）→ 0。长上下文分档：prompt = input + cache_read，超过 threshold
/// 用 long_* 价；long_* 缺失时退回短档价，不把整行算成 0。DeepSeek 系的峰谷在写入侧
/// 就已按「一律取峰价」拍平，读取侧不再看时间。
///
/// `log_alias` 是 usage_logs 的别名（`""` 或 `"l."`，带点）。调用方必须同时 JOIN：
/// `LEFT JOIN upstream_prices up ON up.account_id = <a>account_id AND up.model_id = <a>model`
/// 与 `LEFT JOIN model_prices mp ON mp.model_id = <a>model`。
fn price_expr(log_alias: &str) -> String {
    let tok = |c: &str| format!("IFNULL({log_alias}{c}, 0)");
    let unit = |short: &str, long: &str| {
        format!(
            "CASE WHEN up.long_context_threshold IS NOT NULL \
               AND ({inp} + {cr}) > up.long_context_threshold \
              THEN IFNULL(up.{long}, COALESCE(up.{short}, mp.{short}, 0.0)) \
              ELSE COALESCE(up.{short}, mp.{short}, 0.0) END",
            inp = tok("input_tokens"),
            cr = tok("cache_read_input_tokens"),
        )
    };
    format!(
        "{} * {} + {} * {} + {} * {} + {} * {}",
        tok("input_tokens"),
        unit("input_price", "long_input_price"),
        tok("output_tokens"),
        unit("output_price", "long_output_price"),
        tok("cache_read_input_tokens"),
        unit("cache_read_price", "long_cache_read_price"),
        tok("cache_creation_input_tokens"),
        unit("cache_write_price", "long_cache_write_price"),
    )
}

fn base_log_select(where_clause: &str) -> String {
    format!(
        "SELECT l.id, l.timestamp, l.account_id, l.provider_id, l.model,
                l.input_tokens, l.output_tokens, l.cache_read_input_tokens,
                l.cache_creation_input_tokens, l.latency_ms, l.success,
                l.error_message, l.is_test, l.ttft_ms, l.is_stream,
                l.client_ip, a.alias AS account_name
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

/// 「按网关密钥筛选」的 SQL 片段。与 append_time_filter 成对使用，**必须在其后调用**
/// —— 绑定按出现顺序取参，两个 helper 颠倒会让查询静默读错列（不报错，只是数据不对）。
fn append_key_filter(sql: &mut String, table_alias: &str, api_key_id: Option<i64>) {
    if api_key_id.is_some() {
        let prefix = if table_alias.is_empty() {
            "api_key_id".to_string()
        } else {
            format!("{table_alias}.api_key_id")
        };
        sql.push_str(&format!(" AND {prefix} = ?"));
    }
}

/// 见 append_key_filter —— 绑定顺序必须与拼接顺序一致。
fn bind_key_filter<'q>(
    mut query: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    api_key_id: Option<i64>,
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
    if let Some(api_key_id) = api_key_id {
        query = query.bind(api_key_id);
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
                ttft_ms: row.try_get("ttft_ms")?,
                is_stream: row.try_get::<i64, _>("is_stream")? != 0,
                account_name: row.try_get("account_name")?,
                client_ip: row.try_get("client_ip")?,
            })
        })
        .collect()
}
