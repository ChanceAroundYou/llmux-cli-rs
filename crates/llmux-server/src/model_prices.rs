//! OpenRouter 价目刷新：把「折算成本」的单价灌进复用的 `model_prices` 表。
//!
//! 设计取舍（见迁移 0027）：不新建 `model_price_cache`，而是在 `model_prices`
//! 上加一列 `source`。自动刷新只写 `source='openrouter'` 的行；`source='manual'`
//! 的行（手工填的、以及免费/本地模型记 0 的行）永不被覆盖 —— 这正是当初想
//! 拆表的唯一理由。
//!
//! 单价单位是 **美元 / token**（OpenRouter 原始单位），不是每百万。
//!
//! 匹配分三层（从严到松），只对 `usage_logs` 里真实出现过的模型名做：
//!   1. 完全相同
//!   2. 候选去掉厂商前缀后 == 本地名（`deepseek-v4.1-flash` ↔ `deepseek/deepseek-v4.1-flash`）
//!   3. 再去掉 `-free` 与日期后缀（`qwen3.7-max-2026-06-08` → `qwen/qwen3.7-max`）
//! 公开渠道查不到的模型（聚合站自造名、本地 GGUF）匹配不上，保持无价行 → 计 0，
//! 并在统计里以 `unpricedModels` 显式暴露。

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::SqlitePool;

const OPENROUTER_MODELS_URL: &str = "https://openrouter.ai/api/v1/models";

/// 6h：与 `db_vacuum` 等现有维护循环同频。价格变动本就不频繁，再密只是徒增外网请求。
const REFRESH_INTERVAL_SECS: u64 = 6 * 3600;

pub const SOURCE_OPENROUTER: &str = "openrouter";
pub const SOURCE_MANUAL: &str = "manual";

/// OpenRouter 原始条目归一化后的价目。单位：美元 / token。
#[derive(Debug, Clone, PartialEq)]
pub struct FetchedPrice {
    /// OpenRouter 的模型 id，如 `deepseek/deepseek-v4.1-flash`。
    pub id: String,
    /// id 里 `/` 前的厂商段。
    pub vendor: String,
    pub input: f64,
    pub output: f64,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
}

/// 一次刷新的结果，既回给管理接口，也进日志。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct RefreshReport {
    /// OpenRouter 返回的价目条数。
    pub fetched: usize,
    /// 匹配并写入 openrouter 行的本地模型数。
    pub matched: usize,
    /// 因是 manual 行而跳过的本地模型数。
    pub skipped_manual: usize,
    /// 没有匹配到 OpenRouter 的本地模型名（计 0，暴露给用户手填）。
    pub unmatched: Vec<String>,
}

impl RefreshReport {
    fn summary(&self) -> String {
        format!(
            "OpenRouter {} 条，匹配 {}，跳过人工 {}，未匹配 {}",
            self.fetched,
            self.matched,
            self.skipped_manual,
            self.unmatched.len()
        )
    }
}

/// 解析 `/api/v1/models` 的响应体。缺 `prompt`/`completion` 的条目直接跳过
/// —— 没有基础价就没有折算意义。
pub fn parse_openrouter_models(json: &Value) -> Vec<FetchedPrice> {
    json.get("data")
        .and_then(|d| d.as_array())
        .map(|items| items.iter().filter_map(parse_entry).collect())
        .unwrap_or_default()
}

fn parse_entry(item: &Value) -> Option<FetchedPrice> {
    let id = item.get("id")?.as_str()?.to_string();
    let pricing = item.get("pricing")?;
    let input = price_field(pricing, "prompt")?;
    let output = price_field(pricing, "completion")?;
    let vendor = id.split('/').next().unwrap_or("").to_string();
    Some(FetchedPrice {
        id,
        vendor,
        input,
        output,
        cache_read: price_field(pricing, "input_cache_read"),
        cache_write: price_field(pricing, "input_cache_write"),
    })
}

/// OpenRouter 的价格字段有时是字符串、有时是数字、有时是 null。
fn price_field(pricing: &Value, key: &str) -> Option<f64> {
    let v = pricing.get(key)?;
    if v.is_null() {
        return None;
    }
    if let Some(n) = v.as_f64() {
        return Some(n);
    }
    v.as_str()?.trim().parse::<f64>().ok()
}

/// 把网关侧模型名匹配到 OpenRouter id。三层规则见模块文档。
pub fn match_model(local: &str, candidates: &[String]) -> Option<String> {
    // 第 1 层：完全相同。
    if let Some(c) = candidates.iter().find(|c| c.as_str() == local) {
        return Some(c.clone());
    }
    // 第 2 层：候选去厂商前缀 == 本地名。
    for c in candidates {
        if strip_vendor(c).eq_ignore_ascii_case(local) {
            return Some(c.clone());
        }
    }
    // 第 3 层：两边都剥掉 `-free` / 日期后缀后再比。
    let local_base = strip_suffixes(local);
    for c in candidates {
        if strip_suffixes(strip_vendor(c)).eq_ignore_ascii_case(&local_base) {
            return Some(c.clone());
        }
    }
    None
}

fn strip_vendor(id: &str) -> &str {
    id.rsplit('/').next().unwrap_or(id)
}

/// 剥掉 `-free` 与日期后缀（可叠加），不区分大小写。
fn strip_suffixes(name: &str) -> String {
    let mut s = name.to_string();
    loop {
        let lower = s.to_lowercase();
        if lower.ends_with("-free") {
            s.truncate(s.len() - 5);
            continue;
        }
        if let Some(idx) = date_suffix_start(&s) {
            s.truncate(idx);
            continue;
        }
        break;
    }
    s
}

/// 返回 `-YYYYMMDD` 或 `-YYYY-MM-DD` 起始的字节下标；没有则 None。
fn date_suffix_start(name: &str) -> Option<usize> {
    // -YYYY-MM-DD
    if name.len() >= 11 {
        let tail = &name[name.len() - 11..];
        let b = tail.as_bytes();
        if b[0] == b'-'
            && b[1..5].iter().all(u8::is_ascii_digit)
            && b[5] == b'-'
            && b[6..8].iter().all(u8::is_ascii_digit)
            && b[8] == b'-'
            && b[9..11].iter().all(u8::is_ascii_digit)
        {
            return Some(name.len() - 11);
        }
    }
    // -YYYYMMDD
    if name.len() >= 9 {
        let tail = &name[name.len() - 9..];
        if tail.starts_with('-') && tail[1..].bytes().all(|b| b.is_ascii_digit()) {
            return Some(name.len() - 9);
        }
    }
    None
}

/// 拉一次 OpenRouter 公开价目并写库。被定时循环和管理接口共用。
pub async fn refresh(pool: &SqlitePool) -> Result<RefreshReport> {
    let prices = fetch_openrouter().await?;
    let candidates: Vec<String> = prices.iter().map(|p| p.id.clone()).collect();
    let index: HashMap<&str, &FetchedPrice> =
        prices.iter().map(|p| (p.id.as_str(), p)).collect();

    // 只处理真实出现过的模型名：探活行 is_test=1 不代表在用的流量。
    let locals: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT model FROM usage_logs
         WHERE is_test = 0 AND model IS NOT NULL AND model != ''",
    )
    .fetch_all(pool)
    .await
    .context("列出 usage_logs 里的模型名")?;

    let manual: HashSet<String> =
        sqlx::query_scalar("SELECT model_id FROM model_prices WHERE source = 'manual'")
            .fetch_all(pool)
            .await
            .context("列出人工价目行")?
            .into_iter()
            .collect();

    let mut report = RefreshReport {
        fetched: prices.len(),
        ..Default::default()
    };
    for local in locals {
        if manual.contains(&local) {
            report.skipped_manual += 1;
            continue;
        }
        match match_model(&local, &candidates) {
            Some(id) => {
                if let Some(p) = index.get(id.as_str()) {
                    upsert_openrouter(pool, &local, p).await?;
                    report.matched += 1;
                }
            }
            None => report.unmatched.push(local),
        }
    }
    Ok(report)
}

async fn fetch_openrouter() -> Result<Vec<FetchedPrice>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("构建 HTTP 客户端")?;
    let resp = client
        .get(OPENROUTER_MODELS_URL)
        .send()
        .await
        .context("请求 OpenRouter 模型列表")?
        .error_for_status()
        .context("OpenRouter 返回非 2xx")?;
    let json: Value = resp.json().await.context("解析 OpenRouter JSON")?;
    Ok(parse_openrouter_models(&json))
}

/// 写 openrouter 价目。`WHERE model_prices.source != 'manual'` 是这条语句的
/// 全部意义：并发刷新与人工改价撞上时，人工值赢。
async fn upsert_openrouter(pool: &SqlitePool, model_id: &str, p: &FetchedPrice) -> Result<()> {
    sqlx::query(
        "INSERT INTO model_prices
            (model_id, vendor, input_price, output_price, cache_read_price, cache_write_price, source, source_model_id)
         VALUES (?, ?, ?, ?, ?, ?, 'openrouter', ?)
         ON CONFLICT(model_id) DO UPDATE SET
            vendor = excluded.vendor,
            input_price = excluded.input_price,
            output_price = excluded.output_price,
            cache_read_price = excluded.cache_read_price,
            cache_write_price = excluded.cache_write_price,
            source = 'openrouter',
            source_model_id = excluded.source_model_id,
            updated_at = CURRENT_TIMESTAMP
         WHERE model_prices.source != 'manual'",
    )
    .bind(model_id)
    .bind(&p.vendor)
    .bind(p.input)
    .bind(p.output)
    .bind(p.cache_read)
    .bind(p.cache_write)
    .bind(&p.id)
    .execute(pool)
    .await
    .context("写入 OpenRouter 价目")?;
    Ok(())
}

/// 手工定价：写 `source='manual'`，从此刷新不再覆盖它。
pub async fn set_manual_price(
    pool: &SqlitePool,
    model_id: &str,
    vendor: Option<&str>,
    input: f64,
    output: f64,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO model_prices
            (model_id, vendor, input_price, output_price, cache_read_price, cache_write_price, source, source_model_id)
         VALUES (?, ?, ?, ?, ?, ?, 'manual', NULL)
         ON CONFLICT(model_id) DO UPDATE SET
            vendor = COALESCE(excluded.vendor, model_prices.vendor),
            input_price = excluded.input_price,
            output_price = excluded.output_price,
            cache_read_price = excluded.cache_read_price,
            cache_write_price = excluded.cache_write_price,
            source = 'manual',
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(model_id)
    .bind(vendor)
    .bind(input)
    .bind(output)
    .bind(cache_read)
    .bind(cache_write)
    .execute(pool)
    .await
    .context("写入人工价目")?;
    Ok(())
}

/// 启动时挂上：先立刻拉一次（新部署/重启不必等满 6 小时），然后每 6h 一轮。
///
/// 循环内错误只 warn，不退出 —— 与 `spawn_db_vacuum` 同规矩：刷新失败只是价目
/// 暂时陈旧，绝不能让后台任务静默死掉或影响网关。
pub fn spawn_price_refresh(pool: SqlitePool) {
    tokio::spawn(async move {
        run_once(&pool).await;
        loop {
            tokio::time::sleep(Duration::from_secs(REFRESH_INTERVAL_SECS)).await;
            run_once(&pool).await;
        }
    });
}

async fn run_once(pool: &SqlitePool) {
    match refresh(pool).await {
        Ok(report) => tracing::info!("💰 价目刷新：{}", report.summary()),
        Err(e) => tracing::warn!("💰 价目刷新失败（下一轮重试）: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn candidates(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn matches_exact_id() {
        let c = candidates(&["openai/gpt-5"]);
        assert_eq!(match_model("openai/gpt-5", &c).as_deref(), Some("openai/gpt-5"));
    }

    #[test]
    fn matches_after_adding_vendor_prefix() {
        // 研究里 5/32 → 19/32 靠的就是这一层。
        let c = candidates(&["deepseek/deepseek-v4.1-flash", "xiaomi/mimo-v2.5"]);
        assert_eq!(
            match_model("deepseek-v4.1-flash", &c).as_deref(),
            Some("deepseek/deepseek-v4.1-flash")
        );
        assert_eq!(match_model("mimo-v2.5", &c).as_deref(), Some("xiaomi/mimo-v2.5"));
    }

    #[test]
    fn matches_after_stripping_free_and_date_suffix() {
        let c = candidates(&["qwen/qwen3.7-max", "deepseek/deepseek-v4-flash"]);
        assert_eq!(
            match_model("qwen3.7-max-2026-06-08", &c).as_deref(),
            Some("qwen/qwen3.7-max")
        );
        assert_eq!(
            match_model("qwen3.7-max-20260608", &c).as_deref(),
            Some("qwen/qwen3.7-max")
        );
        assert_eq!(
            match_model("deepseek-v4-flash-free", &c).as_deref(),
            Some("deepseek/deepseek-v4-flash")
        );
    }

    #[test]
    fn leaves_unknown_models_unmatched() {
        let c = candidates(&["deepseek/deepseek-v4.1-flash"]);
        assert_eq!(match_model("omen-alpha", &c), None);
        assert_eq!(match_model("agnes-3.0-flash", &c), None);
    }

    #[test]
    fn parses_string_and_numeric_pricing() {
        let json = json!({
            "data": [
                {
                    "id": "deepseek/deepseek-v4.1-flash",
                    "pricing": {
                        "prompt": "0.0000003",
                        "completion": "0.0000012",
                        "input_cache_read": "0.00000003",
                        "input_cache_write": null
                    }
                },
                {
                    "id": "x/y",
                    "pricing": { "prompt": 1e-6, "completion": 2e-6 }
                },
                {
                    "id": "no/pricing"
                }
            ]
        });
        let parsed = parse_openrouter_models(&json);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].vendor, "deepseek");
        assert_eq!(parsed[0].cache_read, Some(0.00000003));
        assert_eq!(parsed[0].cache_write, None);
        assert_eq!(parsed[1].input, 1e-6);
        assert_eq!(parsed[1].cache_read, None);
    }
}
