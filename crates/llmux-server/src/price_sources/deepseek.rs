//! DeepSeek 官方适配器（`api-docs.deepseek.com/quick_start/pricing`）。
//!
//! 这张表是「竖排」的：行形状在 5 / 4 / 3 个单元格之间跳（rowspan 折叠），
//! **最后两列**恒为两个模型的价：`deepseek-flash` 与 `deepseek-v4-pro`。
//! 行的前几列是标签：`1M INPUT TOKENS (CACHE HIT/MISS)`、`1M OUTPUT TOKENS`，
//! 以及 `OFF-PEAK` / `PEAK`。
//!
//! 口径「DeepSeek 系一律取峰价」：同一分项优先取 PEAK 行，没有才退回 OFF-PEAK。
//! 该表没有 cache write 价。

use super::html::{extract_tables, parse_money};
use super::FetchedPrice;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    CacheHit,
    CacheMiss,
    Output,
}

#[derive(Debug, Default, Clone, Copy)]
struct Acc {
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    input_peak: bool,
    output_peak: bool,
    cache_peak: bool,
}

impl Acc {
    fn set(&mut self, section: Section, v: f64, peak: bool) {
        let (slot, flag) = match section {
            Section::CacheHit => (&mut self.cache_read, &mut self.cache_peak),
            Section::CacheMiss => (&mut self.input, &mut self.input_peak),
            Section::Output => (&mut self.output, &mut self.output_peak),
        };
        // 峰价优先；已是峰价则不再被谷价覆盖。
        if slot.is_none() || (peak && !*flag) {
            *slot = Some(v);
            *flag = peak;
        }
    }
}

pub fn parse(html: &str) -> Vec<FetchedPrice> {
    let tables = extract_tables(html);
    let Some(table) = tables
        .iter()
        .find(|t| t.rows.iter().any(|r| r.iter().any(|c| c.contains("INPUT TOKENS"))))
    else {
        return Vec::new();
    };

    // 模型名列：含 "MODEL" 的那行，最后两格是模型 id。
    let ids: Vec<String> = table
        .rows
        .iter()
        .find(|r| r.iter().any(|c| c.eq_ignore_ascii_case("MODEL")))
        .map(|r| {
            r.iter()
                .rev()
                .take(2)
                .rev()
                .map(|c| strip_footnote(c))
                .collect()
        })
        .unwrap_or_default();
    if ids.len() != 2 {
        return Vec::new();
    }

    let mut accs = [Acc::default(); 2];
    let mut section: Option<Section> = None;
    for row in &table.rows {
        if row.len() < 3 {
            continue;
        }
        let marker = row[..row.len() - 2].join(" ");
        let upper = marker.to_uppercase();
        if upper.contains("TOKENS") {
            section = if upper.contains("CACHE HIT") {
                Some(Section::CacheHit)
            } else if upper.contains("CACHE MISS") {
                Some(Section::CacheMiss)
            } else if upper.contains("OUTPUT") {
                Some(Section::Output)
            } else {
                section
            };
        }
        let Some(sec) = section else { continue };
        let peak = upper.contains("PEAK") && !upper.contains("OFF");
        for (i, acc) in accs.iter_mut().enumerate() {
            if let Some(v) = dollar(&row[row.len() - 2 + i]) {
                acc.set(sec, v, peak);
            }
        }
    }

    ids.iter()
        .zip(accs.iter())
        .filter_map(|(id, a)| {
            let input = a.input?;
            let output = a.output?;
            Some(FetchedPrice {
                model_id: id.clone(),
                vendor: Some("deepseek".to_string()),
                input,
                output,
                cache_read: a.cache_read,
                cache_write: None,
                long_context_threshold: None,
                long_input: None,
                long_output: None,
                long_cache_read: None,
                long_cache_write: None,
            })
        })
        .collect()
}

/// 只认带 `$` 的单元格，避免把 URL / `✓` / 脚注当成价格。
fn dollar(s: &str) -> Option<f64> {
    if !s.contains('$') {
        return None;
    }
    parse_money(s)
}

/// `deepseek-flash(1)` → `deepseek-flash`。
fn strip_footnote(s: &str) -> String {
    s.split('(').next().unwrap_or(s).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 复刻真页面的行形状：5 / 4 / 3 个单元格。
    const PAGE: &str = r#"
    <table>
      <tr><td colspan="3">MODEL</td><td>deepseek-flash<sup>(1)</sup></td><td>deepseek-v4-pro</td></tr>
      <tr><td colspan="3">BASE URL (OpenAI Format)</td><td colspan="2"><a href="https://api.deepseek.com">https://api.deepseek.com</a></td></tr>
      <tr><td rowspan="7">PRICING<sup>(2)</sup></td><td>1M INPUT TOKENS (CACHE HIT)</td><td>OFF-PEAK</td><td>$0.003</td><td>$0.022</td></tr>
      <tr><td>PEAK</td><td>$0.006</td><td>$0.044</td></tr>
      <tr><td>1M INPUT TOKENS (CACHE MISS)</td><td>OFF-PEAK</td><td>$0.15</td><td>$0.66</td></tr>
      <tr><td>PEAK</td><td>$0.3</td><td>$1.32</td></tr>
      <tr><td>1M OUTPUT TOKENS</td><td>OFF-PEAK</td><td>$0.6</td><td>$1.98</td></tr>
      <tr><td>PEAK</td><td>$1.2</td><td>$3.96</td></tr>
    </table>"#;

    #[test]
    fn extracts_peak_prices_for_both_models() {
        let v = parse(PAGE);
        assert_eq!(v.len(), 2);
        let flash = v.iter().find(|p| p.model_id == "deepseek-flash").unwrap();
        assert_eq!(flash.input, 0.3, "cache miss 取峰价");
        assert_eq!(flash.output, 1.2);
        assert_eq!(flash.cache_read, Some(0.006));
        let pro = v.iter().find(|p| p.model_id == "deepseek-v4-pro").unwrap();
        assert_eq!(pro.input, 1.32);
        assert_eq!(pro.output, 3.96);
        assert_eq!(pro.cache_read, Some(0.044));
    }

    #[test]
    fn ignores_rows_before_any_section_and_non_dollar_cells() {
        // BASE URL 行不能让 URL 里的数字变成价格。
        let v = parse(PAGE);
        assert!(v.iter().all(|p| p.input < 100.0));
    }

    #[test]
    fn returns_empty_without_the_table() {
        assert!(parse("<html><body>nope</body></html>").is_empty());
    }

    /// 临时烟测：对真实抓下来的页面跑一遍（`LLMUX_DS_HTML=/tmp/raw-ds.html`）。
    #[test]
    fn real_page_smoke() {
        let Ok(path) = std::env::var("LLMUX_DS_HTML") else {
            return;
        };
        let html = std::fs::read_to_string(&path).expect("read page");
        let v = parse(&html);
        println!("parsed {} models from {path}", v.len());
        for p in &v {
            println!(
                "  {} in={} out={} cr={:?}",
                p.model_id, p.input, p.output, p.cache_read
            );
        }
        assert!(v.len() >= 2, "only parsed {}", v.len());
    }
}
