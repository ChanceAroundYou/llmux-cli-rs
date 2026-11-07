//! Codex `config.toml` 的原地编辑：只改 llmux 负责的字段，其余原样保留。
//!
//! 用 toml_edit 按 section 精确定位（而不是字符串查找）。此前这里是
//! `toml.find("model = \"")` 那一套，实测有三条会把用户的 Codex 配坏的坏行为，
//! 每条都钉在 `tests/codex_toml_contract.rs` 里：
//!   - `model` 的查找会先命中 `review_model = "…"`（后者含 `model = "` 子串）
//!   - 用户自己写的其它 `[model_providers.*]` 段里的同名键会被改成 llmux 的值
//!   - 缺失的顶层键被盲追加到文件末尾，而末尾正好是新加的 `[model_providers.llmux]`
//!     段 —— `model` / `model_provider` 落进 provider 段里，Codex 读顶层读不到
//!
//! 两个刻意的取舍：
//!   - 解析失败（用户手写的 TOML 有语法错误）返回 `Err`，由调用方报错给用户 ——
//!     绝不吞掉错误后拿一个空文档覆盖原文件。
//!   - 段落的**形状跟着用户走**：用户把 `model_providers` 写成 inline table
//!     （`model_providers = { … }`）就沿用他的写法，不强行改成段落；只有文件里
//!     本来没有时才建新段。形状认不出来（`model_providers = 42`）也回 `Err`，
//!     不猜、不覆盖。
//!
//! ⚠️ 别退回 `doc["model_providers"]["llmux"]["base_url"] = value(..)` 那种链式写法：
//! 实测 toml_edit 对链式创建出来的是 **inline table**（`model_providers = { llmux = { … } }`），
//! 语义对但形状不是 Codex 用户期待的段落，而且 `Item::as_table()` 会是 None。

use std::fmt;

use toml_edit::{value, DocumentMut, InlineTable, Item, Table, TableLike, Value};

const PROVIDER_KEY: &str = "model_providers";
const LLMUX_KEY: &str = "llmux";

#[derive(Debug)]
pub enum CodexTomlError {
    /// 用户现有的 config.toml 语法本身有问题，改不了。
    Parse(toml_edit::TomlError),
    /// 键在，但不是我们能改的形状（例如 `model_providers = 42`）。
    UnexpectedShape(String),
}

impl fmt::Display for CodexTomlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "现有 config.toml 语法错误：{e}"),
            Self::UnexpectedShape(path) => write!(
                f,
                "`{path}` 存在但不是一个 TOML 段落，llmux 无法在不破坏它的前提下改写"
            ),
        }
    }
}

impl std::error::Error for CodexTomlError {}

/// 基于现有 TOML 文本合并 llmux 负责的字段，返回新的 TOML 文本。
pub fn patch_codex_toml(
    existing: &str,
    model: &str,
    review_model: &str,
    api_base_url: &str,
    wire_api: &str,
    context_window: Option<u64>,
    auto_compact_limit: Option<u64>,
) -> Result<String, CodexTomlError> {
    let mut doc: DocumentMut = existing.parse().map_err(CodexTomlError::Parse)?;

    // 顶层键（Codex 读的就是顶层这几个）。toml_edit 会把它们落在文档根的键值区，
    // 不会掉进后面某个段里 —— 这正是字符串拼接那套做不到的。
    doc["model_provider"] = value("llmux");
    doc["model"] = value(model);
    doc["review_model"] = value(review_model);
    if let Some(v) = context_window {
        doc["model_context_window"] = value(v as i64);
    }
    if let Some(v) = auto_compact_limit {
        doc["model_auto_compact_token_limit"] = value(v as i64);
    }

    let section = provider_section(&mut doc)?;
    set_provider_keys(section, api_base_url, wire_api);

    Ok(doc.to_string())
}

/// 取得（必要时创建）llmux 自己那段 provider 配置。
///
/// 三条路径：已有 `[model_providers]` 段落 → 在里面确保 `[model_providers.llmux]`；
/// 已有 inline table 写法 → 沿用；都没有 → 建标准段落，且把父段设成 implicit，
/// 这样渲染出来只有 `[model_providers.llmux]` 一个段头，不会多一个空 `[model_providers]`。
fn provider_section(doc: &mut DocumentMut) -> Result<&mut dyn TableLike, CodexTomlError> {
    if doc.get(PROVIDER_KEY).is_none() {
        let mut parent = Table::new();
        parent.set_implicit(true);
        doc[PROVIDER_KEY] = Item::Table(parent);
    }

    match &mut doc[PROVIDER_KEY] {
        Item::Table(parent) => {
            let child = ensure_table_child(parent, LLMUX_KEY)?;
            Ok(child as &mut dyn TableLike)
        }
        Item::Value(v) if v.is_inline_table() => {
            let it = v.as_inline_table_mut().unwrap();
            let child = ensure_inline_child(it, LLMUX_KEY)?;
            Ok(child as &mut dyn TableLike)
        }
        // `model_providers = 42` 这类：形状不对，停下来说清楚，不改写。
        _ => Err(CodexTomlError::UnexpectedShape(PROVIDER_KEY.to_string())),
    }
}

fn ensure_table_child<'a>(
    parent: &'a mut Table,
    name: &str,
) -> Result<&'a mut Table, CodexTomlError> {
    if !parent.contains_key(name) {
        parent[name] = Item::Table(Table::new());
    }
    parent[name]
        .as_table_mut()
        .ok_or_else(|| CodexTomlError::UnexpectedShape(format!("{PROVIDER_KEY}.{name}")))
}

fn ensure_inline_child<'a>(
    parent: &'a mut InlineTable,
    name: &str,
) -> Result<&'a mut InlineTable, CodexTomlError> {
    if parent.get(name).is_none() {
        parent.insert(name, Value::from(InlineTable::new()));
    }
    parent[name]
        .as_inline_table_mut()
        .ok_or_else(|| CodexTomlError::UnexpectedShape(format!("{PROVIDER_KEY}.{name}")))
}

/// llmux 负责的 provider 字段 —— 只这几个，段里其它键（用户的鉴权配置等）不动。
///
/// ⚠️ 走 `dyn TableLike` 时 `insert` 收的是 `Item`（`InlineTable` 自己的同名方法收 `Value`），
/// 两者签名不同 —— 这里必须显式包 `Item::Value`。
fn set_provider_keys(section: &mut dyn TableLike, api_base_url: &str, wire_api: &str) {
    section.insert("name", Item::Value(Value::from("llmux")));
    section.insert("base_url", Item::Value(Value::from(api_base_url)));
    section.insert("wire_api", Item::Value(Value::from(wire_api)));
    section.insert("requires_openai_auth", Item::Value(Value::from(true)));
}
