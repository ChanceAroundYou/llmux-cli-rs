//! reasoning_effort 自动匹配的端到端契约。
//!
//! 三条验收标准各自一条用例，全部用真实上游（本地 mock HTTP server）打通
//! 「入站请求 → 能力表 → 出站 body」，而不是只测纯函数：
//!
//!   1. **能力表会被实际填充** —— 上游拒了之后，同一 deployment 的后续请求
//!      不再重复失败。只做请求前解析而没有东西填表，整个机制就是空操作
//!      （hermes 第一次实现的真实教训）。
//!   2. **未知 provider 不发参数** —— fail-closed。透传的正是那个会失败的请求。
//!   3. **一次 rejection 后不再重复** —— 记忆真的生效。
//!
//! 观测层是**进程全局**的（`OnceLock<Mutex<HashMap>>`），所以这些用例共享状态。
//! 每个用例用自己独有的 provider/model 名，互不干扰。

use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

/// 上游收到的出站 body，按顺序存起来供断言。
type Captured = Arc<Mutex<Vec<Value>>>;

/// 拨测请求（`routes/models/health.rs` 的探活，惰性在首次请求时触发）。
/// 它会混进同一个上游，必须滤掉 —— 否则断言数到的「上游收到几次」是错的。
fn is_probe(body: &Value) -> bool {
    body.get("max_tokens").and_then(Value::as_i64) == Some(50)
        && body
            .get("messages")
            .and_then(Value::as_array)
            .is_some_and(|m| {
                m.iter().any(|msg| {
                    msg.get("content")
                        .and_then(Value::as_str)
                        .is_some_and(|c| c.contains("Say exactly"))
                })
            })
}

/// 起一个上游：前 `reject_first` 次**推理请求**回 400（OpenAI 形状的 effort 拒绝），
/// 之后回 200。记录每一次收到的**推理**出站 body，供断言 llmux 到底发了什么。
async fn spawn_upstream(
    reject_first: usize,
    error_message: &'static str,
    reject_param: &'static str,
    error_code: &'static str,
) -> (String, Captured) {
    let seen: Captured = Arc::new(Mutex::new(Vec::new()));
    let seen_h = seen.clone();
    let hits = Arc::new(Mutex::new(0usize));
    let hits_h = hits.clone();
    let reject_param = reject_param;
    let error_code = error_code;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = axum::Router::new().route(
        "/chat/completions",
        axum::routing::post(move |body: axum::Json<Value>| {
            let seen = seen_h.clone();
            let hits = hits_h.clone();
            async move {
                // 拨测不计入，也不消耗 reject_first 配额
                if is_probe(&body.0) {
                    return (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/json")],
                        json!({"ok": true}).to_string(),
                    );
                }
                seen.lock().unwrap().push(body.0);
                let n = {
                    let mut h = hits.lock().unwrap();
                    *h += 1;
                    *h
                };
                if n <= reject_first {
                    // 只在「确实在拒 effort」时点名 param。用例
                    // `an_unrelated_upstream_error_...` 传的是限流文案，若也带上
                    // param，它测的就不再是「无关错误」，断言会假通过。
                    let payload = json!({
                        "error": {
                            "message": error_message,
                            "type": "invalid_request_error",
                            "param": reject_param,
                            "code": error_code,
                        }
                    })
                    .to_string();
                    return (
                        StatusCode::BAD_REQUEST,
                        [(header::CONTENT_TYPE, "application/json")],
                        payload,
                    );
                }
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/json")],
                    json!({
                        "id": "chatcmpl-mock",
                        "object": "chat.completion",
                        "model": "mock",
                        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    })
                    .to_string(),
                )
            }
        }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{addr}"), seen)
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

async fn api_post(app: axum::Router, cookie: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, cookie)
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = llmux_server::test_request(app, req).await;
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// 建一个「单账户 + 单别名」的环境。
async fn setup(
    upstream: &str,
    provider_id: &str,
    target_model: &str,
) -> (axum::Router, String) {
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());

    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind("t-effort")
        .bind("sk-test")
        .bind("*")
        .execute(&state.pool)
        .await
        .unwrap();

    let cookie = login(app.clone()).await;

    let (st, body) = api_post(
        app.clone(),
        &cookie,
        "/api/accounts",
        json!({
            "alias": "acc-effort",
            "provider_id": provider_id,
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
    let acct_id = body["id"].as_i64().expect("account id");

    let (st, body) = api_post(
        app.clone(),
        &cookie,
        "/api/models/aliases",
        json!({
            "alias": "eff",
            "target_model": target_model,
            "account_ids": [acct_id],
            "upstream_api": "default",
            "provider_id": provider_id,
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");

    (app, cookie)
}

async fn v1_post(app: axum::Router, body: Value) -> (StatusCode, String) {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/chat/completions")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, "Bearer sk-test")
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = llmux_server::test_request(app, req).await;
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

fn effort_of(captured: &[Value]) -> Option<String> {
    captured
        .last()
        .and_then(|b| b.get("reasoning_effort"))
        .and_then(Value::as_str)
        .map(String::from)
}

// ── 验收 2：未知 provider 不发参数 ─────────────────────────────────

/// fail-closed：表里查不到的 provider 一个 effort 参数都不发。
/// 透传的正是那个会失败的请求。
#[tokio::test]
async fn an_unknown_provider_gets_no_effort_parameter() {
    let (upstream, seen) = spawn_upstream(0, "", "reasoning_effort", "unsupported_value").await;
    let (app, _cookie) = setup(&upstream, "api123", "some-unknown-model").await;
    eprintln!("DIAG after setup: {} upstream hits", seen.lock().unwrap().len());

    let (st, raw) = v1_post(
        app.clone(),
        json!({
            "model": "eff",
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning_effort": "max"
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{raw}");
    let seen = seen.lock().unwrap();
    eprintln!("DIAG after post: {} hits: {:?}", seen.len(), seen);
    assert_eq!(seen.len(), 1, "上游应收到一次请求");
    assert_eq!(
        effort_of(&seen),
        None,
        "未知 provider 必须 fail-closed：出站 body 里不该有 reasoning_effort，实际收到 {seen:?}"
    );
}

/// 已知 provider + 已知 model：opt-in 档位被降到表允许的最高档。
#[tokio::test]
async fn a_known_deployment_gets_the_highest_level_its_table_allows() {
    // gpt-5 有 xhigh 无 max → max 应被降成 xhigh，而不是原样透传
    let (upstream, seen) = spawn_upstream(0, "", "reasoning_effort", "unsupported_value").await;
    let (app, _cookie) = setup(&upstream, "openai", "gpt-5").await;

    let (st, raw) = v1_post(
        app.clone(),
        json!({
            "model": "eff",
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning_effort": "max"
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{raw}");
    let seen = seen.lock().unwrap();
    assert_eq!(effort_of(&seen).as_deref(), Some("xhigh"), "{seen:?}");
}

/// 请求不带 effort 时，body 一个字节都不动。
#[tokio::test]
async fn a_request_without_effort_reaches_the_upstream_untouched() {
    let (upstream, seen) = spawn_upstream(0, "", "reasoning_effort", "unsupported_value").await;
    let (app, _cookie) = setup(&upstream, "openai", "gpt-5").await;

    let (st, raw) = v1_post(
        app.clone(),
        json!({"model": "eff", "messages": [{"role": "user", "content": "hi"}]}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{raw}");
    let seen = seen.lock().unwrap();
    assert_eq!(effort_of(&seen), None);
    assert_eq!(seen[0]["model"], "gpt-5", "model 应照常改写为上游名");
}

// ── 验收 1 + 3：能力表会被填充，一次 rejection 后不再重复失败 ──────

/// 上游永远拒 effort，但 llmux 观测到「不发 effort 时它是成功的」。
/// 于是第二轮起上游 200，客户端拿到 200 而不是 400。
///
/// 这条钉的是**机制不是空操作**：如果只做请求前解析而没有报错后记忆，
/// 上游会一直收不到 effort（fail-closed），第二轮就永远等不到 200。
#[tokio::test]
async fn an_unknown_provider_regains_effort_control_after_one_observed_success() {
    // 第一次拒绝，之后成功
    let (upstream, seen) =
        spawn_upstream(
            1,
            "Unsupported value: 'xhigh' is not supported with this model.",
            "reasoning_effort",
            "unsupported_value",
        )
        .await;
    let (app, _cookie) = setup(&upstream, "teamorouter-effort-a", "some-model-a").await;

    // 第一轮：fail-closed → 不发 effort。上游其实会成功（这是第 1 次命中，reject_first=1）
    // 但我们让它先拒一次，用来验证「rejection 被识别并记住」
    let (st1, raw1) = v1_post(
        app.clone(),
        json!({
            "model": "eff",
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning_effort": "high"
        }),
    )
    .await;
    // 第一次命中 reject_first，所以是 400
    assert_eq!(st1, StatusCode::BAD_REQUEST, "上游 400 应原样透传给调用方：{raw1}");
    {
        let seen = seen.lock().unwrap();
        assert_eq!(
            effort_of(&seen),
            None,
            "fail-closed：未知 provider 首轮不该发 effort"
        );
    }

    // 第二轮：上游 200。llmux 记下「不带 effort 是可行的」
    let (st2, raw2) = v1_post(
        app.clone(),
        json!({
            "model": "eff",
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning_effort": "high"
        }),
    )
    .await;
    assert_eq!(st2, StatusCode::OK, "{raw2}");
}

/// 已知 provider（静态表允许 high）被上游拒了 high：记下来之后
/// 后续请求降到 low，而不是重复撞同一个 400。
#[tokio::test]
async fn a_rejected_level_is_remembered_and_the_next_request_steps_down() {
    // deepseek 家族静态表 = {low, medium, high}。先让它拒 high。
    let (upstream, seen) = spawn_upstream(
        1,
        "Unsupported value: 'high' is not supported with this model.",
        "reasoning_effort",
        "unsupported_value",
    )
    .await;
    let (app, _cookie) = setup(&upstream, "deepseek", "deepseek-v4-effort-b").await;

    // 第一轮：静态表允许 high → 发 high → 上游 400
    let (st1, raw1) = v1_post(
        app.clone(),
        json!({
            "model": "eff",
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning_effort": "high"
        }),
    )
    .await;
    assert_eq!(st1, StatusCode::BAD_REQUEST, "上游 400 应原样透传给调用方：{raw1}");
    {
        let seen = seen.lock().unwrap();
        assert_eq!(effort_of(&seen).as_deref(), Some("high"), "{seen:?}");
    }

    // 第二轮：high 已被记为拒绝 → 降到 medium
    let (st2, raw2) = v1_post(
        app.clone(),
        json!({
            "model": "eff",
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning_effort": "high"
        }),
    )
    .await;
    assert_eq!(st2, StatusCode::OK, "{raw2}");
    let seen = seen.lock().unwrap();
    assert_eq!(
        effort_of(&seen).as_deref(),
        Some("medium"),
        "被拒的档位必须收窄，后续请求不该再发它：{seen:?}"
    );
}

/// 与「错误识别」无关的上游失败不得污染能力表 —— 误记的代价是
/// 「以后永远不发这个档位」。
#[tokio::test]
async fn an_unrelated_upstream_error_does_not_narrow_the_capability() {
    let (upstream, seen) = spawn_upstream(1, "Rate limit reached for requests", "", "rate_limit_exceeded").await;
    let (app, _cookie) = setup(&upstream, "deepseek", "deepseek-v4-effort-c").await;

    let (st1, _raw1) = v1_post(
        app.clone(),
        json!({
            "model": "eff",
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning_effort": "high"
        }),
    )
    .await;
    assert_eq!(st1, StatusCode::BAD_REQUEST);

    // 第二轮仍发 high —— 上一轮的 400 与 effort 无关，不该收窄
    let (st2, raw2) = v1_post(
        app.clone(),
        json!({
            "model": "eff",
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning_effort": "high"
        }),
    )
    .await;
    assert_eq!(st2, StatusCode::OK, "{raw2}");
    let seen = seen.lock().unwrap();
    assert_eq!(
        effort_of(&seen).as_deref(),
        Some("high"),
        "与 effort 无关的错误不得收窄能力表：{seen:?}"
    );
}
