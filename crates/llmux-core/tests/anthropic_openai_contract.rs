//! Contract tests for the Anthropic↔OpenAI protocol conversion module.
//! Pure function tests — no network.

use llmux_core::proxy::anthropic_openai::{
    anthropic_to_openai_request, anthropic_to_openai_request_with, cache_usage_from_openai,
    map_tool_choice_to_openai, map_tools_to_openai, openai_to_anthropic_response,
    parse_sse_chunks, sse_data_payload, OpenAISseConverter,
};
use llmux_core::proxy::responses::{
    anthropic_to_responses, chat_to_responses, ResponsesToChatConverter,
};
use serde_json::{json, Value};

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
fn request_keeps_single_text_block_as_array_for_its_marker() {
    // A lone text block stays an array: the shared-prefix marker lives on the
    // part, so flattening to a bare string would drop it.
    let req = anthropic_to_openai_request(
        &json!({
            "model": "m",
            "messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}]
        }),
        "t",
    )
    .unwrap();

    assert_eq!(
        req["messages"][0]["content"],
        json!([{"type": "text", "text": "hi", "prompt_cache_breakpoint": {"mode": "explicit"}}])
    );
}

#[test]
fn request_drops_cache_control_from_the_system_blocks() {
    // 生产实测：Claude Code 的顶层 system 是带 cache_control 的块数组，转换后成为
    // messages[0]；严格 chat 上游（Console Go 的 cat-jam-free）对它回
    // `[invalid_value] Invalid value for 'messages[0].content'`。块数组与文本都要留住，
    // 只把 cache_control 拿掉。
    let req = anthropic_to_openai_request_with(
        &json!({
            "model": "m",
            "system": [
                {"type": "text", "text": "You are terse.", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "Tools are available.", "cache_control": {"type": "ephemeral"}}
            ],
            "messages": [{"role": "user", "content": "hi"}]
        }),
        "t",
        false, // 不给 user 块加 marker，排除干扰
    )
    .unwrap();

    let sys = &req["messages"][0];
    assert_eq!(sys["role"], "system");
    let parts = sys["content"].as_array().expect("system 块数组必须保持数组");
    assert_eq!(parts.len(), 2, "块个数不得改变");
    assert_eq!(parts[0]["text"], "You are terse.");
    assert_eq!(parts[1]["text"], "Tools are available.");
    assert!(parts[0].get("cache_control").is_none(), "system 块不得带 cache_control");
    assert!(parts[1].get("cache_control").is_none(), "system 块不得带 cache_control");
}

#[test]
fn request_drops_cache_control_from_user_text_blocks() {
    // 与 U1 同一类，但走 transform_anthropic_message 的 text 分支。
    // add_bp = true，让 lone text block 因 marker 保持数组形态，从而能断言 part 内部。
    let req = anthropic_to_openai_request_with(
        &json!({
            "model": "m",
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "big", "cache_control": {"type": "ephemeral"}}]
            }]
        }),
        "t",
        true,
    )
    .unwrap();

    let part = &req["messages"][0]["content"][0];
    assert_eq!(part["type"], "text");
    assert_eq!(part["text"], "big");
    assert!(part.get("cache_control").is_none(), "text 块不得带 cache_control");
    assert_eq!(part["prompt_cache_breakpoint"], json!({"mode": "explicit"}), "llmux 自己的 marker 必须保留");
}

#[test]
fn request_drops_is_error_from_tool_messages() {
    // 实测：cat-jam-free 对带 is_error 的 tool 消息回 400
    // `[unsupported_parameter] Unrecognized request argument supplied: messages[2].is_error`。
    // 剥掉它，其余必须原样。
    let req = anthropic_to_openai_request(
        &json!({
            "model": "m",
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "f", "input": {}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "boom", "is_error": true}
                ]}
            ]
        }),
        "t",
    )
    .unwrap();

    let tool_msg = &req["messages"][1];
    assert_eq!(tool_msg["role"], "tool");
    assert_eq!(tool_msg["tool_call_id"], "toolu_1");
    assert_eq!(tool_msg["content"], "boom");
    assert!(tool_msg.get("is_error").is_none(), "tool 消息不得带 is_error");
}

#[test]
fn request_emits_no_anthropic_only_keys_anywhere() {
    // 覆盖所有会产出这两种字段的形状（system 数组 / text 块 / tool_result / 多轮），
    // 断言序列化后的整体里一个都不剩。这是「新加一条路径时忘了剥」的兜底。
    let req = anthropic_to_openai_request_with(
        &json!({
            "model": "m",
            "system": [{"type": "text", "text": "sys", "cache_control": {"type": "ephemeral"}}],
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "a", "cache_control": {"type": "ephemeral"}}
                ]},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "t1", "name": "f", "input": {}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "err", "is_error": true}
                ]}
            ]
        }),
        "t",
        true,
    )
    .unwrap();

    let serialized = serde_json::to_string(&req).unwrap();
    assert!(!serialized.contains("cache_control"), "转换结果里不得出现 cache_control: {serialized}");
    assert!(!serialized.contains("is_error"), "转换结果里不得出现 is_error: {serialized}");
}

#[test]
fn request_flattens_a_lone_text_block_whose_only_extra_was_cache_control() {
    // 单个 text 块只因 cache_control 而保持数组形态。剥掉之后它就该拍平成裸字符串 ——
    // 那是所有上游都接受的形状，对严格后端最稳。注意 llmux 自己的 marker 依旧
    // 会让数组保留（见 U2 与 request_keeps_single_text_block_as_array_for_its_marker）。
    let req = anthropic_to_openai_request_with(
        &json!({
            "model": "m",
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "big", "cache_control": {"type": "ephemeral"}}]
            }]
        }),
        "t",
        false,
    )
    .unwrap();

    assert_eq!(req["messages"][0]["content"], json!("big"));
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
    assert_eq!(assistant["content"][0]["text"], "let me check");
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
}

#[test]
fn request_folds_inline_system_into_the_preceding_message() {
    // Some OpenAI-style clients put a system turn inline (here after a user
    // turn). Qwen/DashScope and ninfer reject a system message that is not
    // first — but lifting it to the front is worse than useless: an inline
    // system is appended once per tool round-trip, so lifting it inserts text
    // *before the whole conversation* every turn, which invalidates an upstream
    // prefix cache from block 0 (measured 0% hit on a 120k-token session).
    // Fold it into the preceding message instead; that message stays
    // byte-identical for the next request, so only new turns are appended.
    let req = anthropic_to_openai_request(
        &json!({
            "model": "m",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "system", "content": "Be terse."},
                {"role": "user", "content": "again"}
            ]
        }),
        "t",
    )
    .unwrap();

    let msgs = req["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[0]["content"], "hi\n\nBe terse.");
    assert_eq!(msgs[1]["content"], "again");
    // No inline system survives.
    assert_eq!(msgs.iter().filter(|m| m["role"] == "system").count(), 0);
}

#[test]
fn request_keeps_top_level_system_and_folds_the_inline_one() {
    let req = anthropic_to_openai_request(
        &json!({
            "model": "m",
            "system": "Top level.",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "system", "content": "Inline."}
            ]
        }),
        "t",
    )
    .unwrap();

    let msgs = req["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2);
    // The top-level system is the client's own header: first, and untouched
    // (a lone string stays a string, byte-identical to before).
    assert_eq!(msgs[0]["role"], "system");
    assert_eq!(msgs[0]["content"], "Top level.");
    assert_eq!(msgs[1]["role"], "user");
    assert_eq!(msgs[1]["content"], "hi\n\nInline.");
}

#[test]
fn request_preserves_non_system_order_and_tool_pairing() {
    let req = anthropic_to_openai_request(
        &json!({
            "model": "m",
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "sunny"}
                ]},
                {"role": "system", "content": "late system"},
                {"role": "user", "content": "next"}
            ]
        }),
        "t",
    )
    .unwrap();

    let msgs = req["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    // Non-system order is untouched: the assistant tool_calls stays adjacent to
    // the tool result that references it.
    assert_eq!(msgs[0]["role"], "assistant");
    assert_eq!(msgs[0]["tool_calls"][0]["id"], "toolu_1");
    assert_eq!(msgs[1]["role"], "tool");
    assert_eq!(msgs[1]["tool_call_id"], "toolu_1");
    // The inline system folded into that tool result rather than moving.
    assert_eq!(msgs[1]["content"], "sunny\n\nlate system");
    assert_eq!(msgs[2]["role"], "user");
    assert_eq!(msgs[2]["content"], "next");
}

#[test]
fn consecutive_turns_stay_a_strict_extension_of_the_previous_request() {
    // The invariant the upstream prefix cache depends on: turn N+1's converted
    // body must contain turn N's converted messages as an exact prefix, so only
    // new turns are appended and every cached block still matches.
    //
    // This mirrors the production shape that measured 0% cache hits on a
    // 120k-token session (capture: 222 messages). Claude Code emits an inline
    // `role: "system"` reminder after each tool round-trip (hook output, a
    // `<total_tokens>` counter), so the number of inline systems grows by one
    // every turn. Lifting them to the front inserted text *before the whole
    // conversation* each turn, shifting every later byte — hence block-0-onward
    // cache invalidation. Real order per round:
    //
    //   user → assistant(tool_use) → user(tool_result) → system(reminder)
    fn turn(rounds: usize) -> Value {
        let mut messages = vec![json!({"role": "user", "content": "start"})];
        for n in 1..=rounds {
            messages.push(json!({"role": "assistant", "content": [
                {"type": "tool_use", "id": format!("call_{n}"), "name": "read_file", "input": {}}
            ]}));
            messages.push(json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": format!("call_{n}"), "content": format!("result {n}")}
            ]}));
            messages.push(json!({
                "role": "system",
                "content": format!("PostToolUse hook: read {n} files.\n\n<total_tokens>{} left</total_tokens>", 15000000 - n * 7919),
            }));
        }
        anthropic_to_openai_request(
            &json!({"model": "m", "system": [{"type": "text", "text": "You are a bot."}],
                    "messages": messages}),
            "t",
        )
        .unwrap()
    }

    for n in 1..5 {
        let prev = turn(n)["messages"].as_array().unwrap().clone();
        let next = turn(n + 1)["messages"].as_array().unwrap().clone();
        assert!(
            next.len() > prev.len(),
            "turn {n} must append messages, got {} -> {}",
            prev.len(),
            next.len()
        );
        assert_eq!(
            &next[..prev.len()],
            &prev[..],
            "turn {n} is not a strict extension: the head changed, which invalidates \
             every cached block downstream"
        );
        // The engine rejects an inline system outright, so none may survive past
        // the leading slot.
        assert!(next[1..].iter().all(|m| m["role"] != "system"));
    }
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
    assert_eq!(msg["content"][0]["text"], "answer");
    assert_eq!(msg["reasoning_content"], "hmm");
    assert_eq!(msg["reasoning_signature"], "sig1");
}

#[test]
fn request_drops_cache_control_from_a_text_block() {
    // 曾叫 request_keeps_cache_control_on_text_block。2026-10-14 生产实测后改契约：
    // 严格 chat 上游（Console Go 的 cat-jam-free）对带 cache_control 的 text 块回 400
    // `[invalid_value]`，而每个真实 Claude Code 请求都带它。该字段只在「上游是
    // Anthropic Messages 端点」时有语义，那条路径不经此转换器（逐字节直通）。
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
    assert!(req["messages"][0]["content"][0].get("cache_control").is_none());
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
    // `thinking` 是 Anthropic 专有参数，OpenAI chat/completions 直接 400
    // （实测 `Unknown parameter: 'thinking'`）。必须换成 `reasoning_effort`。
    assert!(req.get("thinking").is_none(), "thinking must not be forwarded: {req}");
    assert_eq!(req["reasoning_effort"], "medium");
}

#[test]
fn request_thinking_maps_budget_to_effort() {
    // 预算 → 档位：低预算给低档，高预算给高档。
    let cases = [
        (512, "low"),
        (4096, "medium"),
        (32000, "high"),
    ];    for (budget, want) in cases {
        let req = anthropic_to_openai_request(
            &json!({
                "model": "m",
                "messages": [{"role": "user", "content": "hi"}],
                "thinking": {"type": "enabled", "budget_tokens": budget}
            }),
            "t",
        )
        .unwrap();
        assert_eq!(req["reasoning_effort"], want, "budget {budget} -> {want}");
    }
}

#[test]
fn request_thinking_disabled_sends_no_effort() {
    // `disabled` 不该变成 `reasoning_effort: "none"` —— 多数 OpenAI 系上游
    // 连 `none` 一起拒（见 reasoning_effort 模块注释）。
    let req = anthropic_to_openai_request(
        &json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "thinking": {"type": "disabled"}
        }),
        "t",
    )
    .unwrap();

    assert!(req.get("thinking").is_none());
    assert!(req.get("reasoning_effort").is_none());
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

    // 4-store-3-display: fresh input = prompt - read, creation always 0 for OpenAI
    assert_eq!(resp["usage"]["input_tokens"], 20);
    assert_eq!(resp["usage"]["output_tokens"], 10);
    assert_eq!(resp["usage"]["cache_read_input_tokens"], 80);
    assert_eq!(resp["usage"]["cache_creation_input_tokens"], 0);
}

#[test]
fn cache_usage_recognizes_multiple_spellings() {
    assert_eq!(
        cache_usage_from_openai(&json!({"prompt_cache_hit_tokens": 7, "prompt_cache_miss_tokens": 3})),
        (7, 0)
    );
    assert_eq!(
        cache_usage_from_openai(&json!({"prompt_tokens": 50, "prompt_tokens_details": {"cached_tokens": 30}})),
        (30, 0)
    );
    assert_eq!(
        cache_usage_from_openai(&json!({"input_tokens": 1000, "input_tokens_details": {"cached_tokens": 800}})),
        (800, 0)
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
    // (field order is not guaranteed since serde_json dropped preserve_order —
    // assert on event name + key presence, not on JSON key order)
    assert!(text[0].starts_with("event: message_start\n") && text[0].contains("\"type\":\"message_start\""), "bad message_start: {text:?}");
    // thinking block opens
    assert!(text.iter().any(|s| s.contains("content_block_start") && s.contains("\"type\":\"content_block_start\"") && s.contains("\"index\":0") && s.contains("\"type\":\"thinking\"")), "missing thinking start: {text:?}");
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
fn sse_state_machine_emits_anthropic_error_for_unknown_finish_reason() {
    let mut conv = OpenAISseConverter::new("m");
    let mut all = conv.feed(&json!({
        "choices": [{"index": 0, "delta": {}, "finish_reason": "network_error"}]
    }));
    all.extend(conv.finish());

    let text: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
    assert!(
        text.iter().any(|s| s.starts_with("event: error") && s.contains("\"type\":\"api_error\"")),
        "unknown upstream finish reason must become an Anthropic error: {text:?}"
    );
    assert!(
        text.iter().all(|s| !s.contains("\"stop_reason\":\"network_error\"")),
        "vendor finish reason must not leak into Anthropic SSE: {text:?}"
    );
}

#[test]
fn sse_state_machine_usage_capture() {
    let mut conv = OpenAISseConverter::new("m");
    conv.feed(&json!({"choices": [{"index": 0, "delta": {"content": "a"}, "finish_reason": null}]}));
    conv.feed(&json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}], "usage": {
        "prompt_tokens": 12, "completion_tokens": 3,
        "prompt_cache_hit_tokens": 5, "prompt_cache_miss_tokens": 7
    }}));
    assert_eq!(conv.usage_tokens(), (7, 3, 5, 0));

    let evs = conv.finish();
    let delta = evs.iter().find(|s| s.starts_with("event: message_delta")).unwrap();
    assert!(delta.contains("\"output_tokens\":3"), "real usage in message_delta: {delta}");
    assert!(delta.contains("\"input_tokens\":7"), "real input usage in message_delta: {delta}");
    assert!(delta.contains("\"cache_read_input_tokens\":5"), "real cache usage in message_delta: {delta}");
    assert!(delta.contains("\"cache_creation_input_tokens\":0"), "real cache usage in message_delta: {delta}");
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
fn parse_sse_chunks_zero_limit_drains_all_events() {
    // max_events=0 must mean "no limit" (drain every complete event), not
    // "return nothing". A burst of tool_use arg-delta frames landing in one
    // upstream read must not be dropped at EOF.
    let mut buffer = Vec::new();
    for i in 0..300 {
        buffer.extend_from_slice(format!("data: {{\"i\":{i}}}\n\n").as_bytes());
    }
    let evs = parse_sse_chunks(&mut buffer, 0);
    assert_eq!(evs.len(), 300, "0-limit must return all complete events");
    assert!(buffer.is_empty(), "buffer must be fully drained");
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

// ---------------------------------------------------------------------------
// Regression: tool_calls without a complete id and function name
// ---------------------------------------------------------------------------

/// The Anthropic SDK rejects a message whose tool_use block has an empty id or
/// name outright, so a stream that never supplies a *name* must produce no block
/// at all — not a block with `id: ""` / `name: ""`.
///
/// Note the earlier version of this test was justified by a claim that turned
/// out to be false: that deepseek-v4.1-flash "streams arguments-only fragments
/// with no id/name anywhere". That was an artifact of `smart_truncate_body`
/// discarding the middle of long SSE bodies, where the opener lives. Measured
/// against untruncated logs (2026-09-26): 178/178 tool_call deltas carried both
/// id and name, on the **first** delta. This test now covers the genuine
/// failure mode — a name that genuinely never arrives — which a missing id can
/// no longer reach, because the converter synthesizes one.
#[test]
fn sse_never_opens_a_tool_block_without_id_and_name() {
    let mut conv = OpenAISseConverter::new("m");
    let mut all = Vec::new();
    // Arguments stream in with index but no identity.
    for frag in ["m", " json", ".t", "ool", "\":1}"] {
        all.extend(conv.feed(&json!({
            "choices": [{
                "index": 0,
                "delta": {"tool_calls": [{"index": 0, "function": {"arguments": frag}}]},
                "finish_reason": null
            }]
        })));
    }
    all.extend(conv.feed(&json!({
        "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
    })));
    all.extend(conv.finish());

    let text: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
    assert!(
        !text.iter().any(|s| s.contains("\"type\":\"tool_use\"")),
        "must not emit a nameless tool_use block: {text:?}"
    );
    // …and no dangling content_block_stop for a block that never opened.
    let stops = text.iter().filter(|s| s.contains("content_block_stop")).count();
    let starts = text.iter().filter(|s| s.contains("content_block_start")).count();
    assert_eq!(starts, stops, "every started block must be closed: {text:?}");
}

/// Identity arriving in a *later* fragment still opens the block, and the
/// arguments buffered before that point are flushed so the input is complete.
#[test]
fn sse_buffers_arguments_until_id_and_name_arrive() {
    let mut conv = OpenAISseConverter::new("m");
    let mut all = Vec::new();
    all.extend(conv.feed(&json!({
        "choices": [{
            "index": 0,
            "delta": {"tool_calls": [{"index": 0, "function": {"arguments": "{\"a\":"}}]},
            "finish_reason": null
        }]
    })));
    // Identity only now.
    all.extend(conv.feed(&json!({
        "choices": [{
            "index": 0,
            "delta": {"tool_calls": [
                {"index": 0, "id": "call_1", "function": {"name": "Bash", "arguments": "1}"}}
            ]},
            "finish_reason": null
        }]
    })));
    all.extend(conv.feed(&json!({
        "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
    })));
    all.extend(conv.finish());

    let text: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
    let start = text.iter().find(|s| s.contains("\"type\":\"tool_use\"")).unwrap_or_else(|| {
        panic!("block must open once id+name arrive: {text:?}")
    });
    assert!(start.contains("call_1") && start.contains("Bash"), "id/name missing: {start}");
    // Both halves of the arguments must survive, in order.
    let joined: String = text
        .iter()
        .filter(|s| s.contains("input_json_delta"))
        .copied()
        .collect::<Vec<&str>>()
        .concat();
    assert!(
        joined.contains("{\\\"a\\\":") && joined.contains("1}"),
        "buffered arguments must be flushed with the rest: {joined}"
    );
}

/// A well-formed tool call must be unaffected by the buffering (the common case).
#[test]
fn sse_complete_tool_call_is_unaffected() {
    let mut conv = OpenAISseConverter::new("m");
    let mut all = conv.feed(&json!({
        "choices": [{
            "index": 0,
            "delta": {"tool_calls": [
                {"index": 0, "id": "call_1", "function": {"name": "Bash", "arguments": "{}"}}
            ]},
            "finish_reason": null
        }]
    }));
    all.extend(conv.feed(&json!({
        "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
    })));
    all.extend(conv.finish());

    let text: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
    let start = text.iter().find(|s| s.contains("\"type\":\"tool_use\"")).expect("must open immediately");
    assert!(start.contains("call_1") && start.contains("Bash"), "bad start: {start}");
    assert!(
        text.iter().any(|s| s.contains("content_block_stop") && s.contains("\"index\":1")),
        "opened tool block must be closed: {text:?}"
    );
}

/// One malformed tool among several must not suppress the good ones.
#[test]
fn sse_malformed_tool_does_not_hide_a_valid_sibling() {
    let mut conv = OpenAISseConverter::new("m");
    let mut all = conv.feed(&json!({
        "choices": [{
            "index": 0,
            "delta": {"tool_calls": [
                {"index": 1, "id": "call_ok", "function": {"name": "Bash", "arguments": "{}"}}
            ]},
            "finish_reason": null
        }]
    }));
    // index 0 is nameless and never resolves.
    for frag in ["{", "\"x\":1}"] {
        all.extend(conv.feed(&json!({
            "choices": [{
                "index": 0,
                "delta": {"tool_calls": [{"index": 0, "function": {"arguments": frag}}]},
                "finish_reason": null
            }]
        })));
    }
    all.extend(conv.feed(&json!({
        "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
    })));
    all.extend(conv.finish());

    let text: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
    assert!(
        text.iter().any(|s| s.contains("\"type\":\"tool_use\"") && s.contains("call_ok")),
        "valid sibling must survive: {text:?}"
    );
    assert!(
        !text.iter().any(|s| s.contains("\"id\":\"\"")),
        "no empty-id tool_use may be emitted: {text:?}"
    );
}

// ---------------------------------------------------------------------------
// Regression: non-streaming tool_calls missing id or name
// ---------------------------------------------------------------------------

/// `openai_to_anthropic_response` must drop a tool_calls entry that has no id
/// or no name rather than emit a `tool_use` block the SDK will reject. The
/// rest of the message still has to come through.
#[test]
fn nonstream_drops_tool_call_missing_id_or_name() {
    for bad in [
        json!({"index": 0, "function": {"name": "Bash", "arguments": "{}"}}),
        json!({"index": 0, "id": "call_1", "function": {"arguments": "{}"}}),
        json!({"index": 0, "function": {"arguments": "{}"}}),
    ] {
        let out = openai_to_anthropic_response(
            &json!({
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "text kept",
                        "tool_calls": [bad]
                    },
                    "finish_reason": "tool_calls"
                }]
            }),
            "m",
        );
        let text = out.to_string();
        assert!(
            !text.contains("\"type\":\"tool_use\""),
            "malformed tool_call must be dropped, got: {text}"
        );
        assert!(text.contains("text kept"), "rest of the message must survive: {text}");
    }
}

/// A valid tool_call must still convert normally, and a valid sibling must
/// survive alongside a dropped one.
#[test]
fn nonstream_keeps_valid_tool_calls_and_valid_siblings() {
    let out = openai_to_anthropic_response(
        &json!({
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        {"index": 0, "function": {"name": "Nameless", "arguments": "{}"}},
                        {"index": 1, "id": "call_ok", "function": {"name": "Bash", "arguments": "{\"a\":1}"}}
                    ]
                },
                "finish_reason": "tool_calls"
            }]
        }),
        "m",
    );
    let text = out.to_string();
    assert!(text.contains("call_ok") && text.contains("Bash"), "valid call must survive: {text}");
    assert!(!text.contains("Nameless"), "nameless call must be dropped: {text}");
}

// ---------------------------------------------------------------------------
// Responses conversion: stream_options must not leak into a /responses body
// ---------------------------------------------------------------------------

#[test]
fn responses_request_omits_stream_options() {
    // The Responses API has no stream_options parameter — OpenAI rejects the request
    // wholesale ("Unknown parameter: 'stream_options.include_usage'", 400), which
    // takes down every streaming call routed to a /responses upstream.
    let req = anthropic_to_responses(
        &json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }),
        "gpt-6-luna",
    );

    assert_eq!(req["stream"], true);
    assert!(req.get("stream_options").is_none(),
        " Responses body must not carry stream_options: {req}");

    let req = chat_to_responses(
        &json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
            "stream_options": {"include_usage": true}
        }),
        "gpt-6-luna",
    );

    assert_eq!(req["stream"], true);
    assert!(req.get("stream_options").is_none(),
        "a chat-side stream_options must not be forwarded: {req}");
}

#[test]
fn responses_reasoning_maps_budget_to_effort() {
    // Responses API only accepts `reasoning.effort`; sending the Anthropic
    // `thinking` object verbatim 400s on `reasoning.budget_tokens`.
    let req = anthropic_to_responses(
        &json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "thinking": {"type": "enabled", "budget_tokens": 4096}
        }),
        "gpt-6-luna",
    );

    assert!(req.get("thinking").is_none(), "Anthropic `thinking` must not be forwarded: {req}");
    assert_eq!(req["reasoning"]["effort"], "medium");
    // 客户端没给 max_tokens 时，thinking 的预算就是输出上限（Anthropic 的
    // max_tokens **不含** thinking 预算）。
    assert_eq!(req["max_output_tokens"], 4096);
}

#[test]
fn responses_reasoning_disabled_sends_nothing() {
    // 不能发 `reasoning.effort: "none"` —— Responses 模型大多没有这一档。
    let req = anthropic_to_responses(
        &json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "thinking": {"type": "disabled"}
        }),
        "gpt-6-luna",
    );

    assert!(req.get("reasoning").is_none());
    assert!(req.get("max_output_tokens").is_none());
}

#[test]
fn responses_reasoning_does_not_override_client_max_tokens() {
    let req = anthropic_to_responses(
        &json!({
            "model": "m",
            "max_tokens": 128,
            "messages": [{"role": "user", "content": "hi"}],
            "thinking": {"type": "enabled", "budget_tokens": 32000}
        }),
        "gpt-6-luna",
    );

    assert_eq!(req["max_output_tokens"], 128);
    assert_eq!(req["reasoning"]["effort"], "high");
}

#[test]
fn responses_stream_usage_survives_without_stream_options() {
    // Usage accounting must not come to depend on the removed parameter: the
    // converter harvests it from the response.completed event.
    let mut c = ResponsesToChatConverter::new("gpt-6-luna");
    c.feed("event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}}\n\n");

    assert_eq!(c.usage_tokens(), (10, 5));
}
