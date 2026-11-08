//! End-to-end verification for the upstream/downstream protocol split (Tasks 1-9).
//!
//! Two mock upstreams with DISTINCT base URLs make endpoint-column selection
//! observable:
//!
//! * **Mock A** serves `/chat/completions`, `/responses`, `/v1/messages` with
//!   correct per-path shapes and text markers (`A-chat` / `A-resp` / `A-msg`).
//! * **Mock B** serves ONLY `/responses` (`B-resp`); every other path 404s.
//!
//! The test account points `chat_endpoint`/`messages_endpoint` at A and
//! `responses_endpoint` at B. Any combo whose forced/target protocol is
//! Responses must land on B; everything else on A. If the runtime ever stops
//! reading the new `*_endpoint` columns and silently falls back to `base_url`
//! (= A), these assertions fail immediately.
//!
//! Step 4 then clears `responses_endpoint` (+ legacy URLs) and verifies bulk
//! fallback retargets the forced-responses alias to Default, after which its
//! responses-ingress requests fall back to the account's default protocol
//! (chat) on mock A.
//!
//! Run with: `bash scripts/e2e_protocol_split.sh`  (which invokes this test).

use axum::response::IntoResponse;
use axum::{body::{to_bytes, Body}, http::{header, Method, Request, StatusCode}};
use futures_util::StreamExt;
use serde_json::{json, Value};

// --- mock upstreams -----------------------------------------------------------

fn chat_completion_body(marker: &str) -> Value {
    json!({
        "id": "mock_cmpl_1",
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": marker},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 11, "completion_tokens": 22, "total_tokens": 33}
    })
}

/// Non-streaming Responses API object; `extract_responses_text` reads
/// `output[].content[].text` for `output_text` items.
fn responses_body(marker: &str) -> Value {
    json!({
        "id": "mock_resp_1",
        "object": "response",
        "status": "completed",
        "output": [{
            "type": "message",
            "id": "mock_msg_1",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": marker}]
        }],
        "usage": {"input_tokens": 13, "output_tokens": 24, "total_tokens": 37}
    })
}

/// Anthropic Messages reply; `/v1/messages` passthrough relays raw bytes.
fn anthropic_message_body(marker: &str) -> Value {
    json!({
        "id": "mock_msg_1",
        "type": "message",
        "role": "assistant",
        "model": "gpt-4o",
        "content": [{"type": "text", "text": marker}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 14, "output_tokens": 25}
    })
}

/// Responses-API SSE for a streaming request. Mirrors the event sequence a
/// real `/responses` upstream sends (created → delta → completed + usage).
fn responses_sse(marker: &str) -> String {
    format!(concat!(
        "event: response.created\ndata: {{\"type\":\"response.created\",\"response\":{{\"id\":\"mock_resp_1\",\"object\":\"response\",\"status\":\"in_progress\",\"model\":\"gpt-4o\",\"output\":[]}}}}\n\n",
        "event: response.output_item.added\ndata: {{\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{{\"id\":\"mock_msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"status\":\"in_progress\",\"content\":[]}}}}\n\n",
        "event: response.content_part.added\ndata: {{\"type\":\"response.content_part.added\",\"item_id\":\"mock_msg_1\",\"output_index\":0,\"content_index\":0,\"part\":{{\"type\":\"output_text\",\"text\":\"\"}}}}\n\n",
        "event: response.output_text.delta\ndata: {{\"type\":\"response.output_text.delta\",\"item_id\":\"mock_msg_1\",\"output_index\":0,\"content_index\":0,\"delta\":\"{marker}\"}}\n\n",
        "event: response.output_text.done\ndata: {{\"type\":\"response.output_text.done\",\"item_id\":\"mock_msg_1\",\"output_index\":0,\"content_index\":0,\"text\":\"{marker}\"}}\n\n",
        "event: response.completed\ndata: {{\"type\":\"response.completed\",\"response\":{{\"id\":\"mock_resp_1\",\"object\":\"response\",\"status\":\"completed\",\"model\":\"gpt-4o\",\"output\":[{{\"id\":\"mock_msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"status\":\"completed\",\"content\":[{{\"type\":\"output_text\",\"text\":\"{marker}\"}}]}}],\"usage\":{{\"input_tokens\":13,\"output_tokens\":24,\"total_tokens\":37}}}}}}\n\n",
    ), marker = marker)
}

/// Anthropic Messages SSE for a streaming request.
fn anthropic_sse(marker: &str) -> String {
    format!(concat!(
        "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"id\":\"mock_msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"gpt-4o\",\"content\":[],\"usage\":{{\"input_tokens\":14,\"output_tokens\":0}}}}}}\n\n",
        "event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n",
        "event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{marker}\"}}}}\n\n",
        "event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n",
        "event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":25}}}}\n\n",
        "event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n",
    ), marker = marker)
}

fn sse_response(body: String) -> axum::response::Response {
    axum::response::Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(body))
        .unwrap()
}

async fn spawn_mock_upstreams() -> (String, String) {
    let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr_a = listener_a.local_addr().unwrap();
    let router_a = axum::Router::new()
        .route("/chat/completions", axum::routing::post(|axum::Json(body): axum::Json<Value>| async move {
            if body["stream"].as_bool() == Some(true) {
                return sse_response(concat!(
                    "data: {\"id\":\"mock_cmpl_1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"A-chat-stream\"},\"finish_reason\":null}]}\n\n",
                    "data: {\"id\":\"mock_cmpl_1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":22}}\n\n",
                    "data: [DONE]\n\n",
                ).to_string());
            }
            axum::Json(chat_completion_body("A-chat")).into_response()
        }))
        .route("/responses", axum::routing::post(|axum::Json(body): axum::Json<Value>| async move {
            if body["stream"].as_bool() == Some(true) {
                return sse_response(responses_sse("A-resp-stream"));
            }
            axum::Json(responses_body("A-resp")).into_response()
        }))
        .route("/v1/messages", axum::routing::post(|axum::Json(body): axum::Json<Value>| async move {
            if body["stream"].as_bool() == Some(true) {
                return sse_response(anthropic_sse("A-msg-stream"));
            }
            axum::Json(anthropic_message_body("A-msg")).into_response()
        }));
    tokio::spawn(async move {
        let _ = axum::serve(listener_a, router_a).await;
    });

    // B serves ONLY /responses — unmatched paths get axum's default 404.
    let listener_b = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr_b = listener_b.local_addr().unwrap();
    let router_b = axum::Router::new()
        .route("/responses", axum::routing::post(|axum::Json(body): axum::Json<Value>| async move {
            if body["stream"].as_bool() == Some(true) {
                return sse_response(responses_sse("B-resp-stream"));
            }
            axum::Json(responses_body("B-resp")).into_response()
        }));
    tokio::spawn(async move {
        let _ = axum::serve(listener_b, router_b).await;
    });

    (format!("http://{addr_a}"), format!("http://{addr_b}"))
}

// --- request helpers ----------------------------------------------------------

async fn api_put(
    app: axum::Router,
    cookie: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
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
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn api_post(
    app: axum::Router,
    cookie: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
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
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn v1_post(
    app: axum::Router,
    key: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, Value, String) {
    let req = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {key}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = llmux_server::test_request(app, req).await;
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let raw = String::from_utf8_lossy(&bytes).to_string();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value, raw)
}

async fn login(app: axum::Router) -> String {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"username": "admin", "password": "admin"}).to_string(),
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

/// True if either OpenAI (`prompt_tokens`) or Anthropic/Responses
/// (`input_tokens`) usage is non-zero — both shapes appear across conversions.
fn usage_nonzero(v: &Value) -> bool {
    let pt = v["usage"]["prompt_tokens"].as_i64().unwrap_or(0);
    let it = v["usage"]["input_tokens"].as_i64().unwrap_or(0);
    let ot_total = v["usage"]["total_tokens"].as_i64().unwrap_or(0);
    pt > 0 || it > 0 || ot_total > 0
}

// --- the test ----------------------------------------------------------------

#[tokio::test]
async fn e2e_protocol_split_passthrough_and_fallback() {
    let (upstream_a, upstream_b) = spawn_mock_upstreams().await;

    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());

    // API key for the /v1/* ingress (allowed_models = "*" → all models).
    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind("e2e")
        .bind("sk-test")
        .bind("*")
        .execute(&state.pool)
        .await
        .unwrap();

    let cookie = login(app.clone()).await;

    // Step 1: account with distinct per-protocol endpoints. chat/messages → A,
    // responses → B. base_url/anthropic_base_url kept for legacy-compat coverage.
    let (create_status, create_body) = api_post(
        app.clone(),
        &cookie,
        "/api/accounts",
        json!({
            "alias": "mockacc",
            "provider_id": "openai",
            "api_key": "sk-mock",
            "base_url": upstream_a,
            "chat_endpoint": upstream_a,
            "responses_endpoint": upstream_b,
            "messages_endpoint": upstream_a,
            "anthropic_base_url": upstream_a,
            "default_protocol": "chat",
            "skip_validation": true,
        }),
    )
    .await;
    assert_eq!(create_status, StatusCode::OK, "{create_body:?}");
    let acct_id = create_body["id"].as_i64().expect("account id");

    // Step 2: four aliases — default / forced-chat / forced-responses / forced-messages.
    for (alias, upstream_api) in [
        ("of-default", "default"),
        ("of-chat", "chat"),
        ("of-resp", "responses"),
        ("of-msg", "messages"),
    ] {
        let (st, body) = api_post(
            app.clone(),
            &cookie,
            "/api/models/aliases",
            json!({
                "alias": alias,
                "target_model": "gpt-4o",
                "account_ids": [acct_id],
                "upstream_api": upstream_api,
                "provider_id": "openai",
            }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "alias {alias}: {body:?}");
    }

    // Step 3: route matrix — every (alias, ingress) combo must land on the
    // upstream implied by its target protocol, proven by response markers.
    let ingresses: &[(&str, &str)] = &[
        ("/v1/chat/completions", "chat"),
        ("/v1/responses", "responses"),
        ("/v1/messages", "messages"),
    ];
    let mk_body = |ingress: &str| match ingress {
        "chat" => json!({"model": "", "messages": [{"role": "user", "content": "hi"}]}),
        "responses" => json!({"model": "", "input": "hi"}),
        _ => json!({"model": "", "messages": [{"role": "user", "content": "hi"}]}),
    };

    for alias in ["of-default", "of-chat", "of-resp", "of-msg"] {
        for (path, ingress) in ingresses {
            let mut body = mk_body(ingress);
            body["model"] = json!(alias);

            let (st, resp, raw) = v1_post(app.clone(), "sk-test", path, body).await;
            let expected = match (alias, *ingress) {
                // Default mode: pass through whenever supported, else default (chat).
                ("of-default", "chat") => "A-chat",
                ("of-default", "responses") => "B-resp",
                ("of-default", "messages") => "A-msg",
                // Forced chat: always the chat endpoint.
                (_, "chat") if alias == "of-chat" => "A-chat",
                ("of-chat", _) => "A-chat",
                // Forced responses: always the responses endpoint (mock B).
                ("of-resp", _) => "B-resp",
                // Forced messages: always the messages endpoint (mock A), with
                // back-conversion for chat/responses ingresses.
                ("of-msg", _) => "A-msg",
                _ => unreachable!(),
            };
            assert_eq!(
                st,
                StatusCode::OK,
                "{alias} via {ingress}: expected 200, got {st:?}\nraw={raw}"
            );
            assert!(
                usage_nonzero(&resp),
                "{alias} via {ingress}: usage must be non-zero, got {resp:?}"
            );
            assert!(
                raw.contains(expected),
                "{alias} via {ingress}: expected marker '{expected}' in response, got:\n{raw}"
            );
        }
    }

    // A Messages ingress forced to Chat must transform the upstream OpenAI SSE
    // into Anthropic SSE; passing the raw OpenAI chunks makes Anthropic clients
    // receive no usable content.
    let (stream_status, _stream_value, stream_raw) = v1_post(
        app.clone(),
        "sk-test",
        "/v1/messages",
        json!({
            "model": "of-chat",
            "stream": true,
            "max_tokens": 32,
            "messages": [{"role": "user", "content": "hi"}],
        }),
    )
    .await;
    assert_eq!(stream_status, StatusCode::OK, "{stream_raw}");
    assert!(
        stream_raw.contains("event: message_start")
            && stream_raw.contains("event: content_block_delta")
            && stream_raw.contains("A-chat-stream")
            && stream_raw.contains("event: message_stop"),
        "Messages→Chat stream must be Anthropic SSE, got:\n{stream_raw}"
    );
    assert!(
        !stream_raw.contains("\"choices\""),
        "Messages→Chat stream leaked OpenAI SSE: {stream_raw}"
    );

    // Step 4: clear responses_endpoint (+ legacy URLs) → bulk fallback must
    // retarget of-resp to default; responses ingress then lands on chat (A).
    let (upd_status, upd_body) = api_put(
        app.clone(),
        &cookie,
        &format!("/api/accounts/{acct_id}"),
        json!({
            "alias": "mockacc",
            "provider_id": "openai",
            "api_key": "sk-mock",
            "base_url": null,
            "anthropic_base_url": null,
            "responses_endpoint": null,
            "chat_endpoint": upstream_a,
            "messages_endpoint": upstream_a,
            "skip_validation": true,
        }),
    )
    .await;
    assert_eq!(upd_status, StatusCode::OK, "{upd_body:?}");
    let affected = &upd_body["affectedAliases"]["ordinary"];
    assert!(
        affected.as_array().map(|a| a.contains(&json!("of-resp"))).unwrap_or(false),
        "bulk fallback should have retargeted of-resp, got {upd_body:?}"
    );

    // Post-fallback: forced-responses alias is gone; every ingress resolves via
    // default mode → chat endpoint on A.
    // Post-fallback expectations per decision-table semantics:
    // * responses support is GONE (responses_endpoint + base_url cleared) so any
    //   responses-targeting request falls back to the account default (chat).
    // * chat/messages remain supported and keep passing through natively.
    let post_cases: &[(&str, &str, &str)] = &[
        ("of-default", "responses", "A-chat"), // unsupported ingress → default chat
        ("of-resp", "chat", "A-chat"),         // retargeted to default; chat passthrough
        ("of-resp", "responses", "A-chat"),    // retargeted; falls back to chat
        ("of-resp", "messages", "A-msg"),      // messages still supported → native passthrough
    ];
    for (alias, ingress, expected_marker) in post_cases {
        let mut body = mk_body(ingress);
        body["model"] = json!(alias);
        let path = path_ingress_path(ingress);
        let (st, resp, raw) = v1_post(app.clone(), "sk-test", path, body).await;
        assert_eq!(
            st,
            StatusCode::OK,
            "post-fallback {alias} via {ingress}: expected 200, got {st:?}\nraw={raw}"
        );
        assert!(
            usage_nonzero(&resp),
            "post-fallback {alias} via {ingress}: usage must be non-zero, got {resp:?}"
        );
        assert!(
            raw.contains(expected_marker),
            "post-fallback {alias} via {ingress}: expected '{expected_marker}', got:\n{raw}"
        );
    }
}

fn path_ingress_path(ingress: &str) -> &'static str {
    match ingress {
        "chat" => "/v1/chat/completions",
        "responses" => "/v1/responses",
        _ => "/v1/messages",
    }
}

/// Aggregate alias with upstream_api=responses: a Messages-ingress request must
/// travel anthropic→responses (mock B) and come back responses→anthropic
/// (previously dispatch_aggregate_anthropic_via_responses was a placeholder that
/// delegated to the plain aggregate path and POSTed /v1/messages instead).
#[tokio::test]
async fn e2e_aggregate_messages_ingress_responses_upstream() {
    let (upstream_a, upstream_b) = spawn_mock_upstreams().await;

    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());

    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind("e2e-agg")
        .bind("sk-agg")
        .bind("*")
        .execute(&state.pool)
        .await
        .unwrap();

    let cookie = login(app.clone()).await;

    let (create_status, create_body) = api_post(
        app.clone(),
        &cookie,
        "/api/accounts",
        json!({
            "alias": "mockagg",
            "provider_id": "openai",
            "api_key": "sk-mock",
            "base_url": upstream_a,
            "chat_endpoint": upstream_a,
            "responses_endpoint": upstream_b,
            "messages_endpoint": upstream_a,
            "anthropic_base_url": upstream_a,
            "default_protocol": "chat",
        }),
    )
    .await;
    assert_eq!(create_status, StatusCode::OK, "{create_body:?}");
    let acct_id = create_body["id"].as_i64().expect("account id");

    let (st, body) = api_post(
        app.clone(),
        &cookie,
        "/api/aggregate-aliases",
        json!({
            "alias": "agg-resp",
            "candidates": [{"account_id": acct_id, "model": "gpt-4o"}],
            "upstream_api": "responses",
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");

    // Non-streaming: messages ingress → responses upstream (mock B) → anthropic.
    let (st, resp, raw) = v1_post(
        app.clone(),
        "sk-agg",
        "/v1/messages",
        json!({
            "model": "agg-resp",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi"}],
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "expected 200, got {st:?}\nraw={raw}");
    assert!(
        usage_nonzero(&resp),
        "usage must be non-zero, got {resp:?}"
    );
    assert!(
        raw.contains("B-resp"),
        "expected responses-upstream marker B-resp, got: {raw}"
    );
}

/// Forced modes must NOT reject unsupported accounts on save anymore (2026-08):
/// the UI warns per candidate and the runtime skips them. A forced-chat alias
/// bound to an account WITHOUT a chat endpoint must save (200) and skip that
/// account at dispatch time (502 with an explicit guard message) instead of
/// hitting build_passthrough's api.openai.com fallback URL.
#[tokio::test]
async fn e2e_forced_mode_unsupported_account_saves_and_skips() {
    let (upstream_a, upstream_b) = spawn_mock_upstreams().await;

    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());

    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind("e2e-unsup")
        .bind("sk-unsup")
        .bind("*")
        .execute(&state.pool)
        .await
        .unwrap();
    let cookie = login(app.clone()).await;

    // Account with messages+responses endpoints but NO chat endpoint and NO
    // legacy base_url (Chat would otherwise fall back to base_url and appear
    // supported).
    let (create_status, create_body) = api_post(
        app.clone(),
        &cookie,
        "/api/accounts",
        json!({
            "alias": "nofit",
            "provider_id": "openai",
            "api_key": "sk-mock",
            "messages_endpoint": upstream_a,
            "responses_endpoint": upstream_b,
            "default_protocol": "messages",
        }),
    )
    .await;
    assert_eq!(create_status, StatusCode::OK, "{create_body:?}");
    let acct_id = create_body["id"].as_i64().expect("account id");

    // Save must succeed — no more 409 alias_protocol_unsupported.
    let (st, body) = api_post(
        app.clone(),
        &cookie,
        "/api/models/aliases",
        json!({
            "alias": "forced-chat2",
            "target_model": "gpt-4o",
            "account_ids": [acct_id],
            "upstream_api": "chat",
            "provider_id": "openai",
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "forced-mode save must pass, got {st:?} {body:?}");

    // Dispatch must skip the unsupported account and fail with the guard error.
    let (st, _resp, raw) = v1_post(
        app.clone(),
        "sk-unsup",
        "/v1/chat/completions",
        json!({
            "model": "forced-chat2",
            "messages": [{"role": "user", "content": "hi"}],
        }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_GATEWAY, "expected 502, got {st:?}\nraw={raw}");
    assert!(
        raw.contains("does not support"),
        "expected skip guard message, got: {raw}"
    );
    assert!(
        !raw.contains("api.openai.com"),
        "must not hit the fallback URL, got: {raw}"
    );
}

/// Aggregate alias with upstream_api=chat: a Messages-ingress request must be
/// converted to the Chat upstream even when the bound account HAS a messages
/// endpoint (forced mode overrides per-account endpoint auto-selection).
#[tokio::test]
async fn e2e_aggregate_chat_mode_forces_chat_over_messages_endpoint() {
    let (upstream_a, upstream_b) = spawn_mock_upstreams().await;

    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());

    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind("e2e-aggchat")
        .bind("sk-aggchat")
        .bind("*")
        .execute(&state.pool)
        .await
        .unwrap();
    let cookie = login(app.clone()).await;

    // Account with BOTH chat+messages endpoints (messages endpoint on A).
    let (create_status, create_body) = api_post(
        app.clone(),
        &cookie,
        "/api/accounts",
        json!({
            "alias": "mockboth",
            "provider_id": "openai",
            "api_key": "sk-mock",
            "base_url": upstream_a,
            "chat_endpoint": upstream_a,
            "responses_endpoint": upstream_b,
            "messages_endpoint": upstream_a,
            "anthropic_base_url": upstream_a,
            "default_protocol": "chat",
        }),
    )
    .await;
    assert_eq!(create_status, StatusCode::OK, "{create_body:?}");
    let acct_id = create_body["id"].as_i64().expect("account id");

    let (st, body) = api_post(
        app.clone(),
        &cookie,
        "/api/aggregate-aliases",
        json!({
            "alias": "agg-chat",
            "candidates": [{"account_id": acct_id, "model": "gpt-4o"}],
            "upstream_api": "chat",
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");

    // Messages ingress under forced Chat must land on the chat upstream (A-chat),
    // NOT the messages endpoint (A-msg) — regression guard for the go-series
    // endpoints + chat-mode combination.
    let (st, _resp, raw) = v1_post(
        app.clone(),
        "sk-aggchat",
        "/v1/messages",
        json!({
            "model": "agg-chat",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi"}],
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "expected 200, got {st:?}\nraw={raw}");
    assert!(
        raw.contains("A-chat"),
        "forced chat must hit the chat upstream, got: {raw}"
    );
}

/// Aggregate alias with upstream_api=chat against a STRICT chat upstream:
/// the mock rejects any content part carrying keys outside
/// `{type, text, prompt_cache_breakpoint}` (real 400 from Console Go's
/// `cat-jam-free`, 2026-10-14) and any message carrying `is_error`
/// (real `[unsupported_parameter]` from the same model). llmux must strip both
/// Anthropic-only fields in conversion — every real Claude Code request carries
/// them, so without the strip the preferred candidate 400s on every real
/// request while probes (bare-string body) stay green.
///
/// The allowlist deliberately INCLUDES `prompt_cache_breakpoint`: llmux adds
/// that marker to every text part by default (`add_bp`) and the strict
/// upstream accepts it (measured 200). A `{type, text}`-only allowlist would
/// stay red after the fix — wrong mock, not wrong code.
#[tokio::test]
async fn e2e_aggregate_messages_ingress_chat_upstream_rejecting_anthropic_only_keys() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = axum::Router::new().route(
        "/chat/completions",
        axum::routing::post(|axum::Json(body): axum::Json<Value>| async move {
            let msgs = body.get("messages").and_then(Value::as_array).cloned().unwrap_or_default();
            for (i, m) in msgs.iter().enumerate() {
                if m.get("is_error").is_some() {
                    return (
                        StatusCode::BAD_REQUEST,
                        axum::Json(json!({
                            "error": {
                                "param": format!("messages[{i}].is_error"),
                                "type": "invalid_request_error",
                                "message": format!(
                                    "Upstream request failed: [unsupported_parameter] Unrecognized request argument supplied: messages[{i}].is_error."
                                ),
                            }
                        })),
                    )
                        .into_response();
                }
                if let Some(parts) = m.get("content").and_then(Value::as_array) {
                    for p in parts {
                        let extra: Vec<&String> = p
                            .as_object()
                            .map(|o| {
                                o.keys()
                                    .filter(|k| {
                                        *k != "type" && *k != "text" && *k != "prompt_cache_breakpoint"
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        if !extra.is_empty() {
                            return (
                                StatusCode::BAD_REQUEST,
                                axum::Json(json!({
                                    "error": {
                                        "param": format!("messages[{i}].content"),
                                        "type": "invalid_request_error",
                                        "message": format!(
                                            "Upstream request failed: [invalid_value] Invalid value for 'messages[{i}].content': The contents of the message, either a string or a non-empty array of text parts, or null on an assistant message only."
                                        ),
                                    }
                                })),
                            )
                                .into_response();
                        }
                    }
                }
            }
            axum::Json(chat_completion_body("A-chat-strict")).into_response()
        }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    let upstream = format!("http://{addr}");

    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());

    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind("e2e-aggstrict")
        .bind("sk-aggstrict")
        .bind("*")
        .execute(&state.pool)
        .await
        .unwrap();
    let cookie = login(app.clone()).await;

    let (create_status, create_body) = api_post(
        app.clone(),
        &cookie,
        "/api/accounts",
        json!({
            "alias": "strictacc",
            "provider_id": "openai",
            "api_key": "sk-mock",
            "chat_endpoint": upstream,
            "default_protocol": "chat",
        }),
    )
    .await;
    assert_eq!(create_status, StatusCode::OK, "{create_body:?}");
    let acct_id = create_body["id"].as_i64().expect("account id");

    let (st, body) = api_post(
        app.clone(),
        &cookie,
        "/api/aggregate-aliases",
        json!({
            "alias": "agg-strict",
            "candidates": [{"account_id": acct_id, "model": "strict-mock"}],
            "upstream_api": "chat",
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");

    // Fixture carries exactly the two fields llmux strips — and nothing else
    // the strict mock would reject (no `zzz`-style unknown keys; see the
    // spec's known-gap note on a general allowlist).
    let (st, _resp, raw) = v1_post(
        app.clone(),
        "sk-aggstrict",
        "/v1/messages",
        json!({
            "model": "agg-strict",
            "max_tokens": 64,
            "system": [
                {"type": "text", "text": "You are terse.", "cache_control": {"type": "ephemeral"}}
            ],
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "hi", "cache_control": {"type": "ephemeral"}}
                ]},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "f", "input": {}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "boom", "is_error": true}
                ]}
            ],
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "expected 200, got {st:?}\nraw={raw}");
    assert!(
        raw.contains("A-chat-strict"),
        "strict chat upstream must be reached, got: {raw}"
    );
}

/// C-B3：lock 路由往返 —— 建别名 → 锁定（库=1、内存锁）→ 解锁（库=0、内存解锁）。
/// 口径与 UI 共用 `/api/aggregate-aliases` 列表的 `locked` 字段。
#[tokio::test]
async fn aggregate_lock_route_round_trip() {
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    let cookie = login(app.clone()).await;

    // 建一个单候选别名（账户 id 随意写：建别名只校验存在，skip_validation 账户需先建）
    let (st, body) = api_post(
        app.clone(),
        &cookie,
        "/api/accounts",
        json!({
            "alias": "lockacct",
            "provider_id": "openai",
            "api_key": "sk-mock",
            "base_url": "http://127.0.0.1:9",
            "chat_endpoint": "http://127.0.0.1:9",
            "default_protocol": "chat",
            "skip_validation": true,
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    let acct_id = body["id"].as_i64().expect("account id");

    let (st, body) = api_post(
        app.clone(),
        &cookie,
        "/api/aggregate-aliases",
        json!({
            "alias": "agg-lock",
            "candidates": [{"account_id": acct_id, "model": "m"}],
            "upstream_api": "chat",
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");

    let row: (i64, String) = sqlx::query_as("SELECT id, alias FROM aggregate_aliases WHERE alias = 'agg-lock'")
        .fetch_one(&state.pool)
        .await
        .unwrap();

    // 锁定
    let (st, body) = api_post(app.clone(), &cookie, &format!("/api/aggregate-aliases/{}/lock", row.0), json!({"locked": true})).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["locked"], json!(true));
    let locked: i64 = sqlx::query_scalar("SELECT locked FROM aggregate_aliases WHERE alias = 'agg-lock'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(locked, 1);
    assert!(state.aggregate_router.lock().unwrap().is_locked(&row.1));

    // 解锁
    let (st, body) = api_post(app.clone(), &cookie, &format!("/api/aggregate-aliases/{}/lock", row.0), json!({"locked": false})).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["locked"], json!(false));
    let locked: i64 = sqlx::query_scalar("SELECT locked FROM aggregate_aliases WHERE alias = 'agg-lock'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(locked, 0);
    assert!(!state.aggregate_router.lock().unwrap().is_locked(&row.1));
}

async fn api_get(app: axum::Router, cookie: &str, uri: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = llmux_server::test_request(app, req).await;
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// 锁定 = 硬钉：锁定的候选失败时直接报错，不再故障转移到下一个候选。
/// 以前锁只冻结自动迁移，单次请求仍会顺着列表往下走 —— 下游看到的模型
/// 照样换来换去（2026-10-31 用户明确要求「失败也不切换」）。
#[tokio::test]
async fn locked_aggregate_candidate_does_not_fail_over() {
    let (upstream_a, _upstream_b) = spawn_mock_upstreams().await;
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());

    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind("e2e-pin")
        .bind("sk-pin")
        .bind("*")
        .execute(&state.pool)
        .await
        .unwrap();
    let cookie = login(app.clone()).await;

    // 候选 0：连不上；候选 1：mock A 正常应答。
    let (st, body) = api_post(
        app.clone(),
        &cookie,
        "/api/accounts",
        json!({
            "alias": "deadpin",
            "provider_id": "openai",
            "api_key": "sk-mock",
            "base_url": "http://127.0.0.1:9",
            "chat_endpoint": "http://127.0.0.1:9",
            "default_protocol": "chat",
            "skip_validation": true,
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    let dead_id = body["id"].as_i64().expect("dead account id");

    let (st, body) = api_post(
        app.clone(),
        &cookie,
        "/api/accounts",
        json!({
            "alias": "alivepin",
            "provider_id": "openai",
            "api_key": "sk-mock",
            "base_url": upstream_a,
            "chat_endpoint": upstream_a,
            "default_protocol": "chat",
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    let alive_id = body["id"].as_i64().expect("alive account id");

    let (st, body) = api_post(
        app.clone(),
        &cookie,
        "/api/aggregate-aliases",
        json!({
            "alias": "agg-pin",
            "candidates": [
                {"account_id": dead_id, "model": "m-dead"},
                {"account_id": alive_id, "model": "m-alive"},
            ],
            "upstream_api": "chat",
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    let id: i64 = sqlx::query_scalar("SELECT id FROM aggregate_aliases WHERE alias = 'agg-pin'")
        .fetch_one(&state.pool)
        .await
        .unwrap();

    // 锁在 #1（= 候选 0，连不上的那个）
    let (st, body) = api_post(
        app.clone(),
        &cookie,
        &format!("/api/aggregate-aliases/{id}/lock"),
        json!({"locked": true}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");

    let chat_body = json!({
        "model": "agg-pin",
        "max_tokens": 16,
        "messages": [{"role": "user", "content": "hi"}],
    });
    let (st, _resp, raw) = v1_post(app.clone(), "sk-pin", "/v1/chat/completions", chat_body.clone()).await;
    assert_eq!(
        st,
        StatusCode::BAD_GATEWAY,
        "锁定后不得转移到活候选，实际: {st:?} {raw}"
    );

    // 解锁后恢复故障转移
    let (st, body) = api_post(
        app.clone(),
        &cookie,
        &format!("/api/aggregate-aliases/{id}/lock"),
        json!({"locked": false}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    let (st, _resp, raw) = v1_post(app.clone(), "sk-pin", "/v1/chat/completions", chat_body).await;
    assert_eq!(st, StatusCode::OK, "解锁后应能故障转移: {raw}");
    assert!(raw.contains("A-chat"), "应落在 mock A 上，实际: {raw}");
}

/// 保存别名不得悄悄改掉锁定的位置：此前 ON CONFLICT 无条件写回 `active_idx=0`，
/// 锁着的别名被保存一次（改间隔/超时/增删候选）锁就跳到 #1。
#[tokio::test]
async fn saving_a_locked_alias_keeps_the_pin() {
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    let cookie = login(app.clone()).await;

    let mut account_ids = Vec::new();
    for alias in ["pinsrc", "pindst"] {
        let (st, body) = api_post(
            app.clone(),
            &cookie,
            "/api/accounts",
            json!({
                "alias": alias,
                "provider_id": "openai",
                "api_key": "sk-mock",
                "base_url": "http://127.0.0.1:9",
                "chat_endpoint": "http://127.0.0.1:9",
                "default_protocol": "chat",
                "skip_validation": true,
            }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body:?}");
        account_ids.push(body["id"].as_i64().expect("account id"));
    }
    let (a, b) = (account_ids[0], account_ids[1]);

    let (st, body) = api_post(
        app.clone(),
        &cookie,
        "/api/aggregate-aliases",
        json!({
            "alias": "agg-save-pin",
            "candidates": [{"account_id": a, "model": "ma"}, {"account_id": b, "model": "mb"}],
            "upstream_api": "chat",
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    let id: i64 =
        sqlx::query_scalar("SELECT id FROM aggregate_aliases WHERE alias = 'agg-save-pin'")
            .fetch_one(&state.pool)
            .await
            .unwrap();

    // 锁在 #2（候选 b/mb）
    let (st, body) = api_post(
        app.clone(),
        &cookie,
        &format!("/api/aggregate-aliases/{id}/active"),
        json!({"active": 1}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    let (st, body) = api_post(
        app.clone(),
        &cookie,
        &format!("/api/aggregate-aliases/{id}/lock"),
        json!({"locked": true}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");

    // 原样重存一次：位置与锁都必须原样保留
    let (st, body) = api_post(
        app.clone(),
        &cookie,
        "/api/aggregate-aliases",
        json!({
            "alias": "agg-save-pin",
            "candidates": [{"account_id": a, "model": "ma"}, {"account_id": b, "model": "mb"}],
            "upstream_api": "chat",
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    let (active, locked): (Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT active_idx, locked FROM aggregate_aliases WHERE alias = 'agg-save-pin'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(active, Some(1), "保存不该把 active 打回 0");
    assert_eq!(locked, Some(1), "保存不该解锁");

    // 列表口径：保存会清掉内存 entry，这里正是走「库里 active_idx」的回退，
    // UI 的锁标记（locked && active==idx）才不会刷新后跑到 #1。
    let (st, list) = api_get(app.clone(), &cookie, "/api/aggregate-aliases").await;
    assert_eq!(st, StatusCode::OK, "{list:?}");
    let entry = list
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["alias"] == "agg-save-pin")
        .expect("alias in list");
    assert_eq!(entry["active"], json!(1));
    assert_eq!(entry["locked"], json!(1));

    // 把锁定的候选删掉：必须明确解锁（锁一个不存在的候选没有意义）
    let (st, body) = api_post(
        app.clone(),
        &cookie,
        "/api/aggregate-aliases",
        json!({
            "alias": "agg-save-pin",
            "candidates": [{"account_id": a, "model": "ma"}],
            "upstream_api": "chat",
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    let (active, locked): (Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT active_idx, locked FROM aggregate_aliases WHERE alias = 'agg-save-pin'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(active, Some(0));
    assert_eq!(locked, Some(0), "锁定的候选没了就该解锁");
}

/// 前端加载走 `/api/models/summary`，不是 `/api/aggregate-aliases`。
/// 这两个聚合列表此前是**三份拷贝**，其中 summary 与 dashboard 两份**根本不发
/// `locked`**：刷新后前端拿到 `locked === undefined`，`!!agg.locked` 为 false，
/// 锁按钮一律变回「锁定」—— 用户视角就是「一刷新就自动解锁」，而库里 locked=1
/// 一直好好躺着。
#[tokio::test]
async fn summary_and_dashboard_also_report_lock_state() {
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    let cookie = login(app.clone()).await;

    let mut account_ids = Vec::new();
    for alias in ["locksrc", "lockdst"] {
        let (st, body) = api_post(
            app.clone(),
            &cookie,
            "/api/accounts",
            json!({
                "alias": alias,
                "provider_id": "openai",
                "api_key": "sk-mock",
                "base_url": "http://127.0.0.1:9",
                "chat_endpoint": "http://127.0.0.1:9",
                "default_protocol": "chat",
                "skip_validation": true,
            }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body:?}");
        account_ids.push(body["id"].as_i64().expect("account id"));
    }
    let (a, b) = (account_ids[0], account_ids[1]);

    let (st, body) = api_post(
        app.clone(),
        &cookie,
        "/api/aggregate-aliases",
        json!({
            "alias": "agg-refresh-lock",
            "candidates": [{"account_id": a, "model": "ma"}, {"account_id": b, "model": "mb"}],
            "upstream_api": "chat",
            "interval_secs": 120,
            "first_byte_timeout_secs": 45,
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    let id: i64 =
        sqlx::query_scalar("SELECT id FROM aggregate_aliases WHERE alias = 'agg-refresh-lock'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let (st, body) = api_post(
        app.clone(),
        &cookie,
        &format!("/api/aggregate-aliases/{id}/active"),
        json!({"active": 1}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    let (st, body) = api_post(
        app.clone(),
        &cookie,
        &format!("/api/aggregate-aliases/{id}/lock"),
        json!({"locked": true}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");

    // 重存一次：保存会清掉内存 entry，此后三个端点只能靠库里的值（= 重启后的视角）
    let (st, body) = api_post(
        app.clone(),
        &cookie,
        "/api/aggregate-aliases",
        json!({
            "alias": "agg-refresh-lock",
            "candidates": [{"account_id": a, "model": "ma"}, {"account_id": b, "model": "mb"}],
            "upstream_api": "chat",
            "interval_secs": 120,
            "first_byte_timeout_secs": 45,
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");

    // 新进程视角：内存没有 entry（模拟重启后还没请求过这个别名），
    // 三个端点都必须从库里把 locked 与 active 读出来。
    for uri in ["/api/aggregate-aliases", "/api/models/summary", "/api/dashboard"] {
        let (st, payload) = api_get(app.clone(), &cookie, uri).await;
        assert_eq!(st, StatusCode::OK, "{payload:?}");
        // /api/aggregate-aliases 返回裸数组，另两个是 `{ aggregateAliases: [...] }`
        let rows = if payload.is_array() {
            payload.as_array().unwrap()
        } else {
            payload["aggregateAliases"]
                .as_array()
                .unwrap_or_else(|| panic!("{uri} 的 aggregateAliases 不是数组: {payload:?}"))
        };
        let entry = rows
            .iter()
            .find(|e| e["alias"] == "agg-refresh-lock")
            .unwrap_or_else(|| panic!("{uri} 里没有 agg-refresh-lock"));
        assert_eq!(
            entry.get("locked").and_then(Value::as_i64),
            Some(1),
            "{uri} 必须报 locked —— 前端 !!locked 决定锁按钮显示「锁定」还是「解锁」"
        );
        assert_eq!(
            entry.get("active").and_then(Value::as_i64),
            Some(1),
            "{uri} 必须报库里的 active_idx —— 锁标记绑在 active 上"
        );
        assert_eq!(
            entry.get("interval_secs").and_then(Value::as_i64),
            Some(120),
            "{uri}"
        );
        assert_eq!(
            entry.get("first_byte_timeout_secs").and_then(Value::as_i64),
            Some(45),
            "{uri} 必须报 first_byte_timeout_secs —— 否则编辑表单回填成默认值"
        );
    }
}

/// Aggregate matrix: all nine (ingress, forced upstream protocol) pairs must
/// work both non-streaming and streaming. Streaming is the point of this test:
/// when the client sets `stream:true`, the downstream must receive incremental
/// SSE in **its own** protocol, never a buffered JSON reply.
#[tokio::test]
async fn e2e_aggregate_streaming_matrix_keeps_streaming() {
    let (upstream_a, upstream_b) = spawn_mock_upstreams().await;

    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());

    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind("e2e-matrix")
        .bind("sk-matrix")
        .bind("*")
        .execute(&state.pool)
        .await
        .unwrap();
    let cookie = login(app.clone()).await;

    let (create_status, create_body) = api_post(
        app.clone(),
        &cookie,
        "/api/accounts",
        json!({
            "alias": "mockmatrix",
            "provider_id": "openai",
            "api_key": "sk-mock",
            "base_url": upstream_a,
            "chat_endpoint": upstream_a,
            "responses_endpoint": upstream_b,
            "messages_endpoint": upstream_a,
            "anthropic_base_url": upstream_a,
            "default_protocol": "chat",
            "skip_validation": true,
        }),
    )
    .await;
    assert_eq!(create_status, StatusCode::OK, "{create_body:?}");
    let acct_id = create_body["id"].as_i64().expect("account id");

    for (alias, upstream_api) in [("mx-chat", "chat"), ("mx-msg", "messages"), ("mx-resp", "responses")] {
        let (st, body) = api_post(
            app.clone(),
            &cookie,
            "/api/aggregate-aliases",
            json!({
                "alias": alias,
                "candidates": [{"account_id": acct_id, "model": "gpt-4o"}],
                "upstream_api": upstream_api,
            }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "aggregate {alias}: {body:?}");
    }

    let mk_body = |ingress: &str| match ingress {
        "chat" => json!({"messages": [{"role": "user", "content": "hi"}]}),
        "responses" => json!({"input": "hi"}),
        _ => json!({"max_tokens": 64, "messages": [{"role": "user", "content": "hi"}]}),
    };

    let ingresses = [
        ("/v1/chat/completions", "chat"),
        ("/v1/responses", "responses"),
        ("/v1/messages", "messages"),
    ];
    // (aggregate alias, forced upstream, non-stream marker, stream marker)
    let modes = [
        ("mx-chat", "chat", "A-chat", "A-chat-stream"),
        ("mx-msg", "messages", "A-msg", "A-msg-stream"),
        ("mx-resp", "responses", "B-resp", "B-resp-stream"),
    ];

    for (alias, _mode, marker, stream_marker) in modes {
        for (path, ingress) in ingresses {
            let mut body = mk_body(ingress);
            body["model"] = json!(alias);

            let (st, resp, raw) = v1_post(app.clone(), "sk-matrix", path, body.clone()).await;
            assert_eq!(st, StatusCode::OK, "{alias}/{ingress} non-stream: {st:?}\nraw={raw}");
            assert!(raw.contains(marker), "{alias}/{ingress} non-stream lost '{marker}': {raw}");
            assert!(usage_nonzero(&resp), "{alias}/{ingress} non-stream usage: {resp:?}");

            body["stream"] = json!(true);
            let (st, _resp, raw) = v1_post(app.clone(), "sk-matrix", path, body).await;
            assert_eq!(st, StatusCode::OK, "{alias}/{ingress} stream: {st:?}\nraw={raw}");
            assert!(
                raw.contains(stream_marker),
                "{alias}/{ingress} stream lost '{stream_marker}': {raw}"
            );
            match ingress {
                "chat" => assert!(
                    raw.contains("\"choices\""),
                    "{alias}/{ingress} stream must be chat SSE: {raw}"
                ),
                "responses" => assert!(
                    raw.contains("response.output_text.delta") && raw.contains("response.completed"),
                    "{alias}/{ingress} stream must be responses SSE: {raw}"
                ),
                _ => assert!(
                    raw.contains("event: message_start") && raw.contains("event: message_stop"),
                    "{alias}/{ingress} stream must be anthropic SSE: {raw}"
                ),
            }
        }
    }
}

/// Upstreams that emit one complete SSE frame per protocol and then keep the
/// connection open forever. If the gateway buffers a cross-protocol stream
/// until upstream EOF, the downstream receives nothing and these tests time out.
async fn spawn_hanging_upstreams() -> (String, String) {
    fn hang_after(first: String) -> axum::response::Response {
        axum::response::Response::builder()
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from_stream(
                futures_util::stream::once(async move {
                    Ok::<_, std::io::Error>(bytes::Bytes::from(first))
                })
                .chain(futures_util::stream::pending()),
            ))
            .unwrap()
    }

    let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr_a = listener_a.local_addr().unwrap();
    let router_a = axum::Router::new()
        .route("/chat/completions", axum::routing::post(|| async {
            hang_after(concat!(
                "data: {\"id\":\"hang_cmpl\",\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"hang-chat\"},\"finish_reason\":null}]}\n\n",
            ).to_string())
        }))
        .route("/responses", axum::routing::post(|| async {
            hang_after(responses_sse("hang-resp"))
        }))
        .route("/v1/messages", axum::routing::post(|| async {
            hang_after(anthropic_sse("hang-msg"))
        }));
    tokio::spawn(async move {
        let _ = axum::serve(listener_a, router_a).await;
    });

    let listener_b = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr_b = listener_b.local_addr().unwrap();
    let router_b = axum::Router::new().route("/responses", axum::routing::post(|| async {
        hang_after(responses_sse("hang-resp"))
    }));
    tokio::spawn(async move {
        let _ = axum::serve(listener_b, router_b).await;
    });

    (format!("http://{addr_a}"), format!("http://{addr_b}"))
}

/// Strict-streaming proof for the full 3×3 aggregate matrix: the upstream sends
/// exactly one frame and then never closes. A gateway that buffers the whole
/// upstream response would never deliver a downstream frame, so every combo
/// here must produce its marker within the deadline.
#[tokio::test]
async fn e2e_cross_protocol_streams_before_upstream_finishes() {
    let (upstream_a, upstream_b) = spawn_hanging_upstreams().await;

    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());

    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind("e2e-live")
        .bind("sk-live")
        .bind("*")
        .execute(&state.pool)
        .await
        .unwrap();
    let cookie = login(app.clone()).await;

    let (create_status, create_body) = api_post(
        app.clone(),
        &cookie,
        "/api/accounts",
        json!({
            "alias": "mocklive",
            "provider_id": "openai",
            "api_key": "sk-mock",
            "base_url": upstream_a,
            "chat_endpoint": upstream_a,
            "responses_endpoint": upstream_b,
            "messages_endpoint": upstream_a,
            "anthropic_base_url": upstream_a,
            "default_protocol": "chat",
            "skip_validation": true,
        }),
    )
    .await;
    assert_eq!(create_status, StatusCode::OK, "{create_body:?}");
    let acct_id = create_body["id"].as_i64().expect("account id");

    for (alias, upstream_api) in [("lv-chat", "chat"), ("lv-msg", "messages"), ("lv-resp", "responses")] {
        let (st, body) = api_post(
            app.clone(),
            &cookie,
            "/api/aggregate-aliases",
            json!({
                "alias": alias,
                "candidates": [{"account_id": acct_id, "model": "gpt-4o"}],
                "upstream_api": upstream_api,
            }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "aggregate {alias}: {body:?}");
    }

    // (ingress path, ingress protocol, request body, expected upstream marker)
    let ingresses = [
        ("/v1/chat/completions", "chat", json!({"messages": [{"role": "user", "content": "hi"}]})),
        ("/v1/responses", "responses", json!({"input": "hi"})),
        ("/v1/messages", "messages", json!({"max_tokens": 64, "messages": [{"role": "user", "content": "hi"}]})),
    ];
    let modes = [
        ("lv-chat", "hang-chat"),
        ("lv-msg", "hang-msg"),
        ("lv-resp", "hang-resp"),
    ];

    for (alias, marker) in modes {
        for (path, ingress, body) in &ingresses {
            let mut body = body.clone();
            body["model"] = json!(alias);
            body["stream"] = json!(true);
            let req = Request::builder()
                .method(Method::POST)
                .uri(*path)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, "Bearer sk-live")
                .body(Body::from(body.to_string()))
                .unwrap();
            let resp = llmux_server::test_request(app.clone(), req).await;
            let status = resp.status();
            assert_eq!(status, StatusCode::OK, "{alias}/{ingress}: {status:?}");
            let ct = resp
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            assert!(ct.contains("text/event-stream"), "{alias}/{ingress}: content-type={ct}");

            let mut frames = resp.into_body().into_data_stream();
            let mut collected = String::new();
            let saw_marker = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while let Some(Ok(chunk)) = frames.next().await {
                    collected.push_str(&String::from_utf8_lossy(&chunk));
                    if collected.contains(marker) {
                        return true;
                    }
                }
                false
            })
            .await
            .unwrap_or(false);
            assert!(
                saw_marker,
                "{alias}/{ingress}: no downstream frame before the upstream finished (buffered?): {collected}"
            );
            match *ingress {
                "chat" => assert!(collected.contains("\"choices\""), "{alias}/{ingress}: expected chat SSE, got {collected}"),
                "responses" => assert!(
                    collected.contains("response.output_text.delta"),
                    "{alias}/{ingress}: expected responses SSE, got {collected}"
                ),
                _ => assert!(
                    collected.contains("event: message_start"),
                    "{alias}/{ingress}: expected anthropic SSE, got {collected}"
                ),
            }
        }
    }
}

/// Same strict-streaming proof as the aggregate test, but through the ordinary
/// alias dispatcher (`dispatch_with_conversion`). Covers all six cross-protocol
/// pairs plus the three passthroughs: the upstream never closes, so any
/// buffering implementation would time out.
#[tokio::test]
async fn e2e_ordinary_cross_protocol_streams_before_upstream_finishes() {
    let (upstream_a, upstream_b) = spawn_hanging_upstreams().await;

    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());

    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind("e2e-live-ordinary")
        .bind("sk-live-ord")
        .bind("*")
        .execute(&state.pool)
        .await
        .unwrap();
    let cookie = login(app.clone()).await;

    let (create_status, create_body) = api_post(
        app.clone(),
        &cookie,
        "/api/accounts",
        json!({
            "alias": "mockliveord",
            "provider_id": "openai",
            "api_key": "sk-mock",
            "base_url": upstream_a,
            "chat_endpoint": upstream_a,
            "responses_endpoint": upstream_b,
            "messages_endpoint": upstream_a,
            "anthropic_base_url": upstream_a,
            "default_protocol": "chat",
            "skip_validation": true,
        }),
    )
    .await;
    assert_eq!(create_status, StatusCode::OK, "{create_body:?}");
    let acct_id = create_body["id"].as_i64().expect("account id");

    for (alias, upstream_api) in [("lv2-chat", "chat"), ("lv2-msg", "messages"), ("lv2-resp", "responses")] {
        let (st, body) = api_post(
            app.clone(),
            &cookie,
            "/api/models/aliases",
            json!({
                "alias": alias,
                "target_model": "gpt-4o",
                "account_ids": [acct_id],
                "upstream_api": upstream_api,
                "provider_id": "openai",
            }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "alias {alias}: {body:?}");
    }

    let ingresses = [
        ("/v1/chat/completions", "chat", json!({"messages": [{"role": "user", "content": "hi"}]})),
        ("/v1/responses", "responses", json!({"input": "hi"})),
        ("/v1/messages", "messages", json!({"max_tokens": 64, "messages": [{"role": "user", "content": "hi"}]})),
    ];
    let aliases = [
        ("lv2-chat", "hang-chat"),
        ("lv2-msg", "hang-msg"),
        ("lv2-resp", "hang-resp"),
    ];

    for (alias, marker) in aliases {
        for (path, ingress, body) in &ingresses {
            let mut body = body.clone();
            body["model"] = json!(alias);
            body["stream"] = json!(true);
            let req = Request::builder()
                .method(Method::POST)
                .uri(*path)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, "Bearer sk-live-ord")
                .body(Body::from(body.to_string()))
                .unwrap();
            let resp = llmux_server::test_request(app.clone(), req).await;
            let status = resp.status();
            assert_eq!(status, StatusCode::OK, "{alias}/{ingress}: {status:?}");
            let ct = resp
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            assert!(ct.contains("text/event-stream"), "{alias}/{ingress}: content-type={ct}");

            let mut frames = resp.into_body().into_data_stream();
            let mut collected = String::new();
            let saw_marker = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while let Some(Ok(chunk)) = frames.next().await {
                    collected.push_str(&String::from_utf8_lossy(&chunk));
                    if collected.contains(marker) {
                        return true;
                    }
                }
                false
            })
            .await
            .unwrap_or(false);
            assert!(
                saw_marker,
                "{alias}/{ingress}: no downstream frame before the upstream finished (buffered?): {collected}"
            );
            match *ingress {
                "chat" => assert!(collected.contains("\"choices\""), "{alias}/{ingress}: expected chat SSE, got {collected}"),
                "responses" => assert!(
                    collected.contains("response.output_text.delta"),
                    "{alias}/{ingress}: expected responses SSE, got {collected}"
                ),
                _ => assert!(
                    collected.contains("event: message_start"),
                    "{alias}/{ingress}: expected anthropic SSE, got {collected}"
                ),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Aggregate `default` mode
//
// Before the unified dispatcher, `upstream_api=default` bypassed
// `target_protocol`: a Messages-only account fell back to api.openai.com, and a
// codex candidate got `/backend-api/codex/chat/completions` (404). These tests
// pin the fixed behaviour for accounts that a local mock CAN represent. codex's
// base URL is a compile-time constant and cannot be redirected to a mock, so
// that half is covered by the unit test
// `default_codex_account_targets_codex_responses_for_every_ingress`.
// ---------------------------------------------------------------------------

async fn create_test_account(app: axum::Router, cookie: &str, body: Value) -> i64 {
    let (status, created) = api_post(app, cookie, "/api/accounts", body).await;
    assert_eq!(status, StatusCode::OK, "{created:?}");
    created["id"].as_i64().expect("account id")
}

async fn create_aggregate(app: axum::Router, cookie: &str, alias: &str, account_id: i64, upstream_api: &str) {
    let (status, body) = api_post(
        app,
        cookie,
        "/api/aggregate-aliases",
        json!({
            "alias": alias,
            "candidates": [{"account_id": account_id, "model": "gpt-4o"}],
            "upstream_api": upstream_api,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "aggregate {alias}: {body:?}");
}

fn ingress_request(ingress: &str, model: &str) -> Value {
    match ingress {
        "chat" => json!({"model": model, "messages": [{"role": "user", "content": "hi"}]}),
        "responses" => json!({"model": model, "input": "hi"}),
        _ => json!({"model": model, "max_tokens": 64, "messages": [{"role": "user", "content": "hi"}]}),
    }
}

fn assert_ingress_shape(ingress: &str, resp: &Value, raw: &str) {
    match ingress {
        "chat" => {
            assert_eq!(resp["object"], json!("chat.completion"), "chat shape: {raw}");
            assert!(resp["choices"][0]["message"].is_object(), "chat shape: {raw}");
        }
        "responses" => {
            assert_eq!(resp["object"], json!("response"), "responses shape: {raw}");
            assert!(resp["output"].is_array(), "responses shape: {raw}");
        }
        _ => {
            assert_eq!(resp["type"], json!("message"), "messages shape: {raw}");
            assert!(resp["content"].is_array(), "messages shape: {raw}");
        }
    }
}

fn assert_ingress_stream(ingress: &str, raw: &str) {
    match ingress {
        "chat" => {
            // Chunks emitted by the converter don't carry `object`; `choices`
            // is the stable chat-SSE discriminant.
            assert!(raw.contains("\"choices\""), "chat SSE: {raw}");
            assert!(!raw.contains("content_block_delta"), "leaked anthropic SSE: {raw}");
        }
        "responses" => {
            assert!(raw.contains("response.output_text.delta"), "responses SSE: {raw}");
            assert!(!raw.contains("content_block_delta"), "leaked anthropic SSE: {raw}");
        }
        _ => {
            assert!(raw.contains("event: content_block_delta"), "anthropic SSE: {raw}");
            assert!(!raw.contains("chat.completion.chunk"), "leaked chat SSE: {raw}");
        }
    }
}

const INGRESSES: [(&str, &str); 3] = [
    ("/v1/chat/completions", "chat"),
    ("/v1/responses", "responses"),
    ("/v1/messages", "messages"),
];

/// `default` + Messages-only account: every ingress must reach the configured
/// Messages endpoint. Before the fix, Chat/Responses ingress silently fell back
/// to `https://api.openai.com/v1`.
#[tokio::test]
async fn e2e_aggregate_default_messages_only_all_ingresses() {
    let (upstream_a, _upstream_b) = spawn_mock_upstreams().await;

    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind("e2e-default-msg").bind("sk-default-msg").bind("*")
        .execute(&state.pool).await.unwrap();
    let cookie = login(app.clone()).await;

    let account = create_test_account(app.clone(), &cookie, json!({
        "alias": "only-messages",
        "provider_id": "openai",
        "api_key": "sk-mock",
        "base_url": null,
        "chat_endpoint": null,
        "responses_endpoint": null,
        "messages_endpoint": upstream_a,
        "default_protocol": "messages",
        "skip_validation": true,
    })).await;
    create_aggregate(app.clone(), &cookie, "def-msg", account, "default").await;

    for (path, ingress) in INGRESSES {
        let (status, resp, raw) = v1_post(app.clone(), "sk-default-msg", path, ingress_request(ingress, "def-msg")).await;
        assert_eq!(status, StatusCode::OK, "{ingress}: {status:?}\nraw={raw}");
        assert!(raw.contains("A-msg"), "{ingress} must hit the Messages endpoint: {raw}");
        assert_ingress_shape(ingress, &resp, &raw);
        assert!(usage_nonzero(&resp), "{ingress} usage: {resp:?}");

        let mut stream_body = ingress_request(ingress, "def-msg");
        stream_body["stream"] = json!(true);
        let (status, _resp, raw) = v1_post(app.clone(), "sk-default-msg", path, stream_body).await;
        assert_eq!(status, StatusCode::OK, "{ingress} stream: {status:?}\nraw={raw}");
        assert!(raw.contains("A-msg-stream"), "{ingress} stream marker: {raw}");
        assert_ingress_stream(ingress, &raw);
    }
}

/// `default` + Responses-only account: every ingress must reach the configured
/// Responses endpoint (mock B, which serves nothing else).
#[tokio::test]
async fn e2e_aggregate_default_responses_only_all_ingresses() {
    let (_upstream_a, upstream_b) = spawn_mock_upstreams().await;

    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind("e2e-default-resp").bind("sk-default-resp").bind("*")
        .execute(&state.pool).await.unwrap();
    let cookie = login(app.clone()).await;

    let account = create_test_account(app.clone(), &cookie, json!({
        "alias": "only-responses",
        "provider_id": "openai",
        "api_key": "sk-mock",
        "base_url": null,
        "chat_endpoint": null,
        "responses_endpoint": upstream_b,
        "messages_endpoint": null,
        "default_protocol": "responses",
        "skip_validation": true,
    })).await;
    create_aggregate(app.clone(), &cookie, "def-resp", account, "default").await;

    for (path, ingress) in INGRESSES {
        let (status, resp, raw) = v1_post(app.clone(), "sk-default-resp", path, ingress_request(ingress, "def-resp")).await;
        assert_eq!(status, StatusCode::OK, "{ingress}: {status:?}\nraw={raw}");
        assert!(raw.contains("B-resp"), "{ingress} must hit the Responses endpoint: {raw}");
        assert_ingress_shape(ingress, &resp, &raw);
        assert!(usage_nonzero(&resp), "{ingress} usage: {resp:?}");

        let mut stream_body = ingress_request(ingress, "def-resp");
        stream_body["stream"] = json!(true);
        let (status, _resp, raw) = v1_post(app.clone(), "sk-default-resp", path, stream_body).await;
        assert_eq!(status, StatusCode::OK, "{ingress} stream: {status:?}\nraw={raw}");
        assert!(raw.contains("B-resp-stream"), "{ingress} stream marker: {raw}");
        assert_ingress_stream(ingress, &raw);
    }
}

/// zen free tier forces `stream:true` upstream even for a non-streaming
/// downstream. The aggregate dispatcher must assemble the SSE by TARGET
/// protocol (chat vs responses) — the old codex-only coercion 502'd here.
#[tokio::test]
async fn e2e_aggregate_zen_nonstreaming_sse_coercion() {
    let (upstream_a, _upstream_b) = spawn_mock_upstreams().await;

    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind("e2e-zen-agg").bind("sk-zen-agg").bind("*")
        .execute(&state.pool).await.unwrap();
    let cookie = login(app.clone()).await;

    let account = create_test_account(app.clone(), &cookie, json!({
        "alias": "mock-zen",
        "provider_id": "zen",
        "category": "zen",
        "api_key": "sk-mock",
        "base_url": upstream_a,
        "chat_endpoint": upstream_a,
        "responses_endpoint": upstream_a,
        "skip_validation": true,
    })).await;
    create_aggregate(app.clone(), &cookie, "zen-chat", account, "chat").await;
    create_aggregate(app.clone(), &cookie, "zen-resp", account, "responses").await;

    // Chat target: upstream answers chat SSE, downstream gets one JSON object.
    let (status, resp, raw) = v1_post(app.clone(), "sk-zen-agg", "/v1/chat/completions",
        ingress_request("chat", "zen-chat")).await;
    assert_eq!(status, StatusCode::OK, "{status:?}\nraw={raw}");
    assert_eq!(resp["object"], json!("chat.completion"), "{raw}");
    assert!(raw.contains("A-chat-stream"), "{raw}");

    // Responses target: upstream answers responses SSE, downstream gets JSON.
    let (status, resp, raw) = v1_post(app.clone(), "sk-zen-agg", "/v1/responses",
        ingress_request("responses", "zen-resp")).await;
    assert_eq!(status, StatusCode::OK, "{status:?}\nraw={raw}");
    assert_eq!(resp["object"], json!("response"), "{raw}");
    assert!(raw.contains("A-resp-stream"), "{raw}");

    // Cross-protocol: Chat ingress → Responses target.
    let (status, resp, raw) = v1_post(app.clone(), "sk-zen-agg", "/v1/chat/completions",
        ingress_request("chat", "zen-resp")).await;
    assert_eq!(status, StatusCode::OK, "{status:?}\nraw={raw}");
    assert_eq!(resp["object"], json!("chat.completion"), "{raw}");
    assert!(raw.contains("A-resp-stream"), "{raw}");

    // Cross-protocol: Responses ingress → Chat target.
    let (status, resp, raw) = v1_post(app.clone(), "sk-zen-agg", "/v1/responses",
        ingress_request("responses", "zen-chat")).await;
    assert_eq!(status, StatusCode::OK, "{status:?}\nraw={raw}");
    assert_eq!(resp["object"], json!("response"), "{raw}");
    assert!(raw.contains("A-chat-stream"), "{raw}");
}

/// Mock that rejects `/chat/completions` with GitHub Copilot's
/// `unsupported_api_for_model` and serves the same model on `/v1/messages`.
async fn spawn_unsupported_chat_upstream() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = axum::Router::new()
        .route("/chat/completions", axum::routing::post(|| async {
            (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({"error": {"message": "unsupported_api_for_model: use /v1/messages", "type": "invalid_request_error"}})),
            )
        }))
        .route("/v1/messages", axum::routing::post(|axum::Json(body): axum::Json<Value>| async move {
            if body["stream"].as_bool() == Some(true) {
                return sse_response(anthropic_sse("C-msg-stream"));
            }
            axum::Json(anthropic_message_body("C-msg")).into_response()
        }));
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}")
}

/// `unsupported_api_for_model` fallback now applies to all three ingresses: the
/// same candidate is retried with a Messages target and the response is
/// back-translated to whatever the downstream speaks.
#[tokio::test]
async fn e2e_aggregate_unsupported_api_fallback_all_ingresses() {
    let upstream = spawn_unsupported_chat_upstream().await;

    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind("e2e-unsupported").bind("sk-unsupported").bind("*")
        .execute(&state.pool).await.unwrap();
    let cookie = login(app.clone()).await;

    let account = create_test_account(app.clone(), &cookie, json!({
        "alias": "copilot-ish",
        "provider_id": "openai",
        "api_key": "sk-mock",
        "base_url": upstream,
        "chat_endpoint": upstream,
        "messages_endpoint": upstream,
        "default_protocol": "chat",
        "skip_validation": true,
    })).await;
    // Forced chat: every ingress starts on /chat/completions, so the fallback
    // (not the routing table) is what makes Messages ingress work.
    create_aggregate(app.clone(), &cookie, "unsupported-agg", account, "chat").await;

    for (path, ingress) in INGRESSES {
        let (status, resp, raw) = v1_post(app.clone(), "sk-unsupported", path,
            ingress_request(ingress, "unsupported-agg")).await;
        assert_eq!(status, StatusCode::OK, "{ingress}: {status:?}\nraw={raw}");
        assert!(raw.contains("C-msg"), "{ingress} must fall back to /v1/messages: {raw}");
        assert_ingress_shape(ingress, &resp, &raw);

        let mut stream_body = ingress_request(ingress, "unsupported-agg");
        stream_body["stream"] = json!(true);
        let (status, _resp, raw) = v1_post(app.clone(), "sk-unsupported", path, stream_body).await;
        assert_eq!(status, StatusCode::OK, "{ingress} stream: {status:?}\nraw={raw}");
        assert!(raw.contains("C-msg-stream"), "{ingress} stream fallback marker: {raw}");
        assert_ingress_stream(ingress, &raw);
    }
}

/// 普通别名把 target 指到一条**聚合别名**上：请求按聚合路由（不是把聚合名当模型发）。
///
/// 反证点有两个：
///   1. 聚合名不是模型名 —— 走错路会打到 `/chat/completions` 之外拼出来的地址（404/502），
///      下面每条请求都会红；
///   2. 重定向走到底时 `resolve_model` 会回填一个前缀 provider（"gpt-4o" → openai），
///      只看解析结果分不出「别名重定向」和「裸模型名」，所以这里同时钉住另一种情况：
///      直接请求聚合别名本身仍然走聚合（第一条分支），两条路径必须给出同一份响应。
#[tokio::test]
async fn e2e_ordinary_alias_pointing_at_an_aggregate_alias_all_ingresses() {
    let (upstream_a, _upstream_b) = spawn_mock_upstreams().await;

    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind("e2e-redir-agg").bind("sk-redir-agg").bind("*")
        .execute(&state.pool).await.unwrap();
    let cookie = login(app.clone()).await;

    let account = create_test_account(app.clone(), &cookie, json!({
        "alias": "redir-agg-acct",
        "provider_id": "openai",
        "api_key": "sk-mock",
        "base_url": upstream_a,
        "chat_endpoint": upstream_a,
        "responses_endpoint": upstream_a,
        "messages_endpoint": upstream_a,
        "anthropic_base_url": upstream_a,
        "default_protocol": "chat",
        "skip_validation": true,
    })).await;
    create_aggregate(app.clone(), &cookie, "redir-agg-target", account, "default").await;

    // 重定向写法：不绑账户、不指 provider，target_model 就是那条聚合别名
    let (status, body) = api_post(app.clone(), &cookie, "/api/models/aliases", json!({
        "alias": "to-agg",
        "target_model": "redir-agg-target",
        "upstream_api": "default",
    })).await;
    assert_eq!(status, StatusCode::OK, "redirect alias: {body:?}");

    for (path, ingress) in INGRESSES {
        let (status, resp, raw) = v1_post(app.clone(), "sk-redir-agg", path, ingress_request(ingress, "to-agg")).await;
        assert_eq!(status, StatusCode::OK, "{ingress} via redirect alias: {status:?}\nraw={raw}");
        assert_ingress_shape(ingress, &resp, &raw);
        assert!(usage_nonzero(&resp), "{ingress} usage: {resp:?}");

        let mut stream_body = ingress_request(ingress, "to-agg");
        stream_body["stream"] = json!(true);
        let (status, _resp, raw) = v1_post(app.clone(), "sk-redir-agg", path, stream_body).await;
        assert_eq!(status, StatusCode::OK, "{ingress} stream via redirect alias: {status:?}\nraw={raw}");
        assert_ingress_stream(ingress, &raw);
    }
}
