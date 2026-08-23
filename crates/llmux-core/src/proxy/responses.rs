//! Chat/Anthropic → Responses translation and back-translation.
//!
//! No model-name hardcoding — upstream_api decides routing.

use serde_json::{json, Map, Value};

/// Convert Chat Completions content parts to the Responses input vocabulary.
/// Strings are valid in both APIs; structured parts are not.
fn to_responses_content(content: &Value) -> Value {
    match content {
        Value::String(_) => content.clone(),
        Value::Array(parts) => {
            let parts: Vec<Value> = parts
                .iter()
                .filter_map(|part| {
                    if part.is_string() {
                        return Some(json!({"type": "input_text", "text": part}));
                    }
                    match part.get("type").and_then(Value::as_str) {
                        Some("text") => part
                            .get("text")
                            .map(|text| json!({"type": "input_text", "text": text})),
                        Some("image_url") => {
                            let image = part.get("image_url")?;
                            let url = image.get("url")?;
                            let mut input = json!({"type": "input_image", "image_url": url});
                            if let Some(detail) = image.get("detail") {
                                input["detail"] = detail.clone();
                            }
                            Some(input)
                        }
                        Some("input_text") | Some("input_image") | Some("input_file") => {
                            Some(part.clone())
                        }
                        _ => Some(part.clone()),
                    }
                })
                .collect();
            if parts.is_empty() {
                Value::String(String::new())
            } else {
                Value::Array(parts)
            }
        }
        Value::Null => Value::String(String::new()),
        _ => content.clone(),
    }
}

/// chat/completions body → responses body
pub fn chat_to_responses(chat_body: &Value, resolved_model: &str) -> Value {
    let mut out = Map::new();
    out.insert("model".to_string(), json!(resolved_model));
    // messages → input
    if let Some(msgs) = chat_body.get("messages").and_then(Value::as_array) {
        // responses input is an array of {role, content}
        let input: Vec<Value> = msgs
            .iter()
            .map(|m| {
                let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
                let content = to_responses_content(
                    &m.get("content").cloned().unwrap_or(Value::Null),
                );
                // reasoning / tool_calls are passed through as-is in input
                let mut obj = json!({"role": role, "content": content});
                if let Some(tc) = m.get("tool_calls") {
                    obj["tool_calls"] = tc.clone();
                }
                if let Some(rc) = m.get("reasoning_content") {
                    obj["reasoning_content"] = rc.clone();
                }
                obj
            })
            .collect();
        out.insert("input".to_string(), Value::Array(input));
    } else if let Some(inp) = chat_body.get("input") {
        out.insert("input".to_string(), inp.clone());
    }
    if let Some(instr) = chat_body.get("system").or_else(|| chat_body.get("instructions")) {
        out.insert("instructions".to_string(), instr.clone());
    }
    for key in ["temperature", "top_p", "stream", "tools", "tool_choice", "reasoning"] {
        if let Some(v) = chat_body.get(key) {
            out.insert(key.to_string(), v.clone());
        }
    }
    // max_tokens → max_output_tokens
    if let Some(v) = chat_body
        .get("max_output_tokens")
        .or_else(|| chat_body.get("max_tokens"))
        .or_else(|| chat_body.get("max_completion_tokens"))
    {
        out.insert("max_output_tokens".to_string(), v.clone());
    }
    if let Some(v) = chat_body.get("stream_options") {
        out.insert("stream_options".to_string(), v.clone());
    } else if chat_body.get("stream").and_then(Value::as_bool) == Some(true) {
        out.insert("stream_options".to_string(), json!({"include_usage": true}));
    }
    Value::Object(out)
}

/// anthropic messages body → responses body
pub fn anthropic_to_responses(anth_body: &Value, resolved_model: &str) -> Value {
    let mut out = Map::new();
    out.insert("model".to_string(), json!(resolved_model));
    let mut input: Vec<Value> = Vec::new();
    // system → instructions or first system input
    if let Some(sys) = anth_body.get("system") {
        match sys {
            Value::String(s) if !s.is_empty() => {
                out.insert("instructions".to_string(), json!(s));
            }
            Value::Array(arr) => {
                let txt: String = arr
                    .iter()
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n");
                if !txt.is_empty() {
                    out.insert("instructions".to_string(), json!(txt));
                }
            }
            _ => {}
        }
    }
    if let Some(msgs) = anth_body.get("messages").and_then(Value::as_array) {
        for m in msgs {
            let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
            let content = m.get("content").cloned().unwrap_or(Value::Null);
            // Flatten anthropic content blocks to string for responses input
            let content_val = match &content {
                Value::Array(blocks) => {
                    let parts: Vec<Value> = blocks
                        .iter()
                        .filter_map(|b| {
                            let t = b.get("type").and_then(Value::as_str).unwrap_or("");
                            match t {
                                "text" => b.get("text").cloned(),
                                "thinking" => b.get("thinking").and_then(Value::as_str).map(|s| json!(s)),
                                "tool_use" => Some(json!(b)),
                                "tool_result" => Some(json!(b)),
                                "image" => Some(b.clone()),
                                _ => None,
                            }
                        })
                        .collect();
                    if parts.len() == 1 && parts[0].is_string() {
                        parts[0].clone()
                    } else if parts.is_empty() {
                        Value::String(String::new())
                    } else {
                        Value::Array(parts)
                    }
                }
                _ => content.clone(),
            };
            input.push(json!({
                "role": role,
                "content": to_responses_content(&content_val),
            }));
        }
    }
    out.insert("input".to_string(), Value::Array(input));
    for key in ["temperature", "top_p", "stream", "tools", "tool_choice", "reasoning"] {
        if let Some(v) = anth_body.get(key) {
            out.insert(key.to_string(), v.clone());
        }
    }
    if let Some(thinking) = anth_body.get("thinking") {
        // thinking.budget_tokens → reasoning effort
        out.insert("reasoning".to_string(), thinking.clone());
        if anth_body.get("max_tokens").is_none() {
            if let Some(b) = thinking.get("budget_tokens").and_then(Value::as_i64) {
                out.insert("max_output_tokens".to_string(), json!(b));
            }
        }
    }
    if let Some(v) = anth_body.get("max_tokens") {
        out.insert("max_output_tokens".to_string(), v.clone());
    }
    if let Some(v) = anth_body.get("stop_sequences") {
        out.insert("stop".to_string(), v.clone());
    }
    if anth_body.get("stream").and_then(Value::as_bool) == Some(true) {
        out.insert("stream_options".to_string(), json!({"include_usage": true}));
    }
    Value::Object(out)
}

pub fn extract_responses_text(resp: &Value) -> String {
    let mut text = String::new();
    if let Some(output) = resp.get("output").and_then(Value::as_array) {
        for item in output {
            if item.get("type").and_then(Value::as_str) == Some("reasoning") { continue; }
            if let Some(content) = item.get("content").and_then(Value::as_array) {
                for c in content {
                    if c.get("type").and_then(Value::as_str) == Some("output_text") {
                        if let Some(t) = c.get("text").and_then(Value::as_str) { text.push_str(t); }
                    }
                }
            }
            // some providers use item.content as string
            if text.is_empty() {
                if let Some(t) = item.get("content").and_then(Value::as_str) { text.push_str(t); }
            }
        }
    }
    if text.is_empty() {
        if let Some(t) = resp.get("output_text").and_then(Value::as_str) { text = t.to_string(); }
        else if let Some(t) = resp.get("choices").and_then(|c| c.get(0)).and_then(|c| c.get("message")).and_then(|m| m.get("content")).and_then(Value::as_str) { text = t.to_string(); }
        else if let Some(v) = resp.get("content") { text = v.as_str().unwrap_or("").to_string(); }
    }
    text
}

/// responses non-streaming body → chat completions body
pub fn responses_to_chat(resp: &Value, model: &str) -> Value {
    let text = extract_responses_text(resp);
    let usage = resp.get("usage").cloned().unwrap_or(json!({}));
    let prompt = usage.get("input_tokens").or_else(|| usage.get("prompt_tokens")).and_then(Value::as_i64).unwrap_or(0);
    let completion = usage.get("output_tokens").or_else(|| usage.get("completion_tokens")).and_then(Value::as_i64).unwrap_or(0);
    json!({
        "id": resp.get("id").cloned().unwrap_or(json!(format!("resp_{}", uuid_simple()))),
        "object": "chat.completion",
        "created": resp.get("created").cloned().unwrap_or(json!(0)),
        "model": model,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": prompt, "completion_tokens": completion, "total_tokens": prompt + completion}
    })
}

/// Detect if an error body suggests the upstream doesn't support /responses
pub fn is_responses_unsupported(error_body: &str) -> bool {
    let lower = error_body.to_lowercase();
    lower.contains("unknown endpoint")
        || lower.contains("unsupported")
        || lower.contains("not found")
        || (lower.contains("404") && lower.contains("responses"))
        || lower.contains("no such")
        || lower.contains("invalid endpoint")
        || lower.contains("is not a registered")
        || lower.contains("not a registered api route")
}

// ---------------------------------------------------------------------------
// Streaming converters: Responses SSE → Chat SSE / Anthropic SSE
// ---------------------------------------------------------------------------

/// Parse SSE event type and data payload
fn parse_event_type(event_text: &str) -> &str {
    for line in event_text.lines() {
        let t = line.trim();
        if let Some(v) = t.strip_prefix("event:") {
            return v.trim();
        }
    }
    ""
}

fn sse_data(event_text: &str) -> Option<&str> {
    for line in event_text.lines() {
        let t = line.trim();
        if let Some(v) = t.strip_prefix("data:") {
            let p = v.trim();
            if !p.is_empty() {
                return Some(p);
            }
        }
    }
    None
}

/// Responses SSE → OpenAI Chat SSE
pub struct ResponsesToChatConverter {
    model: String,
    id: String,
    created: i64,
    last_usage: Option<Value>,
    finished: bool,
}

impl ResponsesToChatConverter {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            id: format!("chatcmpl_{}", uuid_simple()),
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
            last_usage: None,
            finished: false,
        }
    }

    pub fn feed(&mut self, event_text: &str) -> Vec<String> {
        let mut out = Vec::new();
        let etype = parse_event_type(event_text);
        let Some(data_str) = sse_data(event_text) else { return out };
        if data_str == "[DONE]" {
            out.push("data: [DONE]\n\n".to_string());
            self.finished = true;
            return out;
        }
        let Ok(data) = serde_json::from_str::<Value>(data_str) else { return out };
        // capture usage if present
        if let Some(u) = data.get("usage").or_else(|| data.get("response").and_then(|r| r.get("usage"))) {
            if u.is_object() {
                self.last_usage = Some(u.clone());
            }
        }
        // responses delta events
        let mut delta_text: Option<String> = None;
        if etype.contains("output_text.delta") {
            delta_text = data.get("delta").and_then(Value::as_str).map(|s| s.to_string())
                .or_else(|| data.get("text").and_then(Value::as_str).map(|s| s.to_string()));
        } else if etype.contains("delta") || etype.is_empty() {
            // generic: try delta/text/content fields
            delta_text = data.get("delta").and_then(Value::as_str).map(|s| s.to_string())
                .or_else(|| data.get("text").and_then(Value::as_str).map(|s| s.to_string()))
                .or_else(|| data.get("content").and_then(Value::as_str).map(|s| s.to_string()))
                .or_else(|| {
                    data.get("choices").and_then(|c| c.get(0)).and_then(|ch| ch.get("delta")).and_then(|d| d.get("content")).and_then(Value::as_str).map(|s| s.to_string())
                });
        }
        if let Some(t) = delta_text {
            if !t.is_empty() {
                let chunk = json!({
                    "id": self.id,
                    "object": "chat.completion.chunk",
                    "created": self.created,
                    "model": self.model,
                    "choices": [{"index": 0, "delta": {"content": t}, "finish_reason": null}]
                });
                out.push(format!("data: {}\n\n", chunk));
            }
        }
        if etype.contains("response.completed") || etype.contains("completed") {
            // capture usage from completed event
            if let Some(u) = data.get("response").and_then(|r| r.get("usage")).or_else(|| data.get("usage")) {
                self.last_usage = Some(u.clone());
            }
            let finish = json!({
                "id": self.id,
                "object": "chat.completion.chunk",
                "created": self.created,
                "model": self.model,
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                "usage": self.last_usage.clone().unwrap_or(json!({}))
            });
            out.push(format!("data: {}\n\n", finish));
            out.push("data: [DONE]\n\n".to_string());
            self.finished = true;
        } else if etype.contains("failed") || etype.contains("error") {
            // upstream error — let caller handle
        }
        out
    }

    pub fn finish(&mut self) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let chunk = json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            "usage": self.last_usage.clone().unwrap_or(json!({}))
        });
        vec![format!("data: {}\n\n", chunk), "data: [DONE]\n\n".to_string()]
    }
}

/// Responses SSE → Anthropic SSE
pub struct ResponsesToAnthropicConverter {
    message_id: String,
    model: String,
    started: bool,
    finished: bool,
    last_usage: Option<Value>,
}

impl ResponsesToAnthropicConverter {
    pub fn new(model: &str) -> Self {
        Self {
            message_id: format!("msg_{}", uuid_simple()),
            model: model.to_string(),
            started: false,
            finished: false,
            last_usage: None,
        }
    }

    pub fn feed(&mut self, event_text: &str) -> Vec<String> {
        let mut out = Vec::new();
        let etype = parse_event_type(event_text);
        let Some(data_str) = sse_data(event_text) else { return out };
        if data_str == "[DONE]" { return out; }
        let Ok(data) = serde_json::from_str::<Value>(data_str) else { return out };
        if let Some(u) = data.get("usage").or_else(|| data.get("response").and_then(|r| r.get("usage"))) {
            if u.is_object() { self.last_usage = Some(u.clone()); }
        }
        // responses completed carries final output; if we never emitted deltas, emit full text now
        if etype.contains("response.completed") {
            if let Some(resp) = data.get("response") {
                if let Some(u) = resp.get("usage") { self.last_usage = Some(u.clone()); }
                let full = extract_responses_text(resp);
                if !full.is_empty() && !self.started {
                    self.started = true;
                    out.push(sse_event("message_start", json!({"type":"message_start","message":{"id": self.message_id, "type":"message","role":"assistant","model": self.model, "content":[],"usage":{"input_tokens":0,"output_tokens":0}}})));
                    out.push(sse_event("content_block_start", json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}})));
                    out.push(sse_event("content_block_delta", json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text": full}})));
                }
            }
            return out;
        }
        let mut delta_text: Option<String> = None;
        if etype.contains("output_text.delta") {
            delta_text = data.get("delta").and_then(Value::as_str).map(|s| s.to_string())
                .or_else(|| data.get("text").and_then(Value::as_str).map(|s| s.to_string()));
        } else if etype.contains("delta") || etype.is_empty() {
            delta_text = data.get("delta").and_then(Value::as_str).map(|s| s.to_string())
                .or_else(|| data.get("text").and_then(Value::as_str).map(|s| s.to_string()));
        }
        if let Some(t) = delta_text {
            if !t.is_empty() {
                if !self.started {
                    self.started = true;
                    out.push(sse_event("message_start", json!({"type":"message_start","message":{"id": self.message_id, "type":"message","role":"assistant","model": self.model, "content":[],"usage":{"input_tokens":0,"output_tokens":0}}})));
                    out.push(sse_event("content_block_start", json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}})));
                }
                out.push(sse_event("content_block_delta", json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text": t}})));
            }
        }
        out
    }

    pub fn finish(&mut self) -> Vec<String> {
        if self.finished { return Vec::new(); }
        self.finished = true;
        vec![
            sse_event("content_block_stop", json!({"type":"content_block_stop","index":0})),
            sse_event("message_delta", json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage": self.last_usage.clone().unwrap_or(json!({}))})),
            sse_event("message_stop", json!({"type":"message_stop"})),
        ]
    }
}

fn sse_event(event_type: &str, data: Value) -> String {
    format!("event: {}\ndata: {}\n\n", event_type, data)
}

fn uuid_simple() -> String {
    use uuid::Uuid;
    Uuid::new_v4().simple().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_content_parts_use_responses_vocabulary() {
        let body = json!({
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "describe this"},
                {"type": "image_url", "image_url": {"url": "https://example.com/a.png", "detail": "auto"}}
            ]}]
        });

        let translated = chat_to_responses(&body, "muse");
        let content = &translated["input"][0]["content"];
        assert_eq!(content[0], json!({"type": "input_text", "text": "describe this"}));
        assert_eq!(content[1], json!({
            "type": "input_image",
            "image_url": "https://example.com/a.png",
            "detail": "auto",
        }));
    }

    #[test]
    fn anthropic_structured_parts_are_not_dropped() {
        let body = json!({
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "call the tool"},
                {"type": "tool_result", "tool_use_id": "call_1", "content": "ok"}
            ]}]
        });

        let translated = anthropic_to_responses(&body, "muse");
        let content = &translated["input"][0]["content"];
        assert_eq!(content[0], json!({"type": "input_text", "text": "call the tool"}));
        assert_eq!(content[1]["type"], "tool_result");
    }

    #[test]
    fn chat_string_content_is_preserved() {
        let body = json!({"messages": [{"role": "user", "content": "hello"}]});
        let translated = chat_to_responses(&body, "muse");
        assert_eq!(translated["input"][0]["content"], "hello");
    }
}
