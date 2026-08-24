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
