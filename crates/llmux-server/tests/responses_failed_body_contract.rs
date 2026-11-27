//! HTTP 200 也可能是**失败的回合**。
//!
//! codex 私有后端对容量错误就是这么报的：
//! `200 {"status":"failed","error":{"code":"server_is_overloaded",...}}`。
//! 修复前 llmux 只看状态行：
//!   * Anthropic / Chat 入口把它转成 `content: []` + `end_turn` 的「成功」空回复；
//!   * `usage_logs` 记 `success=1`；
//!   * 聚合路由还会给这个候选记一次成功。
//!
//! 现在三个非流式 Responses 上游分支都必须把这种 body 当失败：试下一个账户，
//! 耗尽后把上游原话报成 502。流式入口由各自的 converter / 记账处理
//! （`response.failed` 必须落成 `upstream failed: <原因>`，不是 `truncated`）。

use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use axum::response::IntoResponse;
use serde_json::{json, Value};

const FAILURE_CODE: &str = "server_is_overloaded";
const FAILURE_MESSAGE: &str = "Our servers are currently overloaded. Please try again later.";

/// 上游：任何请求都回 HTTP 200 + 失败的 Responses body。
async fn spawn_failed_body_upstream() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = axum::Router::new()
        .route(
            "/responses",
            axum::routing::post(|| async move {
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "id": "resp_overloaded",
                        "object": "response",
                        "status": "failed",
                        "error": {"code": FAILURE_CODE, "message": FAILURE_MESSAGE},
                    })),
                )
                    .into_response()
            }),
        )
        .fallback(|| async {
            (
                StatusCode::OK,
                axum::Json(json!({
                    "id": "resp_overloaded",
                    "object": "response",
                    "status": "failed",
                    "error": {"code": FAILURE_CODE, "message": FAILURE_MESSAGE},
                })),
            )
                .into_response()
        });
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{addr}")
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
    panic!("login did not set llmux_session");
}

async fn call(app: axum::Router, cookie: &str, method: Method, path: &str, body: Value) -> (StatusCode, String) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, "Bearer sk-failed-body")
        .header(header::COOKIE, cookie)
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = llmux_server::test_request(app, req).await;
    let status = resp.status();
    let raw = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&raw).to_string())
}

/// 建账户 + 一条普通别名，返回 (app, cookie, account_id)。
async fn setup_ordinary(upstream: &str) -> (axum::Router, String, i64) {
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind("failed-body")
        .bind("sk-failed-body")
        .bind("*")
        .execute(&state.pool)
        .await
        .unwrap();
    let cookie = login(app.clone()).await;

    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/accounts")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, &cookie)
        .body(Body::from(
            json!({
                "alias": "codex-upstream",
                "provider_id": "openai",
                "api_key": "sk-upstream",
                "base_url": upstream,
                "responses_endpoint": upstream,
                "default_protocol": "responses",
                "skip_validation": true,
            })
            .to_string(),
        ))
        .unwrap();
    let resp = llmux_server::test_request(app.clone(), req).await;
    let status = resp.status();
    let raw = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert!(status.is_success(), "create account failed: {status}\n{}", String::from_utf8_lossy(&raw));
    let created: Value = serde_json::from_slice(&raw).unwrap();
    let id = created["id"].as_i64().expect("account id");

    sqlx::query(
        "INSERT INTO model_aliases (alias, target_model, provider_id, account_ids, upstream_api) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind("astra")
    .bind("gpt-6.2-astra")
    .bind("")
    .bind(format!("[{id}]"))
    .bind("responses")
    .execute(&state.pool)
    .await
    .unwrap();

    (app, cookie, id)
}

async fn setup_aggregate(upstream: &str) -> (axum::Router, String) {
    let (app, cookie, id) = setup_ordinary(upstream).await;
    // 直接走管理 API 建聚合别名（与生产同一条校验路径）。
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/aggregate-aliases")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, &cookie)
        .body(Body::from(
            json!({
                "alias": "astra-agg",
                "upstream_api": "responses",
                "interval_secs": 300,
                "candidates": [{"account_id": id, "model": "gpt-6.2-astra"}],
            })
            .to_string(),
        ))
        .unwrap();
    let resp = llmux_server::test_request(app.clone(), req).await;
    let status = resp.status();
    let raw = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert!(
        status.is_success(),
        "create aggregate alias failed: {status}\n{}",
        String::from_utf8_lossy(&raw)
    );
    (app, cookie)
}

fn assert_failure_passthrough(label: &str, status: StatusCode, raw: &str) {
    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "{label}: 上游 HTTP 200 + 失败 body 必须变成 502，不能是空成功\nraw={raw}"
    );
    assert!(
        raw.contains(FAILURE_CODE) || raw.contains(FAILURE_MESSAGE),
        "{label}: 必须把上游真实原因报出来\nraw={raw}"
    );
}

/// 普通别名（单账户 dispatcher / `Back::ChatFromResponses` + `MessagesFromResponses`）。
#[tokio::test]
async fn failed_body_from_responses_upstream_is_an_error_not_an_empty_success() {
    let upstream = spawn_failed_body_upstream().await;
    let (app, cookie, _) = setup_ordinary(&upstream).await;

    for (path, body) in [
        (
            "/v1/chat/completions",
            json!({"model": "astra", "messages": [{"role": "user", "content": "hi"}], "stream": false}),
        ),
        (
            "/v1/messages",
            json!({"model": "astra", "max_tokens": 16, "messages": [{"role": "user", "content": "hi"}]}),
        ),
        (
            "/v1/responses",
            json!({"model": "astra", "input": "hi", "stream": false}),
        ),
    ] {
        let (status, raw) = call(app.clone(), &cookie, Method::POST, path, body).await;
        assert_failure_passthrough(path, status, &raw);
    }
}

/// 聚合别名（aggregate dispatcher）：必须记候选失败并继续，耗尽后同样报上游原话。
#[tokio::test]
async fn aggregate_candidate_with_failed_body_is_marked_failed() {
    let upstream = spawn_failed_body_upstream().await;
    let (app, cookie) = setup_aggregate(&upstream).await;

    for (path, body) in [
        (
            "/v1/chat/completions",
            json!({"model": "astra-agg", "messages": [{"role": "user", "content": "hi"}], "stream": false}),
        ),
        (
            "/v1/messages",
            json!({"model": "astra-agg", "max_tokens": 16, "messages": [{"role": "user", "content": "hi"}]}),
        ),
    ] {
        let (status, raw) = call(app.clone(), &cookie, Method::POST, path, body).await;
        assert_failure_passthrough(path, status, &raw);
    }
}