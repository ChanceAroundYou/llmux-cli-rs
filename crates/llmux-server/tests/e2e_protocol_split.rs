//! End-to-end verification for the upstream/downstream protocol split (Tasks 1-8).
//!
//! This is the Task-9 gray-release verification: it exercises the three ingress
//! protocols (chat / responses / messages) against three alias modes
//! (default / forced-chat / forced-responses) using an in-process mock upstream,
//! then verifies the bulk-fallback-on-endpoint-removal logic (Task 4).
//!
//! No real upstream is contacted. A mock that returns OpenAI-shaped JSON
//! (with non-zero `usage`) on every path stands in for the provider.
//!
//! Run with: `bash scripts/e2e_protocol_split.sh`  (which invokes this test).

use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use axum::response::IntoResponse;
use serde_json::{json, Value};

// --- mock upstream -----------------------------------------------------------

/// Spins up a mock provider that answers with OpenAI-shaped JSON (non-zero
/// usage) on every path. Returns its base URL.
async fn spawn_mock_upstream() -> String {
    async fn handler() -> impl IntoResponse {
        axum::Json(json!({
            "id": "mock_cmpl_1",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hello from mock"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 12, "completion_tokens": 34, "total_tokens": 46}
        }))
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = axum::Router::new()
        .route("/chat/completions", axum::routing::post(handler))
        .route("/responses", axum::routing::post(handler))
        .route("/v1/messages", axum::routing::post(handler))
        .fallback(axum::routing::any(handler));
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}")
}

// --- request helpers ---------------------------------------------------------

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

/// True if either OpenAI (`prompt_tokens`) or Anthropic (`input_tokens`) usage
/// is non-zero — both shapes appear across the ingress protocols.
fn usage_nonzero(v: &Value) -> bool {
    let pt = v["usage"]["prompt_tokens"].as_i64().unwrap_or(0);
    let it = v["usage"]["input_tokens"].as_i64().unwrap_or(0);
    pt > 0 || it > 0
}

// --- the test ----------------------------------------------------------------

#[tokio::test]
async fn e2e_protocol_split_passthrough_and_fallback() {
    let upstream = spawn_mock_upstream().await;

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

    // Step 1: account with 3 identical endpoints + default chat, anthropic passthrough.
    let (create_status, create_body) = api_post(
        app.clone(),
        &cookie,
        "/api/accounts",
        json!({
            "alias": "mockacc",
            "provider_id": "openai",
            "api_key": "sk-mock",
            "base_url": upstream,
            "chat_endpoint": upstream,
            "responses_endpoint": upstream,
            "messages_endpoint": upstream,
            "anthropic_base_url": upstream,
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
            }),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "alias {alias}: {body:?}");
    }

    // Step 3: hit each alias via all three ingresses; assert 200 + usage non-zero.
    let ingresses = [
        ("/v1/chat/completions", "chat"),
        ("/v1/responses", "responses"),
        ("/v1/messages", "messages"),
    ];
    for alias in ["of-default", "of-chat", "of-resp"] {
        for (path, ingress) in ingresses {
            let body = match ingress {
                "chat" => json!({"model": alias, "messages": [{"role": "user", "content": "hi"}]}),
                "responses" => json!({"model": alias, "input": "hi"}),
                "messages" => json!({"model": alias, "messages": [{"role": "user", "content": "hi"}]}),
                _ => unreachable!(),
            };
            let (st, resp, raw) = v1_post(app.clone(), "sk-test", path, body).await;
            assert_eq!(
                st,
                StatusCode::OK,
                "{alias} via {ingress} ({path}): expected 200, got {st:?}\nraw={raw}\nresp={resp:?}"
            );
            assert!(
                usage_nonzero(&resp),
                "{alias} via {ingress} ({path}): usage must be non-zero, got {resp:?}"
            );
        }
    }

    // Step 4: clear responses_endpoint → bulk fallback must retarget of-resp to default.
    let (upd_status, upd_body) = api_put(
        app.clone(),
        &cookie,
        &format!("/api/accounts/{acct_id}"),
        json!({
            "alias": "mockacc",
            "provider_id": "openai",
            "api_key": "sk-mock",
            "responses_endpoint": null,
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

    // After fallback, of-resp routes to the chat default → still 200 + usage on all ingresses.
    for (path, ingress) in ingresses {
        let body = match ingress {
            "chat" => json!({"model": "of-resp", "messages": [{"role": "user", "content": "hi"}]}),
            "responses" => json!({"model": "of-resp", "input": "hi"}),
            "messages" => json!({"model": "of-resp", "messages": [{"role": "user", "content": "hi"}]}),
            _ => unreachable!(),
        };
        let (st, resp, raw) = v1_post(app.clone(), "sk-test", path, body).await;
        assert_eq!(
            st,
            StatusCode::OK,
            "post-fallback of-resp via {ingress} ({path}): expected 200, got {st:?}\nraw={raw}\nresp={resp:?}"
        );
        assert!(
            usage_nonzero(&resp),
            "post-fallback of-resp via {ingress} ({path}): usage must be non-zero, got {resp:?}"
        );
    }
}
