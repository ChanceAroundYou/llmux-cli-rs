//! Chat/Anthropic → Responses translation and back-translation.
//!
//! No model-name hardcoding — upstream_api decides routing.

use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// Convert Chat Completions content parts to the Responses input vocabulary.
/// Strings are valid in both APIs; unsupported structured parts become text so
/// a prior tool-result/history turn cannot invalidate the whole request.
fn to_responses_role(role: &str) -> &str {
    match role {
        "user" | "developer" | "system" => role,
        _ => "user",
    }
}

fn to_responses_tools(tools: &Value) -> Value {
    let Some(tools) = tools.as_array() else {
        return tools.clone();
    };
    Value::Array(
        tools
            .iter()
            .filter_map(|tool| match tool.get("type").and_then(Value::as_str) {
                Some("function") if tool.get("function").is_some() => {
                    let function = tool.get("function")?;
                    Some(json!({
                        "type": "function",
                        "name": function.get("name")?,
                        "description": function.get("description").cloned().unwrap_or(Value::Null),
                        "parameters": function.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                    }))
                }
                Some("function") => Some(tool.clone()),
                None if tool.get("name").is_some() => Some(json!({
                    "type": "function",
                    "name": tool.get("name")?,
                    "description": tool.get("description").cloned().unwrap_or(Value::Null),
                    "parameters": tool.get("input_schema").cloned().unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                })),
                _ => None,
            })
            .collect(),
    )
}

fn to_responses_tool_choice(tool_choice: &Value) -> Value {
    // Console Go's Responses endpoint currently accepts only "auto". Keep
    // tools available rather than sending an unsupported forced-choice form.
    match tool_choice {
        Value::String(choice) if choice == "auto" => tool_choice.clone(),
        _ => json!("auto"),
    }
}

fn function_call_item(call_id: &str, name: &str, arguments: Value) -> Value {
    json!({
        "type": "function_call",
        "call_id": call_id,
        "name": name,
        "arguments": arguments.to_string(),
    })
}

fn function_call_output_item(call_id: &str, output: Value) -> Value {
    let output = match output {
        Value::String(_) => output,
        other => Value::String(other.to_string()),
    };
    json!({"type": "function_call_output", "call_id": call_id, "output": output})
}

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
                        Some("text") | Some("input_text") => part
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
                        Some("input_image") | Some("input_file") => Some(part.clone()),
                        _ => serde_json::to_string(part)
                            .ok()
                            .map(|text| json!({"type": "input_text", "text": text})),
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
        _ => Value::String(content.to_string()),
    }
}

/// chat/completions body → responses body
pub fn chat_to_responses(chat_body: &Value, resolved_model: &str) -> Value {
    let mut out = Map::new();
    out.insert("model".to_string(), json!(resolved_model));
    // messages → input
    if let Some(msgs) = chat_body.get("messages").and_then(Value::as_array) {
        let mut input = Vec::new();
        for message in msgs {
            let role = message.get("role").and_then(Value::as_str).unwrap_or("user");
            if role == "tool" {
                let call_id = message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                input.push(function_call_output_item(
                    call_id,
                    message.get("content").cloned().unwrap_or(Value::Null),
                ));
                continue;
            }
            if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    let call_id = call.get("id").and_then(Value::as_str).unwrap_or_default();
                    let name = call
                        .get("function")
                        .and_then(|function| function.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let arguments = call
                        .get("function")
                        .and_then(|function| function.get("arguments"))
                        .and_then(Value::as_str)
                        .and_then(|arguments| serde_json::from_str(arguments).ok())
                        .unwrap_or_else(|| json!({}));
                    input.push(function_call_item(call_id, name, arguments));
                }
            }
            let content = to_responses_content(
                &message.get("content").cloned().unwrap_or(Value::Null),
            );
            if !content.is_null() && !(role == "assistant" && content == Value::String(String::new())) {
                input.push(json!({"role": to_responses_role(role), "content": content}));
            }
        }
        out.insert("input".to_string(), Value::Array(input));
    } else if let Some(inp) = chat_body.get("input") {
        out.insert("input".to_string(), inp.clone());
    }
    if let Some(instr) = chat_body.get("system").or_else(|| chat_body.get("instructions")) {
        out.insert("instructions".to_string(), instr.clone());
    }
    for key in ["temperature", "top_p", "stream", "reasoning"] {
        if let Some(v) = chat_body.get(key) {
            out.insert(key.to_string(), v.clone());
        }
    }
    if let Some(tools) = chat_body.get("tools") {
        out.insert("tools".to_string(), to_responses_tools(tools));
    }
    if let Some(tool_choice) = chat_body.get("tool_choice") {
        out.insert("tool_choice".to_string(), to_responses_tool_choice(tool_choice));
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
        for message in msgs {
            let role = message.get("role").and_then(Value::as_str).unwrap_or("user");
            let content = message.get("content").cloned().unwrap_or(Value::Null);
            let mut content_parts = Vec::new();
            if let Value::Array(blocks) = &content {
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("tool_use") => {
                            let call_id = block.get("id").and_then(Value::as_str).unwrap_or_default();
                            let name = block.get("name").and_then(Value::as_str).unwrap_or_default();
                            input.push(function_call_item(
                                call_id,
                                name,
                                block.get("input").cloned().unwrap_or_else(|| json!({})),
                            ));
                        }
                        Some("tool_result") => {
                            let call_id = block
                                .get("tool_use_id")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            input.push(function_call_output_item(
                                call_id,
                                block.get("content").cloned().unwrap_or(Value::Null),
                            ));
                        }
                        Some("text") => {
                            if let Some(text) = block.get("text") {
                                content_parts.push(json!({"type": "input_text", "text": text}));
                            }
                        }
                        Some("thinking") => {
                            if let Some(thinking) = block.get("thinking") {
                                content_parts.push(json!({"type": "input_text", "text": thinking}));
                            }
                        }
                        Some("image") => content_parts.push(block.clone()),
                        _ => {}
                    }
                }
            } else {
                content_parts.push(content);
            }
            if !content_parts.is_empty() {
                let content = if content_parts.len() == 1 && content_parts[0].is_string() {
                    content_parts.remove(0)
                } else {
                    to_responses_content(&Value::Array(content_parts))
                };
                input.push(json!({"role": to_responses_role(role), "content": content}));
            }
        }
    }
    out.insert("input".to_string(), Value::Array(input));
    for key in ["temperature", "top_p", "stream", "reasoning"] {
        if let Some(v) = anth_body.get(key) {
            out.insert(key.to_string(), v.clone());
        }
    }
    if let Some(tools) = anth_body.get("tools") {
        out.insert("tools".to_string(), to_responses_tools(tools));
    }
    if let Some(tool_choice) = anth_body.get("tool_choice") {
        out.insert("tool_choice".to_string(), to_responses_tool_choice(tool_choice));
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

pub fn extract_responses_function_calls(resp: &Value) -> Vec<Value> {
    resp.get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .filter_map(|item| {
            let call_id = item.get("call_id").and_then(Value::as_str)?;
            let name = item.get("name").and_then(Value::as_str)?;
            let arguments = item.get("arguments").and_then(Value::as_str).unwrap_or("");
            Some(json!({
                "id": call_id,
                "type": "function",
                "function": {"name": name, "arguments": arguments},
            }))
        })
        .collect()
}

pub fn responses_to_anthropic(resp: &Value, model: &str) -> Value {
    let text = extract_responses_text(resp);
    let calls = extract_responses_function_calls(resp);
    let mut content = Vec::new();
    if !text.is_empty() {
        content.push(json!({"type": "text", "text": text}));
    }
    for call in &calls {
        let arguments = call["function"]["arguments"].as_str().unwrap_or("");
        content.push(json!({
            "type": "tool_use",
            "id": call["id"],
            "name": call["function"]["name"],
            "input": serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| json!({})),
        }));
    }
    let usage = resp.get("usage").cloned().unwrap_or_else(|| json!({}));
    let input = usage.get("input_tokens").or_else(|| usage.get("prompt_tokens")).and_then(Value::as_i64).unwrap_or(0);
    let output = usage.get("output_tokens").or_else(|| usage.get("completion_tokens")).and_then(Value::as_i64).unwrap_or(0);
    json!({
        "id": resp.get("id").cloned().unwrap_or_else(|| json!(format!("msg_{}", uuid_simple()))),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": if calls.is_empty() { "end_turn" } else { "tool_use" },
        "stop_sequence": null,
        "usage": {"input_tokens": input, "output_tokens": output},
    })
}

/// responses non-streaming body → chat completions body
pub fn responses_to_chat(resp: &Value, model: &str) -> Value {
    let text = extract_responses_text(resp);
    let tool_calls = extract_responses_function_calls(resp);
    let usage = resp.get("usage").cloned().unwrap_or(json!({}));
    let prompt = usage.get("input_tokens").or_else(|| usage.get("prompt_tokens")).and_then(Value::as_i64).unwrap_or(0);
    let completion = usage.get("output_tokens").or_else(|| usage.get("completion_tokens")).and_then(Value::as_i64).unwrap_or(0);
    let mut message = json!({"role": "assistant", "content": if text.is_empty() { Value::Null } else { Value::String(text) }});
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }
    json!({
        "id": resp.get("id").cloned().unwrap_or(json!(format!("resp_{}", uuid_simple()))),
        "object": "chat.completion",
        "created": resp.get("created").cloned().unwrap_or(json!(0)),
        "model": model,
        "choices": [{"index": 0, "message": message, "finish_reason": if message.get("tool_calls").is_some() { "tool_calls" } else { "stop" }}],
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

#[derive(Clone)]
struct StreamFunctionCall {
    index: usize,
    arguments: String,
    arguments_seen: bool,
}

fn response_function_call(item: &Value) -> Option<(&str, &str, &str, &str)> {
    if item.get("type").and_then(Value::as_str) != Some("function_call") {
        return None;
    }
    let item_id = item.get("id").and_then(Value::as_str)?;
    let call_id = item.get("call_id").and_then(Value::as_str).unwrap_or(item_id);
    let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
    let arguments = item.get("arguments").and_then(Value::as_str).unwrap_or_default();
    Some((item_id, call_id, name, arguments))
}

fn response_event_error(data: &Value) -> String {
    data.get("error")
        .and_then(|error| error.get("message").or_else(|| error.get("code")))
        .and_then(Value::as_str)
        .or_else(|| data.get("message").and_then(Value::as_str))
        .unwrap_or("Responses upstream terminated without a usable completion")
        .to_string()
}

fn incomplete_reason(data: &Value) -> Option<&str> {
    data.get("response")
        .and_then(|response| response.get("incomplete_details"))
        .or_else(|| data.get("incomplete_details"))
        .and_then(|details| details.get("reason"))
        .and_then(Value::as_str)
}

/// Responses SSE → OpenAI Chat SSE
pub struct ResponsesToChatConverter {
    model: String,
    id: String,
    created: i64,
    last_usage: Option<Value>,
    function_calls: HashMap<String, StreamFunctionCall>,
    next_tool_index: usize,
    role_sent: bool,
    text: String,
    incomplete_max_tokens: bool,
    terminal: bool,
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
            function_calls: HashMap::new(),
            next_tool_index: 0,
            role_sent: false,
            text: String::new(),
            incomplete_max_tokens: false,
            terminal: false,
            finished: false,
        }
    }

    fn chunk(&self, delta: Value) -> String {
        format!("data: {}\n\n", json!({
            "id": self.id, "object": "chat.completion.chunk", "created": self.created,
            "model": self.model, "choices": [{"index": 0, "delta": delta, "finish_reason": null}]
        }))
    }

    fn send_role(&mut self, out: &mut Vec<String>) {
        if self.role_sent { return; }
        self.role_sent = true;
        out.push(self.chunk(json!({"role":"assistant","content":null})));
    }

    fn open_call(&mut self, item: &Value, out: &mut Vec<String>) {
        let Some((item_id, call_id, name, arguments)) = response_function_call(item) else { return };
        if self.function_calls.contains_key(item_id) { return; }
        let index = self.next_tool_index;
        self.next_tool_index += 1;
        self.send_role(out);
        self.function_calls.insert(item_id.to_string(), StreamFunctionCall {
            index, arguments: String::new(), arguments_seen: false,
        });
        out.push(self.chunk(json!({"tool_calls":[{
            "index":index, "id":call_id, "type":"function", "function":{"name":name,"arguments":""}
        }]})));
        self.emit_call_arguments(item_id, arguments, out);
    }

    fn emit_call_arguments(&mut self, item_id: &str, arguments: &str, out: &mut Vec<String>) {
        let (index, suffix) = {
            let Some(call) = self.function_calls.get_mut(item_id) else { return };
            let suffix = arguments.strip_prefix(&call.arguments).unwrap_or(arguments).to_string();
            if suffix.is_empty() { return; }
            call.arguments.push_str(&suffix);
            call.arguments_seen = true;
            (call.index, suffix)
        };
        out.push(self.chunk(json!({"tool_calls":[{"index":index,"function":{"arguments":suffix}}]})));
    }

    fn emit_completed_output(&mut self, response: &Value, out: &mut Vec<String>) {
        for item in response.get("output").and_then(Value::as_array).into_iter().flatten() {
            self.open_call(item, out);
            if let Some((item_id, _, _, arguments)) = response_function_call(item) {
                self.emit_call_arguments(item_id, arguments, out);
            }
        }
        let full_text = extract_responses_text(response);
        let suffix = full_text.strip_prefix(&self.text).unwrap_or(&full_text);
        if !suffix.is_empty() {
            self.text.push_str(suffix);
            self.send_role(out);
            out.push(self.chunk(json!({"content":suffix})));
        }
    }

    pub fn is_done(&self) -> bool { self.terminal }

    pub fn usage_tokens(&self) -> (i64, i64) {
        let usage = self.last_usage.as_ref();
        (
            usage.and_then(|u| u.get("input_tokens").or_else(|| u.get("prompt_tokens"))).and_then(Value::as_i64).unwrap_or(0),
            usage.and_then(|u| u.get("output_tokens").or_else(|| u.get("completion_tokens"))).and_then(Value::as_i64).unwrap_or(0),
        )
    }

    pub fn feed(&mut self, event_text: &str) -> Vec<String> {
        let mut out = Vec::new();
        let etype = parse_event_type(event_text);
        let Some(data_str) = sse_data(event_text) else { return out };
        if data_str == "[DONE]" { self.terminal = true; return out; }
        let Ok(data) = serde_json::from_str::<Value>(data_str) else { return out };
        if let Some(usage) = data.get("usage").or_else(|| data.get("response").and_then(|r| r.get("usage"))) {
            if usage.is_object() { self.last_usage = Some(usage.clone()); }
        }
        if etype == "response.output_item.added" {
            self.open_call(&data["item"], &mut out);
        } else if etype == "response.function_call_arguments.delta" {
            if let (Some(item_id), Some(delta)) = (data.get("item_id").and_then(Value::as_str), data.get("delta").and_then(Value::as_str)) {
                self.emit_call_arguments(item_id, &format!("{}{}", self.function_calls.get(item_id).map(|call| call.arguments.as_str()).unwrap_or_default(), delta), &mut out);
            }
        } else if etype == "response.function_call_arguments.done" {
            if let (Some(item_id), Some(arguments)) = (data.get("item_id").and_then(Value::as_str), data.get("arguments").and_then(Value::as_str)) {
                self.emit_call_arguments(item_id, arguments, &mut out);
            }
        } else if etype == "response.output_item.done" {
            self.open_call(&data["item"], &mut out);
            if let Some((item_id, _, _, arguments)) = response_function_call(&data["item"]) {
                self.emit_call_arguments(item_id, arguments, &mut out);
            }
        } else if etype == "response.output_text.delta" {
            if let Some(delta) = data.get("delta").or_else(|| data.get("text")).and_then(Value::as_str) {
                if !delta.is_empty() { self.send_role(&mut out); self.text.push_str(delta); out.push(self.chunk(json!({"content":delta}))); }
            }
        } else if etype == "response.completed" {
            if let Some(response) = data.get("response") { self.emit_completed_output(response, &mut out); }
            self.terminal = true;
        } else if etype == "response.incomplete" {
            self.terminal = true;
            self.incomplete_max_tokens = incomplete_reason(&data) == Some("max_output_tokens");
            if !self.incomplete_max_tokens {
                out.push(format!("data: {}\n\n", json!({"error":{"message":response_event_error(&data),"type":"server_error"}})));
                out.push("data: [DONE]\n\n".to_string());
                self.finished = true;
            }
        } else if etype == "response.failed" || etype == "error" || etype == "response.error" {
            self.terminal = true;
            self.finished = true;
            out.push(format!("data: {}\n\n", json!({"error":{"message":response_event_error(&data),"type":"server_error"}})));
            out.push("data: [DONE]\n\n".to_string());
        }
        out
    }

    pub fn finish(&mut self) -> Vec<String> {
        if self.finished { return Vec::new(); }
        self.finished = true;
        let finish_reason = if self.incomplete_max_tokens { "length" } else if self.function_calls.is_empty() { "stop" } else { "tool_calls" };
        let finish_reason = if self.terminal { finish_reason } else { "length" };
        vec![format!("data: {}\n\n", json!({
            "id":self.id, "object":"chat.completion.chunk", "created":self.created, "model":self.model,
            "choices":[{"index":0,"delta":{},"finish_reason":finish_reason}],
            "usage":self.last_usage.clone().unwrap_or(json!({}))
        })), "data: [DONE]\n\n".to_string()]
    }
}

/// Responses SSE → Anthropic SSE
pub struct ResponsesToAnthropicConverter {
    message_id: String,
    model: String,
    started: bool,
    text_index: Option<usize>,
    text: String,
    next_content_index: usize,
    function_calls: HashMap<String, StreamFunctionCall>,
    last_usage: Option<Value>,
    incomplete_max_tokens: bool,
    terminal: bool,
    finished: bool,
}

impl ResponsesToAnthropicConverter {
    pub fn new(model: &str) -> Self {
        Self {
            message_id: format!("msg_{}", uuid_simple()), model: model.to_string(), started: false,
            text_index: None, text: String::new(), next_content_index: 0, function_calls: HashMap::new(),
            last_usage: None, incomplete_max_tokens: false, terminal: false, finished: false,
        }
    }

    fn start_message(&mut self, out: &mut Vec<String>) {
        if self.started { return; }
        self.started = true;
        out.push(sse_event("message_start", json!({"type":"message_start","message":{"id":self.message_id,"type":"message","role":"assistant","model":self.model,"content":[],"usage":{"input_tokens":0,"output_tokens":0}}})));
    }

    fn start_text(&mut self, out: &mut Vec<String>) -> usize {
        if let Some(index) = self.text_index { return index; }
        self.start_message(out);
        let index = self.next_content_index;
        self.next_content_index += 1;
        self.text_index = Some(index);
        out.push(sse_event("content_block_start", json!({"type":"content_block_start","index":index,"content_block":{"type":"text","text":""}})));
        index
    }

    fn open_call(&mut self, item: &Value, out: &mut Vec<String>) {
        let Some((item_id, call_id, name, arguments)) = response_function_call(item) else { return };
        if self.function_calls.contains_key(item_id) { return; }
        self.start_message(out);
        let index = self.next_content_index;
        self.next_content_index += 1;
        self.function_calls.insert(item_id.to_string(), StreamFunctionCall { index, arguments: String::new(), arguments_seen: false });
        out.push(sse_event("content_block_start", json!({"type":"content_block_start","index":index,"content_block":{"type":"tool_use","id":call_id,"name":name,"input":{}}})));
        self.emit_call_arguments(item_id, arguments, out);
    }

    fn emit_call_arguments(&mut self, item_id: &str, arguments: &str, out: &mut Vec<String>) {
        let Some(call) = self.function_calls.get_mut(item_id) else { return };
        let suffix = arguments.strip_prefix(&call.arguments).unwrap_or(arguments);
        if suffix.is_empty() { return; }
        call.arguments.push_str(suffix);
        call.arguments_seen = true;
        out.push(sse_event("content_block_delta", json!({"type":"content_block_delta","index":call.index,"delta":{"type":"input_json_delta","partial_json":suffix}})));
    }

    fn emit_text(&mut self, text: &str, out: &mut Vec<String>) {
        if text.is_empty() { return; }
        let index = self.start_text(out);
        self.text.push_str(text);
        out.push(sse_event("content_block_delta", json!({"type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":text}})));
    }

    fn emit_completed_output(&mut self, response: &Value, out: &mut Vec<String>) {
        for item in response.get("output").and_then(Value::as_array).into_iter().flatten() {
            self.open_call(item, out);
            if let Some((item_id, _, _, arguments)) = response_function_call(item) {
                self.emit_call_arguments(item_id, arguments, out);
            }
        }
        let full_text = extract_responses_text(response);
        let suffix = full_text.strip_prefix(&self.text).unwrap_or(&full_text).to_string();
        self.emit_text(&suffix, out);
    }

    pub fn is_done(&self) -> bool { self.terminal }

    pub fn usage_tokens(&self) -> (i64, i64) {
        let usage = self.last_usage.as_ref();
        (
            usage.and_then(|u| u.get("input_tokens").or_else(|| u.get("prompt_tokens"))).and_then(Value::as_i64).unwrap_or(0),
            usage.and_then(|u| u.get("output_tokens").or_else(|| u.get("completion_tokens"))).and_then(Value::as_i64).unwrap_or(0),
        )
    }

    pub fn feed(&mut self, event_text: &str) -> Vec<String> {
        let mut out = Vec::new();
        let etype = parse_event_type(event_text);
        let Some(data_str) = sse_data(event_text) else { return out };
        if data_str == "[DONE]" { self.terminal = true; return out; }
        let Ok(data) = serde_json::from_str::<Value>(data_str) else { return out };
        if let Some(usage) = data.get("usage").or_else(|| data.get("response").and_then(|r| r.get("usage"))) {
            if usage.is_object() { self.last_usage = Some(usage.clone()); }
        }
        if etype == "response.output_item.added" {
            self.open_call(&data["item"], &mut out);
        } else if etype == "response.function_call_arguments.delta" {
            if let (Some(item_id), Some(delta)) = (data.get("item_id").and_then(Value::as_str), data.get("delta").and_then(Value::as_str)) {
                let arguments = format!("{}{}", self.function_calls.get(item_id).map(|call| call.arguments.as_str()).unwrap_or_default(), delta);
                self.emit_call_arguments(item_id, &arguments, &mut out);
            }
        } else if etype == "response.function_call_arguments.done" {
            if let (Some(item_id), Some(arguments)) = (data.get("item_id").and_then(Value::as_str), data.get("arguments").and_then(Value::as_str)) {
                self.emit_call_arguments(item_id, arguments, &mut out);
            }
        } else if etype == "response.output_item.done" {
            self.open_call(&data["item"], &mut out);
            if let Some((item_id, _, _, arguments)) = response_function_call(&data["item"]) {
                self.emit_call_arguments(item_id, arguments, &mut out);
            }
        } else if etype == "response.output_text.delta" {
            if let Some(delta) = data.get("delta").or_else(|| data.get("text")).and_then(Value::as_str) { self.emit_text(delta, &mut out); }
        } else if etype == "response.completed" {
            if let Some(response) = data.get("response") { self.emit_completed_output(response, &mut out); }
            self.terminal = true;
        } else if etype == "response.incomplete" {
            self.terminal = true;
            self.incomplete_max_tokens = incomplete_reason(&data) == Some("max_output_tokens");
            if !self.incomplete_max_tokens {
                self.finished = true;
                out.push(sse_event("error", json!({"type":"error","error":{"type":"api_error","message":response_event_error(&data)}})));
            }
        } else if etype == "response.failed" || etype == "error" || etype == "response.error" {
            self.terminal = true;
            self.finished = true;
            out.push(sse_event("error", json!({"type":"error","error":{"type":"api_error","message":response_event_error(&data)}})));
        }
        out
    }

    pub fn finish(&mut self) -> Vec<String> {
        if self.finished { return Vec::new(); }
        self.finished = true;
        let mut out = Vec::new();
        self.start_message(&mut out);
        if let Some(index) = self.text_index { out.push(sse_event("content_block_stop", json!({"type":"content_block_stop","index":index}))); }
        let mut calls = self.function_calls.values().collect::<Vec<_>>();
        calls.sort_by_key(|call| call.index);
        for call in calls { out.push(sse_event("content_block_stop", json!({"type":"content_block_stop","index":call.index}))); }
        let stop_reason = if self.incomplete_max_tokens { "max_tokens" } else if self.function_calls.is_empty() { "end_turn" } else { "tool_use" };
        out.push(sse_event("message_delta", json!({"type":"message_delta","delta":{"stop_reason":stop_reason,"stop_sequence":null},"usage":self.last_usage.clone().unwrap_or(json!({}))})));
        out.push(sse_event("message_stop", json!({"type":"message_stop"})));
        out
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
    fn historical_tool_blocks_use_function_call_outputs() {
        let body = json!({
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "tool", "tool_call_id": "call_1", "content": "ok"}
            ]
        });

        let translated = chat_to_responses(&body, "muse");
        assert_eq!(translated["input"][1], json!({
            "type": "function_call_output", "call_id": "call_1", "output": "ok"
        }));
    }

    #[test]
    fn anthropic_structured_parts_preserve_tool_results() {
        let body = json!({
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "call the tool"},
                {"type": "tool_result", "tool_use_id": "call_1", "content": "ok"}
            ]}]
        });

        let translated = anthropic_to_responses(&body, "muse");
        assert_eq!(translated["input"][0], json!({
            "type": "function_call_output", "call_id": "call_1", "output": "ok"
        }));
        assert_eq!(translated["input"][1]["content"][0], json!({"type": "input_text", "text": "call the tool"}));
    }

    #[test]
    fn chat_tools_use_responses_schema() {
        let body = json!({
            "messages": [{"role": "user", "content": "Calculate 2+2."}],
            "tools": [{"type": "function", "function": {
                "name": "calculator", "description": "Calculates", "parameters": {"type": "object"}
            }}],
            "tool_choice": {"type": "function", "function": {"name": "calculator"}}
        });

        let translated = chat_to_responses(&body, "muse");
        assert_eq!(translated["tools"][0]["type"], "function");
        assert_eq!(translated["tools"][0]["name"], "calculator");
        assert!(translated["tools"][0].get("function").is_none());
        assert_eq!(translated["tool_choice"], "auto");
    }

    #[test]
    fn anthropic_tools_use_responses_schema() {
        let body = json!({
            "messages": [{"role": "user", "content": "Calculate 2+2."}],
            "tools": [{"name": "calculator", "description": "Calculates", "input_schema": {"type": "object"}}],
            "tool_choice": {"type": "tool", "name": "calculator"}
        });

        let translated = anthropic_to_responses(&body, "muse");
        assert_eq!(translated["tools"][0]["type"], "function");
        assert_eq!(translated["tools"][0]["name"], "calculator");
        assert_eq!(translated["tools"][0]["parameters"], json!({"type": "object"}));
        assert_eq!(translated["tool_choice"], "auto");
    }

    #[test]
    fn chat_string_content_is_preserved() {
        let body = json!({"messages": [{"role": "user", "content": "hello"}]});
        let translated = chat_to_responses(&body, "muse");
        assert_eq!(translated["input"][0]["content"], "hello");
    }

    #[test]
    fn anthropic_tool_history_uses_native_responses_items() {
        let body = json!({
            "messages": [
                {"role": "user", "content": "weather in Paris?"},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_abc", "name": "weather", "input": {"city": "Paris"}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_abc", "content": "18C"}]}
            ]
        });

        let translated = anthropic_to_responses(&body, "muse");
        assert_eq!(translated["input"][1], json!({
            "type": "function_call", "call_id": "toolu_abc", "name": "weather", "arguments": "{\"city\":\"Paris\"}"
        }));
        assert_eq!(translated["input"][2], json!({
            "type": "function_call_output", "call_id": "toolu_abc", "output": "18C"
        }));
    }

    #[test]
    fn responses_function_call_becomes_chat_tool_call() {
        let response = json!({
            "id": "resp_123",
            "output": [{"type": "function_call", "id": "item_1", "call_id": "call_xyz", "name": "weather", "arguments": "{\"city\":\"Paris\"}"}],
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });

        let chat = responses_to_chat(&response, "muse");
        assert_eq!(chat["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(chat["choices"][0]["message"]["content"], Value::Null);
        assert_eq!(chat["choices"][0]["message"]["tool_calls"][0], json!({
            "id": "call_xyz", "type": "function", "function": {"name": "weather", "arguments": "{\"city\":\"Paris\"}"}
        }));
    }

    #[test]
    fn responses_function_call_becomes_anthropic_tool_use() {
        let response = json!({
            "id": "resp_123",
            "output": [{"type": "function_call", "id": "item_1", "call_id": "call_xyz", "name": "weather", "arguments": "{\"city\":\"Paris\"}"}],
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });

        let anthropic = responses_to_anthropic(&response, "muse");
        assert_eq!(anthropic["stop_reason"], "tool_use");
        assert_eq!(anthropic["content"][0], json!({
            "type": "tool_use", "id": "call_xyz", "name": "weather", "input": {"city": "Paris"}
        }));
    }

    #[test]
    fn responses_stream_function_call_becomes_anthropic_tool_use() {
        let mut converter = ResponsesToAnthropicConverter::new("muse");
        let frames = [
            "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"item_1\",\"call_id\":\"call_xyz\",\"name\":\"weather\",\"arguments\":\"\"}}\n\n",
            "event: response.function_call_arguments.delta\ndata: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_1\",\"delta\":\"{\\\"city\\\":\"}\n\n",
            "event: response.function_call_arguments.delta\ndata: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_1\",\"delta\":\"\\\"Paris\\\"}\"}\n\n",
        ];
        let output = frames.into_iter().flat_map(|frame| converter.feed(frame)).collect::<String>() + &converter.finish().concat();
        assert!(output.contains("\"type\":\"tool_use\""));
        assert!(output.contains("\"id\":\"call_xyz\""));
        assert!(output.contains("\"name\":\"weather\""));
        assert!(output.contains("\"type\":\"input_json_delta\""));
        assert!(output.contains("city"));
        assert!(output.contains("\"stop_reason\":\"tool_use\""));
    }

    #[test]
    fn responses_stream_function_call_becomes_chat_tool_call() {
        let mut converter = ResponsesToChatConverter::new("muse");
        let added = "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"id\":\"item_1\",\"call_id\":\"call_xyz\",\"name\":\"weather\",\"arguments\":\"\"}}\n\n";
        let delta = "event: response.function_call_arguments.delta\ndata: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_1\",\"delta\":\"{\\\"city\\\":\\\"Paris\\\"}\"}\n\n";
        let completed = "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}}\n\n";
        let output = [added, delta, completed].into_iter().flat_map(|frame| converter.feed(frame)).collect::<String>() + &converter.finish().concat();
        assert!(output.contains("\"id\":\"call_xyz\""));
        assert!(output.contains("\"arguments\":\"{\\\"city\\\":\\\"Paris\\\"}\""));
        assert!(output.contains("\"finish_reason\":\"tool_calls\""));
        assert_eq!(output.matches("data: [DONE]").count(), 1);
    }

    #[test]
    fn argument_done_is_not_visible_text_and_flushes_once() {
        let mut converter = ResponsesToAnthropicConverter::new("muse");
        let added = "event: response.output_item.added\ndata: {\"item\":{\"type\":\"function_call\",\"id\":\"item_1\",\"call_id\":\"call_xyz\",\"name\":\"weather\",\"arguments\":\"\"}}\n\n";
        let done = "event: response.function_call_arguments.done\ndata: {\"item_id\":\"item_1\",\"arguments\":\"{\\\"city\\\":\\\"Paris\\\"}\"}\n\n";
        let completed = "event: response.completed\ndata: {\"response\":{\"output\":[{\"type\":\"function_call\",\"id\":\"item_1\",\"call_id\":\"call_xyz\",\"name\":\"weather\",\"arguments\":\"{\\\"city\\\":\\\"Paris\\\"}\"}]}}\n\n";
        let output = [added, done, completed].into_iter().flat_map(|frame| converter.feed(frame)).collect::<String>() + &converter.finish().concat();
        assert_eq!(output.matches("input_json_delta").count(), 1);
        assert!(!output.contains("\"type\":\"text_delta\""));
    }

    #[test]
    fn completed_output_fills_missing_tool_call_and_uses_unique_block_indexes() {
        let mut converter = ResponsesToAnthropicConverter::new("muse");
        let text = "event: response.output_text.delta\ndata: {\"delta\":\"Preamble.\"}\n\n";
        let completed = "event: response.completed\ndata: {\"response\":{\"output\":[{\"type\":\"function_call\",\"id\":\"item_1\",\"call_id\":\"call_xyz\",\"name\":\"weather\",\"arguments\":\"{\\\"city\\\":\\\"Paris\\\"}\"}]}}\n\n";
        let output = [text, completed].into_iter().flat_map(|frame| converter.feed(frame)).collect::<String>() + &converter.finish().concat();
        assert!(output.contains("\"type\":\"tool_use\""));
        assert!(output.contains("\"index\":0"));
        assert!(output.contains("\"index\":1"));
        assert!(output.contains("\"type\":\"text\""));
        assert!(output.contains("\"type\":\"tool_use\""));
    }

    #[test]
    fn incomplete_output_maps_to_anthropic_max_tokens() {
        let mut converter = ResponsesToAnthropicConverter::new("muse");
        let incomplete = "event: response.incomplete\ndata: {\"response\":{\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n";
        let output = converter.feed(incomplete).into_iter().chain(converter.finish()).collect::<String>();
        assert!(output.contains("\"stop_reason\":\"max_tokens\""));
    }

    #[test]
    fn text_only_chat_stream_sends_assistant_role() {
        let mut converter = ResponsesToChatConverter::new("muse");
        let delta = "event: response.output_text.delta\ndata: {\"delta\":\"hello\"}\n\n";
        let output = converter.feed(delta).concat();
        assert!(output.contains("\"role\":\"assistant\""));
    }

    #[test]
    fn responses_text_stream_closes_the_opened_anthropic_block() {
        let mut converter = ResponsesToAnthropicConverter::new("muse");
        let delta = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n";
        let output = converter.feed(delta).into_iter().chain(converter.finish()).collect::<String>();

        assert!(output.contains("event: message_start"));
        assert!(output.contains("event: content_block_start"));
        assert!(output.contains("event: content_block_stop"));
        assert!(output.contains("event: message_stop"));
    }

    #[test]
    fn completed_anthropic_stream_starts_before_final_text_block() {
        let mut converter = ResponsesToAnthropicConverter::new("muse");
        let completed = "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"hello\"}]}]}}\n\n";
        let output = converter.feed(completed).into_iter().chain(converter.finish()).collect::<String>();

        assert!(output.starts_with("event: message_start"));
        assert!(output.contains("event: content_block_start"));
        assert!(output.contains("event: content_block_stop"));
    }
}
