//! 本地 prompt 体积估算。
//!
//! 存在一类上游（第三方 Claude 中转）：响应里的 `usage` 与**我们发出去的那条
//! 请求**无关。2026-11-14 直连实测（rsrouter 192.168.10.106:3000）：
//!
//! | 请求 | 上游回报 prompt 侧合计 |
//! |---|---|
//! | `say hi`（6 字符） | 10,940 |
//! | 同一句 `say hi`，几分钟后 | 43,552 |
//! | 20000 字符 | 59,872 |
//! | 100000 字符（真实约 25k token） | 385,481 |
//!
//! 这些数字是上游自己那套 harness 的上下文，不是这条请求的大小。它对
//! `/v1/messages/count_tokens` 却能给出正确值（20000 → 5001），所以问题只
//! 出在响应体的 usage 上。
//!
//! 按它计量的客户端会立刻失准：Claude Code 的 auto-compact 阈值直接取
//! `input + cache_read + cache_creation`，于是 200k 窗口上刚压缩完就"又满了"，
//! 反复压缩直到 thrashing。`usage_estimate_accounts` 里登记过的账户，出站
//! 用法改成这里估出来的值。
//!
//! 估算口径：ASCII 按 4 字符/token，非 ASCII（中日韩等）按 1 字符/token。
//! 早先一律 `len/4`，对中文会低估约 3 倍 —— 对压缩阈值而言低估比高估危险
//! （客户端以为还有空间，实际已经撞上模型上限），所以非 ASCII 取保守值。
//! `/v1/messages/count_tokens` 也走这里，两边口径必须一致。

use serde_json::Value;

/// 一段文本的 token 估算。空串记 0，由调用方决定最小值。
pub fn estimate_text_tokens(text: &str) -> u64 {
    let mut ascii = 0usize;
    let mut wide = 0usize;
    for ch in text.chars() {
        if ch.is_ascii() {
            ascii += 1;
        } else {
            wide += 1;
        }
    }
    ((ascii as u64) + 3) / 4 + wide as u64
}

/// Anthropic Messages 请求（`system` / `messages` / `tools`）的 prompt 估算。
pub fn estimate_anthropic_tokens(body: &Value) -> u64 {
    let mut tokens: u64 = 0;
    if let Some(messages) = body["messages"].as_array() {
        for message in messages {
            tokens += anthropic_content_tokens(&message["content"]);
        }
    }
    tokens += anthropic_content_tokens(&body["system"]);
    if let Some(tools) = body["tools"].as_array() {
        tokens += estimate_text_tokens(&serde_json::to_string(tools).unwrap_or_default());
    }
    tokens.max(1)
}

/// 一张图片按这个上限计。Anthropic 的长边会缩到 1568px，token 上限约
/// `1568×1568/750 ≈ 3.3k`，实拍截图多在 1–1.6k。取 1600 是**故意偏高**：
/// 客户端拿这个数算还剩多少空间，低估会让它撞上模型真实上限才反应过来。
const IMAGE_TOKENS: u64 = 1_600;

fn anthropic_content_tokens(content: &Value) -> u64 {
    match content {
        Value::String(text) => estimate_text_tokens(text),
        Value::Array(blocks) => blocks.iter().map(anthropic_block_tokens).sum(),
        _ => 0,
    }
}

fn anthropic_block_tokens(block: &Value) -> u64 {
    match block["type"].as_str() {
        // 图片与文档的正文不是文本，按上限记；漏掉它们会显著低估
        // （Claude Code 贴一张截图就少算一千多 token）。
        // 刻意**不**按 base64 长度折算 —— 那是字节数，与 token 不成比例。
        Some("image") | Some("document") => IMAGE_TOKENS,
        _ => {
            let text = block["text"].as_str().map(estimate_text_tokens).unwrap_or(0);
            // 历史里的 thinking 块会被重新发回上游，别当成 0。
            let thinking = block["thinking"]
                .as_str()
                .map(estimate_text_tokens)
                .unwrap_or(0);
            // tool_use 的 input 是结构化参数，序列化后按同一口径估。
            // `null.to_string()` 是 `"null"`，会给每个没有 input 的块白加 1 token。
            let input = if block["input"].is_null() {
                0
            } else {
                estimate_text_tokens(&block["input"].to_string())
            };
            text + thinking + input + anthropic_content_tokens(&block["content"])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ascii_counts_four_chars_per_token() {
        assert_eq!(estimate_text_tokens(&"x".repeat(20_000)), 5_000);
    }

    #[test]
    fn cjk_counts_one_token_per_char() {
        // 中文在 UTF-8 里是 3 字节；按字节数/4 会低估到 1/3。
        assert_eq!(estimate_text_tokens("你好世界"), 4);
        assert!(estimate_text_tokens("你好世界") > estimate_text_tokens("abcd"));
    }

    #[test]
    fn empty_message_body_still_reports_one() {
        assert_eq!(estimate_anthropic_tokens(&json!({ "messages": [] })), 1);
    }

    #[test]
    fn counts_system_messages_and_tools() {
        let body = json!({
            "model": "x",
            "system": "abcdabcdabcdabcd",
            "tools": [{ "name": "t", "description": "desc" }],
            "messages": [
                { "role": "user", "content": "hello world foo" },
                { "role": "assistant", "content": [
                    { "type": "tool_use", "id": "1", "name": "t", "input": { "a": 1 } }
                ] }
            ]
        });
        // 16 字符 system = 4，工具定义 + 消息正文都非零，总量必须明显大于 4。
        assert!(estimate_anthropic_tokens(&body) > 10);
    }

    #[test]
    fn images_and_thinking_are_not_counted_as_zero() {
        let body = json!({
            "messages": [{
                "role": "assistant",
                "content": [
                    { "type": "thinking", "thinking": "x".repeat(4_000), "signature": "sig" },
                    { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "AAAA" } }
                ]
            }]
        });
        // 4000 字符 thinking = 1000，图片按上限 1600；只按 text 字段取会得到 0。
        assert_eq!(estimate_anthropic_tokens(&body), 1_000 + IMAGE_TOKENS);
    }

    #[test]
    fn tool_result_text_is_counted_recursively() {
        let body = json!({
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "1",
                    "content": [{ "type": "text", "text": "x".repeat(8_000) }]
                }]
            }]
        });
        // tool_result 的正文是嵌套 content，读 `text` 直接取会漏掉这 2000 token。
        assert_eq!(estimate_anthropic_tokens(&body), 2_000);
    }
}