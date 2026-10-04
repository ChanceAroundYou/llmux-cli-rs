//! OpenCode Zen / Go 适配器。
//!
//! 两个文档页结构相同（Go 的定价表多一列 Monthly limit）：定价表用 display 名
//! （"DeepSeek V4.1 Flash"），同一页的 Endpoints 表给 display → model id
//! （"deepseek-v4.1-flash"）。两表按 display join，拿不到就 slugify 兜底。
//!
//! 长上下文分档：同一模型出现两行 `(≤ 272K tokens)` / `(> 272K tokens)`，
//! 合并成一条 `FetchedPrice`，短档进 input/output，长档进 long_*。

use std::collections::HashMap;

use super::html::{extract_tables, parse_money, Table};
use super::FetchedPrice;

#[derive(Debug, Clone)]
struct Row {
    input: f64,
    output: f64,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
}

#[derive(Debug, Default, Clone)]
struct Group {
    base: String,
    short: Option<Row>,
    long: Option<Row>,
    threshold: Option<i64>,
}

/// 定价行末尾括号里的分档标签。
#[derive(Debug, Clone, Copy, PartialEq)]
enum Tier {
    Plain,
    /// 长上下文短档：`(≤ 272K tokens)`
    Short(i64),
    /// 长上下文长档：`(> 272K tokens)`
    Long(i64),
    /// `(Off-Peak)` —— DeepSeek 系谷价，按口径丢弃（除非没有峰价行）。
    OffPeak,
    /// `(Peak)` —— DeepSeek 系峰价，采用。
    Peak,
}

pub fn parse(html: &str) -> Vec<FetchedPrice> {
    let tables = extract_tables(html);
    let id_by_display = tables
        .iter()
        .find(|t| t.headers_contain(&["Model", "Model ID"]))
        .map(endpoints_map)
        .unwrap_or_default();

    let Some(pricing) = tables
        .iter()
        .find(|t| t.headers_contain(&["Input", "Output", "Cached Read"]))
    else {
        return Vec::new();
    };

    let mut groups: Vec<Group> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for row in &pricing.rows {
        let Some(display) = row.first() else { continue };
        if display.is_empty() {
            continue;
        }
        let cell = |i: usize| row.get(i).map(String::as_str).unwrap_or("");
        let (Some(input), Some(output)) = (parse_money(cell(1)), parse_money(cell(2))) else {
            continue;
        };
        let (base, tier) = classify_tier(display);
        let gi = *index.entry(base.clone()).or_insert_with(|| {
            groups.push(Group {
                base: base.clone(),
                ..Default::default()
            });
            groups.len() - 1
        });
        let r = Row {
            input,
            output,
            cache_read: parse_money(cell(3)),
            cache_write: parse_money(cell(4)),
        };
        let g = &mut groups[gi];
        match tier {
            Tier::Plain => g.short = Some(r),
            Tier::Short(thr) => {
                g.short = Some(r);
                g.threshold = Some(thr);
            }
            Tier::Long(thr) => {
                g.long = Some(r);
                g.threshold = g.threshold.or(Some(thr));
            }
            // 口径「DeepSeek 系一律取峰价」：谷价行只在没有峰价时兜底。
            Tier::OffPeak => {
                if g.short.is_none() {
                    g.short = Some(r);
                }
            }
            Tier::Peak => g.short = Some(r),
        }
    }

    groups
        .into_iter()
        .filter_map(|g| {
            let short = g.short.clone().or_else(|| g.long.clone())?;
            let model_id = id_by_display
                .get(&g.base.to_lowercase())
                .cloned()
                .unwrap_or_else(|| slugify(&g.base));
            Some(FetchedPrice {
                model_id,
                vendor: None,
                input: short.input,
                output: short.output,
                cache_read: short.cache_read,
                cache_write: short.cache_write,
                long_context_threshold: if g.long.is_some() { g.threshold } else { None },
                long_input: g.long.as_ref().map(|r| r.input),
                long_output: g.long.as_ref().map(|r| r.output),
                long_cache_read: g.long.as_ref().and_then(|r| r.cache_read),
                long_cache_write: g.long.as_ref().and_then(|r| r.cache_write),
            })
        })
        .collect()
}

fn endpoints_map(t: &Table) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for row in &t.rows {
        if row.len() >= 2 {
            let display = row[0].to_lowercase();
            let id = row[1].trim();
            if !display.is_empty() && !id.is_empty() && !id.contains(' ') {
                m.insert(display, id.to_string());
            }
        }
    }
    m
}

/// 把 display 名拆成「去掉括号标签的名字 + 分档」。
fn classify_tier(display: &str) -> (String, Tier) {
    let Some(open) = display.find('(') else {
        return (display.trim().to_string(), Tier::Plain);
    };
    let base = display[..open].trim().to_string();
    let inner = &display[open..];
    let lower = inner.to_lowercase();
    if lower.contains("peak") {
        return (
            base,
            if lower.contains("off") {
                Tier::OffPeak
            } else {
                Tier::Peak
            },
        );
    }
    let is_long = inner.contains('>');
    let is_short = inner.contains('<') || inner.contains('≤');
    let digits: String = inner.chars().filter(|c| c.is_ascii_digit()).collect();
    if !(is_long || is_short) || digits.is_empty() {
        return (base, Tier::Plain);
    }
    let n: i64 = digits.parse().unwrap_or(0);
    let threshold = if lower.contains('k') {
        n * 1_000
    } else if lower.contains('m') {
        n * 1_000_000
    } else {
        n
    };
    (
        base,
        if is_long {
            Tier::Long(threshold)
        } else {
            Tier::Short(threshold)
        },
    )
}

fn slugify(display: &str) -> String {
    display.trim().to_lowercase().replace(' ', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(pricing_rows: &str) -> String {
        format!(
            r#"
        <table><thead><tr>
          <th>Model</th><th>Model ID</th><th>Endpoint</th><th>AI SDK Package</th>
        </tr></thead><tbody>
          <tr><td>DeepSeek V4.1 Flash</td><td>deepseek-v4.1-flash</td><td><code>x</code></td><td>y</td></tr>
          <tr><td>Claude Sonnet 4.5</td><td>claude-sonnet-4-5</td><td><code>x</code></td><td>y</td></tr>
        </tbody></table>
        <table><thead><tr>
          <th>Model</th><th>Input</th><th>Output</th><th>Cached Read</th><th>Cached Write</th>
        </tr></thead><tbody>{pricing_rows}</tbody></table>
        "#
        )
    }

    #[test]
    fn parses_prices_and_joins_model_ids() {
        let html = page(
            r#"
            <tr><td>DeepSeek V4.1 Flash</td><td>$0.30</td><td>$1.20</td><td>$0.006</td><td>-</td></tr>
            <tr><td>Space Bunny Free</td><td>Free</td><td>Free</td><td>Free</td><td>-</td></tr>
            "#,
        );
        let v = parse(&html);
        let d = v
            .iter()
            .find(|p| p.model_id == "deepseek-v4.1-flash")
            .expect("deepseek row");
        assert_eq!(d.input, 0.30);
        assert_eq!(d.output, 1.20);
        assert_eq!(d.cache_read, Some(0.006));
        assert_eq!(d.cache_write, None);
        // 端点表里没有的免费模型走 slugify 兜底。
        let free = v.iter().find(|p| p.model_id == "space-bunny-free").unwrap();
        assert_eq!(free.input, 0.0);
    }

    #[test]
    fn merges_long_context_tiers() {
        let html = page(
            r#"
            <tr><td>Claude Sonnet 4.5 (≤ 200K tokens)</td><td>$3.00</td><td>$15.00</td><td>$0.30</td><td>$3.75</td></tr>
            <tr><td>Claude Sonnet 4.5 (&gt; 200K tokens)</td><td>$6.00</td><td>$22.50</td><td>$0.60</td><td>$7.50</td></tr>
            "#,
        );
        let v = parse(&html);
        let c = v
            .iter()
            .find(|p| p.model_id == "claude-sonnet-4-5")
            .expect("claude row");
        assert_eq!(c.input, 3.0);
        assert_eq!(c.long_context_threshold, Some(200_000));
        assert_eq!(c.long_input, Some(6.0));
        assert_eq!(c.long_output, Some(22.5));
        assert_eq!(c.long_cache_write, Some(7.5));
    }

    #[test]
    fn prefers_peak_over_off_peak_regardless_of_row_order() {
        let html = page(
            r#"
            <tr><td>DeepSeek V4.1 Flash (Peak)</td><td>$0.30</td><td>$1.20</td><td>$0.006</td><td>-</td></tr>
            <tr><td>DeepSeek V4.1 Flash (Off-Peak)</td><td>$0.15</td><td>$0.60</td><td>$0.003</td><td>-</td></tr>
            "#,
        );
        let v = parse(&html);
        let d = v
            .iter()
            .find(|p| p.model_id == "deepseek-v4.1-flash")
            .unwrap();
        assert_eq!(d.input, 0.30, "峰价必须赢，且与行序无关");
        assert_eq!(d.output, 1.20);
        assert_eq!(d.cache_read, Some(0.006));
    }

    #[test]
    fn returns_empty_when_no_pricing_table() {
        assert!(parse("<html><body><p>nope</p></body></html>").is_empty());
    }

    /// 临时烟测：对真实抓下来的页面跑一遍（`LLMUX_ZEN_HTML=/tmp/raw-xxx.html`）。
    #[test]
    fn real_page_smoke() {
        let Ok(path) = std::env::var("LLMUX_ZEN_HTML") else {
            return;
        };
        let html = std::fs::read_to_string(&path).expect("read page");
        let v = parse(&html);
        println!("parsed {} models from {path}", v.len());
        for p in v.iter().filter(|p| p.model_id.contains("deepseek")) {
            println!(
                "  {} in={} out={} cr={:?} thr={:?} long_in={:?}",
                p.model_id, p.input, p.output, p.cache_read, p.long_context_threshold, p.long_input
            );
        }
        assert!(v.len() > 20, "only parsed {}", v.len());
    }
}
