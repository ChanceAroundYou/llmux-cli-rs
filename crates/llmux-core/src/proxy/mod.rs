pub mod anthropic_openai;
pub mod openai_anthropic;
pub mod responses;

use crate::adapters::{join_upstream_url, Account, ProviderRequest};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnthropicUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub cache_creation_input_tokens: i64,
}

pub fn build_anthropic_target_url(provider_base_url: &str) -> String {
    join_upstream_url(provider_base_url, "v1/messages")
}

pub fn build_anthropic_passthrough_request(
    original_body: &Value,
    account: &Account,
    provider_base_url: &str,
    resolved_model: &str,
    anthropic_beta: Option<&str>,
) -> anyhow::Result<ProviderRequest> {
    let mut patched = original_body
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Anthropic passthrough body must be an object"))?;
    patched.insert("model".to_string(), json!(resolved_model));

    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    headers.insert("x-api-key".to_string(), account.api_key.clone());
    headers.insert("anthropic-version".to_string(), "2023-06-01".to_string());
    if let Some(beta) = anthropic_beta {
        headers.insert("anthropic-beta".to_string(), beta.to_string());
    }

    Ok(ProviderRequest {
        method: "POST".to_string(),
        url: build_anthropic_target_url(provider_base_url),
        headers,
        body: Value::Object(patched),
        // 目标是 Anthropic Messages，没有 reasoning_effort 枚举。
        effort: Default::default(),
        // 同上：这条路径的 body 已定稿，max_tokens 收敛在 adapters 侧完成。
        max_tokens: Default::default(),
        first_byte_timeout_secs: None,
    })
}

pub fn extract_anthropic_usage_from_json(data: &Value) -> AnthropicUsage {
    let usage = &data["usage"];
    AnthropicUsage {
        input_tokens: usage["input_tokens"].as_i64().unwrap_or_default(),
        output_tokens: usage["output_tokens"].as_i64().unwrap_or_default(),
        cache_read_input_tokens: usage["cache_read_input_tokens"]
            .as_i64()
            .unwrap_or_default(),
        cache_creation_input_tokens: usage["cache_creation_input_tokens"]
            .as_i64()
            .unwrap_or_default(),
    }
}

/// OpenAI chat 请求体清洗，修两类会被严格上游按 schema 拒收的畸形字段
/// （400 → 网关 502），同一个 bug 的两个面：
///
/// 1. assistant 消息上的 `tool_calls: []` 空数组被严格上游（DeepSeek、Console Go
///    等）以 minLength 1 拒绝。空数组语义等价于"无工具调用"，直接删字段对任何
///    上游都安全。
/// 2. `reasoning_details`（OpenRouter 扩展，schema 是对象数组）被客户端**双重
///    编码**成 JSON 字符串，例如
///    `"[{\"type\":\"reasoning.text\",\"text\":\"…\"}]"`。原样转发必然 400。
///    能解析回数组就还原；解析不出就删——它是辅助 reasoning 元数据，删掉最坏只
///    损失一段轨迹（assistant 的 `content`/`tool_calls` 完好），留着则整轮对话
///    直接失败。
///
/// 还原只对上面这一个白名单字段生效，**不做「任何能解析成 JSON 的字符串都还原」**
/// —— `content` 本就是 string，且完全合法地可能是 `"[1, 2, 3]"` 这种字面量正文，
/// 无差别还原会静默篡改用户内容。
/// Normalize the message list so every `system` message sits at the front,
/// merged into a single leading message.
///
/// Some OpenAI-compatible upstreams (notably Qwen/DashScope) require exactly
/// one `system` message and require it to be first — an inline one is rejected
/// with `invalid_prompt` / "System message must be at the beginning".
///
/// Only system messages move; the relative order of every other message is
/// untouched, so an assistant `tool_calls` message stays adjacent to the `tool`
/// results that reference it.
pub(crate) fn normalize_system_messages(messages: Vec<Value>) -> Vec<Value> {
    let mut system_contents: Vec<Value> = Vec::new();
    let mut rest: Vec<Value> = Vec::with_capacity(messages.len());
    for msg in messages {
        if msg.get("role").and_then(Value::as_str) == Some("system") {
            system_contents.push(msg.get("content").cloned().unwrap_or(Value::Null));
        } else {
            rest.push(msg);
        }
    }
    if system_contents.is_empty() {
        return rest;
    }
    let mut out = Vec::with_capacity(rest.len() + 1);
    out.push(json!({ "role": "system", "content": merge_system_content(system_contents) }));
    out.extend(rest);
    out
}

/// Fold N system contents into one. A lone content passes through unchanged
/// (string stays a string, so the common single-system case is byte-identical
/// to before). Multiple contents become one array of text parts, concatenated
/// in order.
fn merge_system_content(contents: Vec<Value>) -> Value {
    if contents.len() == 1 {
        return contents.into_iter().next().unwrap();
    }
    let mut parts: Vec<Value> = Vec::new();
    for c in contents {
        match c {
            Value::String(s) => parts.push(json!({ "type": "text", "text": s })),
            Value::Array(a) => parts.extend(a),
            Value::Null => {}
            other => parts.push(json!({ "type": "text", "text": other.to_string() })),
        }
    }
    Value::Array(parts)
}

pub fn sanitize_chat_messages(body: &mut Value) {
    let Some(msgs) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for m in &mut *msgs {
        let Some(obj) = m.as_object_mut() else { continue };
        if obj.get("tool_calls").and_then(Value::as_array).is_some_and(Vec::is_empty) {
            obj.remove("tool_calls");
        }
        // Only a string is suspect — a real array is already schema-valid.
        if matches!(obj.get("reasoning_details"), Some(Value::String(_))) {
            let restored = match obj.get("reasoning_details").and_then(Value::as_str) {
                Some(s) => serde_json::from_str::<Value>(s).ok().filter(Value::is_array),
                None => None,
            };
            match restored {
                Some(arr) => {
                    obj.insert("reasoning_details".to_string(), arr);
                }
                None => {
                    obj.remove("reasoning_details");
                }
            }
        }
    }
    // Some upstreams (Qwen/DashScope) require exactly one system message at the
    // front; an inline one is rejected with `invalid_prompt`. Reorder + merge.
    let normalized = normalize_system_messages(msgs.clone());
    *msgs = normalized;
}

pub fn extract_anthropic_usage_from_sse(stream_text: &str) -> AnthropicUsage {
    let mut usage = AnthropicUsage::default();
    for line in stream_text.lines() {
        let trimmed = line.trim();
        let Some(payload) = trimmed.strip_prefix("data: ") else {
            continue;
        };
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(payload) else {
            continue;
        };
        let source = if event["type"] == "message_start" {
            &event["message"]["usage"]
        } else {
            &event["usage"]
        };
        merge_usage_max(&mut usage, source);
    }
    usage
}

fn merge_usage_max(usage: &mut AnthropicUsage, source: &Value) {
    if let Some(value) = source["input_tokens"].as_i64() {
        usage.input_tokens = usage.input_tokens.max(value);
    }
    if let Some(value) = source["output_tokens"].as_i64() {
        usage.output_tokens = usage.output_tokens.max(value);
    }
    if let Some(value) = source["cache_read_input_tokens"].as_i64() {
        usage.cache_read_input_tokens = usage.cache_read_input_tokens.max(value);
    }
    if let Some(value) = source["cache_creation_input_tokens"].as_i64() {
        usage.cache_creation_input_tokens = usage.cache_creation_input_tokens.max(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_removes_empty_tool_calls() {
        let mut body = json!({
            "model": "od",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "text", "tool_calls": []},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "call_1", "type": "function", "function": {"name": "f", "arguments": "{}"}}
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": "ok"}
            ]
        });
        sanitize_chat_messages(&mut body);
        let msgs = body["messages"].as_array().unwrap();
        assert!(msgs[1].get("tool_calls").is_none(), "空数组应被删除");
        assert!(msgs[2]["tool_calls"].is_array(), "非空 tool_calls 应保留");
        assert_eq!(msgs[2]["tool_calls"].as_array().unwrap().len(), 1);
        assert!(msgs[0].get("tool_calls").is_none(), "无字段消息不应新增字段");
        assert!(msgs[3].get("tool_calls").is_none(), "tool 消息无字段");
    }

    #[test]
    fn sanitize_noop_without_messages() {
        let mut body = json!({"model": "m", "max_tokens": 10});
        sanitize_chat_messages(&mut body);
        assert_eq!(body["model"], "m");
    }

    #[test]
    fn sanitize_restores_stringified_reasoning_details() {
        // 线上真实故障：客户端把数组 json.dumps 进 string 字段，上游按
        // "expected array, received string" 拒收（messages.416.reasoning_details）。
        let mut body = json!({
            "model": "od",
            "messages": [{
                "role": "assistant",
                "content": "修好了。",
                "reasoning_details": "[{\"type\":\"reasoning.text\",\"text\":\"let me write\",\"index\":0}]"
            }]
        });
        sanitize_chat_messages(&mut body);
        let rd = &body["messages"][0]["reasoning_details"];
        assert!(rd.is_array(), "应还原成数组，实际 {}", rd);
        assert_eq!(rd[0]["type"], "reasoning.text");
        assert_eq!(rd[0]["text"], "let me write");
        assert_eq!(body["messages"][0]["content"], "修好了。", "content 不得受影响");
    }

    #[test]
    fn sanitize_keeps_valid_reasoning_details_array() {
        let mut body = json!({
            "model": "od",
            "messages": [{
                "role": "assistant",
                "content": "ok",
                "reasoning_details": [{"type": "reasoning.text", "text": "t"}]
            }]
        });
        sanitize_chat_messages(&mut body);
        assert!(body["messages"][0]["reasoning_details"].is_array(), "合法数组应原样保留");
        assert_eq!(body["messages"][0]["reasoning_details"][0]["text"], "t");
    }

    #[test]
    fn sanitize_drops_unparseable_reasoning_details() {
        // 留原值必然 400；删掉最坏只损失 reasoning 轨迹，content 仍可用。
        for bad in ["not json", "42", "{\"type\":\"x\"}", "null"] {
            let mut body = json!({
                "model": "od",
                "messages": [{
                    "role": "assistant",
                    "content": "keep me",
                    "reasoning_details": bad
                }]
            });
            sanitize_chat_messages(&mut body);
            assert!(
                body["messages"][0].get("reasoning_details").is_none(),
                "非数组的 {bad:?} 应被删除"
            );
            assert_eq!(body["messages"][0]["content"], "keep me", "content 必须保留");
        }
    }

    #[test]
    fn sanitize_never_touches_content_that_looks_like_json() {
        // 防回归：白名单式还原的核心理由。content 本就是 string，
        // "[1, 2, 3]" 这类字面量正文必须逐字节不变。
        let mut body = json!({
            "model": "od",
            "messages": [
                {"role": "user", "content": "[1, 2, 3]"},
                {"role": "assistant", "content": "{\"a\": 1}"},
                {"role": "user", "content": "普通文本"}
            ]
        });
        let before = body["messages"].clone();
        sanitize_chat_messages(&mut body);
        assert_eq!(body["messages"], before, "content 形似 JSON 时消息体必须完全不变");
    }

    #[test]
    fn sanitize_does_not_add_field_to_messages_without_it() {
        let mut body = json!({
            "model": "od",
            "messages": [{"role": "assistant", "content": "x"}]
        });
        sanitize_chat_messages(&mut body);
        assert!(body["messages"][0].get("reasoning_details").is_none(), "不得新增字段");
    }

    #[test]
    fn sanitize_moves_inline_system_to_front() {
        // hermes 等 OpenAI 客户端偶尔把 system 放在 user 之后，ninfer/Qwen 会
        // 报 `invalid_prompt`。修复：把 system 提到开头。
        let mut body = json!({
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "system", "content": "You are a bot."}
            ]
        });
        sanitize_chat_messages(&mut body);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
    }

    #[test]
    fn sanitize_merges_multiple_system_into_one_leading() {
        let mut body = json!({
            "messages": [
                {"role": "system", "content": "A"},
                {"role": "user", "content": "hi"},
                {"role": "system", "content": "B"}
            ]
        });
        sanitize_chat_messages(&mut body);
        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
        assert_eq!(body["messages"][0]["role"], "system");
        // 两个 system 合并成数组
        assert_eq!(
            body["messages"][0]["content"],
            json!([{"type": "text", "text": "A"}, {"type": "text", "text": "B"}])
        );
        assert_eq!(body["messages"][1]["role"], "user");
    }

    #[test]
    fn sanitize_single_leading_system_is_byte_identical() {
        let mut body = json!({
            "messages": [
                {"role": "system", "content": "You are a bot."},
                {"role": "user", "content": "hi"}
            ]
        });
        let before = body["messages"].clone();
        sanitize_chat_messages(&mut body);
        assert_eq!(body["messages"], before);
    }

    #[test]
    fn sanitize_preserves_non_system_order() {
        let mut body = json!({
            "messages": [
                {"role": "system", "content": "sys"},
                {"role": "user", "content": "1"},
                {"role": "assistant", "content": "2"},
                {"role": "user", "content": "3"},
                {"role": "assistant", "tool_calls": [{"id": "x"}]},
                {"role": "tool", "tool_call_id": "x", "content": "4"}
            ]
        });
        sanitize_chat_messages(&mut body);
        let roles: Vec<_> = body["messages"].as_array().unwrap().iter()
            .filter_map(|m| m.get("role").and_then(Value::as_str)).collect();
        assert_eq!(roles, vec!["system", "user", "assistant", "user", "assistant", "tool"]);
        // tool 消息仍紧邻其 tool_calls
        assert_eq!(body["messages"][5]["tool_call_id"], "x");
    }
}
