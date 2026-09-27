//! 上游全部因 429 耗尽时的返回码契约。
//!
//! 背景：瞬时 429（"temporarily unavailable" / overloaded）按 `is_quota_exhausted`
//! 的判断**不**进冷却，于是同一个账户会被反复打、反复 429。之前耗尽后一律回
//! 502 —— 对调用方是「网关坏了」，它会立刻重试，正好再吃一次 429，把瞬时限流
//! 放大成 14% 的失败率（09-25 实测 696 次 502，主因就是 poolside 的瞬时 429）。
//!
//! 正确行为：耗尽原因是 429 时回 429 + `Retry-After`，让调用方退避。
//! 非 429 耗尽（502/504 等真网关故障）仍回 502。

use axum::response::IntoResponse;
use axum::{body::{to_bytes, Body}, http::{header, Method, Request, StatusCode}};
use serde_json::{json, Value};

/// 起一个恒定回给定状态码的上游。
async fn spawn_status_upstream(status: u16, message: &str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let message = message.to_string();
    let message_for_messages = message.clone();
    let router = axum::Router::new()
        .route(
            "/chat/completions",
            axum::routing::post(move || {
                let message = message.clone();
                async move {
                    let payload = json!({"error": {"message": message}}).to_string();
                    axum::response::Response::builder()
                        .status(StatusCode::from_u16(status).unwrap())
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(payload))
                        .unwrap()
                        .into_response()
                }
            }),
        )
        .route(
            "/v1/messages",
            axum::routing::post(move || {
                let message = message_for_messages.clone();
                async move {
                    let payload = json!({"error": {"message": message}}).to_string();
                    axum::response::Response::builder()
                        .status(StatusCode::from_u16(status).unwrap())
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(payload))
                        .unwrap()
                        .into_response()
                }
            }),
        );
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}")
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
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn login(app: axum::Router) -> String {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"username": "admin", "password": "admin"}).to_string()))
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

/// 建一个「单账户 + 单别名」的环境，上游恒定回 `status`。
async fn setup(status: u16, message: &str) -> (axum::Router, String) {
    let upstream = spawn_status_upstream(status, message).await;
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());

    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind("t429")
        .bind("sk-test")
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
            "alias": "acc429",
            "provider_id": "openai",
            "api_key": "sk-mock",
            "base_url": upstream,
            "chat_endpoint": upstream,
            "messages_endpoint": upstream,
            "default_protocol": "chat",
            "skip_validation": true,
        }),
    )
    .await;
    assert_eq!(create_status, StatusCode::OK, "{create_body:?}");
    let acct_id = create_body["id"].as_i64().expect("account id");

    let (st, body) = api_post(
        app.clone(),
        &cookie,
        "/api/models/aliases",
        json!({
            "alias": "a429",
            "target_model": "gpt-4o",
            "account_ids": [acct_id],
            "upstream_api": "default",
            "provider_id": "openai",
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");

    (app, cookie)
}

async fn v1_post(app: axum::Router, path: &str, body: Value) -> (StatusCode, Value, String, Option<String>) {
    let req = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, "Bearer sk-test")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = llmux_server::test_request(app, req).await;
    let status = resp.status();
    let retry_after = resp
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let raw = String::from_utf8_lossy(&bytes).to_string();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value, raw, retry_after)
}

#[tokio::test]
async fn transient_429_exhaustion_returns_429_with_retry_after() {
    // 瞬时 429：文案不含 quota/额度 关键字 → 不进冷却 → 靠 last_status 判 429。
    let (app, _cookie) = setup(429, "Upstream model provider is temporarily unavailable.").await;

    for path in ["/v1/chat/completions", "/v1/messages"] {
        let (st, _v, raw, retry_after) = v1_post(
            app.clone(),
            path,
            json!({"model": "a429", "messages": [{"role": "user", "content": "hi"}]}),
        )
        .await;

        assert_eq!(
            st,
            StatusCode::TOO_MANY_REQUESTS,
            "{path}: 瞬时 429 耗尽应回 429（调用方据此退避），实际 {st:?}\nraw={raw}"
        );
        assert!(
            retry_after.is_some(),
            "{path}: 429 必须带 Retry-After，否则调用方不知道该等多久"
        );
    }
}

#[tokio::test]
async fn quota_429_exhaustion_returns_429_with_retry_after() {
    // 配额类 429：文案含 quota → 进冷却 → 旧分支（cooling down）已覆盖，
    // 这里确认新逻辑没有把它改回 502。
    let (app, _cookie) = setup(429, "You exceeded your quota for this model.").await;

    let (st, _v, raw, retry_after) = v1_post(
        app.clone(),
        "/v1/chat/completions",
        json!({"model": "a429", "messages": [{"role": "user", "content": "hi"}]}),
    )
    .await;

    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "raw={raw}");
    assert!(retry_after.is_some(), "配额 429 也要带 Retry-After");
}

#[tokio::test]
async fn non_429_exhaustion_still_returns_502() {
    // 真网关故障不该被误报成限流 —— 502 语义不同，别一起改掉。
    let (app, _cookie) = setup(502, "bad gateway").await;

    for path in ["/v1/chat/completions", "/v1/messages"] {
        let (st, _v, raw, retry_after) = v1_post(
            app.clone(),
            path,
            json!({"model": "a429", "messages": [{"role": "user", "content": "hi"}]}),
        )
        .await;

        assert_eq!(
            st,
            StatusCode::BAD_GATEWAY,
            "{path}: 非 429 耗尽仍应回 502，实际 {st:?}\nraw={raw}"
        );
        assert!(retry_after.is_none(), "{path}: 502 不该带 Retry-After");
    }
}
