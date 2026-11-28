use crate::models::UsageLogParams;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

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

/// 统计筛选维度。`alias_kind` 由调用方显式给出：ordinary 按 requested_model，
/// aggregate 按 aggregate_alias；同一行 X→G 会同时出现在两个视角里。
#[derive(Debug, Clone, Default)]
pub struct UsageFilter {
    pub account_id: Option<i64>,
    pub alias: Option<String>,
    pub alias_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CandidateBreakdown {
    pub account_id: i64,
    pub account_name: Option<String>,
    pub model: Option<String>,
    pub candidate_idx: Option<i64>,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_create: i64,
    pub requests: i64,
    pub success_count: i64,
    pub avg_latency: f64,
    pub avg_ttft: f64,
    pub p95_ttft: f64,
    pub avg_tps: f64,
    pub est_cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageLogRecord {
    pub id: i64,
    pub timestamp: i64,
    pub account_id: Option<i64>,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    /// 客户端请求名（别名或裸模型）与路由归属；0039 之前为 NULL。
    pub requested_model: Option<String>,
    pub aggregate_alias: Option<String>,
    pub candidate_idx: Option<i64>,
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

/// 流式成功历史的性能行。`avgTtft` / `p95Ttft` 用秒，`avgTps` 用 token/s。
/// TTFT/TPS 只取成功且口径合格的行；成功率的分母是全部流式请求（含失败）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceRow {
    pub model: Option<String>,
    pub account_id: Option<i64>,
    pub account_name: Option<String>,
    pub stream_requests: i64,
    pub success_rate: f64,
    pub avg_ttft: f64,
    pub p95_ttft: f64,
    pub avg_tps: f64,
    pub prev_stream_requests: i64,
    pub prev_success_rate: f64,
    pub prev_avg_ttft: f64,
    pub prev_p95_ttft: f64,
    pub prev_avg_tps: f64,
    pub delta_ttft_pct: f64,
    pub delta_tps_pct: f64,
    /// 成功率变化用百分点差，不是相对百分比。
    pub delta_success_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PerformancePoint {
    pub model: Option<String>,
    pub account_id: Option<i64>,
    pub bucket: i64,
    pub stream_requests: i64,
    pub avg_ttft: f64,
    pub p95_ttft: f64,
    pub avg_tps: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceData {
    pub granularity_ms: i64,
    pub group_by: String,
    pub rows: Vec<PerformanceRow>,
    pub series: Vec<PerformancePoint>,
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
    pub account_id: Option<i64>,
    pub alias: Option<String>,
    pub alias_kind: Option<String>,
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
        filter: &UsageFilter,
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
        append_attribution_filter(&mut sql, "", filter);
        let mut query = sqlx::query(&sql);
        query = bind_time_filter(query, start_time, end_time);
        query = bind_key_filter(query, api_key_id);
        query = bind_attribution_filter(query, filter);
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
        append_attribution_filter(&mut raw_sql, "", filter);
        let mut raw_query = sqlx::query(&raw_sql);
        raw_query = bind_time_filter(raw_query, start_time, end_time);
        raw_query = bind_key_filter(raw_query, api_key_id);
        raw_query = bind_attribution_filter(raw_query, filter);
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
        filter: &UsageFilter,
    ) -> Result<FailoverStats> {
        let mut failure_sql = String::from(
            "SELECT COUNT(*) AS failed_requests,
                    IFNULL(SUM(CASE WHEN error_message LIKE '%429%' OR error_message LIKE '%401%' OR error_message LIKE '%403%' THEN 1 ELSE 0 END), 0) AS failover_triggers
             FROM usage_logs
             WHERE is_test = 0 AND success = 0",
        );
        append_time_filter(&mut failure_sql, "", start_time, end_time);
        append_key_filter(&mut failure_sql, "", api_key_id);
        append_attribution_filter(&mut failure_sql, "", filter);
        let mut failure_query = sqlx::query(&failure_sql);
        failure_query = bind_time_filter(failure_query, start_time, end_time);
        failure_query = bind_key_filter(failure_query, api_key_id);
        failure_query = bind_attribution_filter(failure_query, filter);
        let failure_row = failure_query.fetch_one(&self.pool).await?;
        let failed_requests: i64 = failure_row.try_get("failed_requests")?;
        let failover_triggers: i64 = failure_row.try_get("failover_triggers")?;

        let summary = self.get_summary(start_time, end_time, api_key_id, filter).await?;
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
        filter: &UsageFilter,
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
        append_attribution_filter(&mut sql, "", filter);
        let mut query = sqlx::query(&sql);
        query = bind_time_filter(query, start_time, end_time);
        query = bind_key_filter(query, api_key_id);
        query = bind_attribution_filter(query, filter);
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
        filter: &UsageFilter,
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
        append_attribution_filter(&mut sql, "", filter);
        let mut query = sqlx::query(&sql);
        query = bind_time_filter(query, start_time, end_time);
        query = bind_key_filter(query, api_key_id);
        query = bind_attribution_filter(query, filter);
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
        filter: &UsageFilter,
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
        append_attribution_filter(&mut sql, "l", filter);
        let mut query = sqlx::query(&sql);
        query = bind_time_filter(query, start_time, end_time);
        query = bind_key_filter(query, api_key_id);
        query = bind_attribution_filter(query, filter);
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

    /// 别名内流量走向：按「当时的账户 × 模型 × 候选下标」分组。
    /// alias_kind=aggregate 按归属列聚合别名筛，其余按客户端请求的别名筛。
    pub async fn get_breakdown_by_candidate(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        api_key_id: Option<i64>,
        account_id: Option<i64>,
        alias: &str,
        alias_kind: &str,
    ) -> Result<Vec<CandidateBreakdown>> {
        let col = if alias_kind == "aggregate" {
            "l.aggregate_alias"
        } else {
            "l.requested_model"
        };
        let mut sql = format!(
            "SELECT l.account_id AS id,
                    a.alias AS name,
                    l.model AS model,
                    l.candidate_idx AS candidate_idx,
                    IFNULL(l.input_tokens, 0) AS input_tokens,
                    IFNULL(l.output_tokens, 0) AS output_tokens,
                    IFNULL(l.cache_read_input_tokens, 0) AS cache_read,
                    IFNULL(l.cache_creation_input_tokens, 0) AS cache_create,
                    l.latency_ms, l.ttft_ms,
                    CASE WHEN l.success = 1 THEN 1 ELSE 0 END AS success,
                    {} AS est_cost
             FROM usage_logs l
             LEFT JOIN accounts a ON a.id = l.account_id
             LEFT JOIN upstream_prices up
                    ON up.account_id = l.account_id AND up.model_id = l.model
             LEFT JOIN model_prices mp ON mp.model_id = l.model
             WHERE l.is_test = 0 AND {col} = ?",
            price_expr("l.")
        );
        if account_id.is_some() {
            sql.push_str(" AND l.account_id = ?");
        }
        append_time_filter(&mut sql, "l", start_time, end_time);
        append_key_filter(&mut sql, "l", api_key_id);
        let mut query = sqlx::query(&sql);
        query = query.bind(alias.to_string());
        if let Some(account_id) = account_id {
            query = query.bind(account_id);
        }
        query = bind_time_filter(query, start_time, end_time);
        query = bind_key_filter(query, api_key_id);
        let rows = query.fetch_all(&self.pool).await?;

        use std::collections::HashMap;
        let mut groups: HashMap<
            (i64, Option<String>, Option<i64>),
            (Option<String>, GroupAcc),
        > = HashMap::new();
        for row in rows {
            let key = (
                row.try_get::<i64, _>("id")?,
                row.try_get::<Option<String>, _>("model")?,
                row.try_get::<Option<i64>, _>("candidate_idx")?,
            );
            let entry = groups
                .entry(key)
                .or_insert_with(|| (row.try_get("name").ok().flatten(), GroupAcc::default()));
            let acc = &mut entry.1;
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

        let mut out: Vec<CandidateBreakdown> = Vec::new();
        for ((id, model, candidate_idx), (name, acc)) in groups {
            let (avg_latency, _p50l, _p95l, avg_ttft, _p50t, p95_ttft, avg_tps, _cache_hit_rate) =
                finalize_group(&acc);
            out.push(CandidateBreakdown {
                account_id: id,
                account_name: name,
                model,
                candidate_idx,
                input: acc.input,
                output: acc.output,
                cache_read: acc.cache_read,
                cache_create: acc.cache_create,
                requests: acc.requests,
                success_count: acc.success_count,
                avg_latency,
                avg_ttft,
                p95_ttft,
                avg_tps,
                est_cost: acc.est_cost,
            });
        }
        out.sort_by(|a, b| a.account_id.cmp(&b.account_id).then(a.candidate_idx.cmp(&b.candidate_idx)));
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
        filter: &UsageFilter,
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
        append_attribution_filter(&mut sql, "", filter);
        let mut query = sqlx::query(&sql);
        // 顺序即位置：gran 的两个 ? 在 SELECT 里，时间的在其后，key 的最后。
        // 调整这个顺序不会报错，只会让 gran 与 timestamp 错位、静默返回错分桶。
        query = query.bind(gran).bind(gran);
        query = bind_time_filter(query, start_time, end_time);
        query = bind_key_filter(query, api_key_id);
        query = bind_attribution_filter(query, filter);
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

    /// 流式请求的历史性能。只取真实流量，口径与自动分流的 PerfRegistry 对齐：
    /// TTFT 取成功行的非空样本，TPS 取解码段 ≥500ms 且输出 ≥8 token 的成功行。
    /// 前一等长窗口用于 `prev*` 与变化量。
    pub async fn get_performance(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        api_key_id: Option<i64>,
        filter: &UsageFilter,
        model: Option<&str>,
        group_by: &str,
        granularity_ms: i64,
    ) -> Result<PerformanceData> {
        let end = end_time.unwrap_or_else(now_millis);
        let start = start_time.unwrap_or_else(|| end - DAY_MS);
        let span = (end - start).max(0);
        let gran = granularity_ms.max(60_000);
        let group_by = if group_by == "account_model" {
            "account_model"
        } else {
            "model"
        };

        let current = self
            .fetch_perf_rows(Some(start), Some(end), api_key_id, filter, model, gran)
            .await?;
        let previous = self
            .fetch_perf_rows(Some(start - span), Some(start), api_key_id, filter, model, gran)
            .await?;

        let cur_groups = group_perf_rows(&current, group_by);
        let prev_groups = group_perf_rows(&previous, group_by);
        let mut rows = Vec::new();
        for (key, (account_name, acc)) in cur_groups {
            let (avg_ttft, p95_ttft, avg_tps) = acc.metrics();
            let success_rate = acc.success_rate();
            let prev = prev_groups.get(&key);
            let (prev_avg_ttft, prev_p95_ttft, prev_avg_tps) = prev
                .map(|(_, a)| a.metrics())
                .unwrap_or((0.0, 0.0, 0.0));
            let prev_success_rate = prev.map(|(_, a)| a.success_rate()).unwrap_or(0.0);
            rows.push(PerformanceRow {
                model: key.1.clone(),
                account_id: key.0,
                account_name,
                stream_requests: acc.requests,
                success_rate,
                avg_ttft,
                p95_ttft,
                avg_tps,
                prev_stream_requests: prev.map(|(_, a)| a.requests).unwrap_or(0),
                prev_success_rate,
                prev_avg_ttft,
                prev_p95_ttft,
                prev_avg_tps,
                delta_ttft_pct: delta_pct(avg_ttft, prev_avg_ttft),
                delta_tps_pct: delta_pct(avg_tps, prev_avg_tps),
                delta_success_rate: success_rate - prev_success_rate,
            });
        }

        let mut buckets: BTreeMap<(Option<i64>, Option<String>, i64), PerfAcc> = BTreeMap::new();
        for r in &current {
            let key = perf_key(r, group_by);
            buckets.entry((key.0, key.1, r.bucket)).or_default().fold(r);
        }
        let series = buckets
            .into_iter()
            .map(|((account_id, model, bucket), acc)| {
                let (avg_ttft, p95_ttft, avg_tps) = acc.metrics();
                PerformancePoint {
                    model,
                    account_id,
                    bucket,
                    stream_requests: acc.requests,
                    avg_ttft,
                    p95_ttft,
                    avg_tps,
                }
            })
            .collect();

        Ok(PerformanceData {
            granularity_ms: gran,
            group_by: group_by.to_string(),
            rows,
            series,
        })
    }

    async fn fetch_perf_rows(
        &self,
        start_time: Option<i64>,
        end_time: Option<i64>,
        api_key_id: Option<i64>,
        filter: &UsageFilter,
        model: Option<&str>,
        gran: i64,
    ) -> Result<Vec<PerfRaw>> {
        let mut sql = format!(
            "SELECT CAST(usage_logs.timestamp / ? AS INTEGER) * ? AS bucket,
                    usage_logs.account_id, usage_logs.model, a.alias AS account_name,
                    usage_logs.success, usage_logs.latency_ms, usage_logs.ttft_ms,
                    usage_logs.output_tokens
             FROM usage_logs
             LEFT JOIN accounts a ON usage_logs.account_id = a.id
             WHERE usage_logs.is_test = 0 AND usage_logs.is_stream = 1"
        );
        append_time_filter(&mut sql, "", start_time, end_time);
        append_key_filter(&mut sql, "", api_key_id);
        if model.is_some() {
            sql.push_str(" AND usage_logs.model = ?");
        }
        append_attribution_filter(&mut sql, "", filter);
        let mut query = sqlx::query(&sql)
            .bind(gran)
            .bind(gran);
        query = bind_time_filter(query, start_time, end_time);
        query = bind_key_filter(query, api_key_id);
        if let Some(model) = model {
            query = query.bind(model.to_string());
        }
        query = bind_attribution_filter(query, filter);
        let rows = query.fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                Ok(PerfRaw {
                    bucket: row.try_get("bucket")?,
                    account_id: row.try_get("account_id")?,
                    model: row.try_get("model")?,
                    account_name: row.try_get("account_name")?,
                    success: row.try_get::<i64, _>("success")? != 0,
                    latency_ms: row.try_get("latency_ms")?,
                    ttft_ms: row.try_get("ttft_ms")?,
                    output_tokens: row.try_get("output_tokens")?,
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
        if options.is_stream.is_some() {
            sql.push_str(" AND l.is_stream = ?");
        }
        if options.api_key_id.is_some() {
            sql.push_str(" AND l.api_key_id = ?");
        }
        let filter = UsageFilter {
            account_id: options.account_id,
            alias: options.alias.clone(),
            alias_kind: options.alias_kind.clone(),
        };
        append_attribution_filter(&mut sql, "l", &filter);
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
        query = bind_attribution_filter(query, &filter);
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
        let filter = UsageFilter {
            account_id: options.account_id,
            alias: options.alias.clone(),
            alias_kind: options.alias_kind.clone(),
        };
        append_attribution_filter(&mut sql, "l", &filter);

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
        query = bind_attribution_filter(query, &filter);
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

struct PerfRaw {
    bucket: i64,
    account_id: Option<i64>,
    model: Option<String>,
    account_name: Option<String>,
    success: bool,
    latency_ms: i64,
    ttft_ms: Option<i64>,
    output_tokens: i64,
}

#[derive(Default)]
struct PerfAcc {
    requests: i64,
    successes: i64,
    ttfts: Vec<i64>,
    tps: Vec<f64>,
}

impl PerfAcc {
    fn fold(&mut self, r: &PerfRaw) {
        self.requests += 1;
        if !r.success {
            return;
        }
        self.successes += 1;
        let Some(ttft) = r.ttft_ms else { return };
        self.ttfts.push(ttft);
        // 与 PerfRegistry 同口径：解码段太短/输出太少的行不足以估算 TPS。
        let decode_ms = r.latency_ms - ttft;
        if decode_ms >= 500 && r.output_tokens >= 8 {
            self.tps.push(r.output_tokens as f64 / (decode_ms as f64 / 1000.0));
        }
    }

    fn success_rate(&self) -> f64 {
        if self.requests > 0 {
            self.successes as f64 / self.requests as f64
        } else {
            0.0
        }
    }

    /// (avg_ttft 秒, p95_ttft 秒, avg_tps)。
    fn metrics(&self) -> (f64, f64, f64) {
        let mut ttfts = self.ttfts.clone();
        let avg_ttft = if ttfts.is_empty() {
            0.0
        } else {
            ttfts.iter().sum::<i64>() as f64 / ttfts.len() as f64 / 1000.0
        };
        let p95_ttft = percentile(&mut ttfts, 95.0) / 1000.0;
        let avg_tps = if self.tps.is_empty() {
            0.0
        } else {
            self.tps.iter().sum::<f64>() / self.tps.len() as f64
        };
        (avg_ttft, p95_ttft, avg_tps)
    }
}

fn perf_key(r: &PerfRaw, group_by: &str) -> (Option<i64>, Option<String>) {
    if group_by == "account_model" {
        (r.account_id, r.model.clone())
    } else {
        (None, r.model.clone())
    }
}

fn group_perf_rows(
    rows: &[PerfRaw],
    group_by: &str,
) -> BTreeMap<(Option<i64>, Option<String>), (Option<String>, PerfAcc)> {
    let mut out: BTreeMap<_, _> = BTreeMap::new();
    for r in rows {
        let key = perf_key(r, group_by);
        let entry = out.entry(key).or_insert_with(|| {
            let name = if group_by == "account_model" { r.account_name.clone() } else { None };
            (name, PerfAcc::default())
        });
        entry.1.fold(r);
    }
    out
}

fn delta_pct(current: f64, previous: f64) -> f64 {
    if previous.abs() < 1e-9 {
        0.0
    } else {
        (current - previous) / previous * 100.0
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
                l.requested_model, l.aggregate_alias, l.candidate_idx,
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

/// 账户 / 别名筛选。必须在 time/key 之后拼接，绑定顺序同样在后；
/// `alias_kind=aggregate` 走归属列，其余（含未指定）走请求名。
/// 空表别名时用 `usage_logs.` 前缀：这些查询都 JOIN 了价格表，
/// 裸 `account_id` 会报 ambiguous column。
fn append_attribution_filter(sql: &mut String, table_alias: &str, filter: &UsageFilter) {
    let prefix = |col: &str| {
        if table_alias.is_empty() {
            format!("usage_logs.{col}")
        } else {
            format!("{table_alias}.{col}")
        }
    };
    if filter.account_id.is_some() {
        sql.push_str(&format!(" AND {} = ?", prefix("account_id")));
    }
    if let Some(alias) = &filter.alias {
        let col = if filter.alias_kind.as_deref() == Some("aggregate") {
            "aggregate_alias"
        } else {
            "requested_model"
        };
        sql.push_str(&format!(" AND {} = ?", prefix(col)));
    }
}

fn bind_attribution_filter<'q>(
    mut query: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    filter: &UsageFilter,
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
    if let Some(account_id) = filter.account_id {
        query = query.bind(account_id);
    }
    if let Some(alias) = &filter.alias {
        query = query.bind(alias.clone());
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
                requested_model: row.try_get("requested_model")?,
                aggregate_alias: row.try_get("aggregate_alias")?,
                candidate_idx: row.try_get("candidate_idx")?,
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
