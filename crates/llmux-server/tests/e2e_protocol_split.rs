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

use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
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
        .route("/chat/completions", axum::routing::post(|| async {
            axum::Json(chat_completion_body("A-chat"))
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

    // Step 2: three aliases — default / forced-chat / forced-responses.
    for (alias, upstream_api) in [
        ("of-default", "default"),
        ("of-chat", "chat"),
        ("of-resp", "responses"),
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

    for alias in ["of-default", "of-chat", "of-resp"] {
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
