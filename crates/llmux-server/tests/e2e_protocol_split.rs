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
