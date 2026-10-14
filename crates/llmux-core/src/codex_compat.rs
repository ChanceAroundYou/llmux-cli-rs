//! Codex 私有 Responses payload ↔ 标准 OpenAI Responses API 双向翻译。
//!
//! 从 sidecar `compat.go` 移植（`chatToResponse` / `responseToChat` /
//! `normalizeResponseInput` / `normalizeInputItem` / `prepareResponse` /
//! `normalizeCodexPayload` / `normalizeBackendEvent`）。
//!
//! 触发条件：`provider_id=codex` 且目标协议为 Responses 时。

use serde_json::{json, Map, Value};

/// codex 后端地址（`backend.go` 的 `BackendURL` 默认值）。
pub const CODEX_BACKEND_BASE: &str = "https://chatgpt.com/backend-api/codex";
/// `originator` 私有头（`backend.go:headers`）。
pub const CODEX_ORIGINATOR: &str = "openai-api-server-via-codex";
/// `GET /codex/models` 的 `?client_version=`。服务端按它过滤兼容的模型：
/// 老版本（如 0.30.0）回空列表，未知的新版本（如 1.0.0/99.0.0）直接放行全量。
/// 取官方 CLI 最新 release（2026-10-14 实测 `0.162.1` 回 10 个模型），跟 CLI 走。
pub const CODEX_MODELS_CLIENT_VERSION: &str = "0.162.1";

// --- 小工具（对应 compat.go 顶部的 mapAny/sliceAny/stringValue/setDefault/valueOr）---

fn as_obj(v: &Value) -> Option<&Map<String, Value>> {
    v.as_object()
}

fn arr_of(v: Option<&Value>) -> Vec<&Value> {
    match v {
        Some(Value::Array(a)) => a.iter().collect(),
        _ => Vec::new(),
    }
}

fn str_of(v: Option<&Value>) -> &str {
    v.and_then(Value::as_str).unwrap_or("")
}

fn set_default(m: &mut Map<String, Value>, key: &str, value: Value) {
    if !m.contains_key(key) {
        m.insert(key.to_string(), value);
    }
}

// --- normalizeInputItem：单个 input 条目规范化 ---

/// 规范化单个 Responses input 条目。`None` = 丢弃（reasoning 无 encrypted_content）。
fn normalize_input_item(value: &Value) -> Option<Value> {
    let item = as_obj(value)?;
    match item.get("type").and_then(Value::as_str) {
        Some("reasoning") => {
            let enc = item.get("encrypted_content").and_then(Value::as_str).unwrap_or("");
            if enc.is_empty() {
                return None;
            }
            let summary = item
                .get("summary")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Some(json!({
                "type": "reasoning",
                "encrypted_content": enc,
                "summary": summary,
            }))
        }
        Some("message") => {
            if item.get("role").and_then(Value::as_str) == Some("assistant") {
                let mut result = Map::new();
                result.insert("role".to_string(), json!("assistant"));
                result.insert("content".to_string(), Value::String(message_text(value)));
                if let Some(phase) = item.get("phase") {
                    result.insert("phase".to_string(), phase.clone());
                }
                Some(Value::Object(result))
            } else {
                Some(value.clone())
            }
        }
        Some("function_call") => {
            let mut result = item.clone();
            result.remove("id");
            result.remove("status");
            let call_id = str_of(item.get("call_id"));
            let id = str_of(item.get("id"));
            let call_id = if call_id.is_empty() { id } else { call_id };
            result.insert("call_id".to_string(), Value::String(call_id.to_string()));
            let args = str_of(item.get("arguments"));
            let args = if args.is_empty() { "{}" } else { args };
            result.insert("arguments".to_string(), Value::String(args.to_string()));
            Some(Value::Object(result))
        }
        Some("function_call_output") => {
            let mut result = item.clone();
            result.remove("id");
            result.remove("status");
            let call_id = str_of(item.get("call_id"));
            let call_id = if call_id.is_empty() { "unknown" } else { call_id };
            result.insert("call_id".to_string(), Value::String(call_id.to_string()));
            Some(Value::Object(result))
        }
        _ => Some(value.clone()),
    }
}

// --- normalizeResponseInput：Responses input 字段规范化 ---

/// 接受 nil / string / array / 单对象，把 input 变成规范数组。
pub fn normalize_response_input(value: &Value) -> Vec<Value> {
    match value {
        Value::Null => Vec::new(),
        Value::String(s) => vec![json!({"role": "user", "content": s})],
        Value::Array(items) => items
            .iter()
            .filter_map(normalize_input_item)
            .collect(),
        Value::Object(_) => normalize_input_item(value).into_iter().collect(),
        other => vec![json!({"role": "user", "content": other.to_string()})],
    }
}

// --- prepareResponse：Responses 请求补默认值 ---

/// 标准 Responses 请求 → 补 `model` / `input` 规范化 / `instructions` / `store=false`。
pub fn prepare_response(body: &Value, model: &str) -> Value {
    let mut result = body.as_object().cloned().unwrap_or_default();
    if str_of(result.get("model")).is_empty() {
        result.insert("model".to_string(), Value::String(model.to_string()));
    }
    let input = normalize_response_input(result.get("input").unwrap_or(&Value::Null));
    result.insert("input".to_string(), Value::Array(input));
    set_default(
        &mut result,
        "instructions",
        Value::String("You are a helpful assistant.".to_string()),
    );
    set_default(&mut result, "store", Value::Bool(false));
    Value::Object(result)
}

// --- chatToResponse：Chat 请求 → Responses 请求 ---

fn content_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => {
            let mut out = String::new();
            for raw in parts {
                if let Some(p) = raw.as_object() {
                    match p.get("type").and_then(Value::as_str) {
                        Some("text") | Some("input_text") | Some("output_text") => {
                            out.push_str(p.get("text").and_then(Value::as_str).unwrap_or(""));
                        }
                        _ => {}
                    }
                }
            }
            out
        }
        _ => String::new(),
    }
}

fn chat_content(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(_)) => value.cloned().unwrap_or(Value::Null),
        Some(Value::Array(items)) => {
            let mut parts: Vec<Value> = Vec::new();
            for raw in items {
                let Some(p) = raw.as_object() else { continue };
                match p.get("type").and_then(Value::as_str) {
                    Some("text") | Some("input_text") => {
                        parts.push(json!({
                            "type": "input_text",
                            "text": p.get("text").and_then(Value::as_str).unwrap_or(""),
                        }));
                    }
                    Some("image_url") => {
                        let u = p.get("image_url");
                        let url = match u {
                            Some(Value::Object(m)) => {
                                m.get("url").cloned().unwrap_or(Value::Null)
                            }
                            other => other.cloned().unwrap_or(Value::Null),
                        };
                        parts.push(json!({"type": "input_image", "image_url": url}));
                    }
                    Some("input_image") => parts.push(raw.clone()),
                    _ => {}
                }
            }
            Value::Array(parts)
        }
        _ => Value::Null,
    }
}

fn chat_tool_choice(value: &Value) -> Value {
    if let Some(m) = value.as_object() {
        if m.get("type").and_then(Value::as_str) == Some("function") {
            let name = m
                .get("function")
                .and_then(|f| f.as_object())
                .and_then(|f| f.get("name"))
                .cloned()
                .unwrap_or(Value::Null);
            return json!({"type": "function", "name": name});
        }
    }
    value.clone()
}

/// Chat Completions 请求 → 标准 Responses 请求。
pub fn chat_to_response(body: &Value, model: &str) -> Value {
    let body_obj = body.as_object().cloned().unwrap_or_default();
    let model_name = str_of(body_obj.get("model"));
    let model_name = if model_name.is_empty() { model } else { model_name };

    let mut instructions: Vec<String> = Vec::new();
    let mut input: Vec<Value> = Vec::new();

    for raw in arr_of(body_obj.get("messages")) {
        let Some(msg) = raw.as_object() else { continue };
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
        match role {
            "system" | "developer" => {
                let t = content_text(msg.get("content"));
                if !t.is_empty() {
                    instructions.push(t);
                }
            }
            "tool" | "function" => {
                let call_id = str_of(msg.get("tool_call_id"));
                let name = str_of(msg.get("name"));
                let call_id = if call_id.is_empty() { name } else { call_id };
                let call_id = if call_id.is_empty() { "unknown" } else { call_id };
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": content_text(msg.get("content")),
                }));
            }
            "assistant" => {
                if let Some(fc) = msg.get("function_call").and_then(|f| f.as_object()) {
                    let name = str_of(fc.get("name"));
                    input.push(json!({
                        "type": "function_call",
                        "call_id": if name.is_empty() { "function" } else { name },
                        "name": fc.get("name").cloned().unwrap_or(Value::Null),
                        "arguments": fc.get("arguments").and_then(Value::as_str).filter(|s| !s.is_empty()).map(|s| Value::String(s.to_string())).unwrap_or(json!("{}")),
                    }));
                }
                for tc_raw in arr_of(msg.get("tool_calls")) {
                    let Some(tc) = tc_raw.as_object() else { continue };
                    if let Some(f) = tc.get("function").and_then(|f| f.as_object()) {
                        input.push(json!({
                            "type": "function_call",
                            "call_id": tc.get("id").cloned().unwrap_or(Value::Null),
                            "name": f.get("name").cloned().unwrap_or(Value::Null),
                            "arguments": f.get("arguments").and_then(Value::as_str).filter(|s| !s.is_empty()).map(|s| Value::String(s.to_string())).unwrap_or(json!("{}")),
                        }));
                    }
                }
                let text = content_text(msg.get("content"));
                let has_tool_calls = msg
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .map(|a| !a.is_empty())
                    .unwrap_or(false);
                if !text.is_empty() || !has_tool_calls {
                    input.push(json!({"role": "assistant", "content": text}));
                }
            }
            "user" => {
                input.push(json!({"role": "user", "content": chat_content(msg.get("content"))}));
            }
            _ => {}
        }
    }

    let instructions = instructions.join("\n\n");
    let instructions = if instructions.is_empty() {
        "You are a helpful assistant.".to_string()
    } else {
        instructions
    };

    let mut result = Map::new();
    result.insert("model".to_string(), Value::String(model_name.to_string()));
    result.insert("input".to_string(), Value::Array(input));
    result.insert("store".to_string(), Value::Bool(false));
    result.insert("instructions".to_string(), Value::String(instructions));

    for key in ["parallel_tool_calls", "service_tier", "temperature", "top_p", "user"] {
        if let Some(v) = body_obj.get(key) {
            result.insert(key.to_string(), v.clone());
        }
    }
    if let Some(v) = body_obj.get("max_completion_tokens").or_else(|| body_obj.get("max_tokens")) {
        result.insert("max_output_tokens".to_string(), v.clone());
    }
    if let Some(v) = body_obj.get("reasoning") {
        result.insert("reasoning".to_string(), v.clone());
    } else if let Some(effort) = body_obj.get("reasoning_effort") {
        result.insert("reasoning".to_string(), json!({"effort": effort}));
    }
    if let Some(v) = body_obj.get("verbosity") {
        result.insert("text".to_string(), json!({"verbosity": v}));
    }

    let mut tools: Vec<Value> = Vec::new();
    for raw in arr_of(body_obj.get("tools")) {
        let Some(tool) = raw.as_object() else { continue };
        if tool.get("type").and_then(Value::as_str) == Some("function") {
            if let Some(f) = tool.get("function").and_then(|f| f.as_object()) {
                let mut copied = f.clone();
                copied.insert("type".to_string(), json!("function"));
                tools.push(Value::Object(copied));
            }
        } else {
            tools.push(Value::Object(tool.clone()));
        }
    }
    for raw in arr_of(body_obj.get("functions")) {
        if let Some(f) = raw.as_object() {
            let mut copied = f.clone();
            copied.insert("type".to_string(), json!("function"));
            tools.push(Value::Object(copied));
        }
    }
    if !tools.is_empty() {
        result.insert("tools".to_string(), Value::Array(tools));
    }
    if let Some(choice) = body_obj.get("tool_choice") {
        result.insert("tool_choice".to_string(), chat_tool_choice(choice));
    } else if let Some(choice) = body_obj.get("function_call") {
        // `{"name": ...}` → `{"type":"function","name":...}`
        let name = choice
            .as_object()
            .and_then(|m| m.get("name"))
            .cloned()
            .unwrap_or(Value::Null);
        result.insert("tool_choice".to_string(), json!({"type": "function", "name": name}));
    }
    if let Some(format) = body_obj.get("response_format").and_then(|f| f.as_object()) {
        match format.get("type").and_then(Value::as_str) {
            Some("json_object") => {
                let mut text = result
                    .get("text")
                    .and_then(|t| t.as_object())
                    .cloned()
                    .unwrap_or_default();
                text.insert("format".to_string(), json!({"type": "json_object"}));
                result.insert("text".to_string(), Value::Object(text));
            }
            Some("json_schema") => {
                let schema = format
                    .get("json_schema")
                    .and_then(|s| s.as_object())
                    .cloned()
                    .unwrap_or_default();
                let mut f = Map::new();
                f.insert("type".to_string(), json!("json_schema"));
                for k in ["name", "schema", "strict", "description"] {
                    if let Some(v) = schema.get(k) {
                        f.insert(k.to_string(), v.clone());
                    }
                }
                let mut text = result
                    .get("text")
                    .and_then(|t| t.as_object())
                    .cloned()
                    .unwrap_or_default();
                text.insert("format".to_string(), Value::Object(f));
                result.insert("text".to_string(), Value::Object(text));
            }
            _ => {}
        }
    }

    Value::Object(result)
}

// --- normalizeCodexPayload：codex 后端请求预处理 ---

/// 标准 Responses 请求 → codex 后端私有负载。
/// 对应 `backend.go:normalizeCodexPayload`：删 `max_output_tokens`，强制
/// `stream=true` / `store=false`，默认 `verbosity=low`，补
/// `include=["reasoning.encrypted_content"]`。
pub fn normalize_codex_payload(prepared: &Value, drop_params: &[String]) -> Value {
    let mut result = prepared.as_object().cloned().unwrap_or_default();
    for name in drop_params {
        result.remove(name);
    }
    result.remove("max_output_tokens");
    // 每条 input.content 去掉 prompt_cache_breakpoint（codex 不认）
    if let Some(Value::Array(items)) = result.get_mut("input") {
        for item in items.iter_mut() {
            if let Some(parts) = item.get_mut("content").and_then(|c| c.as_array_mut()) {
                for part in parts.iter_mut() {
                    if let Some(obj) = part.as_object_mut() {
                        obj.remove("prompt_cache_breakpoint");
                    }
                }
            }
        }
    }
    result.insert("stream".to_string(), Value::Bool(true));
    result.insert("store".to_string(), Value::Bool(false));
    set_default(&mut result, "tool_choice", json!("auto"));
    set_default(&mut result, "parallel_tool_calls", Value::Bool(true));
    let mut text = result
        .get("text")
        .and_then(|t| t.as_object())
        .cloned()
        .unwrap_or_default();
    set_default(&mut text, "verbosity", json!("low"));
    result.insert("text".to_string(), Value::Object(text));
    let mut include: Vec<Value> = result
        .get("include")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !include.iter().any(|v| v.as_str() == Some("reasoning.encrypted_content")) {
        include.push(json!("reasoning.encrypted_content"));
    }
    result.insert("include".to_string(), Value::Array(include));
    Value::Object(result)
}

// --- normalizeBackendEvent：SSE 事件规范化 ---

/// `response.done` → `response.completed`；非法 `status` 删掉。
/// 对应 `backend.go:normalizeBackendEvent`。
pub fn normalize_backend_event(event: &mut Value) {
    if event.get("type").and_then(Value::as_str) == Some("response.done") {
        if let Some(obj) = event.as_object_mut() {
            obj.insert("type".to_string(), json!("response.completed"));
        }
    }
    let valid = matches!(
        event
            .get("response")
            .and_then(|r| r.get("status"))
            .and_then(Value::as_str),
        Some("completed" | "failed" | "in_progress" | "cancelled" | "queued" | "incomplete")
    );
    if !valid {
        if let Some(resp) = event.get_mut("response").and_then(|r| r.as_object_mut()) {
            resp.remove("status");
        }
    }
}

// --- 响应侧：codex 响应文本提取 / Chat 转换 ---

fn message_text_value(item: &Value) -> String {
    let mut out = String::new();
    if let Some(content) = item.get("content").and_then(Value::as_array) {
        for raw in content {
            if let Some(p) = raw.as_object() {
                if p.get("type").and_then(Value::as_str) == Some("output_text") {
                    out.push_str(p.get("text").and_then(Value::as_str).unwrap_or(""));
                }
            }
        }
    }
    out
}

/// codex 响应 `output` 数组拼出纯文本。
pub fn response_text(response: &Value) -> String {
    if let Some(s) = response.get("output_text").and_then(Value::as_str) {
        return s.to_string();
    }
    let mut out = String::new();
    for raw in arr_of(response.get("output")) {
        if let Some(item) = raw.as_object() {
            if item.get("type").and_then(Value::as_str) == Some("message") {
                out.push_str(&message_text_value(raw));
            }
        }
    }
    out
}

/// codex 响应 `output` 转成 Responses 上下文条目（多轮对话续接用）。
pub fn response_context(response: &Value) -> Vec<Value> {
    let mut result: Vec<Value> = Vec::new();
    for raw in arr_of(response.get("output")) {
        if let Some(norm) = normalize_input_item(raw) {
            result.push(norm);
        }
    }
    if result.is_empty() {
        let text = response_text(response);
        if !text.is_empty() {
            result.push(json!({"role": "assistant", "content": text}));
        }
    }
    result
}

fn message_text(item: &Value) -> String {
    message_text_value(item)
}

/// Codex Responses 响应 → Chat Completions 响应。
/// 对应 `compat.go:responseToChat`。`legacy=true` 时 tool call 走旧 `function_call`。
pub fn response_to_chat(response: &Value, fallback_model: &str, legacy: bool, n: usize) -> Value {
    let usage = response.get("usage").and_then(|u| u.as_object());
    let prompt = usage
        .and_then(|u| u.get("input_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let completion = usage
        .and_then(|u| u.get("output_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let mut total = usage
        .and_then(|u| u.get("total_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if total == 0 {
        total = prompt + completion;
    }

    let mut tool_calls: Vec<Value> = Vec::new();
    let mut legacy_call: Option<Value> = None;
    for raw in arr_of(response.get("output")) {
        let Some(item) = raw.as_object() else { continue };
        if item.get("type").and_then(Value::as_str) != Some("function_call") {
            continue;
        }
        let name = str_of(item.get("name"));
        let args = str_of(item.get("arguments"));
        let f = json!({"name": name, "arguments": args});
        if legacy {
            if legacy_call.is_none() {
                legacy_call = Some(f);
            }
        } else {
            let call_id = str_of(item.get("call_id"));
            let id = str_of(item.get("id"));
            let call_id = if call_id.is_empty() { id } else { call_id };
            tool_calls.push(json!({
                "id": call_id,
                "type": "function",
                "function": f,
            }));
        }
    }

    let mut message = Map::new();
    message.insert("role".to_string(), json!("assistant"));
    message.insert("content".to_string(), Value::String(response_text(response)));
    if let Some(lc) = legacy_call.clone() {
        message.insert("function_call".to_string(), lc);
    } else if !tool_calls.is_empty() {
        message.insert("content".to_string(), Value::Null);
        message.insert("tool_calls".to_string(), Value::Array(tool_calls.clone()));
    }

    let mut finish = "stop";
    if !tool_calls.is_empty() || legacy_call.is_some() {
        finish = "tool_calls";
    }
    if response.get("status").and_then(Value::as_str) == Some("incomplete") {
        finish = "length";
    }

    let n = n.max(1);
    let choices: Vec<Value> = (0..n)
        .map(|i| {
            json!({
                "index": i,
                "message": message,
                "finish_reason": finish,
                "logprobs": Value::Null,
            })
        })
        .collect();

    let id = str_of(response.get("id"));
    let id = if let Some(rest) = id.strip_prefix("resp_") {
        format!("chatcmpl_{rest}")
    } else {
        format!("chatcmpl_{id}")
    };
    let created = response
        .get("created_at")
        .and_then(Value::as_i64)
        .filter(|&c| c != 0)
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        });
    let model = str_of(response.get("model"));
    let model = if model.is_empty() { fallback_model } else { model };

    json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": choices,
        "usage": {
            "prompt_tokens": prompt,
            "completion_tokens": completion,
            "total_tokens": total,
        },
    })
}

/// 补全 Responses 响应缺省字段。对应 `compat.go:ensureResponse`。
pub fn ensure_response(response: &Value, request: &Value) -> Value {
    let mut result = response.as_object().cloned().unwrap_or_default();
    use std::time::{SystemTime, UNIX_EPOCH};
    if str_of(result.get("id")).is_empty() {
        let rand_hex: String = (0..8).map(|_| format!("{:02x}", rand::random::<u8>())).collect();
        result.insert("id".to_string(), json!(format!("resp_{rand_hex}")));
    }
    set_default(&mut result, "object", json!("response"));
    if result.get("created_at").is_none() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        result.insert("created_at".to_string(), json!(now));
    }
    set_default(&mut result, "status", json!("completed"));
    if str_of(result.get("model")).is_empty() {
        let m = str_of(request.get("model"));
        result.insert("model".to_string(), Value::String(m.to_string()));
    }
    set_default(&mut result, "output", json!([]));
    set_default(&mut result, "parallel_tool_calls", Value::Bool(true));
    if result.get("tool_choice").is_none() {
        let tc = request.get("tool_choice").cloned().unwrap_or(json!("auto"));
        result.insert("tool_choice".to_string(), tc);
    }
    if result.get("tools").is_none() {
        let tools = request.get("tools").and_then(Value::as_array).cloned().unwrap_or_default();
        result.insert("tools".to_string(), Value::Array(tools));
    }
    if !result.contains_key("previous_response_id") {
        result.insert(
            "previous_response_id".to_string(),
            request.get("previous_response_id").cloned().unwrap_or(Value::Null),
        );
    }
    if !result.contains_key("usage") {
        result.insert("usage".to_string(), Value::Null);
    }
    Value::Object(result)
}

// --- collect_stream：SSE 事件流组装成完整 Responses 对象 ---

/// Codex SSE 事件流 → 完整 Responses 对象（`backend.go:collect` 移植）。
///
/// codex 后端对非流式请求回的不是标准 JSON（实测首字节是 `event:` 行），
/// 直接 `serde_json::from_slice` 必炸。按 sidecar 的做法：逐事件累积
/// `output_item.done` 的 item、`output_text.delta` 的文本、`completed` 的
/// response，拼出完整对象。调用方保证传进来的是**已解析的事件 JSON 数组**
///（SSE 切分 + `normalize_backend_event` 由调用方做，纯函数不管 IO）。
pub fn collect_stream(events: &[Value], request: &Value) -> Value {
    let mut completed: Option<Value> = None;
    let mut output: Vec<Value> = Vec::new();
    let mut text = String::new();
    let mut response_id = String::new();

    for event in events {
        match event.get("type").and_then(Value::as_str).unwrap_or("") {
            "response.created" => {
                if let Some(id) = event.get("response").and_then(|r| r.get("id")).and_then(Value::as_str) {
                    response_id = id.to_string();
                }
            }
            "response.output_text.delta" => {
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    text.push_str(delta);
                }
            }
            "response.output_item.done" => {
                if let Some(item) = event.get("item") {
                    output.push(item.clone());
                }
            }
            "response.completed" | "response.incomplete" => {
                if let Some(resp) = event.get("response") {
                    completed = Some(resp.clone());
                }
            }
            "response.failed" => {
                if let Some(resp) = event.get("response") {
                    completed = Some(resp.clone());
                }
            }
            _ => {}
        }
    }

    if let Some(mut done) = completed {
        // completed.output 为空时用累积的 output 补上（sidecar 同逻辑）。
        let empty = done.get("output").and_then(Value::as_array).map(|a| a.is_empty()).unwrap_or(true);
        if empty && !output.is_empty() {
            done["output"] = Value::Array(output);
        }
        return ensure_response(&done, request);
    }
    // 没收到 completed：用累积的文本拼一个最小响应（sidecar 的 fallback 分支）。
    if response_id.is_empty() {
        let rand_hex: String = (0..8).map(|_| format!("{:02x}", rand::random::<u8>())).collect();
        response_id = format!("resp_{rand_hex}");
    }
    let model = str_of(request.get("model")).to_string();
    let tool_choice = request.get("tool_choice").cloned().unwrap_or(json!("auto"));
    let tools = request.get("tools").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut out = Map::new();
    out.insert("id".to_string(), Value::String(response_id));
    out.insert("object".to_string(), json!("response"));
    out.insert(
        "created_at".to_string(),
        json!(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)),
    );
    out.insert("status".to_string(), json!("completed"));
    out.insert("model".to_string(), Value::String(model));
    out.insert(
        "output".to_string(),
        json!([{
            "id": format!("msg_{}", &out.get("id").and_then(Value::as_str).unwrap_or("resp_x")[5..].chars().take(8).collect::<String>()),
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "phase": "final_answer",
            "content": [{"type": "output_text", "text": text, "annotations": []}],
        }]),
    );
    out.insert("parallel_tool_calls".to_string(), Value::Bool(true));
    out.insert("tool_choice".to_string(), tool_choice);
    out.insert("tools".to_string(), Value::Array(tools));
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_to_response_basic() {
        let body = json!({
            "model": "gpt-5.4",
            "messages": [
                {"role": "system", "content": "sys"},
                {"role": "user", "content": "hi"},
            ],
        });
        let out = chat_to_response(&body, "gpt-5.4");
        assert_eq!(out["model"], json!("gpt-5.4"));
        assert_eq!(out["instructions"], json!("sys"));
        assert_eq!(out["store"], json!(false));
        let input = out["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["role"], json!("user"));
    }

    #[test]
    fn chat_to_response_tool_calls() {
        let body = json!({
            "model": "m",
            "messages": [
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        {"id": "call_1", "function": {"name": "f", "arguments": "{}"}},
                    ],
                },
                {"role": "tool", "tool_call_id": "call_1", "content": "out"},
            ],
        });
        let out = chat_to_response(&body, "m");
        let input = out["input"].as_array().unwrap();
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["type"], json!("function_call"));
        assert_eq!(input[0]["call_id"], json!("call_1"));
        assert_eq!(input[1]["type"], json!("function_call_output"));
    }

    #[test]
    fn normalize_drops_reasoning_without_encrypted_content() {
        let item = json!({"type": "reasoning"});
        assert!(normalize_input_item(&item).is_none());
        let item = json!({"type": "reasoning", "encrypted_content": "enc", "summary": []});
        assert!(normalize_input_item(&item).is_some());
    }

    #[test]
    fn normalize_sanitizes_function_call() {
        let item = json!({"type": "function_call", "id": "x", "status": "ok", "name": "f"});
        let out = normalize_input_item(&item).unwrap();
        assert!(out.get("id").is_none());
        assert!(out.get("status").is_none());
        assert_eq!(out["call_id"], json!("x"));
        assert_eq!(out["arguments"], json!("{}"));
    }

    #[test]
    fn codex_payload_forces_stream_and_verbosity() {
        let body = json!({"model": "m", "input": [], "max_output_tokens": 100});
        let out = normalize_codex_payload(&body, &[]);
        assert!(out.get("max_output_tokens").is_none());
        assert_eq!(out["stream"], json!(true));
        assert_eq!(out["store"], json!(false));
        assert_eq!(out["text"]["verbosity"], json!("low"));
        assert!(out["include"].as_array().unwrap().contains(&json!("reasoning.encrypted_content")));
    }

    #[test]
    fn backend_event_renames_done() {
        let mut ev = json!({"type": "response.done", "response": {"status": "completed"}});
        normalize_backend_event(&mut ev);
        assert_eq!(ev["type"], json!("response.completed"));
    }

    #[test]
    fn backend_event_drops_bad_status() {
        let mut ev = json!({"type": "x", "response": {"status": "bogus"}});
        normalize_backend_event(&mut ev);
        assert!(ev["response"].get("status").is_none());
    }

    #[test]
    fn response_to_chat_basic() {
        let resp = json!({
            "id": "resp_123",
            "model": "m",
            "output": [
                {"type": "message", "content": [{"type": "output_text", "text": "hi"}]},
            ],
            "usage": {"input_tokens": 3, "output_tokens": 5, "total_tokens": 8},
        });
        let out = response_to_chat(&resp, "m", false, 1);
        assert_eq!(out["id"], json!("chatcmpl_123"));
        assert_eq!(out["choices"][0]["finish_reason"], json!("stop"));
        assert_eq!(out["usage"]["total_tokens"], json!(8));
    }

    #[test]
    fn ensure_response_fills_defaults() {
        let out = ensure_response(&json!({}), &json!({"model": "m"}));
        assert_eq!(out["object"], json!("response"));
        assert_eq!(out["status"], json!("completed"));
        assert_eq!(out["model"], json!("m"));
        assert!(str_of(out.get("id")).starts_with("resp_"));
    }

    #[test]
    fn collect_stream_assembles_completed_response() {
        let events = vec![
            json!({"type": "response.created", "response": {"id": "resp_abc"}}),
            json!({"type": "response.output_text.delta", "delta": "hel"}),
            json!({"type": "response.output_text.delta", "delta": "lo"}),
            json!({"type": "response.completed", "response": {"id": "resp_abc", "model": "m", "output": [], "status": "completed"}}),
        ];
        let out = collect_stream(&events, &json!({"model": "m"}));
        assert_eq!(out["id"], json!("resp_abc"));
        assert_eq!(out["status"], json!("completed"));
    }

    #[test]
    fn collect_stream_falls_back_to_text_when_no_completed() {
        let events = vec![
            json!({"type": "response.output_text.delta", "delta": "hi"}),
            json!({"type": "response.output_item.done", "item": {"type": "message", "content": []}}),
        ];
        let out = collect_stream(&events, &json!({"model": "m"}));
        assert_eq!(out["status"], json!("completed"));
        assert!(str_of(out.get("id")).starts_with("resp_"));
    }
}
