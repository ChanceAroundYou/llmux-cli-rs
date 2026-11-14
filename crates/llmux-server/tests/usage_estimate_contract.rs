//! 上游 usage 虚报时，被标记账户的下发 usage 改用本地估算。
//!
//! 现场：第三方 Claude 中转（192.168.10.106:3000）对同一条 `say hi` 先报
//! 10,940、几分钟后报 43,552，100 KB 的 prompt 报 385,481 —— 这些数字是它
//! 自己那套 harness 的上下文，与转发出去的请求无关。Claude Code 的
//! auto-compact 阈值直接取 `input + cache_read + cache_creation`，于是 200k
//! 窗口「刚压缩完又满了」，反复压缩到 thrashing。
//!
//! 这里用一个会虚报的 mock 上游把三层都钉住：
//!   * 登记前 → 客户端看到上游原值（对照组：网关没有擅自改数）；
//!   * 登记后 → 非流式下发 `input_tokens = 本地估算`、缓存拆分归零、output 保留；
//!   * 登记后 → 流式 `message_start` / `message_delta` 同样改写，且事件边界
//!     被打断（mock 按 7 字节切块）时不能吐出半截 JSON 或吞掉终止事件。

use axum::response::IntoResponse;
use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
};
use serde_json::{json, Value};

const UPSTREAM_INPUT: i64 = 12_000;
const UPSTREAM_CACHE_READ: i64 = 16_000;
const UPSTREAM_CACHE_CREATE: i64 = 14_000;
const UPSTREAM_OUTPUT: i64 = 25;

fn inflated_message() -> Value {
    json!({
        "id": "mock_msg_1",
        "type": "message",
        "role": "assistant",
        "model": "claude-opus-5",
        "content": [{ "type": "text", "text": "ok" }],
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": UPSTREAM_INPUT,
            "cache_read_input_tokens": UPSTREAM_CACHE_READ,
            "cache_creation_input_tokens": UPSTREAM_CACHE_CREATE,
            "output_tokens": UPSTREAM_OUTPUT,
        }
    })
}

fn inflated_sse() -> String {
    format!(concat!(
        "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"id\":\"mock_msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-opus-5\",\"content\":[],\"usage\":{{\"input_tokens\":{i},\"cache_read_input_tokens\":{r},\"cache_creation_input_tokens\":{c},\"output_tokens\":1}}}}}}\n\n",
        "event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n",
        "event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"ok\"}}}}\n\n",
        "event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n",
        "event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"input_tokens\":{i},\"cache_read_input_tokens\":{r},\"cache_creation_input_tokens\":{c},\"output_tokens\":{o}}}}}\n\n",
        "event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n",
    ), i = UPSTREAM_INPUT, r = UPSTREAM_CACHE_READ, c = UPSTREAM_CACHE_CREATE, o = UPSTREAM_OUTPUT)
}

/// 流式 mock：把 SSE 拆成 7 字节一段下发，逼出「事件被 chunk 边界切开」的情况。
async fn spawn_mock_upstream() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = axum::Router::new().route(
        "/v1/messages",
        axum::routing::post(|axum::Json(body): axum::Json<Value>| async move {
            if body["stream"].as_bool() == Some(true) {
                let text = inflated_sse();
                let chunks: Vec<Result<bytes::Bytes, std::io::Error>> = text
                    .as_bytes()
                    .chunks(7)
                    .map(|part| Ok(bytes::Bytes::copy_from_slice(part)))
                    .collect();
                return axum::response::Response::builder()
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(futures_util::stream::iter(chunks)))
                    .unwrap();
            }
            axum::Json(inflated_message()).into_response()
        }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}")
}

async fn api_put(app: axum::Router, cookie: &str, uri: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(Method::PUT)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, cookie)
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = llmux_server::test_request(app, req).await;
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn api_post(app: axum::Router, cookie: &str, uri: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, cookie)
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = llmux_server::test_request(app, req).await;
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn v1_messages(app: axum::Router, key: &str, body: Value) -> (StatusCode, Value, String) {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/messages")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {key}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = llmux_server::test_request(app, req).await;
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let raw = String::from_utf8_lossy(&bytes).to_string();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null), raw)
}

async fn login(app: axum::Router) -> String {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "username": "admin", "password": "admin" }).to_string(),
        ))
        .unwrap();
    let resp = llmux_server::test_request(app, req).await;
    for val in resp.headers().get_all(header::SET_COOKIE) {
        if let Ok(s) = val.to_str() {
            if let Some(tok) = s.split(';').next() {
                if tok.starts_with("llmux_session=") {
                    return tok.to_string();
                }
            }
        }
    }
    panic!("no session cookie from login");
}

/// data 行里的 JSON（SSE 事件体）。
fn sse_events(raw: &str) -> Vec<Value> {
    raw.split("\n\n")
        .flat_map(|event| event.lines())
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|payload| serde_json::from_str::<Value>(payload.trim()).ok())
        .collect()
}

#[tokio::test]
async fn flagged_account_gets_estimated_prompt_tokens() {
    let upstream = spawn_mock_upstream().await;
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());

    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind("usage-estimate")
        .bind("sk-test")
        .bind("*")
        .execute(&state.pool)
        .await
        .unwrap();
    let cookie = login(app.clone()).await;

    let (status, created) = api_post(
        app.clone(),
        &cookie,
        "/api/accounts",
        json!({
            "alias": "lying-relay",
            "provider_id": "custom",
            "api_key": "sk-mock",
            "base_url": upstream,
            "anthropic_base_url": upstream,
            "messages_endpoint": upstream,
            "default_protocol": "messages",
            "skip_validation": true,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created:?}");
    let account_id = created["id"].as_i64().expect("account id");

    let (status, body) = api_post(
        app.clone(),
        &cookie,
        "/api/models/aliases",
        json!({
            "alias": "op",
            "target_model": "claude-opus-5",
            "account_ids": [account_id],
            "upstream_api": "default",
            "provider_id": "custom",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");

    let prompt = "x".repeat(20_000);
    let expected = llmux_core::usage_estimate::estimate_anthropic_tokens(&json!({
        "messages": [{ "role": "user", "content": prompt }]
    })) as i64;
    assert!(expected > 4_000 && expected < 6_000, "estimate={expected}");

    // 对照组：没登记时原样转发上游 usage —— 网关不替任何账户猜数字。
    let (status, control, _) = v1_messages(
        app.clone(),
        "sk-test",
        json!({ "model": "op", "max_tokens": 16, "messages": [{ "role": "user", "content": prompt }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(control["usage"]["input_tokens"].as_i64(), Some(UPSTREAM_INPUT));
    assert_eq!(
        control["usage"]["cache_read_input_tokens"].as_i64(),
        Some(UPSTREAM_CACHE_READ)
    );

    // 登记这个账户（运行期生效，不需要重启）。
    let (status, body) = api_put(
        app.clone(),
        &cookie,
        "/api/settings",
        json!({ "usage_estimate_accounts": account_id.to_string() }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert!(state.usage_estimate_enabled(account_id), "settings 应即时刷新内存快照");

    // 非流式：prompt 侧换成估算值，缓存拆分归零，output 保留上游值。
    let (status, rewritten, _) = v1_messages(
        app.clone(),
        "sk-test",
        json!({ "model": "op", "max_tokens": 16, "messages": [{ "role": "user", "content": prompt }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rewritten["usage"]["input_tokens"].as_i64(), Some(expected));
    assert_eq!(rewritten["usage"]["cache_read_input_tokens"].as_i64(), Some(0));
    assert_eq!(
        rewritten["usage"]["cache_creation_input_tokens"].as_i64(),
        Some(0)
    );
    assert_eq!(rewritten["usage"]["output_tokens"].as_i64(), Some(UPSTREAM_OUTPUT));
    // 正文与其余字段原样透传。
    assert_eq!(rewritten["content"][0]["text"].as_str(), Some("ok"));

    // 流式：message_start / message_delta 都要改写；mock 每 7 字节切一次，
    // 事件边界被打断也不能出半截 JSON。
    let (status, _, raw) = v1_messages(
        app.clone(),
        "sk-test",
        json!({
            "model": "op",
            "max_tokens": 16,
            "stream": true,
            "messages": [{ "role": "user", "content": prompt }]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let events = sse_events(&raw);
    assert!(!events.is_empty(), "raw={raw}");
    let start = events
        .iter()
        .find(|event| event["type"] == "message_start")
        .expect("message_start");
    assert_eq!(
        start["message"]["usage"]["input_tokens"].as_i64(),
        Some(expected)
    );
    assert_eq!(
        start["message"]["usage"]["cache_read_input_tokens"].as_i64(),
        Some(0)
    );
    let delta = events
        .iter()
        .find(|event| event["type"] == "message_delta")
        .expect("message_delta");
    assert_eq!(delta["usage"]["input_tokens"].as_i64(), Some(expected));
    assert_eq!(delta["usage"]["output_tokens"].as_i64(), Some(UPSTREAM_OUTPUT));
    assert!(
        raw.contains("message_stop"),
        "终止事件不能被改写器吞掉: {raw}"
    );
    // 正文增量原样转发。
    let text_delta = events
        .iter()
        .find(|event| event["type"] == "content_block_delta")
        .expect("content_block_delta");
    assert_eq!(text_delta["delta"]["text"].as_str(), Some("ok"));
}

// ---------------------------------------------------------------------------
// 转换路径：客户端发 Anthropic Messages，上游是 chat completions。
//
// 这是 `os`（驭风 chat 聚合）在线上真正走的路径 —— 只改 passthrough 的话，
// 用户看到的数字一点没变。非流式与流式都必须改写，流式还得扛住事件边界被切碎。
// ---------------------------------------------------------------------------

const CHAT_PROMPT: i64 = 30_000;
const CHAT_CACHED: i64 = 16_000;
const CHAT_COMPLETION: i64 = 25;

fn chat_usage() -> Value {
    json!({
        "prompt_tokens": CHAT_PROMPT,
        "completion_tokens": CHAT_COMPLETION,
        "total_tokens": CHAT_PROMPT + CHAT_COMPLETION,
        "prompt_tokens_details": { "cached_tokens": CHAT_CACHED },
    })
}

fn chat_completion_body() -> Value {
    json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion",
        "created": 1,
        "model": "mock-chat",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "ok" },
            "finish_reason": "stop",
        }],
        "usage": chat_usage(),
    })
}

fn chat_sse_text() -> String {
    let mut text = String::new();
    text.push_str(&format!(
        "data: {}\n\n",
        json!({
            "id": "chatcmpl-mock",
            "object": "chat.completion.chunk",
            "created": 1,
            "model": "mock-chat",
            "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "ok" }, "finish_reason": Value::Null }],
        })
    ));
    text.push_str(&format!(
        "data: {}\n\n",
        json!({
            "id": "chatcmpl-mock",
            "object": "chat.completion.chunk",
            "created": 1,
            "model": "mock-chat",
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        })
    ));
    // include_usage 尾块：转换器从这里取 prompt 侧真值。
    text.push_str(&format!(
        "data: {}\n\n",
        json!({
            "id": "chatcmpl-mock",
            "object": "chat.completion.chunk",
            "created": 1,
            "model": "mock-chat",
            "choices": [],
            "usage": chat_usage(),
        })
    ));
    text.push_str("data: [DONE]\n\n");
    text
}

async fn spawn_chat_mock_upstream() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = axum::Router::new().route(
        "/chat/completions",
        axum::routing::post(|axum::Json(body): axum::Json<Value>| async move {
            if body["stream"].as_bool() == Some(true) {
                let text = chat_sse_text();
                let chunks: Vec<Result<bytes::Bytes, std::io::Error>> = text
                    .as_bytes()
                    .chunks(7)
                    .map(|part| Ok(bytes::Bytes::copy_from_slice(part)))
                    .collect();
                return axum::response::Response::builder()
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(futures_util::stream::iter(chunks)))
                    .unwrap();
            }
            axum::Json(chat_completion_body()).into_response()
        }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn flagged_account_gets_estimated_usage_on_chat_conversion_path() {
    let upstream = spawn_chat_mock_upstream().await;
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());

    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind("usage-estimate-chat")
        .bind("sk-chat-test")
        .bind("*")
        .execute(&state.pool)
        .await
        .unwrap();
    let cookie = login(app.clone()).await;

    let (status, created) = api_post(
        app.clone(),
        &cookie,
        "/api/accounts",
        json!({
            "alias": "lying-chat-relay",
            "provider_id": "custom",
            "api_key": "sk-mock",
            "base_url": upstream,
            "anthropic_base_url": upstream,
            "default_protocol": "chat",
            "skip_validation": true,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created:?}");
    let account_id = created["id"].as_i64().expect("account id");

    let (status, body) = api_post(
        app.clone(),
        &cookie,
        "/api/models/aliases",
        json!({
            "alias": "os",
            "target_model": "deepseek-v4.1-flash",
            "account_ids": [account_id],
            "upstream_api": "chat",
            "provider_id": "custom",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");

    let prompt = "x".repeat(20_000);
    let expected = llmux_core::usage_estimate::estimate_anthropic_tokens(&json!({
        "messages": [{ "role": "user", "content": prompt }]
    })) as i64;
    let upstream_fresh_input = CHAT_PROMPT - CHAT_CACHED;

    // 对照组：登记前，转换器按上游 chat usage 折算（prompt - cached）。
    let (status, control, _) = v1_messages(
        app.clone(),
        "sk-chat-test",
        json!({ "model": "os", "max_tokens": 16, "messages": [{ "role": "user", "content": prompt }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{control:?}");
    assert_eq!(control["usage"]["input_tokens"].as_i64(), Some(upstream_fresh_input));
    assert_eq!(
        control["usage"]["cache_read_input_tokens"].as_i64(),
        Some(CHAT_CACHED)
    );

    let (status, body) = api_put(
        app.clone(),
        &cookie,
        "/api/settings",
        json!({ "usage_estimate_accounts": account_id.to_string() }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");

    // 非流式转换：prompt 侧换成本地估算，缓存拆分归零，output 保留。
    let (status, rewritten, _) = v1_messages(
        app.clone(),
        "sk-chat-test",
        json!({ "model": "os", "max_tokens": 16, "messages": [{ "role": "user", "content": prompt }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rewritten:?}");
    assert_eq!(rewritten["usage"]["input_tokens"].as_i64(), Some(expected));
    assert_eq!(rewritten["usage"]["cache_read_input_tokens"].as_i64(), Some(0));
    assert_eq!(
        rewritten["usage"]["cache_creation_input_tokens"].as_i64(),
        Some(0)
    );
    assert_eq!(
        rewritten["usage"]["output_tokens"].as_i64(),
        Some(CHAT_COMPLETION)
    );
    assert_eq!(rewritten["content"][0]["text"].as_str(), Some("ok"));

    // 流式转换：message_start 与 message_delta 都要改写；事件被 7 字节切块。
    let (status, _, raw) = v1_messages(
        app.clone(),
        "sk-chat-test",
        json!({
            "model": "os",
            "max_tokens": 16,
            "stream": true,
            "messages": [{ "role": "user", "content": prompt }]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let events = sse_events(&raw);
    assert!(!events.is_empty(), "raw={raw}");
    let start = events
        .iter()
        .find(|event| event["type"] == "message_start")
        .expect("message_start");
    assert_eq!(
        start["message"]["usage"]["input_tokens"].as_i64(),
        Some(expected),
        "转换路径的 message_start 也要换成本地估算"
    );
    let delta = events
        .iter()
        .find(|event| event["type"] == "message_delta")
        .expect("message_delta");
    assert_eq!(delta["usage"]["input_tokens"].as_i64(), Some(expected));
    assert_eq!(
        delta["usage"]["cache_read_input_tokens"].as_i64(),
        Some(0),
        "上游 16000 的 cache_read 不能漏到客户端"
    );
    assert_eq!(delta["usage"]["output_tokens"].as_i64(), Some(CHAT_COMPLETION));
    assert!(raw.contains("message_stop"), "终止事件不能被改写器吞掉: {raw}");
    let text_delta = events
        .iter()
        .find(|event| event["type"] == "content_block_delta")
        .expect("content_block_delta");
    assert_eq!(text_delta["delta"]["text"].as_str(), Some("ok"));
}