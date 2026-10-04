//! 上游价目来源：把「每个上游账号 × 模型」的真实单价抓下来写进 `upstream_prices`。
//!
//! 为什么不只用 OpenRouter：同一个 `deepseek-v4.1-flash` 被 go2/go6/go7（OpenCode
//! Go 订阅）、command、DeepSeek 官方等多个上游服务，价各不相同 —— OpenRouter 的
//! 缓存读价（$0.03/M）比 OpenCode Go（$0.006/M）贵 5 倍。只按模型名取价是实打实的错。
//!
//! 单位约定：`FetchedPrice` 一律是 **美元 / 百万 token**（各家文档的原始口径，也读得懂）；
//! 写库时 ÷1e6 换成 `upstream_prices` 约定的 **美元 / token**。

pub mod html;
mod deepseek;
mod openrouter;
mod teamorouter;
mod zen;

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::SqlitePool;

use crate::model_prices::match_model;

/// 上游价目来源。`manual` = 抓不到、只能人工填；`free` = 本地/订阅内不计费。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    OpenRouter,
    Zen,
    ZenGo,
    DeepSeek,
    TeamoRouter,
    Manual,
    Free,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::OpenRouter => "openrouter",
            Source::Zen => "zen",
            Source::ZenGo => "zen-go",
            Source::DeepSeek => "deepseek",
            Source::TeamoRouter => "teamorouter",
            Source::Manual => "manual",
            Source::Free => "free",
        }
    }

    /// 已实现自动抓取的来源。
    pub fn is_fetchable(self) -> bool {
        matches!(
            self,
            Source::OpenRouter
                | Source::Zen
                | Source::ZenGo
                | Source::DeepSeek
                | Source::TeamoRouter
        )
    }

    fn url(self) -> Option<&'static str> {
        match self {
            Source::OpenRouter => Some("https://openrouter.ai/api/v1/models"),
            Source::Zen => Some("https://opencode.ai/docs/zen/"),
            Source::ZenGo => Some("https://opencode.ai/docs/go/"),
            Source::DeepSeek => Some("https://api-docs.deepseek.com/quick_start/pricing"),
            Source::TeamoRouter => Some("https://teamorouter.cn/"),
            Source::Manual | Source::Free => None,
        }
    }
}

/// 一条抓到的价目，单位 **美元 / 百万 token**。
#[derive(Debug, Clone, PartialEq)]
pub struct FetchedPrice {
    /// 上游侧模型 id（Zen/Go 已用端点表把 display 名换成 id）。
    pub model_id: String,
    pub vendor: Option<String>,
    pub input: f64,
    pub output: f64,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    pub long_context_threshold: Option<i64>,
    pub long_input: Option<f64>,
    pub long_output: Option<f64>,
    pub long_cache_read: Option<f64>,
    pub long_cache_write: Option<f64>,
}

/// 一次全量刷新的结果。
#[derive(Debug, Default, serde::Serialize)]
pub struct RefreshReport {
    pub accounts: usize,
    pub sources_fetched: Vec<String>,
    pub matched: usize,
    pub free_rows: usize,
    pub unmatched: Vec<String>,
    /// 有 adapter 但还没实现的来源（DeepSeek / TeamoRouter），下一轮补。
    pub pending_accounts: Vec<String>,
    /// 抓不到公开价目、只能人工填的来源（command / 百炼 / api123 / agnes）。
    pub manual_accounts: Vec<String>,
    pub failures: Vec<String>,
}

impl RefreshReport {
    pub fn summary(&self) -> String {
        format!(
            "已抓来源 [{}]，匹配 {}，free {}，未匹配 {}，待实现 {}，人工 {}，失败 {}",
            self.sources_fetched.join(","),
            self.matched,
            self.free_rows,
            self.unmatched.len(),
            self.pending_accounts.len(),
            self.manual_accounts.len(),
            self.failures.len()
        )
    }
}

/// 账号 base_url / alias → 价目来源。
pub fn source_for(base_url: &str, alias: &str) -> Source {
    let u = base_url.to_lowercase();
    if u.contains("opencode.ai/zen/go") {
        Source::ZenGo
    } else if u.contains("opencode.ai/zen") {
        Source::Zen
    } else if u.contains("api.deepseek.com") {
        Source::DeepSeek
    } else if u.contains("openrouter.ai") {
        Source::OpenRouter
    } else if u.contains("teamorouter.cn") {
        Source::TeamoRouter
    } else if u.contains("commandcode.ai")
        || u.contains("aliyuncs.com")
        || u.contains("api123go.com")
        || u.contains("agnes-ai.cn")
    {
        Source::Manual
    } else if alias.eq_ignore_ascii_case("local")
        || u.contains("pc.xiaokubao")
        || u.contains("192.168.")
        || u.contains("://10.")
    {
        Source::Free
    } else {
        Source::Manual
    }
}

/// 某些上游用 legacy 模型名，映射到官方表里的名字（如 DeepSeek 官方注释：
/// `deepseek-v4-flash` 已退役，请求由 V4.1-Flash 服务、按 Flash 价计费）。
fn apply_alias_map(src: Source, local: &str) -> String {
    let table: &[(&str, &str)] = match src {
        Source::DeepSeek => &[
            ("deepseek-v4-flash", "deepseek-flash"),
            ("deepseek-v4-flash-vision-exp", "deepseek-flash"),
        ],
        _ => &[],
    };
    table
        .iter()
        .find(|(from, _)| *from == local)
        .map(|(_, to)| (*to).to_string())
        .unwrap_or_else(|| local.to_string())
}

/// 已知免费 / stealth 模型：名字带 `-free` / `:free`，或明确在名单里。
///
/// 这类模型在**任何**上游都不该产生成本 —— 不能依赖某个来源的价目表恰好把它标成
/// 0，否则来源改版、或该账号走全局兜底时就会被误计费。`space-bunny-alpha`、
/// `omen-alpha`、`big-pickle` 是各家文档里点名的 stealth 免费模型。
pub fn is_known_free(model_id: &str) -> bool {
    let m = model_id.to_lowercase();
    if m.ends_with("-free") || m.ends_with(":free") || m.contains("-free-") {
        return true;
    }
    matches!(
        m.as_str(),
        "stealth/space-bunny-alpha"
            | "space-bunny-alpha"
            | "space-bunny-free"
            | "omen-alpha"
            | "big-pickle"
    )
}

/// 全量刷新：按账号判定来源 → 每来源抓一次 → 匹配本地模型名 → 写 `upstream_prices`。
pub async fn refresh_all(pool: &SqlitePool) -> Result<RefreshReport> {
    let accounts: Vec<(i64, String, Option<String>)> =
        sqlx::query_as("SELECT id, alias, base_url FROM accounts")
            .fetch_all(pool)
            .await
            .context("列出账号")?;

    let mut report = RefreshReport {
        accounts: accounts.len(),
        ..Default::default()
    };
    let mut cache: HashMap<Source, Vec<FetchedPrice>> = HashMap::new();

    for (id, alias, base_url) in accounts {
        let src = source_for(base_url.as_deref().unwrap_or(""), &alias);
        let locals: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT model FROM usage_logs
             WHERE account_id = ? AND is_test = 0 AND model IS NOT NULL AND model != ''",
        )
        .bind(id)
        .fetch_all(pool)
        .await
        .context("列出账号下的模型")?;
        if locals.is_empty() {
            continue;
        }

        // 已知免费 / stealth 模型：任何上游都不该产生成本，先落 free 0 价行 ——
        // 不能指望某个来源的价目表恰好把它标成 0（space-bunny / omen 就是这种）。
        let (free_locals, paid_locals): (Vec<String>, Vec<String>) =
            locals.into_iter().partition(|m| is_known_free(m));
        for m in &free_locals {
            upsert_zero(pool, id, m, Source::Free.as_str()).await?;
            report.free_rows += 1;
        }
        if paid_locals.is_empty() {
            continue;
        }

        match src {
            Source::Free => {
                for m in paid_locals {
                    upsert_zero(pool, id, &m, Source::Free.as_str()).await?;
                    report.free_rows += 1;
                }
            }
            Source::OpenRouter | Source::Zen | Source::ZenGo | Source::DeepSeek | Source::TeamoRouter => {
                if !cache.contains_key(&src) {
                    match fetch_source(src).await {
                        Ok(v) => {
                            cache.insert(src, v);
                        }
                        Err(e) => {
                            report.failures.push(format!("{}: {e}", src.as_str()));
                            continue;
                        }
                    }
                }
                let prices = &cache[&src];
                let candidates: Vec<String> = prices.iter().map(|p| p.model_id.clone()).collect();
                for local in paid_locals {
                    let want = apply_alias_map(src, &local);
                    let hit = match_model(&want, &candidates)
                        .and_then(|found| prices.iter().find(|p| p.model_id == found).map(|p| (found, p)));
                    match hit {
                        Some((found, p)) => {
                            upsert_price(pool, id, &local, p, src.as_str(), &found).await?;
                            report.matched += 1;
                        }
                        None => report.unmatched.push(format!("{local}@{alias}")),
                    }
                }
            }
            Source::Manual => report.manual_accounts.push(alias),
        }
    }
    report.sources_fetched = cache.keys().map(|s| s.as_str().to_string()).collect();
    report.sources_fetched.sort();
    Ok(report)
}

async fn fetch_source(src: Source) -> Result<Vec<FetchedPrice>> {
    let url = src.url().context("该来源没有可抓 URL")?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("构建 HTTP 客户端")?;
    let resp = client
        .get(url)
        // 有些站（CDN）对无 UA 的请求直接拒；给个明确身份比伪装浏览器体面。
        .header("User-Agent", "llmux-price-refresh/0.1")
        .send()
        .await
        .with_context(|| format!("请求 {}", src.as_str()))?
        .error_for_status()
        .with_context(|| format!("{} 返回非 2xx", src.as_str()))?;
    match src {
        Source::OpenRouter => {
            let json: Value = resp.json().await.context("解析 OpenRouter JSON")?;
            Ok(openrouter::parse(&json))
        }
        Source::Zen | Source::ZenGo => {
            let html = resp.text().await.context("读 Zen/Go 文档")?;
            Ok(zen::parse(&html))
        }
        Source::DeepSeek => {
            let html = resp.text().await.context("读 DeepSeek 文档")?;
            Ok(deepseek::parse(&html))
        }
        Source::TeamoRouter => {
            let html = resp.text().await.context("读 TeamoRouter 页面")?;
            Ok(teamorouter::parse(&html))
        }
        _ => Ok(Vec::new()),
    }
}

const UPSERT_SQL: &str = "INSERT INTO upstream_prices
        (account_id, model_id, vendor, input_price, output_price,
         cache_read_price, cache_write_price, long_context_threshold,
         long_input_price, long_output_price, long_cache_read_price, long_cache_write_price,
         source, source_model_id)
     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
     ON CONFLICT(account_id, model_id) DO UPDATE SET
        vendor = excluded.vendor,
        input_price = excluded.input_price,
        output_price = excluded.output_price,
        cache_read_price = excluded.cache_read_price,
        cache_write_price = excluded.cache_write_price,
        long_context_threshold = excluded.long_context_threshold,
        long_input_price = excluded.long_input_price,
        long_output_price = excluded.long_output_price,
        long_cache_read_price = excluded.long_cache_read_price,
        long_cache_write_price = excluded.long_cache_write_price,
        source = excluded.source,
        source_model_id = excluded.source_model_id,
        updated_at = CURRENT_TIMESTAMP
     WHERE upstream_prices.source != 'manual'";

fn per_token(per_million: f64) -> f64 {
    per_million / 1_000_000.0
}

async fn upsert_price(
    pool: &SqlitePool,
    account_id: i64,
    model_id: &str,
    p: &FetchedPrice,
    source: &str,
    source_model_id: &str,
) -> Result<()> {
    sqlx::query(UPSERT_SQL)
        .bind(account_id)
        .bind(model_id)
        .bind(&p.vendor)
        .bind(per_token(p.input))
        .bind(per_token(p.output))
        .bind(p.cache_read.map(per_token))
        .bind(p.cache_write.map(per_token))
        .bind(p.long_context_threshold)
        .bind(p.long_input.map(per_token))
        .bind(p.long_output.map(per_token))
        .bind(p.long_cache_read.map(per_token))
        .bind(p.long_cache_write.map(per_token))
        .bind(source)
        .bind(source_model_id)
        .execute(pool)
        .await
        .context("写入 upstream_prices")?;
    Ok(())
}

/// free 行：全 0，标 `source='free'`。`manual` 保护同样生效。
async fn upsert_zero(pool: &SqlitePool, account_id: i64, model_id: &str, source: &str) -> Result<()> {
    sqlx::query(UPSERT_SQL)
        .bind(account_id)
        .bind(model_id)
        .bind(Option::<String>::None)
        .bind(0.0)
        .bind(0.0)
        .bind(0.0)
        .bind(0.0)
        .bind(Option::<i64>::None)
        .bind(Option::<f64>::None)
        .bind(Option::<f64>::None)
        .bind(Option::<f64>::None)
        .bind(Option::<f64>::None)
        .bind(source)
        .bind(Option::<String>::None)
        .execute(pool)
        .await
        .context("写入 free 价目行")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_accounts_to_sources() {
        assert_eq!(source_for("https://opencode.ai/zen/go/v1", "go6"), Source::ZenGo);
        assert_eq!(source_for("https://opencode.ai/zen/v1", "zen"), Source::Zen);
        assert_eq!(source_for("https://api.deepseek.com/v1", "DeepSeek"), Source::DeepSeek);
        assert_eq!(source_for("https://openrouter.ai/api/v1", "openrouter"), Source::OpenRouter);
        assert_eq!(source_for("https://api.teamorouter.cn/v1", "teamorouter"), Source::TeamoRouter);
        assert_eq!(source_for("https://api.commandcode.ai/provider/v1", "command"), Source::Manual);
        assert_eq!(source_for("https://api.agnes-ai.cn/v1", "agnes"), Source::Manual);
        assert_eq!(source_for("http://pc.xiaokubao.space:8080/v1", "local"), Source::Free);
        assert_eq!(source_for("http://192.168.1.6:25001/v1", "Copilot"), Source::Free);
    }

    #[test]
    fn recognises_free_and_stealth_models() {
        assert!(is_known_free("stealth/space-bunny-alpha"));
        assert!(is_known_free("space-bunny-free"));
        assert!(is_known_free("omen-alpha"));
        assert!(is_known_free("big-pickle"));
        assert!(is_known_free("deepseek-v4-flash-free"));
        assert!(is_known_free("inclusionai/ling-3.0-flash-sante:free"));
        assert!(!is_known_free("gpt-6-astra"));
        assert!(!is_known_free("deepseek-v4.1-flash"));
    }

    /// 临时端到端烟测：对一份库副本跑全量刷新（`LLMUX_PRICE_DB=/tmp/.../llmux_db.db`）。
    /// 会走外网；只用于手动验证，CI 里无 env 即空转。
    #[tokio::test]
    async fn refresh_all_smoke_on_a_db_copy() {
        let Ok(path) = std::env::var("LLMUX_PRICE_DB") else {
            return;
        };
        let url = format!("sqlite://{path}");
        let pool = llmux_core::db::connect_sqlite(&url).await.unwrap();
        llmux_core::db::init_db(&pool).await.unwrap();
        let report = refresh_all(&pool).await.unwrap();
        println!("REPORT {}", serde_json::to_string(&report).unwrap());
        let rows: Vec<(i64, String, f64, Option<f64>, String)> = sqlx::query_as(
            "SELECT account_id, model_id, input_price, cache_read_price, source
             FROM upstream_prices ORDER BY account_id, model_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        for (acct, model, input, cr, src) in &rows {
            println!("  acct={acct} {model} in={input} cr={cr:?} src={src}");
        }
        assert!(!rows.is_empty(), "应写入若干 upstream_prices 行");
    }
}
