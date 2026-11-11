//! Chat/Anthropic → Responses translation and back-translation.
//!
//! No model-name hardcoding — upstream_api decides routing.

use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

/// 上游 Responses 实现对 `max_output_tokens < 16` 直接 400
/// (`integer below minimum value. Expected a value >= 16`)。Claude Desktop 的
/// 连通性探测会发 `max_tokens: 1`，原样转发会让整条聚合链路假失败 —— 只是个
/// 上限、不是预留额度，抬到下限不影响语义。同 `probe::PROBE_MAX_TOKENS`。
const MIN_MAX_OUTPUT_TOKENS: i64 = 16;

fn clamp_max_output_tokens(v: &Value) -> Value {
    match v.as_i64() {
        Some(n) if n < MIN_MAX_OUTPUT_TOKENS => json!(MIN_MAX_OUTPUT_TOKENS),
        _ => v.clone(),
    }
}

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
                        "strict": false,
                    }))
                }
                Some("function") => Some(tool.clone()),
                None if tool.get("name").is_some() => Some(json!({
                    "type": "function",
                    "name": tool.get("name")?,
                    "description": tool.get("description").cloned().unwrap_or(Value::Null),
                    "parameters": tool.get("input_schema").cloned().unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                    "strict": false,
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

/// Keep ordinary transcript messages out of a batch of pending tool calls.
/// Some Responses providers reject a message inserted after several
/// `function_call` items but before their matching `function_call_output`s.
/// Anthropic histories can contain such interleaving, so defer those messages
/// until every call in the batch has a result.
fn group_responses_tool_history(input: Vec<Value>) -> Vec<Value> {
    let mut ordered = Vec::with_capacity(input.len());
    let mut pending = HashSet::new();
    let mut deferred = Vec::new();

    for item in input {
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                if let Some(call_id) = item.get("call_id").and_then(Value::as_str) {
                    pending.insert(call_id.to_string());
                }
                ordered.push(item);
            }
            Some("function_call_output") => {
                if let Some(call_id) = item.get("call_id").and_then(Value::as_str) {
                    pending.remove(call_id);
                }
                ordered.push(item);
                if pending.is_empty() {
                    ordered.append(&mut deferred);
                }
            }
            _ if !pending.is_empty() => deferred.push(item),
            _ => ordered.push(item),
        }
    }

    ordered.append(&mut deferred);
    ordered
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
        out.insert("max_output_tokens".to_string(), clamp_max_output_tokens(v));
    }
    // stream_options is a chat/completions parameter; the Responses API rejects it
    // outright ("Unknown parameter: 'stream_options.include_usage'") and always
    // reports usage on its response.completed event, so there is nothing to ask for.
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
    out.insert(
        "input".to_string(),
        Value::Array(group_responses_tool_history(input)),
    );
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
        // thinking.budget_tokens → reasoning.effort.
        // 只搬 effort：Responses API 不认 budget_tokens（实测
        // `Unknown parameter: 'reasoning.budget_tokens'` → 400），而它认
        // 的 effort 已经 packed 在下面循环的 key 白名单里。
        let disabled = thinking.get("type").and_then(Value::as_str) == Some("disabled");
        if !disabled {
            if let Some(effort) = super::anthropic_openai::effort_from_thinking_budget(thinking) {
                out.insert("reasoning".to_string(), json!({ "effort": effort }));
            }
            // Anthropic 的 max_tokens 不含 thinking 预算，只给 budget 时补上输出上限。
            if anth_body.get("max_tokens").is_none() {
                if let Some(b) = thinking.get("budget_tokens").and_then(Value::as_i64) {
                    out.insert("max_output_tokens".to_string(), json!(b));
                }
            }
        }
    }
    if let Some(v) = anth_body.get("max_tokens") {
        out.insert("max_output_tokens".to_string(), clamp_max_output_tokens(v));
    }
    // Anthropic Messages accepts `stop_sequences`, but the Responses API does
    // not accept the Chat Completions `stop` parameter. Forwarding it causes
    // otherwise valid requests to fail with a generic invalid-parameters 400.
    // stream_options is a chat/completions parameter; the Responses API rejects it
    // outright ("Unknown parameter: 'stream_options.include_usage'") and always
    // reports usage on its response.completed event, so there is nothing to ask for.
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
    let raw_input = usage.get("input_tokens").or_else(|| usage.get("prompt_tokens")).and_then(Value::as_i64).unwrap_or(0);
    let output = usage.get("output_tokens").or_else(|| usage.get("completion_tokens")).and_then(Value::as_i64).unwrap_or(0);
    let (cache_read, cache_create) = super::anthropic_openai::cache_usage_from_openai(&usage);
    let input = (raw_input - cache_read).max(0);
    json!({
        "id": resp.get("id").cloned().unwrap_or_else(|| json!(format!("msg_{}", uuid_simple()))),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": if calls.is_empty() { "end_turn" } else { "tool_use" },
        "stop_sequence": null,
        "usage": {"input_tokens": input, "output_tokens": output, "cache_read_input_tokens": cache_read, "cache_creation_input_tokens": cache_create},
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
// ---------------------------------------------------------------------------
// /v1/responses ingress conversions — Responses ⇄ Chat/Messages request and
// response directions, used when a Responses-ingress client reaches a
// non-Responses upstream (dispatch_with_conversion).
// ---------------------------------------------------------------------------

/// Flatten responses content parts into a plain string — chat upstreams expect
/// string content on messages.
///
/// Accepts `input_text` (user/system turns) and `output_text` (assistant turns)
/// alongside legacy `text`. Codex sends nothing but `input_text`/`output_text`;
/// filtering on `text` alone dropped every part of its history, turning a
/// 148 KB request into a handful of null-content messages that upstreams reject
/// with `messages.0.content` / `messages[8].role`.
fn flatten_content(v: Value) -> Value {
    let Some(parts) = v.as_array() else { return v; };
    let mut s = String::new();
    for p in parts {
        let kind = p.get("type").and_then(Value::as_str).unwrap_or("text");
        if matches!(kind, "text" | "input_text" | "output_text") {
            if let Some(t) = p.get("text").and_then(Value::as_str) { s.push_str(t); }
        }
    }
    Value::String(s)
}

/// Tool definitions flattened out of a Responses request for a chat-style
/// upstream, plus the names declared `type:"custom"`.
struct FlattenedTools {
    /// Chat-format `{"type":"function","function":{...}}` entries.
    tools: Vec<Value>,
    custom_names: HashSet<String>,
}

/// Flatten one Responses tool entry (optionally inside a namespace) into the
/// chat vocabulary.
///
/// Codex wraps its tools in `additional_tools` items shaped
/// `{"type":"namespace","name":"functions","tools":[...]}`; the model sees and
/// calls them by bare name (`exec`, `wait`, …), so namespaces are dropped.
/// `type:"custom"` tools (raw-text tools such as `exec`) become functions that
/// take a single `input` string — the closest chat representation.
fn push_chat_tool(tool: &Value, out: &mut FlattenedTools, seen: &mut HashSet<String>) {
    if tool.get("type").and_then(Value::as_str) == Some("namespace") {
        for child in tool.get("tools").and_then(Value::as_array).into_iter().flatten() {
            push_chat_tool(child, out, seen);
        }
        return;
    }
    let Some(name) = tool.get("name").and_then(Value::as_str) else { return; };
    if name.is_empty() || !seen.insert(name.to_string()) {
        return;
    }
    let is_custom = tool.get("type").and_then(Value::as_str) == Some("custom");
    let parameters = if is_custom {
        json!({
            "type": "object",
            "properties": {"input": {"type": "string", "description": "Raw tool input"}},
            "required": ["input"],
        })
    } else {
        tool.get("parameters").cloned().unwrap_or(json!({}))
    };
    let mut function = Map::new();
    function.insert("name".into(), json!(name));
    function.insert("description".into(), tool.get("description").cloned().unwrap_or(Value::Null));
    function.insert("parameters".into(), parameters);
    if let Some(strict) = tool.get("strict") {
        function.insert("strict".into(), strict.clone());
    }
    out.tools.push(json!({"type": "function", "function": Value::Object(function)}));
    if is_custom {
        out.custom_names.insert(name.to_string());
    }
}

/// Collect tools from both the top-level `tools` array and the
/// `additional_tools` input items Codex uses.
fn collect_tools(v: &Value) -> FlattenedTools {
    let mut out = FlattenedTools { tools: Vec::new(), custom_names: HashSet::new() };
    let mut seen = HashSet::new();
    for tool in v.get("tools").and_then(Value::as_array).into_iter().flatten() {
        push_chat_tool(tool, &mut out, &mut seen);
    }
    for item in v.get("input").and_then(Value::as_array).into_iter().flatten() {
        if item.get("type").and_then(Value::as_str) != Some("additional_tools") {
            continue;
        }
        for tool in item.get("tools").and_then(Value::as_array).into_iter().flatten() {
            push_chat_tool(tool, &mut out, &mut seen);
        }
    }
    out
}

/// Names the request declared as `type:"custom"`. The response direction needs
/// them to emit `custom_tool_call` instead of `function_call` — Codex's
/// custom-tool handlers (e.g. `exec`) accept only raw-text input and error out
/// on a function-call payload.
pub fn responses_custom_tool_names(v: &Value) -> HashSet<String> {
    collect_tools(v).custom_names
}

/// Map Responses/Codex roles onto the chat role vocabulary. `developer` carries
/// the same content as a system prompt; most OpenAI-compatible validators
/// (sglang, DeepSeek behind New API) reject it or reject a null-content user
/// message, so anything unknown becomes a user turn.
fn chat_role_for(role: &str) -> &'static str {
    match role {
        "assistant" => "assistant",
        "system" | "developer" => "system",
        "tool" | "function" => "tool",
        _ => "user",
    }
}

/// Append one assistant `tool_calls` entry, merging into the trailing assistant
/// message when there is one (parallel calls in a single turn).
fn push_chat_tool_call(messages: &mut Vec<Value>, call_id: String, name: String, arguments: String) {
    let call = json!({"id": call_id, "type": "function", "function": {"name": name, "arguments": arguments}});
    if let Some(m) = messages.last_mut() {
        if m.get("role").and_then(Value::as_str) == Some("assistant") {
            m["tool_calls"] = match m.get("tool_calls").cloned() {
                Some(Value::Array(mut a)) => { a.push(call); Value::Array(a) }
                _ => json!([call]),
            };
            return;
        }
    }
    messages.push(json!({"role": "assistant", "content": "", "tool_calls": [call]}));
}

/// Tool output → plain text. Codex sends `custom_tool_call_output` as an array
/// of `input_text` parts; JSON-encoding that array would hand the model a blob
/// of markup instead of the result.
fn tool_output_text(v: Value) -> String {
    match flatten_content(v.clone()) {
        Value::String(s) if !s.is_empty() => s,
        _ => stringify_output(v),
    }
}

/// Append a Messages turn, merging into the previous one when the role matches.
/// The Messages API requires alternating user/assistant turns, while a Codex
/// history is a flat item list (several developer/user items in a row, parallel
/// tool results, …). Merging preserves the content without inventing roles.
fn push_anthropic_message(messages: &mut Vec<Value>, role: &str, blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    if let Some(last) = messages.last_mut() {
        if last.get("role").and_then(Value::as_str) == Some(role) {
            let mut merged = match last.get("content").cloned() {
                Some(Value::Array(a)) => a,
                Some(Value::String(s)) if !s.is_empty() => vec![json!({"type": "text", "text": s})],
                _ => Vec::new(),
            };
            merged.extend(blocks);
            last["content"] = Value::Array(merged);
            return;
        }
    }
    messages.push(json!({"role": role, "content": blocks}));
}

/// responses tool_choice → chat tool_choice ("auto"/"none"/"required" share
/// their string form; a named choice maps to {"type":"function"}).
fn remap_tool_choice_to_chat(tc: &Value) -> Value {
    match tc {
        Value::String(s) => Value::String(s.clone()),
        Value::Object(_) => {
            let name = tc["name"].as_str().unwrap_or_default();
            if name.is_empty() { json!("auto") } else { json!({"type": "function", "function": {"name": name}}) }
        }
        _ => json!("auto"),
    }
}

/// responses request body → chat completions request body (inverse of
/// `chat_to_responses`). `stream` is preserved: a streaming Responses client
/// must stay streaming across the conversion, so the caller streams the Chat
/// SSE through `ChatToResponsesConverter`.
pub fn responses_req_to_chat_req(v: &Value) -> Value {
    let mut out = Map::new();
    if let Some(m) = v.get("model") { out.insert("model".into(), m.clone()); }
    let mut messages: Vec<Value> = Vec::new();
    if let Some(instr) = v.get("instructions").and_then(Value::as_str) {
        if !instr.is_empty() {
            messages.push(json!({"role": "system", "content": instr}));
        }
    }
    match v.get("input") {
        Some(Value::String(s)) => messages.push(json!({"role": "user", "content": s})),
        Some(Value::Array(items)) => {
            for item in items {
                match item.get("type").and_then(Value::as_str) {
                    Some("function_call") => {
                        let call_id = item.get("call_id").or_else(|| item.get("id")).and_then(Value::as_str).unwrap_or_default().to_string();
                        let name = item.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
                        let args_val = item.get("arguments").cloned().unwrap_or(json!({}));
                        let arguments = match &args_val {
                            Value::String(s) => s.clone(),
                            _ => serde_json::to_string(&args_val).unwrap_or_else(|_| "{}".into()),
                        };
                        push_chat_tool_call(&mut messages, call_id, name, arguments);
                    }
                    Some("custom_tool_call") => {
                        // Raw-text custom call (Codex `exec`): the chat side
                        // models it as a function whose single argument is the
                        // raw input string.
                        let call_id = item.get("call_id").or_else(|| item.get("id")).and_then(Value::as_str).unwrap_or_default().to_string();
                        let name = item.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
                        let input = item.get("input").and_then(Value::as_str).unwrap_or_default();
                        push_chat_tool_call(&mut messages, call_id, name, json!({"input": input}).to_string());
                    }
                    Some("function_call_output") | Some("custom_tool_call_output") => {
                        let call_id = item.get("call_id").and_then(Value::as_str).unwrap_or_default().to_string();
                        let output = tool_output_text(item.get("output").cloned().unwrap_or(Value::Null));
                        messages.push(json!({"role": "tool", "tool_call_id": call_id, "content": output}));
                    }
                    Some("message") | None => {
                        let Some(role) = item.get("role").and_then(Value::as_str) else { continue; };
                        let content = flatten_content(item.get("content").cloned().unwrap_or(Value::Null));
                        let text = content.as_str().unwrap_or_default();
                        // Empty turns carry nothing forward; emitting
                        // `content: null` makes strict validators reject the
                        // whole request (the original Codex 502).
                        if text.is_empty() { continue; }
                        messages.push(json!({"role": chat_role_for(role), "content": text}));
                    }
                    // `reasoning` is encrypted and unforwardable; `additional_tools`
                    // became `tools` above; other Codex-only items (`compaction`,
                    // `tool_search_call`, …) have no chat representation. Skipping
                    // them beats inventing a null-content message.
                    _ => {}
                }
            }
        }
        _ => {}
    }
    if !messages.is_empty() { out.insert("messages".into(), Value::Array(messages)); }
    for key in ["temperature", "top_p", "reasoning"] {
        if let Some(x) = v.get(key) { out.insert(key.into(), x.clone()); }
    }
    if let Some(x) = v.get("max_output_tokens") {
        out.insert("max_tokens".into(), x.clone());
    }
    let collected = collect_tools(v);
    if !collected.tools.is_empty() { out.insert("tools".into(), Value::Array(collected.tools)); }
    if let Some(tc) = v.get("tool_choice") {
        out.insert("tool_choice".into(), remap_tool_choice_to_chat(tc));
    }
    // Streaming stays streaming across the conversion. Ask for the usage tail
    // chunk too — the Responses client expects token counts on
    // `response.completed`, and OpenAI-compatible upstreams only send them when
    // `stream_options.include_usage` is set.
    if let Some(stream) = v.get("stream") {
        out.insert("stream".into(), stream.clone());
        if stream.as_bool() == Some(true) {
            out.insert("stream_options".into(), json!({ "include_usage": true }));
        }
    }
    Value::Object(out)
}

fn stringify_output(v: Value) -> String {
    match v {
        Value::String(s) => s,
        other => serde_json::to_string(&other).unwrap_or_default(),
    }
}

/// Extract the raw input of a custom tool call from the chat-side arguments
/// object (`{"input": "<raw text>"}`). Falls back to the raw arguments string
/// when the upstream did not wrap it — Codex's handlers need the original text,
/// not a JSON envelope.
fn custom_input_from_arguments(arguments: &str) -> String {
    serde_json::from_str::<Value>(arguments)
        .ok()
        .and_then(|v| v.get("input").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| arguments.to_string())
}

/// chat completions body → responses-format response object (inverse of
/// `responses_to_chat`). Text becomes an `output_text` item; chat tool_calls
/// become `function_call` items with their call ids preserved. Names the
/// client declared `type:"custom"` come back as `custom_tool_call` (raw input)
/// — Codex rejects a function-call payload for those tools.
pub fn chat_resp_to_responses_resp(chat: &Value, model: &str, custom_tools: &HashSet<String>) -> Value {
    let msg = &chat["choices"][0]["message"];
    let mut output: Vec<Value> = Vec::new();
    if let Some(text) = msg.get("content").and_then(Value::as_str) {
        if !text.is_empty() {
            output.push(json!({
                "type": "message",
                "id": format!("msg_{}", uuid_simple()),
                "role": "assistant",
                "status": "completed",
                "content": [{"type": "output_text", "text": text}],
            }));
        }
    }
    for c in msg.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
        let call_id = c.get("id").and_then(Value::as_str).unwrap_or_default();
        let name = c["function"]["name"].as_str().unwrap_or_default();
        let arguments = c["function"]["arguments"].as_str().unwrap_or("{}");
        if custom_tools.contains(name) {
            output.push(json!({
                "type": "custom_tool_call",
                "id": format!("ctc_{}", uuid_simple()),
                "call_id": call_id,
                "name": name,
                "input": custom_input_from_arguments(arguments),
                "status": "completed",
            }));
        } else {
            output.push(function_call_item(
                call_id,
                name,
                serde_json::from_str(arguments).unwrap_or(json!({})),
            ));
        }
    }
    let usage = &chat["usage"];
    json!({
        "id": chat.get("id").cloned().unwrap_or_else(|| json!(format!("resp_{}", uuid_simple()))),
        "object": "response",
        "model": model,
        "status": "completed",
        "output": Value::Array(output),
        "usage": {
            "input_tokens": usage["prompt_tokens"].as_i64().unwrap_or(0),
            "output_tokens": usage["completion_tokens"].as_i64().unwrap_or(0),
        },
    })
}

/// responses request body → anthropic messages request body. Anthropic
/// requires max_tokens; defaults to 4096 when the source has none.
pub fn responses_req_to_anthropic_req(v: &Value, resolved_model: &str) -> Value {
    let mut out = Map::new();
    out.insert("model".into(), json!(resolved_model));
    out.insert("max_tokens".into(), v.get("max_output_tokens").cloned().unwrap_or(json!(4096)));
    if let Some(instr) = v.get("instructions") { out.insert("system".into(), instr.clone()); }
    let mut messages: Vec<Value> = Vec::new();
    match v.get("input") {
        Some(Value::String(s)) => messages.push(json!({"role": "user", "content": s})),
        Some(Value::Array(items)) => {
            for item in items {
                match item.get("type").and_then(Value::as_str) {
                    Some("function_call") => {
                        // tool_use blocks attach to a trailing assistant message.
                        let call_id = item.get("call_id").or_else(|| item.get("id")).and_then(Value::as_str).unwrap_or_default();
                        let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
                        let args = item.get("arguments").cloned().unwrap_or(json!({}));
                        let input_val = match &args {
                            Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or(json!({})),
                            other => other.clone(),
                        };
                        let block = json!({"type": "tool_use", "id": call_id, "name": name, "input": input_val});
                        push_anthropic_message(&mut messages, "assistant", vec![block]);
                    }
                    Some("custom_tool_call") => {
                        let call_id = item.get("call_id").or_else(|| item.get("id")).and_then(Value::as_str).unwrap_or_default();
                        let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
                        let input = item.get("input").and_then(Value::as_str).unwrap_or_default();
                        let block = json!({"type": "tool_use", "id": call_id, "name": name, "input": {"input": input}});
                        push_anthropic_message(&mut messages, "assistant", vec![block]);
                    }
                    Some("function_call_output") | Some("custom_tool_call_output") => {
                        let call_id = item.get("call_id").and_then(Value::as_str).unwrap_or_default();
                        let output = tool_output_text(item.get("output").cloned().unwrap_or(Value::Null));
                        let block = json!({"type": "tool_result", "tool_use_id": call_id, "content": output});
                        push_anthropic_message(&mut messages, "user", vec![block]);
                    }
                    Some("message") | None => {
                        let Some(role) = item.get("role").and_then(Value::as_str) else { continue; };
                        // `developer` is a user-visible instruction block; the
                        // Messages API only knows user/assistant.
                        let role = if role == "assistant" { "assistant" } else { "user" };
                        let content = flatten_content(item.get("content").cloned().unwrap_or(Value::Null));
                        let text = content.as_str().unwrap_or_default();
                        if text.is_empty() { continue; }
                        push_anthropic_message(&mut messages, role, vec![json!({"type": "text", "text": text})]);
                    }
                    // Encrypted reasoning, tool-definition carriers and other
                    // Codex-only items have no Messages representation.
                    _ => {}
                }
            }
        }
        _ => {}
    }
    if !messages.is_empty() { out.insert("messages".into(), Value::Array(messages)); }
    let collected = collect_tools(v);
    if !collected.tools.is_empty() {
        let mapped: Vec<Value> = collected.tools.iter().filter_map(|t| {
            let f = t.get("function")?;
            let name = f.get("name").and_then(Value::as_str)?;
            Some(json!({
                "name": name,
                "description": f.get("description").cloned().unwrap_or(Value::Null),
                "input_schema": f.get("parameters").cloned().unwrap_or(json!({})),
            }))
        }).collect();
        if !mapped.is_empty() { out.insert("tools".into(), Value::Array(mapped)); }
    }
    if let Some(tc) = v.get("tool_choice") {
        let mapped = match tc {
            Value::String(s) if s == "required" => json!({"type": "any"}),
            Value::String(s) if s == "none" => json!({"type": "none"}),
            Value::String(_) => json!({"type": "auto"}),
            Value::Object(_) => {
                let name = tc["name"].as_str().unwrap_or_default();
                if name.is_empty() { json!({"type": "auto"}) } else { json!({"type": "tool", "name": name}) }
            }
            _ => json!({"type": "auto"}),
        };
        out.insert("tool_choice".into(), mapped);
    }
    // Streaming stays streaming across the conversion: the Anthropic SSE is
    // fed through `AnthropicToResponsesConverter` rather than buffered.
    if let Some(stream) = v.get("stream") {
        out.insert("stream".into(), stream.clone());
    }
    Value::Object(out)
}

/// anthropic messages reply → responses-format response object. `max_tokens`
/// stop reason maps to Responses `incomplete` status with details.
pub fn anthropic_resp_to_responses_resp(anth: &Value, model: &str) -> Value {
    let mut output: Vec<Value> = Vec::new();
    for b in anth["content"].as_array().into_iter().flatten() {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = b.get("text").and_then(Value::as_str) {
                    if !t.is_empty() {
                        output.push(json!({
                            "type": "message",
                            "id": format!("msg_{}", uuid_simple()),
                            "role": "assistant",
                            "status": "completed",
                            "content": [{"type": "output_text", "text": t}],
                        }));
                    }
                }
            }
            Some("tool_use") => {
                output.push(function_call_item(
                    b.get("id").and_then(Value::as_str).unwrap_or_default(),
                    b.get("name").and_then(Value::as_str).unwrap_or_default(),
                    b.get("input").cloned().unwrap_or(json!({})),
                ));
            }
            _ => {}
        }
    }
    let incomplete = anth["stop_reason"].as_str() == Some("max_tokens");
    let usage = &anth["usage"];
    let mut resp = json!({
        "id": anth.get("id").cloned().unwrap_or_else(|| json!(format!("resp_{}", uuid_simple()))),
        "object": "response",
        "model": model,
        "status": if incomplete { "incomplete" } else { "completed" },
        "output": Value::Array(output),
        "usage": {
            "input_tokens": usage["input_tokens"].as_i64().unwrap_or(0),
            "output_tokens": usage["output_tokens"].as_i64().unwrap_or(0),
            "input_tokens_details": {
                "cached_tokens": usage["cache_read_input_tokens"].as_i64().unwrap_or(0)
            },
        },
    });
    if incomplete {
        resp["incomplete_details"] = json!({"reason": "max_output_tokens"});
    }
    resp
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

/// Resolve an SSE event's type. Prefer the `event:` line; when the upstream omits
/// it and carries the type only inside the JSON payload, fall back to the body's
/// `type` field. The Codex/ChatGPT Responses backend (`chatgpt.com/backend-api/codex`)
/// streams `data:` lines with no `event:` line — without this fallback every
/// branch below is skipped and the reply degrades to an empty message
/// (2026-10-12: `op`/gpt-6-luna streamed no `content_block_delta`).
fn resolve_event_type<'a>(event_text: &'a str, data: &'a Value) -> &'a str {
    let from_line = parse_event_type(event_text);
    if from_line.is_empty() {
        data.get("type").and_then(Value::as_str).unwrap_or("")
    } else {
        from_line
    }
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
    // A name-less function_call can't become a tool_use block: the Anthropic
    // SDK rejects the message outright ("tool_calls without a complete id and
    // function name"), so refusing here keeps the rest of the reply usable.
    // Both the streaming and non-streaming responses paths funnel through this
    // one chokepoint.
    if call_id.is_empty() || name.is_empty() {
        tracing::warn!(
            item_id,
            has_call_id = !call_id.is_empty(),
            has_name = !name.is_empty(),
            "responses function_call without a name; dropped"
        );
        return None;
    }
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
    /// 上游明确报了错（`response.failed` / `error` / `response.error`，或
    /// `response.incomplete` 且不是撞 max_tokens）。与 `terminal` 分开：
    /// 错误事件同样要终止流，所以 `terminal=true`，但**不能**因此被当成
    /// 成功记账（否则会写 success=1 并解除账户冷却）。
    failed: bool,
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
            failed: false,
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

    /// 上游报了错 —— 记账时不能算成功（`is_done()` 只表示「流可以停了」）。
    pub fn is_failed(&self) -> bool { self.failed }

    pub fn usage_tokens(&self) -> (i64, i64) {
        let usage = self.last_usage.as_ref();
        let raw = usage.and_then(|u| u.get("input_tokens").or_else(|| u.get("prompt_tokens"))).and_then(Value::as_i64).unwrap_or(0);
        let (read, _) = self.usage_cache();
        let fresh = (raw - read).max(0);
        (
            fresh,
            usage.and_then(|u| u.get("output_tokens").or_else(|| u.get("completion_tokens"))).and_then(Value::as_i64).unwrap_or(0),
        )
    }

    /// Cache token counts — 4-store-3-display: creation always 0 for /responses.
    pub fn usage_cache(&self) -> (i64, i64) {
        self.last_usage.as_ref().map(super::anthropic_openai::cache_usage_from_openai).unwrap_or((0, 0))
    }

    pub fn feed(&mut self, event_text: &str) -> Vec<String> {
        let mut out = Vec::new();
        let Some(data_str) = sse_data(event_text) else { return out };
        if data_str == "[DONE]" { self.terminal = true; return out; }
        let Ok(mut data) = serde_json::from_str::<Value>(data_str) else { return out };
        // codex 私有后端事件规范化：`response.done` → `response.completed`，
        // 非法 `status` 删掉（否则下游按未知状态走失败分支）。
        crate::codex_compat::normalize_backend_event(&mut data);
        let etype = resolve_event_type(event_text, &data);
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
                self.failed = true;
                out.push(format!("data: {}\n\n", json!({"error":{"message":response_event_error(&data),"type":"server_error"}})));
                out.push("data: [DONE]\n\n".to_string());
                self.finished = true;
            }
        } else if etype == "response.failed" || etype == "error" || etype == "response.error" {
            self.terminal = true;
            self.finished = true;
            self.failed = true;
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
    /// 见 `ResponsesToChatConverter::failed`。
    failed: bool,
}

impl ResponsesToAnthropicConverter {
    pub fn new(model: &str) -> Self {
        Self {
            message_id: format!("msg_{}", uuid_simple()), model: model.to_string(), started: false,
            text_index: None, text: String::new(), next_content_index: 0, function_calls: HashMap::new(),
            last_usage: None, incomplete_max_tokens: false, terminal: false, finished: false, failed: false,
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

    /// 上游报了错 —— 记账时不能算成功（`is_done()` 只表示「流可以停了」）。
    pub fn is_failed(&self) -> bool { self.failed }

    pub fn usage_tokens(&self) -> (i64, i64) {
        let usage = self.last_usage.as_ref();
        let raw = usage.and_then(|u| u.get("input_tokens").or_else(|| u.get("prompt_tokens"))).and_then(Value::as_i64).unwrap_or(0);
        let (read, _) = self.usage_cache();
        let fresh = (raw - read).max(0);
        (
            fresh,
            usage.and_then(|u| u.get("output_tokens").or_else(|| u.get("completion_tokens"))).and_then(Value::as_i64).unwrap_or(0),
        )
    }

    /// Cache token counts — 4-store-3-display: creation always 0 for /responses.
    pub fn usage_cache(&self) -> (i64, i64) {
        self.last_usage.as_ref().map(super::anthropic_openai::cache_usage_from_openai).unwrap_or((0, 0))
    }

    pub fn feed(&mut self, event_text: &str) -> Vec<String> {
        let mut out = Vec::new();
        let Some(data_str) = sse_data(event_text) else { return out };
        if data_str == "[DONE]" { self.terminal = true; return out; }
        let Ok(mut data) = serde_json::from_str::<Value>(data_str) else { return out };
        // codex 私有后端事件规范化：`response.done` → `response.completed`，
        // 非法 `status` 删掉（否则下游按未知状态走失败分支）。
        crate::codex_compat::normalize_backend_event(&mut data);
        let etype = resolve_event_type(event_text, &data);
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
                self.failed = true;
                out.push(sse_event("error", json!({"type":"error","error":{"type":"api_error","message":response_event_error(&data)}})));
            }
        } else if etype == "response.failed" || etype == "error" || etype == "response.error" {
            self.terminal = true;
            self.finished = true;
            self.failed = true;
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
        let usage = self.last_usage.as_ref();
        let raw_input = usage.and_then(|u| u.get("input_tokens").or_else(|| u.get("prompt_tokens"))).and_then(Value::as_i64).unwrap_or(0).max(0);
        let output = usage.and_then(|u| u.get("output_tokens").or_else(|| u.get("completion_tokens"))).and_then(Value::as_i64).unwrap_or(0).max(0);
        let (cache_read, cache_create) = self.usage_cache();
        let fresh_input = (raw_input - cache_read).max(0);
        let canonical_usage = json!({
            "input_tokens": fresh_input,
            "output_tokens": output,
            "cache_read_input_tokens": cache_read,
            "cache_creation_input_tokens": cache_create,
        });
        out.push(sse_event("message_delta", json!({"type":"message_delta","delta":{"stop_reason":stop_reason,"stop_sequence":null},"usage":canonical_usage})));
        out.push(sse_event("message_stop", json!({"type":"message_stop"})));
        out
    }
}

/// OpenAI Chat SSE → Responses SSE. The inverse of `ResponsesToChatConverter`:
/// text deltas become `response.output_text.delta`, Chat `tool_calls` fragments
/// become `function_call` items, and the terminal Chat chunk becomes
/// `response.completed`.
pub struct ChatToResponsesConverter {
    response_id: String,
    model: String,
    created_at: i64,
    started: bool,
    items: Vec<OutputItem>,
    /// chat `tool_calls[].index` → index into `items`.
    tool_items: HashMap<u64, usize>,
    /// chat `tool_calls[].index` → (id, name, arguments-so-far) held until both
    /// id and name are known. Opening the item earlier would emit an
    /// `output_item.added` with an empty `call_id`/`name`, which Responses
    /// clients use to address the call — the same trap `OpenAISseConverter`
    /// documents for tool_use blocks.
    pending_calls: HashMap<u64, (String, String, String)>,
    /// Tool names the client declared `type:"custom"` (Codex `exec`, …).
    /// Their calls must come back as `custom_tool_call`, not `function_call`.
    custom_tools: HashSet<String>,
    last_usage: Option<Value>,
    finish_reason: Option<String>,
    terminal: bool,
    finished: bool,
    failed: bool,
}

impl ChatToResponsesConverter {
    pub fn new(model: &str) -> Self {
        Self {
            response_id: format!("resp_{}", uuid_simple()),
            model: model.to_string(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
            started: false,
            items: Vec::new(),
            tool_items: HashMap::new(),
            pending_calls: HashMap::new(),
            custom_tools: HashSet::new(),
            last_usage: None,
            finish_reason: None,
            terminal: false,
            finished: false,
            failed: false,
        }
    }

    /// Mark which tool names are `type:"custom"` in the client's request.
    /// Must be called before feeding chunks; only affects how tool calls are
    /// shaped on the way back.
    pub fn set_custom_tools(&mut self, names: HashSet<String>) {
        self.custom_tools = names;
    }

    fn start(&mut self, out: &mut Vec<String>) {
        if self.started {
            return;
        }
        self.started = true;
        out.push(sse_event(
            "response.created",
            json!({
                "type": "response.created",
                "response": {
                    "id": self.response_id,
                    "object": "response",
                    "created_at": self.created_at,
                    "status": "in_progress",
                    "model": self.model,
                    "output": [],
                }
            }),
        ));
    }

    fn push_item(&mut self, kind: OutputItemKind, out: &mut Vec<String>) -> usize {
        self.start(out);
        let output_index = self.items.len();
        let (id, call_id, name) = match kind {
            OutputItemKind::Message => (format!("msg_{}", uuid_simple()), String::new(), String::new()),
            OutputItemKind::FunctionCall => (format!("fc_{}", uuid_simple()), String::new(), String::new()),
            OutputItemKind::CustomToolCall => (format!("ctc_{}", uuid_simple()), String::new(), String::new()),
            OutputItemKind::Reasoning => (format!("rs_{}", uuid_simple()), String::new(), String::new()),
        };
        let item = OutputItem { id, kind, output_index, text: String::new(), call_id, name, arguments: String::new(), args_emitted: false, closed: false };
        let added = item.added();
        let item_id = item.id.clone();
        self.items.push(item);
        out.push(sse_event(
            "response.output_item.added",
            json!({"type": "response.output_item.added", "output_index": output_index, "item": added}),
        ));
        if kind == OutputItemKind::Message {
            out.push(sse_event(
                "response.content_part.added",
                json!({
                    "type": "response.content_part.added",
                    "item_id": item_id,
                    "output_index": output_index,
                    "content_index": 0,
                    "part": {"type": "output_text", "text": ""},
                }),
            ));
        }
        output_index
    }

    fn emit_text(&mut self, text: &str, out: &mut Vec<String>) {
        if text.is_empty() {
            return;
        }
        let index = self
            .items
            .last()
            .filter(|item| item.kind == OutputItemKind::Message && !item.closed)
            .map(|item| item.output_index);
        let index = match index {
            Some(index) => index,
            None => self.push_item(OutputItemKind::Message, out),
        };
        let item = &mut self.items[index];
        item.text.push_str(text);
        let item_id = item.id.clone();
        out.push(sse_event(
            "response.output_text.delta",
            json!({
                "type": "response.output_text.delta",
                "item_id": item_id,
                "output_index": index,
                "content_index": 0,
                "delta": text,
            }),
        ));
    }

    fn emit_reasoning(&mut self, text: &str, out: &mut Vec<String>) {
        if text.is_empty() {
            return;
        }
        let index = self
            .items
            .iter()
            .find(|item| item.kind == OutputItemKind::Reasoning && !item.closed)
            .map(|item| item.output_index);
        let index = match index {
            Some(index) => index,
            None => self.push_item(OutputItemKind::Reasoning, out),
        };
        let item = &mut self.items[index];
        item.text.push_str(text);
        let item_id = item.id.clone();
        out.push(sse_event(
            "response.reasoning_summary_text.delta",
            json!({
                "type": "response.reasoning_summary_text.delta",
                "item_id": item_id,
                "output_index": index,
                "summary_index": 0,
                "delta": text,
            }),
        ));
    }

    /// One Chat `tool_calls` fragment. Identity (`id`, `function.name`) may
    /// arrive on the first fragment only; the item is opened once both are
    /// known, and everything buffered so far is emitted as one delta.
    fn emit_tool_delta(&mut self, fragment: &Value, out: &mut Vec<String>) {
        let chat_index = fragment.get("index").and_then(Value::as_u64).unwrap_or(0);
        let call_id = fragment.get("id").and_then(Value::as_str).unwrap_or_default();
        let name = fragment
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let arguments = fragment
            .get("function")
            .and_then(|f| f.get("arguments"))
            .and_then(Value::as_str)
            .unwrap_or_default();

        if let Some(&item_index) = self.tool_items.get(&chat_index) {
            let item = &mut self.items[item_index];
            item.arguments.push_str(arguments);
            // Custom tools stream their raw input as one block at close time
            // (`response.custom_tool_call_input.delta`); JSON argument deltas
            // would expose the `{"input": …}` envelope instead of the text.
            if !arguments.is_empty() && item.kind == OutputItemKind::FunctionCall {
                let item_id = item.id.clone();
                let output_index = item.output_index;
                out.push(sse_event(
                    "response.function_call_arguments.delta",
                    json!({
                        "type": "response.function_call_arguments.delta",
                        "item_id": item_id,
                        "output_index": output_index,
                        "delta": arguments,
                    }),
                ));
            }
            return;
        }

        let pending = self.pending_calls.entry(chat_index).or_default();
        if !call_id.is_empty() {
            pending.0 = call_id.to_string();
        }
        if !name.is_empty() {
            pending.1 = name.to_string();
        }
        pending.2.push_str(arguments);
        if pending.0.is_empty() || pending.1.is_empty() {
            return;
        }
        let (call_id, name, buffered) = self
            .pending_calls
            .remove(&chat_index)
            .unwrap_or_default();
        let index = self.push_tool_item(&call_id, &name, out);
        self.tool_items.insert(chat_index, index);
        let item_id = self.items[index].id.clone();
        let output_index = self.items[index].output_index;
        self.items[index].arguments = buffered.clone();
        let is_function_call = self.items[index].kind == OutputItemKind::FunctionCall;
        if !buffered.is_empty() {
            self.items[index].args_emitted = true;
            if is_function_call {
                out.push(sse_event(
                    "response.function_call_arguments.delta",
                    json!({
                        "type": "response.function_call_arguments.delta",
                        "item_id": item_id,
                        "output_index": output_index,
                        "delta": buffered,
                    }),
                ));
            }
        }
    }

    /// Open a `function_call` item whose identity is already known, so the
    /// `output_item.added` event is addressable from the very first frame.
    fn push_tool_item(&mut self, call_id: &str, name: &str, out: &mut Vec<String>) -> usize {
        self.start(out);
        let output_index = self.items.len();
        let kind = if self.custom_tools.contains(name) {
            OutputItemKind::CustomToolCall
        } else {
            OutputItemKind::FunctionCall
        };
        let id = if kind == OutputItemKind::CustomToolCall {
            format!("ctc_{}", uuid_simple())
        } else {
            format!("fc_{}", uuid_simple())
        };
        let item = OutputItem {
            id,
            kind,
            output_index,
            text: String::new(),
            call_id: call_id.to_string(),
            name: name.to_string(),
            arguments: String::new(),
            args_emitted: false,
            closed: false,
        };
        let added = item.added();
        self.items.push(item);
        out.push(sse_event(
            "response.output_item.added",
            json!({"type": "response.output_item.added", "output_index": output_index, "item": added}),
        ));
        output_index
    }

    pub fn is_done(&self) -> bool {
        self.terminal
    }

    pub fn is_failed(&self) -> bool {
        self.failed
    }

    pub fn produced_output(&self) -> bool {
        self.items.iter().any(|item| !item.text.is_empty() || !item.arguments.is_empty())
    }

    pub fn usage_tokens(&self) -> (i64, i64) {
        let usage = self.last_usage.as_ref();
        let raw = usage
            .and_then(|u| u.get("prompt_tokens").or_else(|| u.get("input_tokens")))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let (read, _) = self.usage_cache();
        let fresh = (raw - read).max(0);
        (
            fresh,
            usage
                .and_then(|u| u.get("completion_tokens").or_else(|| u.get("output_tokens")))
                .and_then(Value::as_i64)
                .unwrap_or(0),
        )
    }

    pub fn usage_cache(&self) -> (i64, i64) {
        self.last_usage
            .as_ref()
            .map(super::anthropic_openai::cache_usage_from_openai)
            .unwrap_or((0, 0))
    }

    /// `[DONE]` is not JSON, so the route layer reports it here.
    pub fn mark_done(&mut self) {
        self.terminal = true;
    }

    pub fn feed(&mut self, chunk: &Value) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        let mut out = Vec::new();
        if let Some(error) = chunk.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Chat upstream reported an error");
            self.fail(message, &mut out);
            return out;
        }
        if let Some(usage) = chunk.get("usage") {
            if usage.is_object() {
                self.last_usage = Some(usage.clone());
            }
        }
        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        else {
            return out;
        };
        if let Some(delta) = choice.get("delta") {
            if let Some(text) = delta.get("content").and_then(Value::as_str) {
                self.emit_text(text, &mut out);
            }
            if let Some(text) = delta.get("reasoning_content").and_then(Value::as_str) {
                self.emit_reasoning(text, &mut out);
            }
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    self.emit_tool_delta(call, &mut out);
                }
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            if !reason.is_empty() {
                self.finish_reason = Some(reason.to_string());
            }
        }
        out
    }

    fn fail(&mut self, message: &str, out: &mut Vec<String>) {
        self.start(out);
        self.failed = true;
        self.terminal = true;
        self.finished = true;
        out.push(sse_event(
            "response.failed",
            json!({
                "type": "response.failed",
                "response": {
                    "id": self.response_id,
                    "object": "response",
                    "created_at": self.created_at,
                    "status": "failed",
                    "model": self.model,
                    "output": [],
                    "error": {"code": "server_error", "message": message},
                }
            }),
        ));
        out.push("data: [DONE]\n\n".to_string());
    }

    fn close_item(&mut self, index: usize, out: &mut Vec<String>) {
        let item = &mut self.items[index];
        if item.closed {
            return;
        }
        item.closed = true;
        let output_index = item.output_index;
        let done = item.done();
        match item.kind {
            OutputItemKind::Message => {
                let item_id = item.id.clone();
                let text = item.text.clone();
                out.push(sse_event(
                    "response.output_text.done",
                    json!({
                        "type": "response.output_text.done",
                        "item_id": item_id,
                        "output_index": output_index,
                        "content_index": 0,
                        "text": text,
                    }),
                ));
                out.push(sse_event(
                    "response.content_part.done",
                    json!({
                        "type": "response.content_part.done",
                        "item_id": item_id,
                        "output_index": output_index,
                        "content_index": 0,
                        "part": {"type": "output_text", "text": text},
                    }),
                ));
            }
            OutputItemKind::FunctionCall => {
                let item_id = item.id.clone();
                let arguments = item.arguments.clone();
                out.push(sse_event(
                    "response.function_call_arguments.done",
                    json!({
                        "type": "response.function_call_arguments.done",
                        "item_id": item_id,
                        "output_index": output_index,
                        "arguments": arguments,
                    }),
                ));
            }
            OutputItemKind::CustomToolCall => {
                let item_id = item.id.clone();
                let input = custom_input_from_arguments(&item.arguments);
                if !input.is_empty() {
                    out.push(sse_event(
                        "response.custom_tool_call_input.delta",
                        json!({
                            "type": "response.custom_tool_call_input.delta",
                            "item_id": item_id,
                            "output_index": output_index,
                            "delta": input,
                        }),
                    ));
                }
                out.push(sse_event(
                    "response.custom_tool_call_input.done",
                    json!({
                        "type": "response.custom_tool_call_input.done",
                        "item_id": item_id,
                        "output_index": output_index,
                        "input": input,
                    }),
                ));
            }
            OutputItemKind::Reasoning => {
                let item_id = item.id.clone();
                let text = item.text.clone();
                out.push(sse_event(
                    "response.reasoning_summary_text.done",
                    json!({
                        "type": "response.reasoning_summary_text.done",
                        "item_id": item_id,
                        "output_index": output_index,
                        "summary_index": 0,
                        "text": text,
                    }),
                ));
            }
        }
        out.push(sse_event(
            "response.output_item.done",
            json!({
                "type": "response.output_item.done",
                "output_index": output_index,
                "item": done,
            }),
        ));
    }

    pub fn finish(&mut self) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let mut out = Vec::new();
        self.start(&mut out);
        // Drop incomplete tool identities: a function_call without id/name is
        // not addressable downstream (same rule as the other converters).
        let dropped: Vec<usize> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                matches!(item.kind, OutputItemKind::FunctionCall | OutputItemKind::CustomToolCall)
                    && (item.call_id.is_empty() || item.name.is_empty())
                    && !item.closed
            })
            .map(|(index, _)| index)
            .collect();
        for index in dropped {
            tracing::warn!(
                output_index = self.items[index].output_index,
                "chat tool_calls without a complete id/name; dropped"
            );
            self.items[index].closed = true;
        }
        for index in 0..self.items.len() {
            if !self.items[index].closed {
                self.close_item(index, &mut out);
            }
        }
        let incomplete = matches!(self.finish_reason.as_deref(), Some("length"))
            || (self.finish_reason.is_none() && !self.terminal);
        let output: Vec<Value> = self
            .items
            .iter()
            .filter(|item| !(matches!(item.kind, OutputItemKind::FunctionCall | OutputItemKind::CustomToolCall) && (item.call_id.is_empty() || item.name.is_empty())))
            .map(OutputItem::done)
            .collect();
        let status = if incomplete { "incomplete" } else { "completed" };
        let response = json!({
            "id": self.response_id,
            "object": "response",
            "created_at": self.created_at,
            "status": status,
            "model": self.model,
            "output": output,
            "usage": response_usage_from_chat(self.last_usage.as_ref()),
            "incomplete_details": if incomplete {
                json!({"reason": "max_output_tokens"})
            } else {
                Value::Null
            },
        });
        let event_type = if incomplete { "response.incomplete" } else { "response.completed" };
        out.push(sse_event(
            event_type,
            json!({"type": event_type, "response": response}),
        ));
        out.push("data: [DONE]\n\n".to_string());
        out
    }
}

/// Anthropic Messages SSE → Responses SSE. The inverse of
/// `ResponsesToAnthropicConverter`: `text_delta` becomes
/// `response.output_text.delta`, `thinking_delta` becomes reasoning summary
/// deltas, `tool_use` blocks become `function_call` items, and `message_stop`
/// becomes `response.completed`.
pub struct AnthropicToResponsesConverter {
    response_id: String,
    model: String,
    created_at: i64,
    started: bool,
    items: Vec<OutputItem>,
    /// Anthropic content block index → index into `items`.
    blocks: HashMap<i64, usize>,
    last_usage: Option<Value>,
    stop_reason: Option<String>,
    terminal: bool,
    finished: bool,
    failed: bool,
}

impl AnthropicToResponsesConverter {
    pub fn new(model: &str) -> Self {
        Self {
            response_id: format!("resp_{}", uuid_simple()),
            model: model.to_string(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
            started: false,
            items: Vec::new(),
            blocks: HashMap::new(),
            last_usage: None,
            stop_reason: None,
            terminal: false,
            finished: false,
            failed: false,
        }
    }

    fn start(&mut self, out: &mut Vec<String>) {
        if self.started {
            return;
        }
        self.started = true;
        out.push(sse_event(
            "response.created",
            json!({
                "type": "response.created",
                "response": {
                    "id": self.response_id,
                    "object": "response",
                    "created_at": self.created_at,
                    "status": "in_progress",
                    "model": self.model,
                    "output": [],
                }
            }),
        ));
    }

    fn merge_usage(&mut self, usage: &Value) {
        if !usage.is_object() {
            return;
        }
        let existing = self.last_usage.get_or_insert_with(|| json!({}));
        if let (Some(existing), Some(incoming)) = (existing.as_object_mut(), usage.as_object()) {
            for (key, value) in incoming {
                existing.insert(key.clone(), value.clone());
            }
        }
    }

    fn push_item(&mut self, kind: OutputItemKind, out: &mut Vec<String>) -> usize {
        self.start(out);
        let output_index = self.items.len();
        let id = match kind {
            OutputItemKind::Message => format!("msg_{}", uuid_simple()),
            OutputItemKind::FunctionCall => format!("fc_{}", uuid_simple()),
            OutputItemKind::CustomToolCall => format!("ctc_{}", uuid_simple()),
            OutputItemKind::Reasoning => format!("rs_{}", uuid_simple()),
        };
        let item = OutputItem {
            id,
            kind,
            output_index,
            text: String::new(),
            call_id: String::new(),
            name: String::new(),
            arguments: String::new(),
            args_emitted: false,
            closed: false,
        };
        let added = item.added();
        let item_id = item.id.clone();
        self.items.push(item);
        out.push(sse_event(
            "response.output_item.added",
            json!({"type": "response.output_item.added", "output_index": output_index, "item": added}),
        ));
        if kind == OutputItemKind::Message {
            out.push(sse_event(
                "response.content_part.added",
                json!({
                    "type": "response.content_part.added",
                    "item_id": item_id,
                    "output_index": output_index,
                    "content_index": 0,
                    "part": {"type": "output_text", "text": ""},
                }),
            ));
        }
        output_index
    }

    fn item_for_block(&mut self, block_index: i64, kind: OutputItemKind, out: &mut Vec<String>) -> usize {
        if let Some(index) = self.blocks.get(&block_index) {
            return *index;
        }
        let index = self.push_item(kind, out);
        self.blocks.insert(block_index, index);
        index
    }

    fn emit_text(&mut self, block_index: i64, text: &str, out: &mut Vec<String>) {
        if text.is_empty() {
            return;
        }
        let index = self.item_for_block(block_index, OutputItemKind::Message, out);
        let item = &mut self.items[index];
        item.text.push_str(text);
        let item_id = item.id.clone();
        out.push(sse_event(
            "response.output_text.delta",
            json!({
                "type": "response.output_text.delta",
                "item_id": item_id,
                "output_index": index,
                "content_index": 0,
                "delta": text,
            }),
        ));
    }

    fn emit_reasoning(&mut self, block_index: i64, text: &str, out: &mut Vec<String>) {
        if text.is_empty() {
            return;
        }
        let index = self.item_for_block(block_index, OutputItemKind::Reasoning, out);
        let item = &mut self.items[index];
        item.text.push_str(text);
        let item_id = item.id.clone();
        out.push(sse_event(
            "response.reasoning_summary_text.delta",
            json!({
                "type": "response.reasoning_summary_text.delta",
                "item_id": item_id,
                "output_index": index,
                "summary_index": 0,
                "delta": text,
            }),
        ));
    }

    fn open_tool_use(&mut self, block_index: i64, block: &Value, out: &mut Vec<String>) {
        let call_id = block.get("id").and_then(Value::as_str).unwrap_or_default();
        let name = block.get("name").and_then(Value::as_str).unwrap_or_default();
        // An id/name the upstream didn't send can't be papered over with "": the
        // Responses consumer keys tool calls by call_id and dispatches on name,
        // so an empty pair is worse than no call at all. Drop it before it is
        // ever announced (`output_item.added` must be addressable).
        if call_id.is_empty() || name.is_empty() {
            tracing::warn!(
                block_index,
                has_id = !call_id.is_empty(),
                has_name = !name.is_empty(),
                "anthropic tool_use without id/name; dropped"
            );
            return;
        }
        let index = match self.blocks.get(&block_index) {
            Some(&index) => index,
            None => {
                self.start(out);
                let output_index = self.items.len();
                let item = OutputItem {
                    id: format!("fc_{}", uuid_simple()),
                    kind: OutputItemKind::FunctionCall,
                    output_index,
                    text: String::new(),
                    call_id: call_id.to_string(),
                    name: name.to_string(),
                    arguments: String::new(),
                    args_emitted: false,
                    closed: false,
                };
                let added = item.added();
                self.items.push(item);
                out.push(sse_event(
                    "response.output_item.added",
                    json!({"type": "response.output_item.added", "output_index": output_index, "item": added}),
                ));
                self.blocks.insert(block_index, output_index);
                output_index
            }
        };
        let item = &mut self.items[index];
        item.call_id = call_id.to_string();
        item.name = name.to_string();
        if let Some(input) = block.get("input").filter(|v| v.as_object().is_some_and(|o| !o.is_empty())) {
            item.arguments = serde_json::to_string(input).unwrap_or_default();
        }
        if item.arguments.is_empty() {
            return;
        }
        item.args_emitted = true;
        let item_id = item.id.clone();
        let output_index = item.output_index;
        let delta = item.arguments.clone();
        out.push(sse_event(
            "response.function_call_arguments.delta",
            json!({
                "type": "response.function_call_arguments.delta",
                "item_id": item_id,
                "output_index": output_index,
                "delta": delta,
            }),
        ));
    }

    fn emit_arguments(&mut self, block_index: i64, delta: &str, out: &mut Vec<String>) {
        let Some(&index) = self.blocks.get(&block_index) else {
            return;
        };
        let item = &mut self.items[index];
        if item.call_id.is_empty() || item.name.is_empty() {
            return;
        }
        item.arguments.push_str(delta);
        if delta.is_empty() {
            return;
        }
        item.args_emitted = true;
        let item_id = item.id.clone();
        let output_index = item.output_index;
        out.push(sse_event(
            "response.function_call_arguments.delta",
            json!({
                "type": "response.function_call_arguments.delta",
                "item_id": item_id,
                "output_index": output_index,
                "delta": delta,
            }),
        ));
    }

    fn close_item(&mut self, index: usize, out: &mut Vec<String>) {
        let item = &mut self.items[index];
        if item.closed {
            return;
        }
        item.closed = true;
        let output_index = item.output_index;
        let done = item.done();
        match item.kind {
            OutputItemKind::Message => {
                let item_id = item.id.clone();
                let text = item.text.clone();
                out.push(sse_event(
                    "response.output_text.done",
                    json!({
                        "type": "response.output_text.done",
                        "item_id": item_id,
                        "output_index": output_index,
                        "content_index": 0,
                        "text": text,
                    }),
                ));
                out.push(sse_event(
                    "response.content_part.done",
                    json!({
                        "type": "response.content_part.done",
                        "item_id": item_id,
                        "output_index": output_index,
                        "content_index": 0,
                        "part": {"type": "output_text", "text": text},
                    }),
                ));
            }
            OutputItemKind::FunctionCall => {
                let item_id = item.id.clone();
                let arguments = item.arguments.clone();
                out.push(sse_event(
                    "response.function_call_arguments.done",
                    json!({
                        "type": "response.function_call_arguments.done",
                        "item_id": item_id,
                        "output_index": output_index,
                        "arguments": arguments,
                    }),
                ));
            }
            OutputItemKind::CustomToolCall => {
                let item_id = item.id.clone();
                let input = custom_input_from_arguments(&item.arguments);
                if !input.is_empty() {
                    out.push(sse_event(
                        "response.custom_tool_call_input.delta",
                        json!({
                            "type": "response.custom_tool_call_input.delta",
                            "item_id": item_id,
                            "output_index": output_index,
                            "delta": input,
                        }),
                    ));
                }
                out.push(sse_event(
                    "response.custom_tool_call_input.done",
                    json!({
                        "type": "response.custom_tool_call_input.done",
                        "item_id": item_id,
                        "output_index": output_index,
                        "input": input,
                    }),
                ));
            }
            OutputItemKind::Reasoning => {
                let item_id = item.id.clone();
                let text = item.text.clone();
                out.push(sse_event(
                    "response.reasoning_summary_text.done",
                    json!({
                        "type": "response.reasoning_summary_text.done",
                        "item_id": item_id,
                        "output_index": output_index,
                        "summary_index": 0,
                        "text": text,
                    }),
                ));
            }
        }
        out.push(sse_event(
            "response.output_item.done",
            json!({
                "type": "response.output_item.done",
                "output_index": output_index,
                "item": done,
            }),
        ));
    }

    pub fn is_done(&self) -> bool {
        self.terminal
    }

    pub fn is_failed(&self) -> bool {
        self.failed
    }

    pub fn produced_output(&self) -> bool {
        self.items.iter().any(|item| !item.text.is_empty() || !item.arguments.is_empty())
    }

    pub fn usage_tokens(&self) -> (i64, i64) {
        let usage = self.last_usage.as_ref();
        let raw = usage
            .and_then(|u| u.get("input_tokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let (read, _) = self.usage_cache();
        let fresh = (raw - read).max(0);
        (
            fresh,
            usage
                .and_then(|u| u.get("output_tokens"))
                .and_then(Value::as_i64)
                .unwrap_or(0),
        )
    }

    pub fn usage_cache(&self) -> (i64, i64) {
        let usage = self.last_usage.as_ref();
        (
            usage
                .and_then(|u| u.get("cache_read_input_tokens"))
                .and_then(Value::as_i64)
                .unwrap_or(0),
            usage
                .and_then(|u| u.get("cache_creation_input_tokens"))
                .and_then(Value::as_i64)
                .unwrap_or(0),
        )
    }

    pub fn feed(&mut self, event: &Value) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        let mut out = Vec::new();
        let Some(etype) = event.get("type").and_then(Value::as_str) else {
            return out;
        };
        match etype {
            "message_start" => {
                self.start(&mut out);
                if let Some(usage) = event.get("message").and_then(|m| m.get("usage")) {
                    self.merge_usage(usage);
                }
            }
            "content_block_start" => {
                let block_index = event.get("index").and_then(Value::as_i64).unwrap_or(0);
                let block = event.get("content_block").cloned().unwrap_or(Value::Null);
                match block.get("type").and_then(Value::as_str) {
                    // Text/thinking blocks are opened lazily on their first
                    // delta so an empty block does not create an empty item.
                    Some("tool_use") => self.open_tool_use(block_index, &block, &mut out),
                    _ => {}
                }
            }
            "content_block_delta" => {
                let block_index = event.get("index").and_then(Value::as_i64).unwrap_or(0);
                let delta = event.get("delta").unwrap_or(&Value::Null);
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(text) = delta.get("text").and_then(Value::as_str) {
                            self.emit_text(block_index, text, &mut out);
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(text) = delta.get("thinking").and_then(Value::as_str) {
                            self.emit_reasoning(block_index, text, &mut out);
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(partial) = delta.get("partial_json").and_then(Value::as_str) {
                            self.emit_arguments(block_index, partial, &mut out);
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let block_index = event.get("index").and_then(Value::as_i64).unwrap_or(0);
                if let Some(index) = self.blocks.remove(&block_index) {
                    self.close_item(index, &mut out);
                }
            }
            "message_delta" => {
                if let Some(reason) = event.get("delta").and_then(|d| d.get("stop_reason")).and_then(Value::as_str) {
                    self.stop_reason = Some(reason.to_string());
                }
                if let Some(usage) = event.get("usage") {
                    self.merge_usage(usage);
                }
            }
            "message_stop" => {
                self.terminal = true;
                self.emit_completion(true, &mut out);
            }
            "error" => {
                let message = event
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("Anthropic upstream reported an error");
                self.start(&mut out);
                self.failed = true;
                self.terminal = true;
                self.finished = true;
                out.push(sse_event(
                    "response.failed",
                    json!({
                        "type": "response.failed",
                        "response": {
                            "id": self.response_id,
                            "object": "response",
                            "created_at": self.created_at,
                            "status": "failed",
                            "model": self.model,
                            "output": [],
                            "error": {"code": "server_error", "message": message},
                        }
                    }),
                ));
                out.push("data: [DONE]\n\n".to_string());
            }
            _ => {}
        }
        out
    }

    fn emit_completion(&mut self, terminal: bool, out: &mut Vec<String>) {
        self.start(out);
        // Drop tool items that never carried a usable identity; they are not
        // addressable downstream (same rule as the other converters).
        for (index, item) in self.items.iter_mut().enumerate() {
            if item.kind == OutputItemKind::FunctionCall
                && (item.call_id.is_empty() || item.name.is_empty())
                && !item.closed
            {
                tracing::warn!(output_index = index, "anthropic tool_use without id/name; dropped");
                item.closed = true;
            }
        }
        for index in 0..self.items.len() {
            if !self.items[index].closed {
                self.close_item(index, out);
            }
        }
        let incomplete = !terminal
            || matches!(self.stop_reason.as_deref(), Some("max_tokens"))
            || (self.stop_reason.is_none() && !self.terminal);
        let output: Vec<Value> = self
            .items
            .iter()
            .filter(|item| !(matches!(item.kind, OutputItemKind::FunctionCall | OutputItemKind::CustomToolCall) && (item.call_id.is_empty() || item.name.is_empty())))
            .map(OutputItem::done)
            .collect();
        let status = if incomplete { "incomplete" } else { "completed" };
        let response = json!({
            "id": self.response_id,
            "object": "response",
            "created_at": self.created_at,
            "status": status,
            "model": self.model,
            "output": output,
            "usage": response_usage_from_anthropic(self.last_usage.as_ref()),
            "incomplete_details": if incomplete {
                json!({"reason": "max_output_tokens"})
            } else {
                Value::Null
            },
        });
        let event_type = if incomplete { "response.incomplete" } else { "response.completed" };
        out.push(sse_event(
            event_type,
            json!({"type": event_type, "response": response}),
        ));
        out.push("data: [DONE]\n\n".to_string());
        self.finished = true;
    }

    pub fn finish(&mut self) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        let mut out = Vec::new();
        self.emit_completion(self.terminal, &mut out);
        out
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OutputItemKind {
    Message,
    FunctionCall,
    CustomToolCall,
    Reasoning,
}

/// One item in a Responses `output` array, kept mutable until the stream ends
/// so `response.completed` can carry the full final shape (the same object the
/// non-streaming converters produce).
struct OutputItem {
    id: String,
    kind: OutputItemKind,
    output_index: usize,
    text: String,
    call_id: String,
    name: String,
    arguments: String,
    args_emitted: bool,
    closed: bool,
}

impl OutputItem {
    fn added(&self) -> Value {
        match self.kind {
            OutputItemKind::Message => json!({
                "id": self.id, "type": "message", "status": "in_progress",
                "role": "assistant", "content": [],
            }),
            OutputItemKind::FunctionCall => json!({
                "id": self.id, "type": "function_call", "status": "in_progress",
                "call_id": self.call_id, "name": self.name, "arguments": "",
            }),
            OutputItemKind::CustomToolCall => json!({
                "id": self.id, "type": "custom_tool_call", "status": "in_progress",
                "call_id": self.call_id, "name": self.name, "input": "",
            }),
            OutputItemKind::Reasoning => json!({
                "id": self.id, "type": "reasoning", "summary": [],
            }),
        }
    }

    fn done(&self) -> Value {
        match self.kind {
            OutputItemKind::Message => json!({
                "id": self.id, "type": "message", "status": "completed",
                "role": "assistant",
                "content": [{"type": "output_text", "text": self.text}],
            }),
            OutputItemKind::FunctionCall => json!({
                "id": self.id, "type": "function_call", "status": "completed",
                "call_id": self.call_id, "name": self.name, "arguments": self.arguments,
            }),
            OutputItemKind::CustomToolCall => json!({
                "id": self.id, "type": "custom_tool_call", "status": "completed",
                "call_id": self.call_id, "name": self.name,
                "input": custom_input_from_arguments(&self.arguments),
            }),
            OutputItemKind::Reasoning => json!({
                "id": self.id, "type": "reasoning",
                "summary": [{"type": "summary_text", "text": self.text}],
            }),
        }
    }
}

fn response_usage_from_chat(usage: Option<&Value>) -> Value {
    let usage = usage.unwrap_or(&Value::Null);
    let raw_input = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let output = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let (cache_read, _) = super::anthropic_openai::cache_usage_from_openai(usage);
    json!({
        "input_tokens": raw_input,
        "input_tokens_details": { "cached_tokens": cache_read },
        "output_tokens": output,
        "total_tokens": raw_input + output,
    })
}

fn response_usage_from_anthropic(usage: Option<&Value>) -> Value {
    let usage = usage.unwrap_or(&Value::Null);
    let raw_input = usage.get("input_tokens").and_then(Value::as_i64).unwrap_or(0);
    let output = usage.get("output_tokens").and_then(Value::as_i64).unwrap_or(0);
    let cache_read = usage
        .get("cache_read_input_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    json!({
        "input_tokens": raw_input,
        "input_tokens_details": { "cached_tokens": cache_read },
        "output_tokens": output,
        "total_tokens": raw_input + output,
    })
}

fn sse_event(event_type: &str, data: Value) -> String {
    format!("event: {}\ndata: {}\n\n", event_type, data)
}

/// OpenAI chat SSE chunk 流 → 完整 chat.completion 对象。zen 免费档强制上游
/// `stream:true`，非流式下游在这里收成 JSON（spec §5：上游永远 SSE，下流要
/// 流式直通、要非流式本地聚合）。调用方保证传的是**已解析的 data JSON 数组**
///（SSE 切分由调用方做，`[DONE]` 不必预先剔除）；纯函数，不管 IO。
///
/// 组装规则：`choices[0].delta.content` 按序拼；`delta.tool_calls[]` 按 index
/// 归并（`function.arguments` 是分片 JSON 字符串，到齐再拼）；首个 chunk 的
/// `id`/`created` 留作响应头；`usage` 尾片保留；空流 → None（调用方记失败）。
pub fn collect_chat_stream(events: &[Value]) -> Option<Value> {
    struct Call {
        id: String,
        name: String,
        args: String,
    }
    let mut resp_id: Option<String> = None;
    let mut created: Option<Value> = None;
    let mut model: Option<String> = None;
    let mut content = String::new();
    let mut calls: HashMap<usize, Call> = HashMap::new();
    let mut finish: Option<String> = None;
    let mut usage: Option<Value> = None;
    let mut seen = false;

    for event in events {
        if event.get("object").and_then(Value::as_str) != Some("chat.completion.chunk") {
            continue;
        }
        seen = true;
        if resp_id.is_none() {
            resp_id = event.get("id").and_then(Value::as_str).map(str::to_string);
        }
        if created.is_none() {
            created = event.get("created").cloned();
        }
        if model.is_none() {
            model = event.get("model").and_then(Value::as_str).map(str::to_string);
        }
        if let Some(u) = event.get("usage") {
            if !u.is_null() {
                usage = Some(u.clone());
            }
        }
        let Some(choice) = event.get("choices").and_then(|c| c.as_array()).and_then(|a| a.first()) else {
            continue;
        };
        if let Some(fr) = choice.get("finish_reason").and_then(Value::as_str) {
            if !fr.is_empty() && fr != "null" {
                finish = Some(fr.to_string());
            }
        }
        let delta = choice.get("delta").unwrap_or(&Value::Null);
        if let Some(t) = delta.get("content").and_then(Value::as_str) {
            content.push_str(t);
        }
        if let Some(tcs) = delta.get("tool_calls").and_then(Value::as_array) {
            for tc in tcs {
                let idx = tc.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let call = calls.entry(idx).or_insert(Call {
                    id: String::new(),
                    name: String::new(),
                    args: String::new(),
                });
                if let Some(id) = tc.get("id").and_then(Value::as_str) {
                    if !id.is_empty() {
                        call.id = id.to_string();
                    }
                }
                if let Some(name) = tc.get("function").and_then(|f| f.get("name")).and_then(Value::as_str) {
                    if !name.is_empty() {
                        call.name = name.to_string();
                    }
                }
                if let Some(args) = tc.get("function").and_then(|f| f.get("arguments")).and_then(Value::as_str) {
                    call.args.push_str(args);
                }
            }
        }
    }

    if !seen {
        return None;
    }
    let mut order: Vec<usize> = calls.keys().cloned().collect();
    order.sort_unstable();
    let mut tool_calls = Vec::new();
    for idx in order {
        let call = &calls[&idx];
        tool_calls.push(json!({
            "id": call.id,
            "type": "function",
            "index": idx,
            "function": {"name": call.name, "arguments": call.args},
        }));
    }
    let has_tools = !tool_calls.is_empty();
    let mut message = Map::new();
    message.insert("role".to_string(), json!("assistant"));
    message.insert(
        "content".to_string(),
        if content.is_empty() { Value::Null } else { json!(content) },
    );
    if has_tools {
        message.insert("tool_calls".to_string(), Value::Array(tool_calls));
    }
    // finish_reason 为空时按有没有 tool_calls 兜底（上游偶尔只发 tool_calls 终片）。
    let finish_reason = finish.unwrap_or_else(|| if has_tools { "tool_calls".to_string() } else { "stop".to_string() });
    let model_name = model.unwrap_or_default();
    json!({
        "id": resp_id.map(Value::String).unwrap_or_else(|| json!(format!("chatcmpl-{}", uuid_simple()))),
        "object": "chat.completion",
        "created": created.unwrap_or(json!(0)),
        "model": model_name,
        "choices": [{"index": 0, "message": Value::Object(message), "finish_reason": finish_reason}],
        "usage": usage.unwrap_or(json!({"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0})),
    })
    .into()
}

fn uuid_simple() -> String {
    use uuid::Uuid;
    Uuid::new_v4().simple().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// spec §6：3 片 content + 2 片 tool_call arguments → 单 JSON；usage 尾片保留。
    #[test]
    fn collect_chat_stream_assembles_content_tool_calls_and_usage() {
        let events: Vec<Value> = [
            json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 7, "model": "m",
                   "choices": [{"index": 0, "delta": {"role": "assistant", "content": "hel"}, "finish_reason": null}]}),
            json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 7, "model": "m",
                   "choices": [{"index": 0, "delta": {"content": "lo "}, "finish_reason": null}]}),
            json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 7, "model": "m",
                   "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-1", "type": "function",
                       "function": {"name": "bash", "arguments": "{\"comm"}}]}, "finish_reason": null}]}),
            json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 7, "model": "m",
                   "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0,
                       "function": {"arguments": "and\": \"ls\"}"}}]}, "finish_reason": null}]}),
            json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 7, "model": "m",
                   "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}],
                   "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}}),
        ]
        .into_iter()
        .collect();
        let out = collect_chat_stream(&events).expect("non-empty stream must assemble");
        assert_eq!(out["id"], json!("chatcmpl-1"));
        assert_eq!(out["object"], json!("chat.completion"));
        assert_eq!(out["model"], json!("m"));
        assert_eq!(out["choices"][0]["message"]["content"], json!("hello "));
        let tc = &out["choices"][0]["message"]["tool_calls"][0];
        assert_eq!(tc["id"], json!("call-1"));
        assert_eq!(tc["function"]["name"], json!("bash"));
        assert_eq!(tc["function"]["arguments"], json!("{\"command\": \"ls\"}"));
        assert_eq!(out["choices"][0]["finish_reason"], json!("tool_calls"));
        assert_eq!(out["usage"]["prompt_tokens"], json!(10));
        assert_eq!(out["usage"]["completion_tokens"], json!(5));
    }

    /// spec §6：空流 → None（调用方记失败，不 panic）。
    #[test]
    fn collect_chat_stream_returns_none_for_empty_stream() {
        assert!(collect_chat_stream(&[]).is_none());
        let junk = vec![json!({"object": "not-a-chat-chunk"})];
        assert!(collect_chat_stream(&junk).is_none());
    }

    /// spec §6：纯文本流 → content 有值、无 tool_calls、finish stop。
    #[test]
    fn collect_chat_stream_text_only_has_no_tool_calls() {
        let events = vec![
            json!({"id": "c1", "object": "chat.completion.chunk", "created": 1, "model": "m",
                   "choices": [{"index": 0, "delta": {"content": "hi"}, "finish_reason": null}]}),
            json!({"id": "c1", "object": "chat.completion.chunk", "created": 1, "model": "m",
                   "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
        ];
        let out = collect_chat_stream(&events).expect("must assemble");
        assert_eq!(out["choices"][0]["message"]["content"], json!("hi"));
        assert!(out["choices"][0]["message"].get("tool_calls").is_none());
        assert_eq!(out["choices"][0]["finish_reason"], json!("stop"));
    }

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
    fn anthropic_parallel_tool_results_precede_intervening_messages() {
        let body = json!({
            "messages": [
                {"role": "user", "content": "inspect this"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "call_1", "name": "Read", "input": {"path": "a"}},
                    {"type": "tool_use", "id": "call_2", "name": "Read", "input": {"path": "b"}}
                ]},
                {"role": "user", "content": "tool progress"},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "call_2", "content": "b contents"},
                    {"type": "tool_result", "tool_use_id": "call_1", "content": "a contents"}
                ]},
                {"role": "user", "content": "continue"}
            ]
        });

        let translated = anthropic_to_responses(&body, "muse");
        let input = translated["input"].as_array().unwrap();
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[3], json!({
            "type": "function_call_output", "call_id": "call_2", "output": "b contents"
        }));
        assert_eq!(input[4], json!({
            "type": "function_call_output", "call_id": "call_1", "output": "a contents"
        }));
        assert_eq!(input[5]["content"], "tool progress");
        assert_eq!(input[6]["content"], "continue");
    }

    /// Claude Desktop 的连通性探测发 `max_tokens: 1`；上游 Responses 实现对该
    /// 字段 < 16 直接 400，原样转发会让整条聚合链路假失败。
    #[test]
    fn max_output_tokens_is_clamped_to_the_upstream_minimum() {
        let anth = json!({
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert_eq!(anthropic_to_responses(&anth, "muse")["max_output_tokens"], json!(16));

        let chat = json!({
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert_eq!(chat_to_responses(&chat, "muse")["max_output_tokens"], json!(16));

        // 正常值原样透传
        let anth = json!({
            "max_tokens": 4096,
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert_eq!(anthropic_to_responses(&anth, "muse")["max_output_tokens"], json!(4096));
    }

    #[test]
    fn anthropic_stop_sequences_are_not_sent_as_responses_stop() {
        let body = json!({
            "max_tokens": 128,
            "stop_sequences": ["\n\nHuman:"],
            "messages": [{"role": "user", "content": "hi"}]
        });

        let translated = anthropic_to_responses(&body, "muse");
        assert!(translated.get("stop").is_none());
        assert!(translated.get("stop_sequences").is_none());
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
        assert_eq!(translated["tools"][0]["strict"], false);
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
        assert_eq!(translated["tools"][0]["strict"], false);
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

    // 上游可能不发 `event:` 行，类型只在 JSON 体的 `type` 字段里（Codex/ChatGPT 的
    // Responses 后端就是这样）。此时正文/收尾都必须照常，否则整条流退化成空消息
    // —— 2026-10-12 实测的 op(gpt-6-luna) 事故：客户端只收到空 assistant 消息。
    #[test]
    fn data_only_frame_without_event_line_still_emits_text() {
        let delta = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n";

        let mut anth = ResponsesToAnthropicConverter::new("muse");
        let output = anth.feed(delta).into_iter().chain(anth.finish()).collect::<String>();
        assert!(output.contains("event: content_block_delta"), "got {output}");
        assert!(output.contains("hello"), "got {output}");

        let mut chat = ResponsesToChatConverter::new("muse");
        let out = chat.feed(delta).concat();
        assert!(out.contains("hello"), "got {out}");
    }

    #[test]
    fn data_only_completed_frame_terminates_stream() {
        let mut anth = ResponsesToAnthropicConverter::new("muse");
        let _ = anth.feed("data: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n\n");
        assert!(anth.is_done(), "无 event: 行的 response.completed 也要终止流");
        assert!(!anth.is_failed());
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

    // 错误事件同样要终止流（is_done），但**不能**被当成成功记账（is_failed）。
    // 曾经只设 terminal，调用侧用 is_done() 写 success=1 并解除账户冷却。

    #[test]
    fn chat_converter_marks_response_failed_as_failed_and_done() {
        let mut converter = ResponsesToChatConverter::new("muse");
        let failed = "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"boom\"}}}\n\n";
        let _ = converter.feed(failed);

        assert!(converter.is_done(), "错误事件应终止流");
        assert!(converter.is_failed(), "response.failed 必须判为失败");
    }

    #[test]
    fn chat_converter_marks_error_event_as_failed() {
        let mut converter = ResponsesToChatConverter::new("muse");
        let _ = converter.feed("event: error\ndata: {\"type\":\"error\",\"message\":\"boom\"}\n\n");

        assert!(converter.is_done());
        assert!(converter.is_failed());
    }

    #[test]
    fn anthropic_converter_marks_response_failed_as_failed_and_done() {
        let mut converter = ResponsesToAnthropicConverter::new("muse");
        let failed = "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"boom\"}}}\n\n";
        let _ = converter.feed(failed);

        assert!(converter.is_done());
        assert!(converter.is_failed());
    }

    #[test]
    fn anthropic_converter_marks_error_event_as_failed() {
        let mut converter = ResponsesToAnthropicConverter::new("muse");
        let _ = converter.feed("event: error\ndata: {\"type\":\"error\",\"message\":\"boom\"}\n\n");

        assert!(converter.is_done());
        assert!(converter.is_failed());
    }

    #[test]
    fn completed_stream_is_not_marked_failed() {
        let mut chat = ResponsesToChatConverter::new("muse");
        let _ = chat.feed("event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n\n");
        assert!(chat.is_done());
        assert!(!chat.is_failed(), "正常完成不该算失败");

        let mut anth = ResponsesToAnthropicConverter::new("muse");
        let _ = anth.feed("event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n\n");
        assert!(anth.is_done());
        assert!(!anth.is_failed());
    }

    #[test]
    fn max_token_incomplete_is_not_marked_failed() {
        // 撞 max_tokens 是正常收尾，不该记成失败。
        let mut converter = ResponsesToChatConverter::new("muse");
        let _ = converter.feed("event: response.incomplete\ndata: {\"type\":\"response.incomplete\",\"response\":{\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n");

        assert!(converter.is_done());
        assert!(!converter.is_failed(), "max_output_tokens 收尾不该算失败");
    }

    #[test]
    fn non_token_incomplete_is_marked_failed() {
        let mut converter = ResponsesToChatConverter::new("muse");
        let _ = converter.feed("event: response.incomplete\ndata: {\"type\":\"response.incomplete\",\"response\":{\"incomplete_details\":{\"reason\":\"content_filter\"}}}\n\n");

        assert!(converter.is_failed(), "非 token 上限的 incomplete 是真失败");
    }

    fn event_payloads(events: &[String]) -> Vec<Value> {
        events
            .iter()
            .filter_map(|event| sse_data(event))
            .filter(|data| *data != "[DONE]")
            .filter_map(|data| serde_json::from_str::<Value>(data).ok())
            .collect()
    }

    #[test]
    fn responses_chat_request_keeps_stream_and_asks_for_usage() {
        let body = json!({
            "model": "m",
            "input": "hi",
            "stream": true,
            "max_output_tokens": 64,
        });
        let out = responses_req_to_chat_req(&body);
        assert_eq!(out["stream"], json!(true));
        assert_eq!(out["stream_options"]["include_usage"], json!(true));
        assert_eq!(out["max_tokens"], json!(64));
    }

    #[test]
    fn responses_anthropic_request_keeps_stream() {
        let body = json!({
            "model": "m",
            "input": "hi",
            "stream": true,
            "max_output_tokens": 64,
        });
        let out = responses_req_to_anthropic_req(&body, "m");
        assert_eq!(out["stream"], json!(true));
        assert_eq!(out["max_tokens"], json!(64));
    }

    /// The exact shape Codex 0.5x sends: `additional_tools` namespaces with a
    /// `custom` tool, `input_text`/`output_text` parts, encrypted reasoning and
    /// custom tool call/output pairs.
    fn codex_responses_body() -> Value {
        json!({
            "model": "gpt-6-luna",
            "instructions": "be helpful",
            "input": [
                {"type": "message", "role": "developer", "content": [{"type": "input_text", "text": "You are Codex."}]},
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "ok"}]},
                {"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "gAAAA"},
                {"type": "additional_tools", "id": "at_1", "role": "developer", "tools": [
                    {"type": "namespace", "name": "functions", "description": "", "tools": [
                        {"type": "custom", "name": "exec", "description": "run js", "format": {"type": "grammar"}},
                        {"type": "function", "name": "wait", "description": "wait", "strict": false,
                         "parameters": {"type": "object", "properties": {"ms": {"type": "integer"}}}}
                    ]}
                ]},
                {"type": "custom_tool_call", "call_id": "call-1", "name": "exec", "input": "const r = 1;"},
                {"type": "custom_tool_call_output", "call_id": "call-1", "output": [{"type": "input_text", "text": "done\n"}]}
            ]
        })
    }

    #[test]
    fn codex_history_keeps_text_roles_and_tools() {
        let out = responses_req_to_chat_req(&codex_responses_body());
        let msgs = out["messages"].as_array().expect("messages");

        // Every turn survives, in order, with its text intact. Before the fix
        // `input_text` parts were dropped and this was a list of empty turns.
        assert_eq!(msgs[0], json!({"role": "system", "content": "be helpful"}));
        assert_eq!(msgs[1], json!({"role": "system", "content": "You are Codex."}));
        assert_eq!(msgs[2], json!({"role": "user", "content": "ok"}));
        // No message may carry null content — strict validators reject the
        // whole request (`messages.0.content`, `messages[8].role`).
        assert!(msgs.iter().all(|m| m["content"].is_string()), "null content leaked: {msgs:?}");

        let call = msgs.iter().find(|m| m["tool_calls"].is_array()).expect("assistant tool_calls");
        assert_eq!(call["tool_calls"][0]["id"], json!("call-1"));
        assert_eq!(call["tool_calls"][0]["function"]["name"], json!("exec"));
        assert_eq!(call["tool_calls"][0]["function"]["arguments"], json!("{\"input\":\"const r = 1;\"}"));

        let tool = msgs.iter().find(|m| m["role"] == json!("tool")).expect("tool result");
        assert_eq!(tool["content"], json!("done\n"));

        // Tools declared inside `additional_tools` reach the upstream.
        let tools = out["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 2, "{tools:?}");
        let exec = tools.iter().find(|t| t["function"]["name"] == json!("exec")).expect("exec");
        assert_eq!(exec["function"]["parameters"]["required"], json!(["input"]));
        let wait = tools.iter().find(|t| t["function"]["name"] == json!("wait")).expect("wait");
        assert_eq!(wait["function"]["parameters"]["properties"]["ms"]["type"], json!("integer"));
    }

    #[test]
    fn codex_custom_tool_names_are_collected() {
        let names = responses_custom_tool_names(&codex_responses_body());
        assert!(names.contains("exec"), "{names:?}");
        assert!(!names.contains("wait"), "function tools are not custom: {names:?}");
    }

    #[test]
    fn custom_tool_call_non_streaming_round_trip() {
        let chat = json!({"choices": [{"message": {"content": null, "tool_calls": [
            {"id": "call-1", "function": {"name": "exec", "arguments": "{\"input\":\"console.log(1)\"}"}},
            {"id": "call-2", "function": {"name": "wait", "arguments": "{\"ms\":5}"}}
        ]}}]});
        let custom: HashSet<String> = ["exec".to_string()].into_iter().collect();
        let resp = chat_resp_to_responses_resp(&chat, "m", &custom);
        let output = resp["output"].as_array().expect("output");
        assert_eq!(output[0]["type"], json!("custom_tool_call"));
        assert_eq!(output[0]["call_id"], json!("call-1"));
        assert_eq!(output[0]["input"], json!("console.log(1)"));
        assert_eq!(output[1]["type"], json!("function_call"));
        assert_eq!(output[1]["arguments"], json!("{\"ms\":5}"));
    }

    #[test]
    fn custom_tool_call_streaming_round_trip() {
        let mut converter = ChatToResponsesConverter::new("m");
        converter.set_custom_tools(["exec".to_string()].into_iter().collect());
        let mut events = Vec::new();
        events.extend(converter.feed(&json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-1", "function": {"name": "exec", "arguments": "{\"input\":\"const "}}]}, "finish_reason": null}]
        })));
        events.extend(converter.feed(&json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "function": {"arguments": "r = 1;\"}"}}]}, "finish_reason": "tool_calls"}]
        })));
        converter.mark_done();
        events.extend(converter.finish());

        let payloads = event_payloads(&events);
        let added = payloads
            .iter()
            .find(|e| e["type"] == json!("response.output_item.added"))
            .expect("item added");
        assert_eq!(added["item"]["type"], json!("custom_tool_call"));
        assert_eq!(added["item"]["call_id"], json!("call-1"));
        assert_eq!(added["item"]["name"], json!("exec"));
        assert!(
            !payloads.iter().any(|e| e["type"] == json!("response.function_call_arguments.delta")),
            "custom tools must not stream JSON argument deltas"
        );
        let input_done = payloads
            .iter()
            .find(|e| e["type"] == json!("response.custom_tool_call_input.done"))
            .expect("custom input done");
        assert_eq!(input_done["input"], json!("const r = 1;"));

        let completed = payloads.iter().find(|e| e["type"] == json!("response.completed")).expect("completed");
        let item = completed["response"]["output"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["type"] == json!("custom_tool_call"))
            .expect("custom_tool_call item");
        assert_eq!(item["input"], json!("const r = 1;"));
        assert_eq!(item["call_id"], json!("call-1"));
    }

    #[test]
    fn codex_history_to_anthropic_never_emits_invalid_messages() {
        let out = responses_req_to_anthropic_req(&codex_responses_body(), "m");
        let msgs = out["messages"].as_array().expect("messages");
        for m in msgs {
            assert!(m["role"] == json!("user") || m["role"] == json!("assistant"), "{m}");
            assert!(m["content"].is_array(), "null content leaked: {m}");
        }
        // developer + user merge into one user turn; text survives.
        let texts: Vec<&str> = msgs[0]["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect();
        assert_eq!(texts, vec!["You are Codex.", "ok"]);

        let tool_use = msgs
            .iter()
            .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
            .find(|b| b["type"] == json!("tool_use"))
            .expect("tool_use block");
        assert_eq!(tool_use["name"], json!("exec"));
        assert_eq!(tool_use["input"], json!({"input": "const r = 1;"}));

        let tool_result = msgs
            .iter()
            .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
            .find(|b| b["type"] == json!("tool_result"))
            .expect("tool_result block");
        assert_eq!(tool_result["content"], json!("done\n"));

        let tools = out["tools"].as_array().expect("tools");
        let exec = tools.iter().find(|t| t["name"] == json!("exec")).expect("exec");
        assert_eq!(exec["input_schema"]["required"], json!(["input"]));
    }

    #[test]
    fn chat_stream_becomes_responses_events() {
        let mut converter = ChatToResponsesConverter::new("muse");
        let mut events = Vec::new();
        events.extend(converter.feed(&json!({
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": "hel"}, "finish_reason": null}]
        })));
        events.extend(converter.feed(&json!({
            "choices": [{"index": 0, "delta": {"content": "lo"}, "finish_reason": null}]
        })));
        events.extend(converter.feed(&json!({
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        })));
        converter.mark_done();
        events.extend(converter.finish());

        let payloads = event_payloads(&events);
        assert_eq!(payloads[0]["type"], json!("response.created"));
        assert!(payloads.iter().any(|e| e["type"] == json!("response.output_text.delta") && e["delta"] == json!("hel")));
        let done = payloads.iter().find(|e| e["type"] == json!("response.output_text.done")).expect("text done event");
        assert_eq!(done["text"], json!("hello"));
        let completed = payloads.iter().find(|e| e["type"] == json!("response.completed")).expect("completed event");
        assert_eq!(completed["response"]["status"], json!("completed"));
        assert_eq!(completed["response"]["output"][0]["content"][0]["text"], json!("hello"));
        assert_eq!(completed["response"]["usage"]["input_tokens"], json!(10));
        assert_eq!(completed["response"]["usage"]["output_tokens"], json!(5));
        assert!(events.last().is_some_and(|e| e == "data: [DONE]\n\n"));
    }

    #[test]
    fn chat_stream_tool_fragments_become_function_call_item() {
        let mut converter = ChatToResponsesConverter::new("muse");
        let mut events = Vec::new();
        // Identity and arguments arrive split across fragments, then the
        // remainder only carries arguments — the converter must buffer until
        // the call is addressable and still emit every byte exactly once.
        events.extend(converter.feed(&json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "function": {"arguments": "{\"cmd\":"}}]}, "finish_reason": null}]
        })));
        events.extend(converter.feed(&json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-1", "type": "function", "function": {"name": "bash"}}]}, "finish_reason": null}]
        })));
        events.extend(converter.feed(&json!({
            "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "function": {"arguments": "\"ls\"}"}}]}, "finish_reason": null}]
        })));
        events.extend(converter.feed(&json!({
            "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
        })));
        converter.mark_done();
        events.extend(converter.finish());

        let payloads = event_payloads(&events);
        let deltas: String = payloads
            .iter()
            .filter(|e| e["type"] == json!("response.function_call_arguments.delta"))
            .filter_map(|e| e["delta"].as_str())
            .collect();
        assert_eq!(deltas, "{\"cmd\":\"ls\"}");
        let added = payloads
            .iter()
            .find(|e| e["type"] == json!("response.output_item.added") && e["item"]["type"] == json!("function_call"))
            .expect("function_call added event");
        assert_eq!(added["item"]["call_id"], json!("call-1"), "added must be addressable");
        assert_eq!(added["item"]["name"], json!("bash"), "added must carry the tool name");
        let completed = payloads.iter().find(|e| e["type"] == json!("response.completed")).expect("completed event");
        let call = completed["response"]["output"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["type"] == json!("function_call"))
            .expect("function_call item");
        assert_eq!(call["call_id"], json!("call-1"));
        assert_eq!(call["name"], json!("bash"));
        assert_eq!(call["arguments"], json!("{\"cmd\":\"ls\"}"));
    }

    #[test]
    fn chat_stream_error_marks_converter_failed() {
        let mut converter = ChatToResponsesConverter::new("muse");
        let events = converter.feed(&json!({"error": {"message": "boom"}}));
        let payloads = event_payloads(&events);
        let failed = payloads.iter().find(|e| e["type"] == json!("response.failed")).expect("failed event");
        assert_eq!(failed["response"]["error"]["message"], json!("boom"));
        assert!(events.last().is_some_and(|e| e == "data: [DONE]\n\n"));
        assert!(converter.is_done());
        assert!(converter.is_failed());
    }

    #[test]
    fn anthropic_stream_becomes_responses_events() {
        let mut converter = AnthropicToResponsesConverter::new("muse");
        let mut events = Vec::new();
        events.extend(converter.feed(&json!({
            "type": "message_start",
            "message": {"id": "msg_1", "usage": {"input_tokens": 12, "cache_read_input_tokens": 2}}
        })));
        events.extend(converter.feed(&json!({
            "type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}
        })));
        events.extend(converter.feed(&json!({
            "type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "hel"}
        })));
        events.extend(converter.feed(&json!({
            "type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "lo"}
        })));
        events.extend(converter.feed(&json!({"type": "content_block_stop", "index": 0})));
        events.extend(converter.feed(&json!({
            "type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 7}
        })));
        events.extend(converter.feed(&json!({"type": "message_stop"})));
        events.extend(converter.finish());

        let payloads = event_payloads(&events);
        assert_eq!(payloads[0]["type"], json!("response.created"));
        assert!(payloads.iter().any(|e| e["type"] == json!("response.output_text.delta") && e["delta"] == json!("hel")));
        let completed = payloads.iter().find(|e| e["type"] == json!("response.completed")).expect("completed event");
        assert_eq!(completed["response"]["status"], json!("completed"));
        assert_eq!(completed["response"]["output"][0]["content"][0]["text"], json!("hello"));
        assert_eq!(completed["response"]["usage"]["input_tokens"], json!(12));
        assert_eq!(completed["response"]["usage"]["input_tokens_details"]["cached_tokens"], json!(2));
        assert_eq!(completed["response"]["usage"]["output_tokens"], json!(7));
    }

    #[test]
    fn anthropic_stream_tool_use_becomes_function_call_item() {
        let mut converter = AnthropicToResponsesConverter::new("muse");
        let mut events = Vec::new();
        events.extend(converter.feed(&json!({"type": "message_start", "message": {"usage": {}}})));
        events.extend(converter.feed(&json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "tool_use", "id": "toolu_1", "name": "bash", "input": {}}
        })));
        events.extend(converter.feed(&json!({
            "type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"cmd\":"}
        })));
        events.extend(converter.feed(&json!({
            "type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "\"ls\"}"}
        })));
        events.extend(converter.feed(&json!({"type": "content_block_stop", "index": 0})));
        events.extend(converter.feed(&json!({
            "type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 3}
        })));
        events.extend(converter.feed(&json!({"type": "message_stop"})));

        let payloads = event_payloads(&events);
        let added = payloads
            .iter()
            .find(|e| e["type"] == json!("response.output_item.added") && e["item"]["type"] == json!("function_call"))
            .expect("function_call added event");
        assert_eq!(added["item"]["call_id"], json!("toolu_1"), "added must be addressable");
        assert_eq!(added["item"]["name"], json!("bash"), "added must carry the tool name");
        let completed = payloads.iter().find(|e| e["type"] == json!("response.completed")).expect("completed event");
        let call = completed["response"]["output"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["type"] == json!("function_call"))
            .expect("function_call item");
        assert_eq!(call["call_id"], json!("toolu_1"));
        assert_eq!(call["name"], json!("bash"));
        assert_eq!(call["arguments"], json!("{\"cmd\":\"ls\"}"));
    }

    #[test]
    fn anthropic_max_tokens_is_incomplete_not_failed() {
        let mut converter = AnthropicToResponsesConverter::new("muse");
        let _ = converter.feed(&json!({
            "type": "message_delta", "delta": {"stop_reason": "max_tokens"}, "usage": {"output_tokens": 9}
        }));
        let events = converter.feed(&json!({"type": "message_stop"}));
        let payloads = event_payloads(&events);
        let incomplete = payloads.iter().find(|e| e["type"] == json!("response.incomplete")).expect("incomplete event");
        assert_eq!(incomplete["response"]["incomplete_details"]["reason"], json!("max_output_tokens"));
        assert!(converter.is_done());
        assert!(!converter.is_failed());
    }
}
