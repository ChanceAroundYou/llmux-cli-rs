//! Contract tests for the Anthropic↔OpenAI protocol conversion module.
//! Pure function tests — no network.

use llmux_core::proxy::anthropic_openai::{
    anthropic_to_openai_request, cache_usage_from_openai, map_tool_choice_to_openai,
    map_tools_to_openai, openai_to_anthropic_response, parse_sse_chunks, sse_data_payload,
    OpenAISseConverter,
};
use serde_json::json;

// ---------------------------------------------------------------------------
// Request conversion
// ---------------------------------------------------------------------------

#[test]
fn request_converts_system_and_text_message() {
    let req = anthropic_to_openai_request(
        &json!({
            "model": "od4",
            "system": "Be terse.",
            "max_tokens": 100,
            "temperature": 0.2,
            "stop_sequences": ["END"],
            "messages": [{"role": "user", "content": "hello"}]
        }),
        "deepseek-v4-flash",
    )
    .unwrap();

    assert_eq!(req["model"], "deepseek-v4-flash");
    assert_eq!(req["messages"][0]["role"], "system");
    assert_eq!(req["messages"][0]["content"], "Be terse.");
    assert_eq!(req["messages"][1]["role"], "user");
    assert_eq!(req["messages"][1]["content"], "hello");
    assert_eq!(req["max_tokens"], 100);
    assert_eq!(req["temperature"], 0.2);
    assert_eq!(req["stop"], json!(["END"]));
    assert!(req.get("stream_options").is_none());
}

#[test]
fn request_flattens_single_text_block_to_string() {
    let req = anthropic_to_openai_request(
        &json!({
            "model": "m",
            "messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}]
        }),
        "t",
    )
    .unwrap();

    assert_eq!(req["messages"][0]["content"], "hi");
}

#[test]
fn request_keeps_multi_part_content_as_array() {
    let req = anthropic_to_openai_request(
        &json!({
            "model": "m",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "a"},
                    {"type": "text", "text": "b"}
                ]
            }]
        }),
        "t",
    )
    .unwrap();

    assert_eq!(req["messages"][0]["content"][0]["text"], "a");
    assert_eq!(req["messages"][0]["content"][1]["text"], "b");
}

#[test]
fn request_converts_image_base64_and_url() {
    let req = anthropic_to_openai_request(
        &json!({
            "model": "m",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}},
                    {"type": "image", "source": {"type": "url", "url": "https://x/y.png"}}
                ]
            }]
        }),
        "t",
    )
    .unwrap();

    assert_eq!(req["messages"][0]["content"][0]["type"], "image_url");
    assert_eq!(
        req["messages"][0]["content"][0]["image_url"]["url"],
        "data:image/png;base64,AAAA"
    );
    assert_eq!(
        req["messages"][0]["content"][1]["image_url"]["url"],
        "https://x/y.png"
    );
}

#[test]
fn request_converts_tool_use_and_tool_result() {
    let req = anthropic_to_openai_request(
        &json!({
            "model": "m",
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "text", "text": "let me check"},
                    {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "bj"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "sunny", "is_error": false}
                ]}
            ]
        }),
        "t",
    )
    .unwrap();

    let assistant = &req["messages"][0];
    assert_eq!(assistant["role"], "assistant");
    assert_eq!(assistant["content"], "let me check");
    assert_eq!(assistant["tool_calls"][0]["id"], "toolu_1");
    assert_eq!(assistant["tool_calls"][0]["type"], "function");
    assert_eq!(assistant["tool_calls"][0]["function"]["name"], "get_weather");
    assert_eq!(
        assistant["tool_calls"][0]["function"]["arguments"],
        r#"{"city":"bj"}"#
    );

    let tool_msg = &req["messages"][1];
    assert_eq!(tool_msg["role"], "tool");
    assert_eq!(tool_msg["tool_call_id"], "toolu_1");
    assert_eq!(tool_msg["content"], "sunny");
    assert_eq!(tool_msg["is_error"], false);
}

#[test]
fn request_maps_thinking_block_to_reasoning_fields() {
    let req = anthropic_to_openai_request(
        &json!({
            "model": "m",
            "messages": [{
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "hmm", "signature": "sig1"},
                    {"type": "text", "text": "answer"}
                ]
            }]
        }),
        "t",
    )
    .unwrap();

    let msg = &req["messages"][0];
    assert_eq!(msg["content"], "answer");
    assert_eq!(msg["reasoning_content"], "hmm");
    assert_eq!(msg["reasoning_signature"], "sig1");
}

#[test]
fn request_keeps_cache_control_on_text_block() {
    let req = anthropic_to_openai_request(
        &json!({
            "model": "m",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "big", "cache_control": {"type": "ephemeral"}}
                ]
            }]
        }),
        "t",
    )
    .unwrap();

    assert_eq!(req["messages"][0]["content"][0]["type"], "text");
    assert_eq!(req["messages"][0]["content"][0]["text"], "big");
    assert_eq!(req["messages"][0]["content"][0]["cache_control"]["type"], "ephemeral");
}

#[test]
fn request_maps_tools_and_tool_choice() {
    let req = anthropic_to_openai_request(
        &json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [
                {"name": "f", "description": "desc", "input_schema": {"type": "object", "properties": {"a": {"type": "string"}}}}
            ],
            "tool_choice": {"type": "tool", "name": "f"}
        }),
        "t",
    )
    .unwrap();

    assert_eq!(req["tools"][0]["type"], "function");
    assert_eq!(req["tools"][0]["function"]["name"], "f");
    assert_eq!(req["tools"][0]["function"]["parameters"]["properties"]["a"]["type"], "string");
    assert_eq!(req["tool_choice"]["type"], "function");
    assert_eq!(req["tool_choice"]["function"]["name"], "f");
}

#[test]
fn request_thinking_budget_backstops_max_tokens() {
    let req = anthropic_to_openai_request(
        &json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "thinking": {"type": "enabled", "budget_tokens": 4096}
        }),
        "t",
    )
    .unwrap();

    assert_eq!(req["max_tokens"], 4096);
    assert_eq!(req["thinking"]["type"], "enabled");
}

#[test]
fn request_normalizes_adaptive_thinking_type() {
    // DeepSeek-style clients send `thinking.type: "adaptive"`. Some gateways
    // (Sensenova) pass it through but their backend (Tencent Cloud) only
    // accepts ["enabled","disabled","auto"] → 400. Must normalize to "enabled".
    let req = anthropic_to_openai_request(
        &json!({
            "model": "m",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": "hi"}],
            "thinking": {"type": "adaptive", "budget_tokens": 200}
        }),
        "t",
    )
    .unwrap();

    assert_eq!(req["thinking"]["type"], "enabled");
    assert_eq!(req["thinking"]["budget_tokens"], 200);
    assert_eq!(req["max_tokens"], 100); // max_tokens already present, unchanged
}

#[test]
fn request_adaptive_thinking_backstops_max_tokens() {
    let req = anthropic_to_openai_request(
        &json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "thinking": {"type": "adaptive", "budget_tokens": 512}
        }),
        "t",
    )
    .unwrap();

    assert_eq!(req["max_tokens"], 512);
    assert_eq!(req["thinking"]["type"], "enabled");
}

#[test]
fn request_thinking_enabled_unchanged() {
    let req = anthropic_to_openai_request(
        &json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "thinking": {"type": "enabled", "budget_tokens": 4096}
        }),
        "t",
    )
    .unwrap();

    assert_eq!(req["thinking"]["type"], "enabled");
}

#[test]
fn request_thinking_disabled_unchanged() {
    let req = anthropic_to_openai_request(
        &json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "thinking": {"type": "disabled"}
        }),
        "t",
    )
    .unwrap();

    assert_eq!(req["thinking"]["type"], "disabled");
}

#[test]
fn request_adds_stream_options_for_streaming() {
    let req = anthropic_to_openai_request(
        &json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }),
        "t",
    )
    .unwrap();

    assert_eq!(req["stream"], true);
    assert_eq!(req["stream_options"]["include_usage"], true);
}

#[test]
fn request_empty_anthropic_base_url_falls_to_openai_path() {
    // This mirrors the router bug: Some("") must NOT be treated as a valid
    // anthropic_base_url. The conversion branch is chosen when it's empty.
    let req = anthropic_to_openai_request(
        &json!({"model": "od4", "messages": [{"role": "user", "content": "hi"}]}),
        "deepseek-v4-flash",
    )
    .unwrap();
    assert_eq!(req["model"], "deepseek-v4-flash");
}

// ---------------------------------------------------------------------------
// Response conversion
// ---------------------------------------------------------------------------

#[test]
fn response_converts_text_and_thinking_blocks() {
    let resp = openai_to_anthropic_response(
        &json!({
            "id": "chatcmpl-1",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "reasoning_content": "think hard",
                    "reasoning_signature": "sig",
                    "content": "answer"
                }
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        }),
        "deepseek-v4-flash",
    );

    assert_eq!(resp["type"], "message");
    assert_eq!(resp["role"], "assistant");
    assert_eq!(resp["model"], "deepseek-v4-flash");
    assert_eq!(resp["content"][0]["type"], "thinking");
    assert_eq!(resp["content"][0]["thinking"], "think hard");
    assert_eq!(resp["content"][0]["signature"], "sig");
    assert_eq!(resp["content"][1]["type"], "text");
    assert_eq!(resp["content"][1]["text"], "answer");
    assert_eq!(resp["stop_reason"], "end_turn");
    assert_eq!(resp["usage"]["input_tokens"], 10);
    assert_eq!(resp["usage"]["output_tokens"], 5);
}

#[test]
fn response_converts_tool_calls_to_tool_use() {
    let resp = openai_to_anthropic_response(
        &json!({
            "id": "chatcmpl-2",
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": r#"{"city":"bj"}"#}
                    }]
                }
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        }),
        "m",
    );

    assert_eq!(resp["stop_reason"], "tool_use");
    assert_eq!(resp["content"][0]["type"], "tool_use");
    assert_eq!(resp["content"][0]["id"], "call_1");
    assert_eq!(resp["content"][0]["name"], "get_weather");
    assert_eq!(resp["content"][0]["input"]["city"], "bj");
}

#[test]
fn response_maps_length_finish_reason() {
    let resp = openai_to_anthropic_response(
        &json!({
            "choices": [{"finish_reason": "length", "message": {"content": "x"}}],
            "usage": {}
        }),
        "m",
    );
    assert_eq!(resp["stop_reason"], "max_tokens");
}

#[test]
fn response_maps_cache_usage_fields() {
    let resp = openai_to_anthropic_response(
        &json!({
            "choices": [{"finish_reason": "stop", "message": {"content": "x"}}],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 10,
                "prompt_cache_hit_tokens": 80,
                "prompt_cache_miss_tokens": 20
            }
        }),
        "m",
    );

    assert_eq!(resp["usage"]["input_tokens"], 100);
    assert_eq!(resp["usage"]["output_tokens"], 10);
    assert_eq!(resp["usage"]["cache_read_input_tokens"], 80);
    assert_eq!(resp["usage"]["cache_creation_input_tokens"], 20);
}

#[test]
fn cache_usage_recognizes_multiple_spellings() {
    assert_eq!(
        cache_usage_from_openai(&json!({"prompt_cache_hit_tokens": 7, "prompt_cache_miss_tokens": 3})),
        (7, 3)
    );
    assert_eq!(
        cache_usage_from_openai(&json!({"prompt_tokens": 50, "prompt_tokens_details": {"cached_tokens": 30}})),
        (30, 20)
    );
    assert_eq!(
        cache_usage_from_openai(&json!({"cached_tokens": 9})),
        (9, 0)
    );
    assert_eq!(cache_usage_from_openai(&json!({})), (0, 0));
}

// ---------------------------------------------------------------------------
// Tool / tool_choice mappers
// ---------------------------------------------------------------------------

#[test]
fn tool_choice_mapping() {
    assert_eq!(map_tool_choice_to_openai(&json!("auto")), json!("auto"));
    assert_eq!(map_tool_choice_to_openai(&json!({"type": "auto"})), json!("auto"));
    assert_eq!(map_tool_choice_to_openai(&json!({"type": "any"})), json!("required"));
    assert_eq!(
        map_tool_choice_to_openai(&json!({"type": "tool", "name": "f"})),
        json!({"type": "function", "function": {"name": "f"}})
    );
    assert_eq!(map_tool_choice_to_openai(&json!({})), json!("auto"));
}

#[test]
fn tools_mapping() {
    let mapped = map_tools_to_openai(&json!([
        {"name": "f", "description": "d", "input_schema": {"type": "object"}}
    ]));
    assert_eq!(mapped[0]["type"], "function");
    assert_eq!(mapped[0]["function"]["name"], "f");
    assert_eq!(mapped[0]["function"]["description"], "d");
    assert_eq!(mapped[0]["function"]["parameters"]["type"], "object");
}

// ---------------------------------------------------------------------------
// Streaming SSE state machine
// ---------------------------------------------------------------------------

#[test]
fn sse_state_machine_full_sequence() {
    let mut conv = OpenAISseConverter::new("deepseek-v4-flash");

    let mut all = Vec::new();
    // Reasoning
    all.extend(conv.feed(&json!({
        "id": "x", "object": "chat.completion.chunk", "choices": [{
            "index": 0, "delta": {"reasoning_content": "think"}, "finish_reason": null
        }]
    })));
    // Text
    all.extend(conv.feed(&json!({
        "choices": [{"index": 0, "delta": {"content": "hello"}, "finish_reason": null}]
    })));
    // Tool call
    all.extend(conv.feed(&json!({
        "choices": [{
            "index": 0,
            "delta": {
                "tool_calls": [
                    {"index": 0, "id": "call_1", "function": {"name": "f", "arguments": "{\"a\":\""}}
                ]
            },
            "finish_reason": null
        }]
    })));
    all.extend(conv.feed(&json!({
        "choices": [{
            "index": 0,
            "delta": {
                "tool_calls": [
                    {"index": 0, "function": {"arguments": "1}"}}
                ]
            },
            "finish_reason": null
        }]
    })));
    // Finish
    all.extend(conv.feed(&json!({
        "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
    })));
    all.extend(conv.finish());

    let text: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
    // message_start
    assert!(text[0].starts_with("event: message_start\ndata: {\"type\":\"message_start\""));
    // thinking block opens
    assert!(text.iter().any(|s| s.starts_with("event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\"")), "missing thinking start: {text:?}");
    assert!(text.iter().any(|s| s.contains("\"type\":\"thinking_delta\"")), "missing thinking delta: {text:?}");
    // thinking closes before text
    let think_stop = text.iter().position(|s| s.contains("\"index\":0") && s.contains("content_block_stop"));
    let text_start = text.iter().position(|s| s.contains("\"index\":1") && s.contains("content_block_start"));
    assert!(think_stop.is_some() && text_start.is_some() && think_stop.unwrap() < text_start.unwrap(), "order wrong: {text:?}");
    // text delta
    assert!(text.iter().any(|s| s.contains("\"text_delta\"")), "missing text delta: {text:?}");
    // tool block at index 2 (thinking=0, text=1)
    assert!(text.iter().any(|s| s.contains("\"index\":2") && s.contains("content_block_start")), "missing tool start: {text:?}");
    assert!(text.iter().any(|s| s.contains("\"input_json_delta\"")), "missing input_json_delta: {text:?}");
    // message_delta with stop_reason tool_use
    let delta = text.iter().find(|s| s.starts_with("event: message_delta")).expect("missing message_delta");
    assert!(delta.contains("\"stop_reason\":\"tool_use\""), "bad stop reason: {delta}");
    // message_stop last
    assert_eq!(text.last().unwrap().trim(), "event: message_stop\ndata: {\"type\":\"message_stop\"}");
}

#[test]
fn sse_state_machine_text_only_no_thinking() {
    let mut conv = OpenAISseConverter::new("m");
    let mut all = Vec::new();
    all.extend(conv.feed(&json!({
        "choices": [{"index": 0, "delta": {"content": "hi"}, "finish_reason": null}]
    })));
    all.extend(conv.feed(&json!({
        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
    })));
    all.extend(conv.finish());

    let text: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
    // text block at index 0 (no thinking)
    assert!(text.iter().any(|s| s.contains("\"index\":0") && s.contains("content_block_start")), "missing text start: {text:?}");
    // message_delta end_turn
    let delta = text.iter().find(|s| s.starts_with("event: message_delta")).unwrap();
    assert!(delta.contains("\"stop_reason\":\"end_turn\""));
    assert_eq!(text.last().unwrap().trim().starts_with("event: message_stop"), true);
}

#[test]
fn sse_state_machine_usage_capture() {
    let mut conv = OpenAISseConverter::new("m");
    conv.feed(&json!({"choices": [{"index": 0, "delta": {"content": "a"}, "finish_reason": null}]}));
    conv.feed(&json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}], "usage": {
        "prompt_tokens": 12, "completion_tokens": 3,
        "prompt_cache_hit_tokens": 5, "prompt_cache_miss_tokens": 7
    }}));
    assert_eq!(conv.usage_tokens(), (12, 3, 5, 7));

    let evs = conv.finish();
    let delta = evs.iter().find(|s| s.starts_with("event: message_delta")).unwrap();
    assert!(delta.contains("\"output_tokens\":3"), "real usage in message_delta: {delta}");
}

// ---------------------------------------------------------------------------
// SSE framing
// ---------------------------------------------------------------------------

#[test]
fn parse_sse_chunks_handles_chunk_boundaries() {
    // Event split across two buffer appends.
    let mut buffer = Vec::new();
    buffer.extend_from_slice(b"data: {\"a\":1}\n\npart");
    let evs = parse_sse_chunks(&mut buffer, 16);
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0], "data: {\"a\":1}\n\n");
    // trailing "part" stays buffered
    assert_eq!(buffer, b"part");

    buffer.extend_from_slice(b"ial: 2}\n\n");
    let evs2 = parse_sse_chunks(&mut buffer, 16);
    assert_eq!(evs2.len(), 1);
    assert_eq!(evs2[0], "partial: 2}\n\n");
}

#[test]
fn sse_data_payload_extraction() {
    assert_eq!(
        sse_data_payload("event: message_start\ndata: {\"type\":\"message_start\"}\n\n"),
        Some(r#"{"type":"message_start"}"#)
    );
    assert_eq!(sse_data_payload("data: [DONE]\n\n"), Some("[DONE]"));
    assert_eq!(sse_data_payload("event: ping\n\n"), None);
}
