//! Anthropic Messages API ↔ OpenAI Chat Completions protocol conversion.
//!
//! Pure functions and a streaming SSE state machine. No network I/O here —
//! the route layer (llmux-server) drives HTTP. This mirrors the Bun version's
//! `src/services/anthropic_ingress.ts` and additionally handles the gaps that
//! version ignored: `thinking` request param, `cache_control` passthrough,
//! cache usage fields, real streaming usage, tool_result `is_error`, and
//! `image` url sources.

use serde_json::{json, Map, Value};
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// Request conversion: Anthropic Messages → OpenAI Chat Completions
// ---------------------------------------------------------------------------

/// Convert an Anthropic `/v1/messages` request body to an OpenAI
/// `/chat/completions` request body. `resolved_model` is the dispatcher-resolved
/// target model (alias expansion already applied upstream).
pub fn anthropic_to_openai_request(
    anthropic_body: &Value,
    resolved_model: &str,
) -> anyhow::Result<Value> {
    let body_obj = anthropic_body
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Anthropic request body must be an object"))?;

    let mut messages: Vec<Value> = Vec::new();

    // Anthropic top-level `system` (string or block array) → first system message.
    if let Some(system) = body_obj.get("system") {
        if system.is_string() || system.is_array() {
            messages.push(json!({ "role": "system", "content": system }));
        }
    }

    if let Some(arr) = body_obj.get("messages").and_then(Value::as_array) {
        for msg in arr {
            transform_anthropic_message(msg, &mut messages);
        }
    }

    let mut out = Map::new();
    out.insert("model".to_string(), json!(resolved_model));
    out.insert("messages".to_string(), Value::Array(messages));

    for key in ["max_tokens", "temperature", "top_p"] {
        if let Some(v) = body_obj.get(key) {
            out.insert(key.to_string(), v.clone());
        }
    }
    // Explicitly set `stream` (defaulting to false). Some OpenAI-compatible
    // gateways (e.g. opencode zen/go) intermittently 500 on a request where the
    // field is absent, while `stream: false` is stable — and Anthropic semantics
    // already imply non-streaming when omitted.
    let stream = body_obj.get("stream").and_then(Value::as_bool).unwrap_or(false);
    out.insert("stream".to_string(), json!(stream));
    if let Some(v) = body_obj.get("stop_sequences") {
        out.insert("stop".to_string(), v.clone());
    }
    if let Some(v) = body_obj.get("tools") {
        out.insert("tools".to_string(), map_tools_to_openai(v));
    }
    if let Some(v) = body_obj.get("tool_choice") {
        out.insert("tool_choice".to_string(), map_tool_choice_to_openai(v));
    }

    // Extended thinking: pass through the `thinking` block, and when enabled
    // without a max_tokens, backstop with the budget (Anthropic's max_tokens
    // excludes the thinking budget).
    if let Some(thinking) = body_obj.get("thinking") {
        let mut thinking = thinking.clone();
        // Normalize `thinking.type`: Anthropic sends "enabled"/"disabled";
        // DeepSeek-style clients send "adaptive"; some gateway backends
        // (Tencent Cloud) only accept ["enabled","disabled","auto"]. "adaptive"
        // passes some gateways' front-end validation (e.g. Sensenova's
        // ["enabled","disabled","adaptive"]) but is then rejected by the
        // backend with `'type' must be in ["enabled", "disabled", "auto"]`.
        // Map any non-standard type to "enabled" so thinking keeps working
        // across all OpenAI-compatible upstreams.
        if let Some(t) = thinking.get_mut("type") {
            if !matches!(t.as_str(), Some("enabled" | "disabled")) {
                *t = json!("enabled");
            }
        }
        let enabled = thinking.get("type").and_then(Value::as_str) == Some("enabled");
        if enabled {
            if !out.contains_key("max_tokens") {
                if let Some(budget) = thinking.get("budget_tokens").and_then(Value::as_i64) {
                    out.insert("max_tokens".to_string(), json!(budget));
                }
            }
        }
        out.insert("thinking".to_string(), thinking);
    }

    // Ask for real usage in the stream tail (DeepSeek/OpenAI-compatible gateways
    // honor this). Falls back to zeroed usage when a provider rejects it.
    if out.get("stream").and_then(Value::as_bool) == Some(true) {
        out.insert(
            "stream_options".to_string(),
            json!({ "include_usage": true }),
        );
    }

    Ok(Value::Object(out))
}

/// Expand one Anthropic message into OpenAI messages. `tool_result` blocks
/// become standalone `role: "tool"` messages; the rest (text/image/tool_use/
/// thinking) collapse into a single message carrying content parts, tool_calls,
/// and message-level reasoning fields.
fn transform_anthropic_message(msg: &Value, messages: &mut Vec<Value>) {
    let role = msg
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("user")
        .to_string();
    let content = msg.get("content");

    match content {
        Some(Value::String(s)) => {
            messages.push(json!({ "role": role, "content": s }));
        }
        Some(Value::Array(blocks)) => {
            let mut parts: Vec<Value> = Vec::new();
            let mut tool_calls: Vec<Value> = Vec::new();
            let mut tool_results: Vec<Value> = Vec::new();
            let mut reasoning_content: Option<String> = None;
            let mut reasoning_signature: Option<String> = None;

            for block in blocks {
                let block_type = block.get("type").and_then(Value::as_str).unwrap_or_default();
                match block_type {
                    "thinking" => {
                        if let Some(t) = block.get("thinking").and_then(Value::as_str) {
                            reasoning_content = Some(t.to_string());
                        }
                        if let Some(sig) = block.get("signature").and_then(Value::as_str) {
                            reasoning_signature = Some(sig.to_string());
                        }
                    }
                    "text" => {
                        // Keep `cache_control` on the part (OpenAI gateways ignore
                        // unknown fields; those that understand it will honor it).
                        parts.push(block.clone());
                    }
                    "image" => {
                        if let Some(url) = image_block_to_url(block) {
                            parts.push(json!({ "type": "image_url", "image_url": { "url": url } }));
                        }
                    }
                    "tool_use" => {
                        let id = block.get("id").and_then(Value::as_str).unwrap_or_default();
                        let name = block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                        tool_calls.push(json!({
                            "id": id,
                            "type": "function",
                            "function": { "name": name, "arguments": input.to_string() }
                        }));
                    }
                    "tool_result" => {
                        let tool_use_id = block
                            .get("tool_use_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let content_str = match block.get("content") {
                            Some(Value::String(s)) => s.clone(),
                            Some(v) => v.to_string(),
                            None => String::new(),
                        };
                        let mut tool_msg =
                            json!({ "role": "tool", "tool_call_id": tool_use_id, "content": content_str });
                        // OpenAI has no is_error flag, but some gateways tolerate
                        // the extra field; harmless otherwise.
                        if let Some(err) = block.get("is_error") {
                            tool_msg["is_error"] = err.clone();
                        }
                        tool_results.push(tool_msg);
                    }
                    _ => {
                        // redacted_thinking and any future block types: ignore.
                    }
                }
            }

            // Tool results are standalone messages (they reference a prior
            // assistant tool_use by id). Emit before the containing message.
            for tr in tool_results {
                messages.push(tr);
            }

            let has_parts = !parts.is_empty();
            let has_tools = !tool_calls.is_empty();
            let has_reasoning = reasoning_content.is_some();
            if has_parts || has_tools || has_reasoning {
                let mut out_msg = Map::new();
                out_msg.insert("role".to_string(), json!(role));
                if has_parts {
                    // A single plain text block flattens to a string, but a text
                    // block carrying `cache_control` stays an array to preserve it.
                    let flattenable = parts.len() == 1
                        && parts[0].get("type").and_then(Value::as_str) == Some("text")
                        && parts[0].get("cache_control").is_none();
                    if flattenable {
                        out_msg.insert(
                            "content".to_string(),
                            parts[0].get("text").cloned().unwrap_or(Value::Null),
                        );
                    } else {
                        out_msg.insert("content".to_string(), Value::Array(parts));
                    }
                } else if has_tools {
                    out_msg.insert("content".to_string(), Value::Null);
                } else {
                    out_msg.insert("content".to_string(), Value::String(String::new()));
                }
                if has_tools {
                    out_msg.insert("tool_calls".to_string(), Value::Array(tool_calls));
                }
                if let Some(rc) = reasoning_content {
                    out_msg.insert("reasoning_content".to_string(), json!(rc));
                }
                if let Some(rs) = reasoning_signature {
                    out_msg.insert("reasoning_signature".to_string(), json!(rs));
                }
                messages.push(Value::Object(out_msg));
            }
        }
        _ => {
            // content missing → skip
        }
    }
}

/// Anthropic image block source → OpenAI image_url (base64 data URI or plain url).
fn image_block_to_url(block: &Value) -> Option<String> {
    let source = block.get("source")?;
    let stype = source.get("type").and_then(Value::as_str).unwrap_or_default();
    match stype {
        "base64" => {
            let media = source
                .get("media_type")
                .and_then(Value::as_str)
                .unwrap_or("image/jpeg");
            let data = source.get("data").and_then(Value::as_str).unwrap_or("");
            Some(format!("data:{media};base64,{data}"))
        }
        "url" => source
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

/// Anthropic tools → OpenAI `type: "function"` array.
pub fn map_tools_to_openai(tools: &Value) -> Value {
    let mut out = Vec::new();
    if let Some(arr) = tools.as_array() {
        for tool in arr {
            let mut function = Map::new();
            if let Some(name) = tool.get("name") {
                function.insert("name".to_string(), name.clone());
            }
            if let Some(desc) = tool.get("description") {
                function.insert("description".to_string(), desc.clone());
            }
            let params = tool
                .get("input_schema")
                .cloned()
                .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
            function.insert("parameters".to_string(), params);
            out.push(json!({ "type": "function", "function": Value::Object(function) }));
        }
    }
    Value::Array(out)
}

/// Anthropic tool_choice → OpenAI tool_choice.
pub fn map_tool_choice_to_openai(choice: &Value) -> Value {
    match choice {
        Value::String(s) => json!(s),
        Value::Object(obj) => match obj.get("type").and_then(Value::as_str) {
            Some("auto") => json!("auto"),
            Some("any") => json!("required"),
            Some("tool") => {
                let name = obj.get("name").and_then(Value::as_str).unwrap_or_default();
                json!({ "type": "function", "function": { "name": name } })
            }
            _ => json!("auto"),
        },
        _ => json!("auto"),
    }
}

// ---------------------------------------------------------------------------
// Response conversion: OpenAI Chat Completions → Anthropic Messages
// ---------------------------------------------------------------------------

/// Convert a non-streaming OpenAI Chat Completions response to an Anthropic
/// Messages response. `resolved_model` is echoed back as the response model.
pub fn openai_to_anthropic_response(openai_body: &Value, resolved_model: &str) -> Value {
    let choice = &openai_body["choices"][0];
    let message = &choice["message"];

    let mut content: Vec<Value> = Vec::new();

    if let Some(rc) = message.get("reasoning_content").and_then(Value::as_str) {
        if !rc.is_empty() {
            let mut block = json!({ "type": "thinking", "thinking": rc });
            if let Some(sig) = message.get("reasoning_signature").and_then(Value::as_str) {
                block["signature"] = json!(sig);
            }
            content.push(block);
        }
    }

    match message.get("content") {
        Some(Value::String(s)) => {
            if !s.is_empty() {
                content.push(json!({ "type": "text", "text": s }));
            }
        }
        Some(Value::Array(parts)) => {
            for p in parts {
                if p.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(text) = p.get("text").and_then(Value::as_str) {
                        content.push(json!({ "type": "text", "text": text }));
                    }
                }
            }
        }
        _ => {}
    }

    if let Some(tcs) = message.get("tool_calls").and_then(Value::as_array) {
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
                .unwrap_or("");
            let input = serde_json::from_str::<Value>(args).unwrap_or_else(|_| json!({}));
            content.push(json!({ "type": "tool_use", "id": id, "name": name, "input": input }));
        }
    }

    let finish = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .unwrap_or("stop");
    let stop_reason = map_stop_reason(finish).to_string();

    let usage = &openai_body["usage"];
    let (input_tokens, output_tokens) = (
        usage.get("prompt_tokens").and_then(Value::as_i64).unwrap_or(0),
        usage.get("completion_tokens").and_then(Value::as_i64).unwrap_or(0),
    );
    let (cache_read, cache_create) = cache_usage_from_openai(usage);

    json!({
        "id": openai_body.get("id").cloned().unwrap_or_else(|| json!("msg_unset")),
        "type": "message",
        "role": "assistant",
        "model": resolved_model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "cache_read_input_tokens": cache_read,
            "cache_creation_input_tokens": cache_create,
        }
    })
}

/// Map OpenAI finish_reason to Anthropic stop_reason.
fn map_stop_reason(finish: &str) -> &str {
    match finish {
        "tool_calls" => "tool_use",
        "stop" => "end_turn",
        "length" => "max_tokens",
        other => other,
    }
}

/// Extract cache token counts from an OpenAI usage object.
///
/// Recognizes several vendor spellings: DeepSeek's `prompt_cache_hit_tokens` /
/// `prompt_cache_miss_tokens`, OpenAI's `prompt_tokens_details.cached_tokens`,
/// and a top-level `cached_tokens`.
pub fn cache_usage_from_openai(usage: &Value) -> (i64, i64) {
    let cache_read = usage
        .get("prompt_cache_hit_tokens")
        .and_then(Value::as_i64)
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(Value::as_i64)
        })
        .or_else(|| usage.get("cached_tokens").and_then(Value::as_i64))
        .unwrap_or(0);

    let cache_create = usage
        .get("prompt_cache_miss_tokens")
        .and_then(Value::as_i64)
        .or_else(|| {
            usage
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(Value::as_i64)
                .and_then(|cached| {
                    usage
                        .get("prompt_tokens")
                        .and_then(Value::as_i64)
                        .map(|prompt| (prompt - cached).max(0))
                })
        })
        .unwrap_or(0);

    (cache_read, cache_create)
}

// ---------------------------------------------------------------------------
// Streaming SSE state machine: OpenAI chunks → Anthropic SSE events
// ---------------------------------------------------------------------------

/// Stateful converter from OpenAI stream chunks to Anthropic SSE events.
/// `feed` returns Anthropic event strings (`event: <type>\ndata: <json>\n\n`)
/// to emit for each OpenAI chunk; `finish` closes open blocks and terminates
/// with `message_stop`. Block indexing follows the Bun reference: thinking=0,
/// text=1 (when thinking present, else 0), tools start after text.
pub struct OpenAISseConverter {
    message_started: bool,
    thinking_started: bool,
    text_block_started: bool,
    tool_indices: HashSet<usize>,
    message_id: String,
    model: String,
    pending_stop_reason: Option<String>,
    last_usage: Option<Value>,
    finished: bool,
}

impl OpenAISseConverter {
    pub fn new(model: &str) -> Self {
        let message_id = format!("msg_{}", uuid_simple());
        Self {
            message_started: false,
            thinking_started: false,
            text_block_started: false,
            tool_indices: HashSet::new(),
            message_id,
            model: model.to_string(),
            pending_stop_reason: None,
            last_usage: None,
            finished: false,
        }
    }

    /// Feed one OpenAI SSE `data:` payload (parsed JSON). Returns the Anthropic
    /// SSE event strings to emit, in order.
    pub fn feed(&mut self, chunk: &Value) -> Vec<String> {
        let mut events = Vec::new();

        if !self.message_started {
            self.message_started = true;
            events.push(sse_event(
                "message_start",
                json!({
                    "type": "message_start",
                    "message": {
                        "id": self.message_id,
                        "type": "message",
                        "role": "assistant",
                        "model": self.model,
                        "usage": { "input_tokens": 0, "output_tokens": 0 },
                    }
                }),
            ));
        }

        // stream_options.include_usage tail chunk carries the full usage.
        if let Some(usage) = chunk.get("usage") {
            if usage.is_object() {
                self.last_usage = Some(usage.clone());
            }
        }

        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
        else {
            return events;
        };

        if let Some(delta) = choice.get("delta") {
            // Reasoning (thinking) content.
            if let Some(rc) = delta.get("reasoning_content").and_then(Value::as_str) {
                if !rc.is_empty() {
                    if !self.thinking_started {
                        self.thinking_started = true;
                        events.push(sse_event(
                            "content_block_start",
                            json!({
                                "type": "content_block_start",
                                "index": 0,
                                "content_block": { "type": "thinking", "thinking": "" }
                            }),
                        ));
                    }
                    events.push(sse_event(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": 0,
                            "delta": { "type": "thinking_delta", "thinking": rc }
                        }),
                    ));
                }
            }
            if let Some(sig) = delta.get("reasoning_signature").and_then(Value::as_str) {
                if !sig.is_empty() {
                    events.push(sse_event(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": 0,
                            "delta": { "type": "signature_delta", "signature": sig }
                        }),
                    ));
                }
            }

            // Visible text.
            if let Some(text) = delta.get("content").and_then(Value::as_str) {
                if !text.is_empty() {
                    if !self.text_block_started {
                        let text_index = if self.thinking_started { 1 } else { 0 };
                        // Close the thinking block before opening text (Anthropic
                        // requires blocks to be closed before a sibling opens).
                        if self.thinking_started {
                            events.push(sse_event(
                                "content_block_stop",
                                json!({ "type": "content_block_stop", "index": 0 }),
                            ));
                        }
                        self.text_block_started = true;
                        events.push(sse_event(
                            "content_block_start",
                            json!({
                                "type": "content_block_start",
                                "index": text_index,
                                "content_block": { "type": "text", "text": "" }
                            }),
                        ));
                    }
                    let text_index = if self.thinking_started { 1 } else { 0 };
                    events.push(sse_event(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": text_index,
                            "delta": { "type": "text_delta", "text": text }
                        }),
                    ));
                }
            }

            // Tool call fragments.
            if let Some(tcs) = delta.get("tool_calls").and_then(Value::as_array) {
                for tc in tcs {
                    let Some(tc_index) = tc.get("index").and_then(Value::as_i64) else {
                        continue;
                    };
                    let tc_index = tc_index as usize;
                    let block_index = if self.thinking_started { 2 } else { 1 } + tc_index;
                    if !self.tool_indices.contains(&tc_index) {
                        self.tool_indices.insert(tc_index);
                        let id = tc.get("id").and_then(Value::as_str).unwrap_or_default();
                        let name = tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        events.push(sse_event(
                            "content_block_start",
                            json!({
                                "type": "content_block_start",
                                "index": block_index,
                                "content_block": { "type": "tool_use", "id": id, "name": name, "input": {} }
                            }),
                        ));
                    }
                    if let Some(args) = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(Value::as_str)
                    {
                        if !args.is_empty() {
                            events.push(sse_event(
                                "content_block_delta",
                                json!({
                                    "type": "content_block_delta",
                                    "index": block_index,
                                    "delta": { "type": "input_json_delta", "partial_json": args }
                                }),
                            ));
                        }
                    }
                }
            }
        }

        // Finish reason (may arrive in the final content chunk).
        if let Some(fr) = choice.get("finish_reason").and_then(Value::as_str) {
            if !fr.is_empty() && self.pending_stop_reason.is_none() {
                self.pending_stop_reason = Some(map_stop_reason(fr).to_string());
            }
        }

        events
    }

    /// End-of-stream: close open blocks, emit `message_delta` (real usage when
    /// available), and terminate with `message_stop`. Idempotent.
    pub fn finish(&mut self) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;

        let mut events = Vec::new();

        // Close blocks in order: text first (its thinking sibling was already
        // closed at open time), then tools.
        if self.text_block_started {
            let text_index = if self.thinking_started { 1 } else { 0 };
            events.push(sse_event(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": text_index }),
            ));
        } else if self.thinking_started {
            events.push(sse_event(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": 0 }),
            ));
        }

        let mut sorted: Vec<usize> = self.tool_indices.iter().copied().collect();
        sorted.sort_unstable();
        for tc_index in sorted {
            let block_index = if self.thinking_started { 2 } else { 1 } + tc_index;
            events.push(sse_event(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": block_index }),
            ));
        }

        let stop_reason = self
            .pending_stop_reason
            .clone()
            .unwrap_or_else(|| "end_turn".to_string());

        let mut delta_usage = Map::new();
        if self.last_usage.is_some() {
            // Full usage so downstream (mindfs etc.) can read input tokens.
            let (input, output, cache_read, cache_create) = self.usage_tokens();
            delta_usage.insert("output_tokens".to_string(), json!(output));
            delta_usage.insert("input_tokens".to_string(), json!(input));
            delta_usage.insert("cache_read_input_tokens".to_string(), json!(cache_read));
            delta_usage.insert("cache_creation_input_tokens".to_string(), json!(cache_create));
        }
        events.push(sse_event(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": stop_reason, "stop_sequence": null },
                "usage": Value::Object(delta_usage)
            }),
        ));

        events.push(sse_event("message_stop", json!({ "type": "message_stop" })));
        events
    }

    /// Tokens seen during streaming, from the include_usage tail chunk.
    /// Returns `(input, output, cache_read, cache_create)`.
    pub fn usage_tokens(&self) -> (i64, i64, i64, i64) {
        let Some(usage) = &self.last_usage else {
            return (0, 0, 0, 0);
        };
        let input = usage.get("prompt_tokens").and_then(Value::as_i64).unwrap_or(0);
        let output = usage.get("completion_tokens").and_then(Value::as_i64).unwrap_or(0);
        let (cache_read, cache_create) = cache_usage_from_openai(usage);
        (input, output, cache_read, cache_create)
    }
}

/// Build a single Anthropic SSE frame: `event: <type>\ndata: <json>\n\n`.
fn sse_event(event_type: &str, data: Value) -> String {
    format!("event: {event_type}\ndata: {data}\n\n")
}

// ---------------------------------------------------------------------------
// SSE framing helper
// ---------------------------------------------------------------------------

/// Split complete SSE events out of a byte buffer. Handles chunk boundaries:
/// the buffer accumulates until a blank-line terminator (`\n\n`) is found, then
/// that event is drained. `max_events` bounds the number of events returned per
/// call. Incomplete trailing data stays in `buffer`.
pub fn parse_sse_chunks(buffer: &mut Vec<u8>, max_events: usize) -> Vec<String> {
    let mut events = Vec::new();
    loop {
        if events.len() >= max_events {
            break;
        }
        // Find first "\n\n" (or "\r\n\r\n").
        let mut end = None;
        let n = buffer.len();
        let mut i = 0;
        while i + 1 < n {
            if buffer[i] == b'\n' && buffer[i + 1] == b'\n' {
                end = Some(i + 2);
                break;
            }
            i += 1;
        }
        let Some(end) = end else { break };
        let chunk: Vec<u8> = buffer.drain(..end).collect();
        events.push(String::from_utf8_lossy(&chunk).to_string());
    }
    events
}

/// Extract the JSON payload of an `data:` line from a raw SSE event block.
/// Returns `None` when the event has no `data:` line.
pub fn sse_data_payload(event_text: &str) -> Option<&str> {
    event_text.lines().find_map(|line| {
        let line = line.trim();
        line.strip_prefix("data:").map(str::trim).filter(|p| !p.is_empty())
    })
}

fn uuid_simple() -> String {
    use uuid::Uuid;
    Uuid::new_v4().simple().to_string()
}
