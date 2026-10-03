//! 全部候选耗尽时，**报上去的错误必须是上游真回过的那条**，不是最后一个
//! 被路由层跳过的候选。
//!
//! 背景（2026-10-02 生产实测）：聚合别名 `of` 有 3 个候选，在 20:31–20:36
//! 连续 12 条请求全失败，`usage_logs` 里 `error_message` 写的是
//! `Candidate 2 account 57 not found or inactive`。这句话完全误导 —— 真相在
//! NAS 日志里：候选 0（账户 55）先回了上游 **400 `invalid request error`**
//! （body 1.1 MB，被上游拒），llmux 按「非可重试 → 试下一个候选」继续走，
//! 撞上刚被停用的账户 57 才彻底失败。而账户 55 当时**完全健康**（同一 10 分钟
//! 桶里 10 条成功）—— 它只是在这类超大请求上失败。
//!
//! 病因：`last_error` 被每个后续候选无条件覆盖。路由层的跳过（冷却中 /
//! 账户不可用 / 协议不支持）诊断价值远低于上游真的回过的错误，覆盖掉之后
//! 调用方和 DB 里就只剩那句没用的「account not found」。
//!
//! 修法：跳过走 `helpers::note_skip_reason`（只在空槽时写），上游/网络错误
//! 仍无条件覆盖。见 `helpers::note_skip_reason` 的注释。
//!
//! 这些断言盯的是**对外返回的错误正文**（调用方唯一能看到的东西），不是内部
//! 变量 —— 测 helper 的话，把整个接线删掉测试照样会绿（[[llmux-mutation-testing]]）。

use axum::response::IntoResponse;
use axum::{body::{to_bytes, Body}, http::{header, Method, Request, StatusCode}};
use serde_json::{json, Value};

/// 起一个恒定回给定状态码 + 文案的上游。
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

async fn create_account(app: &axum::Router, cookie: &str, alias: &str, upstream: &str) -> i64 {
    let (st, body) = api_post(
        app.clone(),
        cookie,
        "/api/accounts",
        json!({
            "alias": alias,
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
    assert_eq!(st, StatusCode::OK, "{body:?}");
    body["id"].as_i64().expect("account id")
}

/// 关掉一个账户（走真实管理 API，和生产停用 teamorouter 的路径一致）。
async fn deactivate_account(app: &axum::Router, cookie: &str, id: i64) {
    let req = Request::builder()
        .method(Method::PUT)
        .uri(format!("/api/accounts/{id}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, cookie)
        .body(Body::from(json!({"is_active": 0}).to_string()))
        .unwrap();
    let resp = llmux_server::test_request(app.clone(), req).await;
    assert_eq!(resp.status(), StatusCode::OK, "停用账户 {id} 应成功");
}

async fn set_aggregate(app: &axum::Router, cookie: &str, alias: &str, candidates: Value) {
    let (st, body) = api_post(
        app.clone(),
        cookie,
        "/api/aggregate-aliases",
        json!({
            "alias": alias,
            "candidates": candidates,
            "interval_secs": 300,
            "upstream_api": "default",
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
}

async fn v1_post(app: axum::Router, path: &str, model: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, "Bearer sk-test")
        .body(Body::from(
            json!({"model": model, "messages": [{"role": "user", "content": "hi"}]}).to_string(),
        ))
        .unwrap();
    let resp = llmux_server::test_request(app, req).await;
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

/// 建「候选 0 上游恒回 `status`，候选 1 账户已停用」的聚合环境 —— 即 2026-10-02
/// 的线上现场（候选 0 真的被打过并失败，候选 1 压根没被试过）。
async fn setup_upstream_fails_then_inactive_candidate(
    status: u16,
    message: &str,
) -> (axum::Router, String) {
    let upstream = spawn_status_upstream(status, message).await;
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());

    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind("terr")
        .bind("sk-test")
        .bind("*")
        .execute(&state.pool)
        .await
        .unwrap();

    let cookie = login(app.clone()).await;

    let live = create_account(&app, &cookie, "live", &upstream).await;
    let dead = create_account(&app, &cookie, "dead", &upstream).await;

    // 顺序要紧：建别名时两个账户都得是 active（聚合接口会校验），停用必须发生在
    // 之后 —— 这正是线上顺序（`of` 建好之后我才把 teamorouter 关掉的）。
    set_aggregate(
        &app,
        &cookie,
        "aerr",
        json!([
            {"account_id": live, "model": "gpt-4o"},
            {"account_id": dead, "model": "gpt-4o"},
        ]),
    )
    .await;
    deactivate_account(&app, &cookie, dead).await;

    (app, cookie)
}

/// 回归主用例：候选 0 拿到上游 400，候选 1 已停用 → 对外必须报上游 400 的正文。
#[tokio::test]
async fn upstream_error_is_not_masked_by_a_later_inactive_candidate() {
    let (app, _cookie) =
        setup_upstream_fails_then_inactive_candidate(400, "invalid request error").await;

    for path in ["/v1/chat/completions", "/v1/messages"] {
        let (st, raw) = v1_post(app.clone(), path, "aerr").await;

        assert_eq!(st, StatusCode::BAD_GATEWAY, "{path}: 耗尽仍回 502\nraw={raw}");
        assert!(
            raw.contains("invalid request error"),
            "{path}: 上游真的回过的错误必须出现在响应里 —— 这才是调用方能诊断的东西。\
             实际拿到：{raw}"
        );
        assert!(
            !raw.contains("not found or inactive"),
            "{path}: 路由层的跳过（账户已停用）不该盖掉上游错误，它只是没被试过。\
             实际拿到：{raw}"
        );
    }
}

/// 反向钉住：全部候选都是路由层跳过时，跳过原因**必须**报出来。
///
/// 没有这条，`note_skip_reason` 退化成「什么都不记」也测不出来 —— 而那会让
/// 调用方只看到一句 "All aggregate candidates exhausted"，比现在更糟。
#[tokio::test]
async fn a_pure_skip_exhaustion_still_reports_the_skip_reason() {
    let upstream = spawn_status_upstream(200, "unused").await;
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());

    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind("tskip")
        .bind("sk-test")
        .bind("*")
        .execute(&state.pool)
        .await
        .unwrap();

    let cookie = login(app.clone()).await;
    let only = create_account(&app, &cookie, "only", &upstream).await;

    set_aggregate(
        &app,
        &cookie,
        "askip",
        json!([{"account_id": only, "model": "gpt-4o"}]),
    )
    .await;
    deactivate_account(&app, &cookie, only).await;

    for path in ["/v1/chat/completions", "/v1/messages"] {
        let (st, raw) = v1_post(app.clone(), path, "askip").await;

        assert_eq!(st, StatusCode::BAD_GATEWAY, "{path}: 耗尽仍回 502\nraw={raw}");
        assert!(
            raw.contains("not found or inactive"),
            "{path}: 没有任何上游错误时，跳过原因就是唯一的线索，必须报出来。实际拿到：{raw}"
        );
    }
}