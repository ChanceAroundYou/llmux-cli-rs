//! 极小的 HTML 表格提取器 —— 够解析上游文档的 `<table>` 就行。
//!
//! 刻意不引 `scraper`/`regex`：这些页面是文档站（Docusaurus / 静态 HTML），
//! 结构简单，手写足够；解析失败靠上层「行数骤降即判失败」兜底。若哪天页面改成
//! 运行时 JS 渲染，这个提取器会拿到空表，届时再评估引依赖。

/// 一张表：`headers` 来自表头行（`<th>`），`rows` 是数据行。
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl Table {
    /// 表头里是否同时含这些关键字（大小写不敏感）。
    pub fn headers_contain(&self, needles: &[&str]) -> bool {
        needles.iter().all(|n| {
            self.headers
                .iter()
                .any(|h| h.to_lowercase().contains(&n.to_lowercase()))
        })
    }
}

/// 找出所有 `<table>`。
pub fn extract_tables(html: &str) -> Vec<Table> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find("<table") {
        let after = &rest[start..];
        let Some(end) = after.find("</table>") else {
            break;
        };
        out.push(parse_table(&after[..end]));
        rest = &after[end + "</table>".len()..];
    }
    out
}

fn parse_table(table_html: &str) -> Table {
    let mut header_row: Option<Vec<String>> = None;
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut rest = table_html;
    while let Some(s) = rest.find("<tr") {
        let after = &rest[s..];
        let e = after.find("</tr>").unwrap_or(after.len());
        let (is_header, cells) = cells_of_row(&after[..e]);
        if !cells.is_empty() {
            if is_header && header_row.is_none() {
                header_row = Some(cells);
            } else {
                rows.push(cells);
            }
        }
        if e >= after.len() {
            break;
        }
        rest = &after[e + "</tr>".len()..];
    }
    Table {
        headers: header_row.unwrap_or_default(),
        rows,
    }
}

/// 返回 (本行是否含 `<th>`，单元格文本)。
fn cells_of_row(row_html: &str) -> (bool, Vec<String>) {
    let is_header = row_html.contains("<th");
    // 把闭合标签换成哨兵再 split：每个片段以 `<td…>` / `<th…>` 开头。
    let normalized = row_html.replace("</td>", "\u{1f}").replace("</th>", "\u{1f}");
    let mut cells = Vec::new();
    for part in normalized.split('\u{1f}') {
        if !(part.contains("<td") || part.contains("<th")) {
            continue;
        }
        let content = match part.find('>') {
            Some(i) => &part[i + 1..],
            None => part,
        };
        cells.push(clean_text(content));
    }
    (is_header, cells)
}

/// 去标签 + 解实体 + 压空白。
pub fn clean_text(s: &str) -> String {
    decode_entities(&strip_tags(s))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// 去掉 `<...>`（含 `<br>`、`<code>` 等），标签之间补空格避免词粘连。
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&#x27;", "'")
        .replace("&le;", "≤")
        .replace("&ge;", "≥")
}

/// 解析单价：`$0.30` → `0.3`，`Free` → `0.0`，`-` / 空 → `None`。
pub fn parse_money(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() || t == "-" || t == "—" {
        return None;
    }
    if t.eq_ignore_ascii_case("free") {
        return Some(0.0);
    }
    // 去掉货币符号、千分位、以及 "Cache $0.0036" 这类前缀尾巴。
    let cleaned: String = t
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    cleaned.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_a_simple_table() {
        let html = r#"
        <h2>Pricing</h2>
        <table>
          <thead><tr><th>Model</th><th>Input</th><th>Output</th></tr></thead>
          <tbody>
            <tr><td>DeepSeek V4.1 Flash</td><td>$0.30</td><td>$1.20</td></tr>
            <tr><td>GLM-5.3</td><td>$1.40</td><td>$4.40</td></tr>
          </tbody>
        </table>
        <table><thead><tr><th>Other</th></tr></thead><tbody><tr><td>x</td></tr></tbody></table>
        "#;
        let tables = extract_tables(html);
        assert_eq!(tables.len(), 2);
        assert_eq!(tables[0].headers, vec!["Model", "Input", "Output"]);
        assert_eq!(tables[0].rows.len(), 2);
        assert_eq!(tables[0].rows[0][0], "DeepSeek V4.1 Flash");
        assert_eq!(tables[0].rows[0][1], "$0.30");
        assert!(tables[0].headers_contain(&["Input", "Output"]));
        assert!(!tables[1].headers_contain(&["Input", "Output"]));
    }

    #[test]
    fn strips_nested_tags_and_codes() {
        let html = r#"<table><tr><th>A</th></tr><tr><td>B <code>c</code><br>d</td></tr></table>"#;
        let t = &extract_tables(html)[0];
        assert_eq!(t.rows[0][0], "B c d");
    }

    #[test]
    fn parses_money_forms() {
        assert_eq!(parse_money("$0.30"), Some(0.3));
        assert_eq!(parse_money("$1,234.50"), Some(1234.5));
        assert_eq!(parse_money("Free"), Some(0.0));
        assert_eq!(parse_money("-"), None);
        assert_eq!(parse_money(""), None);
        assert_eq!(parse_money("Cache $0.0036"), Some(0.0036));
    }
}
