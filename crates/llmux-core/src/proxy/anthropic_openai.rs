//! Anthropic Messages API ↔ OpenAI Chat Completions protocol conversion.
//!
//! Pure functions and a streaming SSE state machine. No network I/O here —
//! the route layer (llmux-server) drives HTTP. This mirrors the Bun version's
//! `src/services/anthropic_ingress.ts` and additionally handles the gaps that
//! version ignored: `thinking` request param, `cache_control` passthrough,
//! cache usage fields, real streaming usage, tool_result `is_error`, and
//! `image` url sources.

use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

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
        let mut malformed: Vec<Value> = Vec::new();
        for (i, tc) in tcs.iter().enumerate() {
            let id = tc.get("id").and_then(Value::as_str).unwrap_or_default();
            let name = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            // The Anthropic SDK refuses a message carrying a tool_use block
            // with an empty id or name, losing the whole reply — not just this
            // call. Defaulting to "" here manufactures exactly that block.
            if id.is_empty() || name.is_empty() {
                malformed.push(json!({
                    "index": i,
                    "has_id": !id.is_empty(),
                    "has_name": !name.is_empty(),
                    "keys": tc.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()),
                }));
                continue;
            }
            let args = tc
                .get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let input = serde_json::from_str::<Value>(args).unwrap_or_else(|_| json!({}));
            content.push(json!({ "type": "tool_use", "id": id, "name": name, "input": input }));
        }
        if !malformed.is_empty() {
            tracing::warn!(
                model = %resolved_model,
                dropped = malformed.len(),
                ?malformed,
                "upstream tool_calls missing id/name; dropped"
            );
        }
    }

    let finish = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .unwrap_or("stop");
    let stop_reason = map_stop_reason(finish).to_string();

    let usage = &openai_body["usage"];
    let raw_prompt = usage.get("prompt_tokens").and_then(Value::as_i64).unwrap_or(0);
    let output_tokens = usage.get("completion_tokens").and_then(Value::as_i64).unwrap_or(0);
    let (cache_read, cache_create) = cache_usage_from_openai(usage);
    // 4-store-3-display: fresh input = prompt - read (creation always 0 for OpenAI)
    let input_tokens = (raw_prompt - cache_read).max(0);

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
/// Recognizes several vendor spellings: DeepSeek's `prompt_cache_hit_tokens`,
/// OpenAI's `prompt_tokens_details.cached_tokens`, the /responses
/// `input_tokens_details.cached_tokens`, and a top-level `cached_tokens`.
///
/// 4-store-3-display contract: `cache_creation` is only meaningful for
/// Anthropic native (`cache_creation_input_tokens`). For all OpenAI-compatible
/// upstreams `creation` is always 0 — the uncached portion is `input` (fresh),
/// i.e. `input = prompt - read`. DeepSeek's `prompt_cache_miss_tokens` is
/// therefore *not* a creation count but the fresh input itself.
pub fn cache_usage_from_openai(usage: &Value) -> (i64, i64) {
    let cache_read = usage
        .get("prompt_cache_hit_tokens")
        .and_then(Value::as_i64)
        .or_else(|| detail_cached_tokens(usage))
        .or_else(|| usage.get("cached_tokens").and_then(Value::as_i64))
        .unwrap_or(0);

    // creation is only valid for Anthropic native; for OpenAI-compatible
    // upstreams it is always 0 (uncached tokens belong to `input`).
    let cache_create = 0;

    (cache_read, cache_create)
}

/// `cached_tokens` under either the chat-completions `prompt_tokens_details` or
/// the /responses `input_tokens_details` object.
fn detail_cached_tokens(usage: &Value) -> Option<i64> {
    ["prompt_tokens_details", "input_tokens_details"]
        .iter()
        .find_map(|k| usage.get(k).and_then(|d| d.get("cached_tokens")).and_then(Value::as_i64))
}

// ---------------------------------------------------------------------------
// Streaming SSE state machine: OpenAI chunks → Anthropic SSE events
// ---------------------------------------------------------------------------

/// How long a tool_call fragment may stay incomplete before the converter
/// stops waiting for an identity. Deliberately generous: a slow-but-correct
/// upstream must never lose a call, so this only fires for streams that are
/// genuinely stuck. Measured 2026-09-26: every tool_call seen in production
/// carried id+name on its **first** delta, and the orphan warn never fired —
/// so the deadline is headroom, not a tuned value.
const TOOL_IDENTITY_TIMEOUT_MS: u64 = 30_000;

/// A tool_call whose identity has not fully arrived yet.
struct PendingTool {
    id: String,
    name: String,
    args: String,
    /// Epoch millis of the first fragment for this index — the timeout anchor.
    first_seen_ms: u64,
}

/// Resolve a `PendingTool` now that we can no longer wait.
///
/// The two fields are deliberately treated differently, because they are not
/// symmetric in recoverability:
///
/// - `name` must be one the client declared. There is no way to invent it, and
///   guessing (e.g. "if there is only one tool, it must be that one") silently
///   calls the wrong function. So a missing name ends the call.
/// - `id` is an opaque correlation string with no meaning to either side, so a
///   missing one is simply made up. That is strictly better than dropping: the
///   call survives with a real name and a synthesized id.
///
/// This is the "补上" path — the point being that a recoverable gap should be
/// recovered, not thrown away.
enum Resolution {
    /// Complete: open the block (or the missing id was synthesized).
    Open { id: String, name: String, args: String, synthesized_id: bool },
    /// Unrecoverable: the name never arrived. Drop the call and let the caller
    /// warn — emitting it would have the SDK reject the whole message.
    Drop { reason: &'static str },
}

fn resolve_pending(p: &PendingTool) -> Resolution {
    if p.name.is_empty() {
        return Resolution::Drop { reason: "missing_function_name" };
    }
    if p.id.is_empty() {
        // Anthropic ids are conventionally `toolu_…`; any opaque unique string
        // is valid to the SDK, and this one only has to be unique within the
        // message.
        return Resolution::Open {
            id: format!("toolu_{}", uuid_simple()),
            name: p.name.clone(),
            args: p.args.clone(),
            synthesized_id: true,
        };
    }
    Resolution::Open {
        id: p.id.clone(),
        name: p.name.clone(),
        args: p.args.clone(),
        synthesized_id: false,
    }
}

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
    terminal_error: Option<String>,
    last_usage: Option<Value>,
    /// tool index → (id, name, buffered arguments-so-far, first-seen epoch ms),
    /// held until the block can be opened safely. Emitting on `index` alone
    /// produced `id:""`/`name:""` tool_use blocks, which the Anthropic SDK
    /// rejects ("tool_calls without a complete id and function name") — a hard
    /// client error the gateway had logged as a 200. Never open speculatively.
    ///
    /// The `first_seen` stamp is the timeout anchor: measured from when the
    /// fragment **first appeared**, not from the last argument increment, so a
    /// stream that keeps dribbling arguments cannot postpone the deadline
    /// forever (the arguments arriving say nothing about whether the identity
    /// will).
    pending_tools: HashMap<usize, PendingTool>,
    /// tool indices that were opened and are awaiting `content_block_stop`.
    live_tools: Vec<usize>,
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
            terminal_error: None,
            last_usage: None,
            pending_tools: HashMap::new(),
            live_tools: Vec::new(),
            finished: false,
        }
    }

    /// Feed one OpenAI SSE `data:` payload (parsed JSON). Returns the Anthropic
    /// SSE event strings to emit, in order.
    pub fn feed(&mut self, chunk: &Value) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        let mut events = Vec::new();

        if tracing::enabled!(tracing::Level::TRACE) {
            if let Some(choices) = chunk.get("choices").and_then(Value::as_array) {
                if let Some(first) = choices.first() {
                    let finish = first.get("finish_reason").and_then(Value::as_str).unwrap_or("");
                    let has_content = first.get("delta").and_then(|d| d.get("content")).is_some();
                    let has_tool = first.get("delta").and_then(|d| d.get("tool_calls")).is_some();
                    tracing::trace!(
                        finish_reason = finish,
                        has_content, has_tool, finished = self.finished,
                        "converter.feed"
                    );
                }
            } else if chunk.get("usage").is_some() {
                tracing::trace!("converter.feed: usage chunk");
            }
        }

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

            // Tool call fragments. A block may only be opened once its `id`
            // AND `name` are known — see `pending_tools`. Arguments that
            // arrive before that are held back and flushed on open, so the
            // block still carries the complete input.
            if let Some(tcs) = delta.get("tool_calls").and_then(Value::as_array) {
                for tc in tcs {
                    let Some(tc_index) = tc.get("index").and_then(Value::as_i64) else {
                        continue;
                    };
                    let tc_index = tc_index as usize;
                    let block_index = if self.thinking_started { 2 } else { 1 } + tc_index;
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
                        .unwrap_or_default();

                    // Not open yet: absorb whatever identity arrived plus any
                    // arguments so far, and only open once complete.
                    if !self.tool_indices.contains(&tc_index) {
                        let now = now_ms();
                        let slot = self.pending_tools.entry(tc_index).or_insert_with(|| PendingTool {
                            id: String::new(),
                            name: String::new(),
                            args: String::new(),
                            first_seen_ms: now,
                        });
                        if !id.is_empty() {
                            slot.id = id.to_string();
                        }
                        if !name.is_empty() {
                            slot.name = name.to_string();
                        }
                        if !args.is_empty() {
                            slot.args.push_str(args);
                        }
                        let (pid, pname, buffered) = {
                            let s = &self.pending_tools[&tc_index];
                            (s.id.clone(), s.name.clone(), s.args.clone())
                        };
                        if pid.is_empty() || pname.is_empty() {
                            // Still incomplete — emit nothing yet.
                            continue;
                        }
                        self.pending_tools.remove(&tc_index);
                        self.tool_indices.insert(tc_index);
                        self.live_tools.push(tc_index);
                        events.push(sse_event(
                            "content_block_start",
                            json!({
                                "type": "content_block_start",
                                "index": block_index,
                                "content_block": { "type": "tool_use", "id": pid, "name": pname, "input": {} }
                            }),
                        ));
                        if !buffered.is_empty() {
                            events.push(sse_event(
                                "content_block_delta",
                                json!({
                                    "type": "content_block_delta",
                                    "index": block_index,
                                    "delta": { "type": "input_json_delta", "partial_json": buffered }
                                }),
                            ));
                        }
                        continue;
                    }
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

        // Finish reason (may arrive in the final content chunk).
        if let Some(fr) = choice.get("finish_reason").and_then(Value::as_str) {
            if !fr.is_empty() && self.pending_stop_reason.is_none() {
                match fr {
                    "tool_calls" | "stop" | "length" => {
                        self.pending_stop_reason = Some(map_stop_reason(fr).to_string());
                    }
                    _ => self.terminal_error = Some(format!("Upstream stream ended with {fr}")),
                }
            }
        }

        events.extend(self.expire_stalled_tools(now_ms()));

        events
    }

    /// Give up on tool_calls that have been waiting for an identity past the
    /// deadline. Called on every chunk, so a stream that stays alive but stuck
    /// (arguments trickling, identity never arriving) still gets resolved
    /// instead of buffering until the connection dies.
    ///
    /// This is a bound on the *wait*, not a pause: the converter is driven by
    /// whatever bytes arrive, so "waiting 30s" can only mean "keep buffering
    /// across chunks until 30s have passed". There is no way to actually sleep
    /// here, and none is wanted — the client is being streamed to.
    fn expire_stalled_tools(&mut self, now_ms: u64) -> Vec<String> {
        if self.pending_tools.is_empty() {
            return Vec::new();
        }
        let due: Vec<usize> = self
            .pending_tools
            .iter()
            .filter(|(_, p)| now_ms.saturating_sub(p.first_seen_ms) >= TOOL_IDENTITY_TIMEOUT_MS)
            .map(|(i, _)| *i)
            .collect();
        if due.is_empty() {
            return Vec::new();
        }
        let mut events = Vec::new();
        for idx in due {
            let Some(p) = self.pending_tools.remove(&idx) else {
                continue;
            };
            events.extend(self.emit_resolved(idx, &p, "identity_timeout"));
        }
        events
    }

    /// Emit (or discard) one resolved tool_call. `trigger` only shapes the log
    /// line — the decision comes from the field asymmetry in `resolve_pending`.
    fn emit_resolved(&mut self, tc_index: usize, p: &PendingTool, trigger: &'static str) -> Vec<String> {
        let block_index = if self.thinking_started { 2 } else { 1 } + tc_index;
        match resolve_pending(p) {
            Resolution::Drop { reason } => {
                tracing::warn!(
                    model = %self.model,
                    index = tc_index,
                    reason,
                    trigger,
                    has_id = !p.id.is_empty(),
                    has_name = !p.name.is_empty(),
                    buffered_args = p.args.len(),
                    "dropping tool_call: function name never arrived"
                );
                Vec::new()
            }
            Resolution::Open { id, name, args, synthesized_id } => {
                if synthesized_id {
                    tracing::warn!(
                        model = %self.model,
                        index = tc_index,
                        trigger,
                        name = %name,
                        buffered_args = args.len(),
                        "upstream omitted tool_call id; synthesized one so the call is not lost"
                    );
                }
                self.tool_indices.insert(tc_index);
                self.live_tools.push(tc_index);
                let mut events = vec![sse_event(
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": block_index,
                        "content_block": { "type": "tool_use", "id": id, "name": name, "input": {} }
                    }),
                )];
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
                events
            }
        }
    }

    /// End-of-stream: close open blocks, emit `message_delta` (real usage when
    /// available), and terminate with `message_stop`. Idempotent.
    pub fn finish(&mut self) -> Vec<String> {
        if self.finished {
            tracing::trace!("converter.finish: already finished");
            return Vec::new();
        }
        self.finished = true;

        if let Some(message) = self.terminal_error.take() {
            return vec![sse_event(
                "error",
                json!({"type":"error","error":{"type":"api_error","message":message}}),
            )];
        }

        let stop_reason = self
            .pending_stop_reason
            .clone()
            .unwrap_or_else(|| "end_turn".to_string());
        tracing::trace!(
            text_block = self.text_block_started,
            thinking = self.thinking_started,
            tools = self.tool_indices.len(),
            stop_reason,
            "converter.finish"
        );

        let mut events = Vec::new();

        // Tools still waiting for an identity are resolved **before** the stop
        // events below: resolving may open a block (a synthesized id is enough
        // to make the call usable), and that block then needs its
        // content_block_stop like any other. Same decision as the mid-stream
        // timeout, so the two paths cannot drift.
        if !self.pending_tools.is_empty() {
            let mut pending: Vec<(usize, PendingTool)> = self.pending_tools.drain().collect();
            pending.sort_by_key(|(i, _)| *i);
            for (idx, p) in pending {
                events.extend(self.emit_resolved(idx, &p, "stream_end"));
            }
        }

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

        let mut sorted: Vec<usize> = self.live_tools.clone();
        sorted.sort_unstable();
        for tc_index in sorted {
            let block_index = if self.thinking_started { 2 } else { 1 } + tc_index;
            events.push(sse_event(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": block_index }),
            ));
        }

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
    /// 4-store-3-display: for OpenAI-compatible tails `cache_create` is always 0.
    pub fn usage_tokens(&self) -> (i64, i64, i64, i64) {
        let Some(usage) = &self.last_usage else {
            return (0, 0, 0, 0);
        };
        let raw_prompt = usage.get("prompt_tokens").and_then(Value::as_i64).unwrap_or(0);
        let output = usage.get("completion_tokens").and_then(Value::as_i64).unwrap_or(0);
        let (cache_read, cache_create) = cache_usage_from_openai(usage);
        let input = (raw_prompt - cache_read).max(0);
        (input, output, cache_read, cache_create)
    }

    /// Did this stream carry **any** output — assistant text, thinking, or a
    /// tool call?
    ///
    /// A direct observation of what was emitted, for the empty-response check
    /// in the callers. Token counts are not a substitute: a tool-call-only or
    /// thinking-only turn legitimately reports few or zero completion tokens,
    /// and a stream that emitted nothing at all may still bill tokens.
    ///
    /// `text_block_started` / `thinking_started` are only set when a real
    /// block is opened, and `tool_indices` only when a tool block is opened —
    /// all three are driven by the payload, not by any arrival heuristic.
    pub fn produced_output(&self) -> bool {
        self.text_block_started || self.thinking_started || !self.tool_indices.is_empty()
    }
}

/// Build a single Anthropic SSE frame: `event: <type>\ndata: <json>\n\n`.
fn sse_event(event_type: &str, data: Value) -> String {
    let data_str = data.to_string();
    let mut s = String::with_capacity(16 + event_type.len() + data_str.len());
    s.push_str("event: ");
    s.push_str(event_type);
    s.push_str("\ndata: ");
    s.push_str(&data_str);
    s.push_str("\n\n");
    s
}

// ---------------------------------------------------------------------------
// SSE framing helper
// ---------------------------------------------------------------------------

/// Split complete SSE events out of a byte buffer. Handles chunk boundaries:
/// the buffer accumulates until a blank-line terminator (`\n\n`) is found, then
/// that event is drained. `max_events` bounds the number of events returned per
/// call; `0` means no limit (drain everything). Incomplete trailing data stays
/// in `buffer`.
pub fn parse_sse_chunks(buffer: &mut Vec<u8>, max_events: usize) -> Vec<String> {
    let mut events = Vec::new();
    let mut start = 0usize;
    let len = buffer.len();
    while start + 1 < len {
        if max_events > 0 && events.len() >= max_events {
            break;
        }
        // scan for \n\n from start
        let mut found = None;
        let mut i = start;
        while i + 1 < len {
            if buffer[i] == b'\n' && buffer[i + 1] == b'\n' {
                found = Some(i + 2);
                break;
            }
            i += 1;
        }
        let Some(end) = found else { break };
        // slice is &buffer[start..end], no per-event drain
        events.push(String::from_utf8_lossy(&buffer[start..end]).into_owned());
        start = end;
    }
    if start > 0 {
        buffer.drain(..start);
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

/// Epoch millis, for the tool-identity deadline. A clock before the epoch
/// clamps to 0, which only makes the timeout unreachable — the safe direction,
/// since it degrades to the old "wait for end of stream" behaviour.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(id: &str, name: &str, args: &str) -> PendingTool {
        PendingTool {
            id: id.to_string(),
            name: name.to_string(),
            args: args.to_string(),
            first_seen_ms: 0,
        }
    }

    /// id and name are not equally recoverable: name cannot be invented, id can.
    #[test]
    fn resolve_synthesizes_a_missing_id_but_keeps_the_name() {        match resolve_pending(&pending("", "Bash", "{\"a\":1}")) {
            Resolution::Open { id, name, args, synthesized_id } => {
                assert!(synthesized_id, "id was missing, so it must be flagged synthesized");
                assert!(!id.is_empty(), "a synthesized id must still be a real id");
                assert_eq!(name, "Bash");
                assert_eq!(args, "{\"a\":1}");
            }
            Resolution::Drop { reason } => panic!("must not drop: a real name is present ({reason})"),
        }
    }

    /// Nothing to invent from — the call is unrecoverable, and emitting it
    /// would have the SDK reject the whole message.
    #[test]
    fn resolve_drops_when_the_name_is_missing_even_with_an_id() {
        match resolve_pending(&pending("call_1", "", "{}")) {
            Resolution::Drop { reason } => assert_eq!(reason, "missing_function_name"),
            Resolution::Open { .. } => panic!("must not open a nameless tool_use block"),
        }
    }

    /// The normal case must be untouched: no synthesis, no drop.
    #[test]
    fn resolve_passes_through_a_complete_tool_call() {
        match resolve_pending(&pending("call_1", "Edit", "{}")) {
            Resolution::Open { id, name, args, synthesized_id } => {
                assert!(!synthesized_id);
                assert_eq!(id, "call_1");
                assert_eq!(name, "Edit");
                assert_eq!(args, "{}");
            }
            Resolution::Drop { reason } => panic!("must not drop a complete call ({reason})"),
        }
    }

    /// The deadline must be measured from first sight, not from the last
    /// argument increment — otherwise a stream that keeps dribbling arguments
    /// postpones the deadline forever.
    #[test]
    fn expiry_is_anchored_to_first_sight_not_the_latest_fragment() {
        let mut conv = OpenAISseConverter::new("m");
        // First fragment: name arrives, id never does. Stamps first_seen_ms.
        conv.feed(&json!({
            "choices": [{
                "index": 0,
                "delta": {"tool_calls": [{"index": 0, "function": {"name": "Bash", "arguments": "{"}}]},
                "finish_reason": null
            }]
        }));

        // Well past the deadline, but the pending entry is young in this test's
        // clock unless we move it back. Age it directly.
        let aged = now_ms() - TOOL_IDENTITY_TIMEOUT_MS - 1;
        conv.pending_tools.get_mut(&0).unwrap().first_seen_ms = aged;

        let evs = conv.expire_stalled_tools(now_ms());
        let text: Vec<&str> = evs.iter().map(|s| s.as_str()).collect();
        assert!(
            text.iter().any(|s| s.contains("content_block_start") && s.contains("tool_use")),
            "an expired-but-named call must be recovered, not dropped: {text:?}"
        );
        assert!(
            text.iter().any(|s| s.contains("input_json_delta")),
            "buffered arguments must be flushed on the synthesized open: {text:?}"
        );
        assert!(conv.pending_tools.is_empty(), "expired entry must be removed");
    }

    /// Below the deadline nothing is forced — a slow but correct upstream keeps
    /// its chance to deliver the real id.
    #[test]
    fn nothing_is_expired_before_the_deadline() {
        let mut conv = OpenAISseConverter::new("m");
        conv.feed(&json!({
            "choices": [{
                "index": 0,
                "delta": {"tool_calls": [{"index": 0, "function": {"name": "Bash", "arguments": "{"}}]},
                "finish_reason": null
            }]
        }));
        assert!(conv.expire_stalled_tools(now_ms()).is_empty());
        assert_eq!(conv.pending_tools.len(), 1, "must still be waiting");
    }

    /// A block opened at end-of-stream still needs its content_block_stop.
    /// Regression: resolving pending after the stop loop left it dangling.
    #[test]
    fn a_call_recovered_at_stream_end_is_properly_closed() {
        let mut conv = OpenAISseConverter::new("m");
        let mut all = Vec::new();
        // name but no id — recoverable, so it must open…
        all.extend(conv.feed(&json!({
            "choices": [{
                "index": 0,
                "delta": {"tool_calls": [{"index": 0, "function": {"name": "Bash", "arguments": "{}"}}]},
                "finish_reason": "tool_calls"
            }]
        })));
        // …and stay open until finish() resolves it.
        all.extend(conv.finish());

        let text: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
        let starts = text.iter().filter(|s| s.contains("content_block_start") && s.contains("tool_use")).count();
        let stops = text.iter().filter(|s| s.contains("content_block_stop")).count();
        assert_eq!(starts, 1, "a named call must be recovered at stream end: {text:?}");
        assert_eq!(stops, 1, "every started block must be closed: {text:?}");
    }

    /// A nameless call is still dropped even at end-of-stream, and leaves no
    /// dangling stop event behind.
    #[test]
    fn a_nameless_call_at_stream_end_is_dropped_without_dangling_stop() {
        let mut conv = OpenAISseConverter::new("m");
        let mut all = Vec::new();
        all.extend(conv.feed(&json!({
            "choices": [{
                "index": 0,
                "delta": {"tool_calls": [{"index": 0, "function": {"arguments": "{\"a\":1}"}}]},
                "finish_reason": "tool_calls"
            }]
        })));
        all.extend(conv.finish());

        let text: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
        // Match the content block, not the bare word: `stop_reason` is also
        // "tool_use" and would false-positive here.
        assert!(
            !text.iter().any(|s| s.contains("\"type\":\"tool_use\"")),
            "nameless call must not open: {text:?}"
        );
        let starts = text.iter().filter(|s| s.contains("content_block_start")).count();
        let stops = text.iter().filter(|s| s.contains("content_block_stop")).count();
        assert_eq!(starts, stops, "every started block must be closed: {text:?}");
    }

    // --- produced_output: the direct empty-response signal ------------------
    //
    // Replaces the `output_tokens == 0 && chunks <= 4` proxy that the streaming
    // routes used. That proxy was wrong in both directions (see the callers):
    // a tool-call-only turn reports ~0 completion tokens but DID produce
    // output, while a genuinely empty response can span more than four chunks.

    /// A stream carrying only a tool call produced output, even though it bills
    /// essentially no completion tokens.
    #[test]
    fn a_tool_call_only_stream_counts_as_produced_output() {
        let mut conv = OpenAISseConverter::new("m");
        conv.feed(&json!({
            "choices": [{
                "index": 0,
                "delta": {"tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "Bash", "arguments": "{}"}
                }]},
                "finish_reason": "tool_calls"
            }]
        }));
        assert!(conv.produced_output(), "a delivered tool call IS output");
    }

    /// A thinking-only turn also counts: the model did produce content, it just
    /// did not go into `content`.
    #[test]
    fn a_thinking_only_stream_counts_as_produced_output() {
        let mut conv = OpenAISseConverter::new("m");
        conv.feed(&json!({
            "choices": [{"index": 0, "delta": {"reasoning_content": "thinking…"}, "finish_reason": null}]
        }));
        assert!(conv.produced_output(), "reasoning content IS output");
    }

    /// The case `c7b57e2` existed to catch: a stream that finishes without ever
    /// emitting text, thinking or a tool call — no matter how many keep-alive
    /// frames preceded it.
    #[test]
    fn a_stream_that_only_ever_sent_role_and_keepalives_produced_nothing() {
        let mut conv = OpenAISseConverter::new("m");
        conv.feed(&json!({
            "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": null}]
        }));
        conv.feed(&json!({
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1200, "completion_tokens": 0}
        }));
        assert!(!conv.produced_output(), "no text, no thinking, no tool call");
    }
}
