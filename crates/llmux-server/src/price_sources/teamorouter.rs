//! TeamoRouter 适配器（`teamorouter.cn` 首页的 live pricing 表）。
//!
//! 行是 `<tr data-vendor="OpenAI">`，单元格带 class：`off` 是官方标价、`tr` 是
//! TeamoRouter 的**折扣价**（要的就是这个）。每行 8 格：
//! `[model, ctx, off-in, off-out, tr-in, tr-out, rts, sla]`，缓存价塞在输入格的
//! `<small class="cache" data-en="Cache $0.05">` 里。
//!
//! 通用 `html::Table` 会丢掉 class 属性，所以这里自己扫一遍带 class 的 `<td>`。
//! 页面只列精选模型，抓不到的模型保持无价行 → 计 0 并计入未匹配。

use super::html::{clean_text, parse_money};
use super::FetchedPrice;

pub fn parse(html: &str) -> Vec<FetchedPrice> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(s) = rest.find("<tr") {
        let after = &rest[s..];
        let Some(e) = after.find("</tr>") else {
            break;
        };
        let row = &after[..e];
        rest = &after[e + "</tr>".len()..];

        let Some(display) = ent_name(row) else { continue };
        let tr_cells: Vec<String> = td_cells(row)
            .into_iter()
            .filter(|(class, _)| class.split_whitespace().any(|c| c == "tr"))
            .map(|(_, body)| body)
            .collect();
        if tr_cells.len() < 2 {
            continue;
        }
        let Some(input) = first_dollar(&tr_cells[0]).and_then(|d| parse_money(&d)) else {
            continue;
        };
        let Some(output) = first_dollar(&tr_cells[1]).and_then(|d| parse_money(&d)) else {
            continue;
        };
        // `row` 从 `<tr` 开始，`data-vendor` 就在开头这个标签上。
        let vendor = attr_value(row, "data-vendor");
        out.push(FetchedPrice {
            model_id: display.to_lowercase().replace(' ', "-"),
            vendor,
            input,
            output,
            cache_read: cache_of(&tr_cells[0]),
            cache_write: None,
            long_context_threshold: None,
            long_input: None,
            long_output: None,
            long_cache_read: None,
            long_cache_write: None,
        });
    }
    out
}

fn td_cells(row: &str) -> Vec<(String, String)> {
    let mut cells = Vec::new();
    let mut rest = row;
    while let Some(p) = rest.find("<td") {
        let after = &rest[p..];
        let Some(gt) = after.find('>') else { break };
        let attrs = &after[..gt];
        let body_start = gt + 1;
        let Some(end) = after[body_start..].find("</td>") else {
            break;
        };
        cells.push((
            attr_value(attrs, "class").unwrap_or_default(),
            after[body_start..body_start + end].to_string(),
        ));
        rest = &after[body_start + end + "</td>".len()..];
    }
    cells
}

fn attr_value(tag: &str, name: &str) -> Option<String> {
    let key = format!("{name}=\"");
    let i = tag.find(&key)?;
    let start = i + key.len();
    let end = tag[start..].find('"')?;
    Some(tag[start..start + end].to_string())
}

/// 取 `<span class="ent-name">GPT-5.6 Sol</span>` 里的 display 名。
fn ent_name(row: &str) -> Option<String> {
    let i = row.find("ent-name")?;
    let after = &row[i..];
    let gt = after.find('>')?;
    let end = after[gt + 1..].find('<')?;
    let s = clean_text(&after[gt + 1..gt + 1 + end]);
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// 取字符串里第一个 `$1.23` 形式的金额（截到非数字处），避免把
/// `$0.53 90% off Cache $0.053` 的数字全糊在一起。
fn first_dollar(s: &str) -> Option<String> {
    let i = s.find('$')?;
    let tail = &s[i..];
    let end = tail[1..]
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ','))
        .map(|x| x + 1)
        .unwrap_or(tail.len());
    Some(tail[..end].to_string())
}

fn cache_of(cell: &str) -> Option<f64> {
    if let Some(i) = cell.find("data-en=\"") {
        let start = i + "data-en=\"".len();
        if let Some(end) = cell[start..].find('"') {
            if let Some(d) = first_dollar(&cell[start..start + end]) {
                return parse_money(&d);
            }
        }
    }
    if let Some(i) = cell.find("Cache") {
        if let Some(d) = first_dollar(&cell[i..]) {
            return parse_money(&d);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"
    <table><tbody>
      <tr data-vendor="OpenAI"><td class="model"><span class="ent"><span class="ent-logo">O</span><span class="ent-txt"><span class="ent-name">GPT-5.6 Sol</span><span class="ent-sub">OpenAI</span></span></span></td><td class="ctx">1M</td><td class="off">$5.00<small class="cache" data-en="Cache $0.50">Cache $0.50</small></td><td class="off">$30.00</td><td class="tr">$0.53<small class="cache" data-en="Cache $0.053">Cache $0.053</small></td><td class="tr">$3.18</td><td class="rts"></td><td class="sla">—</td></tr>
      <tr data-vendor="DeepSeek"><td class="model"><span class="ent"><span class="ent-name">DeepSeek V4 Pro</span><span class="ent-sub">DeepSeek</span></span></td><td class="ctx">1M</td><td class="off">$0.435<small class="cache" data-en="Cache $0.0036">Cache $0.0036</small></td><td class="off">$0.87</td><td class="tr">$0.435<small class="cache" data-en="Cache $0.0036">Cache $0.0036</small></td><td class="tr">$0.87</td><td class="rts"></td><td class="sla">—</td></tr>
    </tbody></table>"#;

    #[test]
    fn takes_the_discounted_tr_price_and_cache() {
        let v = parse(PAGE);
        assert_eq!(v.len(), 2);
        let gpt = v.iter().find(|p| p.model_id == "gpt-5.6-sol").unwrap();
        assert_eq!(gpt.input, 0.53, "要折扣价不是标价 $5.00");
        assert_eq!(gpt.output, 3.18);
        assert_eq!(gpt.cache_read, Some(0.053));
        assert_eq!(gpt.vendor.as_deref(), Some("OpenAI"));

        let ds = v.iter().find(|p| p.model_id == "deepseek-v4-pro").unwrap();
        assert_eq!(ds.input, 0.435);
        assert_eq!(ds.cache_read, Some(0.0036));
    }

    #[test]
    fn returns_empty_without_pricing_rows() {
        assert!(parse("<html><body><p>nope</p></body></html>").is_empty());
    }

    /// 临时烟测：对真实抓下来的页面跑一遍（`LLMUX_TR_HTML=/tmp/raw-....html`）。
    #[test]
    fn real_page_smoke() {
        let Ok(path) = std::env::var("LLMUX_TR_HTML") else {
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
        assert!(!v.is_empty(), "应解析出若干精选模型");
    }
}
