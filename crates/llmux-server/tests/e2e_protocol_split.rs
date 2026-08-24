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

async fn spawn_mock_upstreams() -> (String, String) {
    let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr_a = listener_a.local_addr().unwrap();
    let router_a = axum::Router::new()
        .route("/chat/completions", axum::routing::post(|axum::Json(body): axum::Json<Value>| async move {
            if body["stream"].as_bool() == Some(true) {
                return axum::response::Response::builder()
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from(concat!(
                        "data: {\"id\":\"mock_cmpl_1\",\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"A-chat-stream\"},\"finish_reason\":null}]}\n\n",
                        "data: {\"id\":\"mock_cmpl_1\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":22}}\n\n",
                        "data: [DONE]\n\n",
                    )))
                    .unwrap();
            }
            axum::Json(chat_completion_body("A-chat")).into_response()
        }))
        .route("/responses", axum::routing::post(|| async {
            axum::Json(responses_body("A-resp"))
        }))
        .route("/v1/messages", axum::routing::post(|| async {
            axum::Json(anthropic_message_body("A-msg"))
        }));
    tokio::spawn(async move {
        let _ = axum::serve(listener_a, router_a).await;
    });

    // B serves ONLY /responses — unmatched paths get axum's default 404.
    let listener_b = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr_b = listener_b.local_addr().unwrap();
    let router_b = axum::Router::new()
        .route("/responses", axum::routing::post(|| async {
            axum::Json(responses_body("B-resp"))
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
            json!({"username": "xiaokubao", "password": "Xkb111717!"}).to_string(),
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
