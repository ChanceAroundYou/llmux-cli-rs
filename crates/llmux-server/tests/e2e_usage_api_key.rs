//! 端到端验证：`usage_logs.api_key_id` 真的被写进去了。
//!
//! 为什么单独一个文件而不是并进 server_contract：
//! server_contract 里的测试都是直接往库里塞行、然后查接口，**从不经过 v1 鉴权与
//! 落库那条链**。而本特性要钉的恰恰是那条链：task-local 有没有挂上、流式有没有
//! 在 spawn 前捕获、INSERT 有没有多绑一个参数 —— 这三处任何一处漏了，
//! 接口层的筛选测试仍然全绿（因为它们自己塞的行带着正确的 api_key_id）。
//! 接线全删了也测不出来的那种 bug，只有真打一次请求才暴露。

use axum::response::IntoResponse;
use axum::{body::{to_bytes, Body}, http::{header, Method, Request, StatusCode}};
use serde_json::{json, Value};

/// 恒回一次成功的 chat completion，带 usage（没有 usage 就不会算出 token，测不出东西）。
async fn spawn_ok_upstream() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = axum::Router::new().route(
        "/chat/completions",
        axum::routing::post(|| async {
            axum::response::Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "id": "cmpl_1",
                        "object": "chat.completion",
                        "choices": [{"index": 0, "message": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}],
                        "usage": {"prompt_tokens": 11, "completion_tokens": 22, "total_tokens": 33}
                    })
                    .to_string(),
                ))
                .unwrap()
                .into_response()
        }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
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
    panic!("no session cookie from login");
}

/// 打一次非流式 /v1/chat/completions，返回 HTTP 状态。
async fn v1_chat(app: axum::Router, api_key: &str) -> StatusCode {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/chat/completions")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {api_key}"))
        .body(Body::from(
            json!({"model": "keyid-model", "messages": [{"role": "user", "content": "hi"}]})
                .to_string(),
        ))
        .unwrap();
    llmux_server::test_request(app, req).await.status()
}

/// 落库是 `tokio::spawn` 的异步写，请求返回后还得等一小会儿。
/// 用轮询而不是固定 sleep —— 固定 sleep 在慢 CI 上会随机红。
///
/// 等的是 **is_test = 0 的行数**，不是总行数：建账号/建别名那几步本身也会落一行
/// is_test = 1 的探活记录，先到的是它。等总行数会在真实那一行还没写进去时就返回，
/// 后面按 is_test = 0 查就查空 —— 这个测试第一版就是这么随机红的。
async fn wait_for_usage_row(state: &llmux_server::app::AppState) -> i64 {
    for _ in 0..100 {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_logs WHERE is_test = 0")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        if n > 0 {
            return n;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("真实请求的 usage_logs 行没在 5s 内落库");
}

#[tokio::test]
async fn a_real_request_records_the_gateway_key_id_on_its_usage_row() {
    let upstream = spawn_ok_upstream().await;
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    let cookie = login(app.clone()).await;

    // 两把密钥，用其中一把发请求 —— 行上的 api_key_id 必须是**这把**的 id。
    let used_key_id: i64 = sqlx::query_scalar(
        "INSERT INTO api_keys (name, key, allowed_models) VALUES ('used', 'sk-used', '*') RETURNING id",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES ('other', 'sk-other', '*')")
        .execute(&state.pool)
        .await
        .unwrap();

    let (st, body) = {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/accounts")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, &cookie)
            .body(Body::from(
                json!({
                    "alias": "keyid-acct",
                    "provider_id": "openai",
                    "api_key": "sk-mock",
                    "base_url": upstream,
                    "chat_endpoint": upstream,
                    "messages_endpoint": upstream,
                    "default_protocol": "chat",
                    "skip_validation": true,
                })
                .to_string(),
            ))
            .unwrap();
        let resp = llmux_server::test_request(app.clone(), req).await;
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null))
    };
    assert_eq!(st, StatusCode::OK, "{body:?}");
    let acct_id = body["id"].as_i64().expect("account id");

    let (st, body) = {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/models/aliases")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, &cookie)
            .body(Body::from(
                json!({
                    "alias": "keyid-model",
                    "target_model": "gpt-4o",
                    "account_ids": [acct_id],
                    "provider_id": "openai",
                })
                .to_string(),
            ))
            .unwrap();
        let resp = llmux_server::test_request(app.clone(), req).await;
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null))
    };
    assert_eq!(st, StatusCode::OK, "{body:?}");

    assert_eq!(v1_chat(app.clone(), "sk-used").await, StatusCode::OK);
    wait_for_usage_row(&state).await;

    let real_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_logs WHERE is_test = 0")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        real_rows, 1,
        "一次请求只该落一行真实用量 —— 多行说明日志路径被调了两次"
    );

    let (key_on_row, model): (Option<i64>, Option<String>) = sqlx::query_as(
        "SELECT api_key_id, model FROM usage_logs WHERE is_test = 0 ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&state.pool)
    .await
    .expect("usage row");

    assert_eq!(
        key_on_row,
        Some(used_key_id),
        "落库的 usage_logs 行必须带着发起请求那把密钥的 id —— 这正是整个特性的地基"
    );
    assert_eq!(model.as_deref(), Some("gpt-4o"));
}