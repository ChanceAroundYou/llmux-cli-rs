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

/// OpenAI chat 请求体清洗：assistant 消息上的 `tool_calls: []` 空数组会被严格
/// 上游（DeepSeek、Console Go 等）以 minLength 1 拒绝 → 400 → 网关 502。
/// 空数组语义等价于"无工具调用"，直接删字段对任何上游都安全。
pub fn strip_empty_tool_calls(body: &mut Value) {
    let Some(msgs) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for m in msgs {
        if let Some(obj) = m.as_object_mut() {
            if obj.get("tool_calls").and_then(Value::as_array).is_some_and(Vec::is_empty) {
                obj.remove("tool_calls");
            }
        }
    }
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
    fn strip_empty_tool_calls_removes_only_empty_arrays() {
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
        strip_empty_tool_calls(&mut body);
        let msgs = body["messages"].as_array().unwrap();
        assert!(msgs[1].get("tool_calls").is_none(), "空数组应被删除");
        assert!(msgs[2]["tool_calls"].is_array(), "非空 tool_calls 应保留");
        assert_eq!(msgs[2]["tool_calls"].as_array().unwrap().len(), 1);
        assert!(msgs[0].get("tool_calls").is_none(), "无字段消息不应新增字段");
        assert!(msgs[3].get("tool_calls").is_none(), "tool 消息无字段");
    }

    #[test]
    fn strip_empty_tool_calls_noop_without_messages() {
        let mut body = json!({"model": "m", "max_tokens": 10});
        strip_empty_tool_calls(&mut body);
        assert_eq!(body["model"], "m");
    }
}
