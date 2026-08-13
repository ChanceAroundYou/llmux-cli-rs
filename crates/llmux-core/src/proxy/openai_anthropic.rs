//! OpenAI Chat Completions → Anthropic Messages protocol conversion.
//!
//! The reverse of `anthropic_openai`. Used by the OpenAI-compatible route
//! (`/v1/chat/completions`) to fall back to a provider's Anthropic
//! `/v1/messages` endpoint when the model rejects `/chat/completions` — e.g.
//! GitHub Copilot serves GPT-5.x models (`gpt-5.6-luna`, `grok-4.5`) ONLY via
//! the Anthropic Messages / Responses API and answers any `/chat/completions`
//! call with `unsupported_api_for_model`.
//!
//! Pure functions + a streaming SSE state machine. No network I/O.

use serde_json::{json, Map, Value};

// ---------------------------------------------------------------------------
// Request conversion: OpenAI Chat Completions → Anthropic Messages
// ---------------------------------------------------------------------------

/// Convert an OpenAI `/v1/chat/completions` request body to an Anthropic
/// `/v1/messages` request body.
pub fn openai_to_anthropic_request(
    openai_body: &Value,
    resolved_model: &str,
) -> anyhow::Result<Value> {
    let body_obj = openai_body
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("OpenAI request body must be an object"))?;

    let mut system_parts: Vec<String> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    // Pending tool_result blocks (from role:"tool" messages) — Anthropic
    // requires them inside a `user` message referencing the tool_use_id.
    let mut pending_tool_results: Vec<Value> = Vec::new();

    if let Some(arr) = body_obj.get("messages").and_then(Value::as_array) {
        for msg in arr {
            let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
            match role {
                "system" => {
                    if let Some(s) = openai_content_as_string(msg.get("content")) {
                        system_parts.push(s);
                    }
                }
                "tool" => {
                    if let Some(tr) = openai_tool_result_block(msg) {
                        pending_tool_results.push(tr);
                    }
                }
                _ => {
                    let converted = convert_openai_message(msg);
                    if let Some(user_msg) = converted {
                        // Flush pending tool_results into a synthetic user message
                        // before the next real message.
                        if !pending_tool_results.is_empty() {
                            messages.push(json!({
                                "role": "user",
                                "content": Value::Array(std::mem::take(&mut pending_tool_results))
                            }));
                        }
                        messages.push(user_msg);
                    }
                }
            }
        }
    }
    if !pending_tool_results.is_empty() {
        messages.push(json!({
            "role": "user",
            "content": Value::Array(pending_tool_results)
        }));
    }

    let mut out = Map::new();
    out.insert("model".to_string(), json!(resolved_model));
    if !system_parts.is_empty() {
        let system = if system_parts.len() == 1 {
            json!(system_parts[0])
        } else {
            json!(system_parts.join("\n\n"))
        };
        out.insert("system".to_string(), system);
    }
    out.insert("messages".to_string(), Value::Array(messages));

    for key in ["temperature", "top_p"] {
        if let Some(v) = body_obj.get(key) {
            out.insert(key.to_string(), v.clone());
        }
    }
    // max_tokens / max_completion_tokens → Anthropic max_tokens.
    if let Some(v) = body_obj
        .get("max_completion_tokens")
        .or_else(|| body_obj.get("max_tokens"))
    {
        out.insert("max_tokens".to_string(), v.clone());
    }
    if let Some(v) = body_obj.get("stream") {
        out.insert("stream".to_string(), v.clone());
    }
    if let Some(v) = body_obj.get("stop_sequences") {
        out.insert("stop_sequences".to_string(), v.clone());
    } else if let Some(v) = body_obj.get("stop") {
        out.insert("stop_sequences".to_string(), v.clone());
    }
    if let Some(v) = body_obj.get("tools") {
        out.insert("tools".to_string(), openai_tools_to_anthropic(v));
    }
    if let Some(v) = body_obj.get("tool_choice") {
        out.insert("tool_choice".to_string(), openai_tool_choice_to_anthropic(v));
    }

    Ok(Value::Object(out))
}

/// Convert one OpenAI chat message (user/assistant) to an Anthropic message.
fn convert_openai_message(msg: &Value) -> Option<Value> {
    let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
    let content = msg.get("content");

    // Assistant tool_calls → tool_use blocks in content.
    let mut tool_use_blocks: Vec<Value> = Vec::new();
    if let Some(tcs) = msg.get("tool_calls").and_then(Value::as_array) {
        for tc in tcs {
            let id = tc.get("id").and_then(Value::as_str).unwrap_or_default();
            let name = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let args = tc
                .get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(Value::as_str)
                .unwrap_or("{}");
            let input = serde_json::from_str::<Value>(args).unwrap_or_else(|_| json!({}));
            tool_use_blocks.push(json!({
                "type": "tool_use", "id": id, "name": name, "input": input
            }));
        }
    }

    // Assistant reasoning_content → thinking block.
    let mut blocks: Vec<Value> = Vec::new();
    if let Some(rc) = msg.get("reasoning_content").and_then(Value::as_str) {
        if !rc.is_empty() {
            blocks.push(json!({ "type": "thinking", "thinking": rc }));
        }
    }

    // Content parts.
    match content {
        Some(Value::String(s)) => {
            if !s.is_empty() {
                blocks.push(json!({ "type": "text", "text": s }));
            }
        }
        Some(Value::Array(parts)) => {
            for p in parts {
                match p.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = p.get("text").and_then(Value::as_str) {
                            blocks.push(json!({ "type": "text", "text": t }));
                        }
                    }
                    Some("image_url") => {
                        if let Some(url) = p
                            .get("image_url")
                            .and_then(|u| u.get("url"))
                            .and_then(Value::as_str)
                        {
                            // data:media;base64,... → base64 source
                            if let Some((media, data)) = parse_data_uri(url) {
                                blocks.push(json!({
                                    "type": "image",
                                    "source": { "type": "base64", "media_type": media, "data": data }
                                }));
                            } else {
                                blocks.push(json!({
                                    "type": "image",
                                    "source": { "type": "url", "url": url }
                                }));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }

    blocks.extend(tool_use_blocks);

    if blocks.is_empty() {
        return None;
    }

    let content_value = if blocks.len() == 1
        && blocks[0].get("type").and_then(Value::as_str) == Some("text")
    {
        blocks[0].get("text").cloned().unwrap_or(Value::Null)
    } else {
        Value::Array(blocks)
    };

    Some(json!({ "role": role, "content": content_value }))
}

/// Convert an OpenAI `role:"tool"` message to an Anthropic tool_result block.
fn openai_tool_result_block(msg: &Value) -> Option<Value> {
    let tool_use_id = msg.get("tool_call_id").and_then(Value::as_str).unwrap_or_default();
    let content = match msg.get("content") {
        Some(Value::String(s)) => json!(s),
        Some(v) => v.clone(),
        None => json!(""),
    };
    let mut block = json!({ "type": "tool_result", "tool_use_id": tool_use_id, "content": content });
    if let Some(is_error) = msg.get("is_error") {
        block["is_error"] = is_error.clone();
    }
    Some(block)
}

/// OpenAI `data:media;base64,...` URI → (media_type, data).
fn parse_data_uri(uri: &str) -> Option<(String, String)> {
    let rest = uri.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let media = meta.split(';').next().unwrap_or("image/jpeg").to_string();
    let data = data.replace("%2B", "+"); // minimal decode for base64 padding
    Some((media, data))
}

/// OpenAI tools array → Anthropic tools.
pub fn openai_tools_to_anthropic(tools: &Value) -> Value {
    let mut out = Vec::new();
    if let Some(arr) = tools.as_array() {
        for tool in arr {
            let function = tool.get("function");
            let name = function
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mut t = Map::new();
            t.insert("name".to_string(), json!(name));
            if let Some(desc) = function.and_then(|f| f.get("description")).and_then(Value::as_str) {
                t.insert("description".to_string(), json!(desc));
            }
            let input_schema = function
                .and_then(|f| f.get("parameters"))
                .cloned()
                .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
            t.insert("input_schema".to_string(), input_schema);
            out.push(Value::Object(t));
        }
    }
    Value::Array(out)
}

/// OpenAI tool_choice → Anthropic tool_choice.
pub fn openai_tool_choice_to_anthropic(choice: &Value) -> Value {
    match choice {
        Value::String(s) => match s.as_str() {
            "auto" => json!({ "type": "auto" }),
            "required" => json!({ "type": "any" }),
            "none" => json!({ "type": "none" }),
            _ => json!({ "type": "auto" }),
        },
        Value::Object(obj) => match obj.get("type").and_then(Value::as_str) {
            Some("function") => {
                let name = obj
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                json!({ "type": "tool", "name": name })
            }
            Some("auto") => json!({ "type": "auto" }),
            Some("required") => json!({ "type": "any" }),
            _ => json!({ "type": "auto" }),
        },
        _ => json!({ "type": "auto" }),
    }
}

/// Extract a plain string from OpenAI message content (string or single text part).
fn openai_content_as_string(content: Option<&Value>) -> Option<String> {
    match content {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Array(parts)) => {
            let mut buf = String::new();
            for p in parts {
                if p.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(t) = p.get("text").and_then(Value::as_str) {
                        buf.push_str(t);
                    }
                }
            }
            if buf.is_empty() { None } else { Some(buf) }
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Response conversion: Anthropic Messages → OpenAI Chat Completions
// ---------------------------------------------------------------------------

/// Convert a non-streaming Anthropic Messages response to an OpenAI
/// `/v1/chat/completions` response body.
pub fn anthropic_to_openai_response(anthropic_body: &Value, resolved_model: &str) -> Value {
    let mut text_parts: Vec<String> = Vec::new();
    let mut reasoning: Option<String> = None;
    let mut tool_calls: Vec<Value> = Vec::new();

    if let Some(blocks) = anthropic_body.get("content").and_then(Value::as_array) {
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(t) = block.get("text").and_then(Value::as_str) {
                        text_parts.push(t.to_string());
                    }
                }
                Some("thinking") => {
                    if let Some(t) = block.get("thinking").and_then(Value::as_str) {
                        reasoning = Some(t.to_string());
                    }
                }
                Some("tool_use") => {
                    let id = block.get("id").and_then(Value::as_str).unwrap_or_default();
                    let name = block.get("name").and_then(Value::as_str).unwrap_or_default();
                    let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                    tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": { "name": name, "arguments": input.to_string() }
                    }));
                }
                _ => {}
            }
        }
    }

    let content = if text_parts.is_empty() {
        Value::Null
    } else {
        json!(text_parts.join(""))
    };

    let mut message = Map::new();
    message.insert("role".to_string(), json!("assistant"));
    message.insert("content".to_string(), content);
    if let Some(rc) = reasoning {
        message.insert("reasoning_content".to_string(), json!(rc));
    }
    if !tool_calls.is_empty() {
        message.insert("tool_calls".to_string(), Value::Array(tool_calls));
    }

    let stop_reason = anthropic_body
        .get("stop_reason")
        .and_then(Value::as_str)
        .unwrap_or("end_turn");
    let finish_reason = map_stop_reason_to_finish(stop_reason);

    let usage = &anthropic_body["usage"];
    let prompt_tokens = usage.get("input_tokens").and_then(Value::as_i64).unwrap_or(0);
    let completion_tokens = usage.get("output_tokens").and_then(Value::as_i64).unwrap_or(0);

    json!({
        "id": anthropic_body.get("id").cloned().unwrap_or_else(|| json!("chatcmpl-anthropic")),
        "object": "chat.completion",
        "created": chrono_now_unix(),
        "model": resolved_model,
        "choices": [{
            "index": 0,
            "message": Value::Object(message),
            "finish_reason": finish_reason
        }],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens,
        }
    })
}

/// Map Anthropic stop_reason to OpenAI finish_reason.
fn map_stop_reason_to_finish(stop: &str) -> &str {
    match stop {
        "end_turn" => "stop",
        "max_tokens" => "length",
        "tool_use" => "tool_calls",
        "pause_turn" => "stop",
        other => other,
    }
}

/// Seconds since epoch (for the `created` field).
fn chrono_now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Streaming: Anthropic SSE → OpenAI SSE
// ---------------------------------------------------------------------------

/// Stateful converter from Anthropic SSE event payloads to OpenAI SSE
/// `data:` payload strings. `feed` receives each Anthropic event's JSON
/// (`{"type": "content_block_delta", ...}`) and returns OpenAI payloads.
pub struct AnthropicSseConverter {
    started: bool,
    tool_indices: std::collections::HashSet<usize>,
    finished: bool,
    finish_reason: Option<String>,
}

impl AnthropicSseConverter {
    pub fn new(_model: &str) -> Self {
        Self {
            started: false,
            tool_indices: std::collections::HashSet::new(),
            finished: false,
            finish_reason: None,
        }
    }

    /// Feed one Anthropic SSE event JSON payload. Returns OpenAI `data:` lines
    /// (without the trailing blank line; the caller appends `\n\n`).
    pub fn feed(&mut self, event: &Value) -> Vec<String> {
        let mut out = Vec::new();
        let Some(etype) = event.get("type").and_then(Value::as_str) else {
            return out;
        };

        match etype {
            "message_start" => {
                if !self.started {
                    self.started = true;
                    out.push(openai_chunk(&json!({
                        "choices": [{
                            "index": 0,
                            "delta": { "role": "assistant", "content": "" },
                            "finish_reason": null
                        }]
                    })));
                }
            }
            "content_block_start" => {
                let index = event.get("index").and_then(Value::as_i64).unwrap_or(0) as usize;
                let block_type = event
                    .get("content_block")
                    .and_then(|b| b.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if block_type == "tool_use" {
                    self.tool_indices.insert(index);
                    let id = event
                        .get("content_block")
                        .and_then(|b| b.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let name = event
                        .get("content_block")
                        .and_then(|b| b.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let tc_index = self.tool_calls_index(index);
                    out.push(openai_chunk(&json!({
                        "choices": [{
                            "index": 0,
                            "delta": {
                                "tool_calls": [{
                                    "index": tc_index,
                                    "id": id,
                                    "type": "function",
                                    "function": { "name": name, "arguments": "" }
                                }]
                            },
                            "finish_reason": null
                        }]
                    })));
                }
            }
            "content_block_delta" => {
                let index = event.get("index").and_then(Value::as_i64).unwrap_or(0) as usize;
                let delta = event.get("delta").unwrap_or(&Value::Null);
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(t) = delta.get("text").and_then(Value::as_str) {
                            out.push(openai_chunk(&json!({
                                "choices": [{
                                    "index": 0,
                                    "delta": { "content": t },
                                    "finish_reason": null
                                }]
                            })));
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(t) = delta.get("thinking").and_then(Value::as_str) {
                            out.push(openai_chunk(&json!({
                                "choices": [{
                                    "index": 0,
                                    "delta": { "reasoning_content": t },
                                    "finish_reason": null
                                }]
                            })));
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(pj) = delta.get("partial_json").and_then(Value::as_str) {
                            let tc_index = self.tool_calls_index(index);
                            out.push(openai_chunk(&json!({
                                "choices": [{
                                    "index": 0,
                                    "delta": {
                                        "tool_calls": [{
                                            "index": tc_index,
                                            "function": { "arguments": pj }
                                        }]
                                    },
                                    "finish_reason": null
                                }]
                            })));
                        }
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                let stop = event
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str)
                    .unwrap_or("end_turn");
                let fr = map_stop_reason_to_finish(stop).to_string();
                self.finish_reason = Some(fr.clone());
                out.push(openai_chunk(&json!({
                    "choices": [{
                        "index": 0,
                        "delta": {},
                        "finish_reason": fr
                    }]
                })));
            }
            _ => {}
        }
        out
    }

    /// End of stream: emit the trailing usage chunk (if we have usage) and
    /// `[DONE]`. Returns OpenAI `data:` lines (including `[DONE]`).
    pub fn finish(&mut self, usage: Option<&Value>) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;

        let mut out = Vec::new();
        // Emit finish_reason if the upstream never sent message_delta.
        if self.finish_reason.is_none() {
            out.push(openai_chunk(&json!({
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": "stop"
                }]
            })));
        }
        if let Some(usage) = usage {
            let prompt = usage.get("input_tokens").and_then(Value::as_i64).unwrap_or(0);
            let completion = usage.get("output_tokens").and_then(Value::as_i64).unwrap_or(0);
            out.push(openai_chunk(&json!({
                "choices": [],
                "usage": {
                    "prompt_tokens": prompt,
                    "completion_tokens": completion,
                    "total_tokens": prompt + completion,
                }
            })));
        }
        out.push("[DONE]".to_string());
        out
    }

    fn tool_calls_index(&self, block_index: usize) -> usize {
        // Anthropic block indexes include text/thinking blocks; OpenAI tool
        // indexes are 0-based among tool calls only. Recompute by counting
        // tool blocks with a lower index.
        self.tool_indices.iter().copied().filter(|&i| i < block_index).count()
    }
}

fn openai_chunk(obj: &Value) -> String {
    format!("data: {}", serde_json::to_string(obj).unwrap_or_else(|_| "{}".to_string()))
}

/// Detect the GitHub-Copilot-style rejection of `/chat/completions`.
pub fn is_unsupported_api_for_model(error_body: &str) -> bool {
    error_body.contains("unsupported_api_for_model")
        || error_body.contains("not accessible via the /chat/completions endpoint")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_converts_messages_and_system() {
        let body = json!({
            "model": "m",
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello"},
            ],
            "max_tokens": 100
        });
        let out = openai_to_anthropic_request(&body, "gpt-5.6-luna").unwrap();
        assert_eq!(out["system"], "You are helpful.");
        assert_eq!(out["messages"][0]["role"], "user");
        assert_eq!(out["messages"][0]["content"], "hi");
        assert_eq!(out["messages"][1]["role"], "assistant");
        assert_eq!(out["max_tokens"], 100);
        assert_eq!(out["model"], "gpt-5.6-luna");
    }

    #[test]
    fn request_converts_tool_calls_and_tool_results() {
        let body = json!({
            "model": "m",
            "messages": [
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "call_1", "type": "function", "function": {"name": "get_weather", "arguments": "{\"city\":\"Beijing\"}"}}
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": "25C"}
            ]
        });
        let out = openai_to_anthropic_request(&body, "m").unwrap();
        let msgs = out["messages"].as_array().unwrap();
        // tool_result goes in a synthetic user message
        assert_eq!(msgs[0]["content"][0]["type"], "tool_use");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"][0]["type"], "tool_result");
        assert_eq!(msgs[1]["content"][0]["tool_use_id"], "call_1");
    }

    #[test]
    fn response_converts_blocks_and_stop_reason() {
        let body = json!({
            "id": "msg_1",
            "content": [
                {"type": "thinking", "thinking": "hmm"},
                {"type": "text", "text": "Hello"},
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 5, "output_tokens": 3}
        });
        let out = anthropic_to_openai_response(&body, "m");
        assert_eq!(out["choices"][0]["message"]["content"], "Hello");
        assert_eq!(out["choices"][0]["message"]["reasoning_content"], "hmm");
        assert_eq!(out["choices"][0]["finish_reason"], "stop");
        assert_eq!(out["usage"]["prompt_tokens"], 5);
        assert_eq!(out["usage"]["completion_tokens"], 3);
    }

    #[test]
    fn sse_converter_full_sequence() {
        let mut c = AnthropicSseConverter::new("m");
        let mut lines: Vec<String> = Vec::new();
        lines.extend(c.feed(&json!({"type": "message_start", "message": {"id": "m1"}})));
        lines.extend(c.feed(&json!({
            "type": "content_block_start", "index": 0,
            "content_block": {"type": "thinking", "thinking": ""}
        })));
        lines.extend(c.feed(&json!({
            "type": "content_block_delta", "index": 0,
            "delta": {"type": "thinking_delta", "thinking": "think"}
        })));
        lines.extend(c.feed(&json!({
            "type": "content_block_delta", "index": 1,
            "delta": {"type": "text_delta", "text": "Hi"}
        })));
        lines.extend(c.feed(&json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn", "stop_sequence": null}
        })));
        lines.extend(c.finish(None));

        assert!(lines[0].contains("role"));
        assert!(lines.iter().any(|l| l.contains("reasoning_content") && l.contains("think")));
        assert!(lines.iter().any(|l| l.contains("content") && l.contains("Hi")));
        assert!(lines.iter().any(|l| l.contains("\"finish_reason\":\"stop\"")));
        assert_eq!(lines.last().unwrap(), "[DONE]");
    }
}
