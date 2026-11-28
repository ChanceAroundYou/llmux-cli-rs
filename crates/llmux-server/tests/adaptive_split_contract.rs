//! 聚合别名性能自适应分流（Spill Valve）的合约测试。
//!
//! 钉住的四条行为：
//!   1. 开关与最大比例按别名保存、回读；默认关闭，比例夹到 0–90。
//!   2. 开关关闭 / 没有可信基线时，100% 走主候选（不改变现有派发语义）。
//!   3. 主候选相对自身基线恶化时，一部分请求先走下游候选；**不改变 active**（请求侧
//!      锚点没被试过，不能推进 3-confirm）。
//!   4. 基线恢复后自动回切到主候选。
//!   5. 性能样本只收流式成功请求，非流式 / 失败请求不污染基线。

use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
    response::IntoResponse,
};
use llmux_core::perf::{global, PerfRegistry, STALE_AFTER};
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::{Duration, Instant};

// 三个测试共用进程级 PerfRegistry，必须串行，否则一个 clear_perf 会把另一个的样本抹掉。
static PERF_TEST_LOCK: Mutex<()> = Mutex::new(());
fn perf_lock() -> std::sync::MutexGuard<'static, ()> {
    PERF_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn chat_body(marker: &str) -> Value {
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

/// 两个可区分的上游：`{base}/primary/chat/completions` 与 `/secondary/...`。
async fn spawn_mock() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = axum::Router::new()
        .route(
            "/primary/chat/completions",
            axum::routing::post(|axum::Json(_body): axum::Json<Value>| async move {
                axum::Json(chat_body("PRIMARY")).into_response()
            }),
        )
        .route(
            "/secondary/chat/completions",
            axum::routing::post(|axum::Json(_body): axum::Json<Value>| async move {
                axum::Json(chat_body("SECONDARY")).into_response()
            }),
        );
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}")
}

async fn login(app: &axum::Router) -> String {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"username": "admin", "password": "admin"}).to_string()))
        .unwrap();
    let resp = llmux_server::test_request(app.clone(), req).await;
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

async fn api(app: &axum::Router, cookie: &str, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    let req = builder
        .header(header::COOKIE, cookie)
        .body(match body {
            Some(v) => Body::from(v.to_string()),
            None => Body::empty(),
        })
        .unwrap();
    let resp = llmux_server::test_request(app.clone(), req).await;
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn api_post(app: &axum::Router, cookie: &str, uri: &str, body: Value) -> (StatusCode, Value) {
    api(app, cookie, Method::POST, uri, Some(body)).await
}

async fn add_key(state: &llmux_server::AppState, key: &str) {
    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind(format!("split-{key}"))
        .bind(key)
        .bind("*")
        .execute(&state.pool)
        .await
        .unwrap();
}

async fn create_account(app: &axum::Router, cookie: &str, alias: &str, endpoint: &str) -> i64 {
    let (st, body) = api_post(
        app,
        cookie,
        "/api/accounts",
        json!({
            "alias": alias,
            "provider_id": "openai",
            "api_key": "sk-mock",
            "base_url": endpoint,
            "chat_endpoint": endpoint,
            "default_protocol": "chat",
            "skip_validation": true,
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    body["id"].as_i64().expect("account id")
}

async fn save_aggregate(app: &axum::Router, cookie: &str, body: Value) -> Value {
    let (st, body) = api_post(app, cookie, "/api/aggregate-aliases", body).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    body
}

async fn aggregate_row(app: &axum::Router, cookie: &str, name: &str) -> Value {
    let (st, body) = api(app, cookie, Method::GET, "/api/aggregate-aliases", None).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    body.as_array()
        .unwrap()
        .iter()
        .find(|a| a["alias"] == name)
        .cloned()
        .unwrap_or_else(|| panic!("聚合别名 {name} 不在了：{body:?}"))
}

async fn chat(app: &axum::Router, key: &str, model: &str) -> String {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/v1/chat/completions")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {key}"))
        .body(Body::from(
            json!({"model": model, "max_tokens": 16, "messages": [{"role": "user", "content": "hi"}]}).to_string(),
        ))
        .unwrap();
    let resp = llmux_server::test_request(app.clone(), req).await;
    let st = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_eq!(st, StatusCode::OK, "{body:?}");
    body["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_else(|| panic!("没有回复内容：{body:?}"))
        .to_string()
}

/// 稳定的 500ms / 40tps 基线，再叠 40 秒 3000ms / 8tps 的突发 → severity 约 4。
/// 时间全部显式给定，测试不需要真的睡 100 秒。
fn seed_degraded(account_id: i64, model: &str) {
    let mut reg = PerfRegistry::new();
    let base = Instant::now() - Duration::from_secs(120);
    for i in 0..60 {
        reg.record(account_id, model, 500.0, Some(40.0), base + Duration::from_secs(i));
    }
    for i in 0..40 {
        reg.record(account_id, model, 3000.0, Some(8.0), base + Duration::from_secs(60 + i));
    }
    *global().lock().unwrap() = reg;
}

fn clear_perf() {
    *global().lock().unwrap() = PerfRegistry::new();
}

// --- 1. 配置开关与上限 ---------------------------------------------------------

/// 全局性能注册表是进程级单例，四段场景串在同一个测试里跑（各自先清干净）。
#[tokio::test]
async fn adaptive_split_contract() {
    let _perf_guard = perf_lock();
    config_roundtrip().await;
    disabled_and_healthy_stay_on_primary().await;
    degraded_spills_then_recovers().await;
    only_streaming_success_feeds_baseline().await;
}

async fn config_roundtrip() {
    clear_perf();
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    let cookie = login(&app).await;
    let account_id = create_account(&app, &cookie, "split-cfg-account", "http://127.0.0.1:9").await;
    let candidates = json!([{"account_id": account_id, "model": "m"}]);

    let _saved = save_aggregate(&app, &cookie, json!({
        "alias": "split-cfg",
        "candidates": candidates,
        "upstream_api": "chat",
        "adaptive_split": true,
        "adaptive_max_spill_pct": 70,
    }))
    .await;
    let row = aggregate_row(&app, &cookie, "split-cfg").await;
    assert_eq!(row["adaptive_split"], json!(true), "{row:?}");
    assert_eq!(row["adaptive_max_spill_pct"], json!(70), "{row:?}");

    // 更新时省略配置字段 → 显式关掉，比例回落默认 50（不是沿用旧值）。
    save_aggregate(&app, &cookie, json!({
        "alias": "split-cfg",
        "candidates": candidates,
        "upstream_api": "chat",
    }))
    .await;
    let row = aggregate_row(&app, &cookie, "split-cfg").await;
    assert_eq!(row["adaptive_split"], json!(false), "{row:?}");
    assert_eq!(row["adaptive_max_spill_pct"], json!(50), "{row:?}");

    // 上限 90：至少给慢主候选留 10% 流量，基线才能继续更新、才能恢复。
    save_aggregate(&app, &cookie, json!({
        "alias": "split-cfg",
        "candidates": candidates,
        "upstream_api": "chat",
        "adaptive_split": true,
        "adaptive_max_spill_pct": 200,
    }))
    .await;
    let row = aggregate_row(&app, &cookie, "split-cfg").await;
    assert_eq!(row["adaptive_max_spill_pct"], json!(90), "{row:?}");
}

// --- 2. 开关关闭 / 无基线：不分流 -----------------------------------------------

async fn disabled_and_healthy_stay_on_primary() {
    clear_perf();
    let base = spawn_mock().await;
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    add_key(&state, "sk-split-off").await;
    let cookie = login(&app).await;
    let primary = create_account(&app, &cookie, "split-primary-off", &format!("{base}/primary")).await;
    let secondary = create_account(&app, &cookie, "split-secondary-off", &format!("{base}/secondary")).await;
    let candidates = json!([
        {"account_id": primary, "model": "primary-model"},
        {"account_id": secondary, "model": "secondary-model"},
    ]);

    // 开关注明关闭，即使基线已恶化也不分流。
    seed_degraded(primary, "primary-model");
    save_aggregate(&app, &cookie, json!({
        "alias": "split-off", "candidates": candidates, "upstream_api": "chat",
        "adaptive_split": false,
    }))
    .await;
    for _ in 0..12 {
        assert_eq!(chat(&app, "sk-split-off", "split-off").await, "PRIMARY");
    }

    // 开关打开但基线冷启动（无样本）→ 同样 100% 主候选。
    clear_perf();
    save_aggregate(&app, &cookie, json!({
        "alias": "split-healthy", "candidates": candidates, "upstream_api": "chat",
        "adaptive_split": true, "adaptive_max_spill_pct": 90,
    }))
    .await;
    for _ in 0..12 {
        assert_eq!(chat(&app, "sk-split-off", "split-healthy").await, "PRIMARY");
    }
}

// --- 3. 恶化时分流、恢复后回切 ---------------------------------------------------

async fn degraded_spills_then_recovers() {
    clear_perf();
    let base = spawn_mock().await;
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    add_key(&state, "sk-split-on").await;
    let cookie = login(&app).await;
    let primary = create_account(&app, &cookie, "split-primary", &format!("{base}/primary")).await;
    let secondary = create_account(&app, &cookie, "split-secondary", &format!("{base}/secondary")).await;
    save_aggregate(&app, &cookie, json!({
        "alias": "split-on",
        "candidates": [
            {"account_id": primary, "model": "primary-model"},
            {"account_id": secondary, "model": "secondary-model"},
        ],
        "upstream_api": "chat",
        "adaptive_split": true,
        "adaptive_max_spill_pct": 50,
    }))
    .await;

    seed_degraded(primary, "primary-model");
    let mut spilled = 0;
    let mut primary_hits = 0;
    for _ in 0..40 {
        match chat(&app, "sk-split-on", "split-on").await.as_str() {
            "SECONDARY" => spilled += 1,
            "PRIMARY" => primary_hits += 1,
            other => panic!("意外回复：{other}"),
        }
    }
    assert!(spilled >= 10, "基线恶化后应分流，实际 {spilled}/40");
    assert!(primary_hits >= 10, "上限 50% 应给主候选留流量，实际 {primary_hits}/40");

    // 聚合列表要把运行时迟滞状态与分流累加器带给前端，卡片才能显示「分流中」。
    let aggregate = aggregate_row(&app, &cookie, "split-on").await;
    assert_eq!(aggregate["degraded"], json!(true), "{aggregate:?}");
    assert!(aggregate["split_acc"].as_f64().is_some());

    // 真实聚合请求要把客户端别名与命中的候选下标一起落库，统计页才能按别名看流量走向。
    let mut rows: Vec<(Option<String>, Option<i64>)> = Vec::new();
    for _ in 0..100 {
        rows = sqlx::query_as(
            "SELECT requested_model, candidate_idx FROM usage_logs WHERE aggregate_alias = 'split-on'",
        )
        .fetch_all(&state.pool)
        .await
        .unwrap();
        if rows.len() >= 40 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(rows.len(), 40, "40 次聚合请求各落一行归属");
    assert!(
        rows.iter().all(|(model, _)| model.as_deref() == Some("split-on")),
        "requested_model 应是客户端请求的聚合别名：{rows:?}"
    );
    let mut indices: Vec<i64> = rows.iter().filter_map(|(_, idx)| *idx).collect();
    indices.sort_unstable();
    indices.dedup();
    assert_eq!(indices, vec![0, 1], "分流期间两个候选都应被记到：{rows:?}");

    // 分流不推进 active：库里的 active_idx 仍是 0。
    let active: i64 = sqlx::query_scalar("SELECT active_idx FROM aggregate_aliases WHERE alias = 'split-on'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(active, 0, "性能分流不能改 active 指针");

    // 基线恢复（清掉旧样本 = severity 回到 1）→ 后续请求全部回主候选。
    clear_perf();
    for _ in 0..20 {
        assert_eq!(chat(&app, "sk-split-on", "split-on").await, "PRIMARY");
    }
}

// --- 4. 样本入口只收流式成功请求 -------------------------------------------------

async fn only_streaming_success_feeds_baseline() {
    clear_perf();
    let state = llmux_server::test_state().await;
    let account = llmux_core::adapters::Account {
        id: 4242,
        alias: "perf-sample".into(),
        provider_id: "openai".into(),
        api_key: String::new(),
        base_url: None,
        anthropic_base_url: None,
        is_active: 1,
        weight: 1,
        openai_compatible: 1,
        chat_endpoint: None,
        responses_endpoint: None,
        messages_endpoint: None,
        default_protocol: None,
        balance_provider: String::new(),
        balance_auth: String::new(),
        codex_auth_json: String::new(),
        category: String::new(),
    };
    let send = |success: bool, is_stream: bool| {
        llmux_server::routes::v1::helpers::spawn_log_usage_ip(
            state.pool.clone(),
            account.clone(),
            "perf-model".into(),
            "openai".into(),
            10,
            100,
            0,
            0,
            1500,
            success,
            None,
            None,
            None,
            Some(500),
            is_stream,
            None,
            None,
            llmux_server::routes::v1::helpers::UsageAttribution::default(),
        );
    };

    send(true, false);
    send(false, true);
    assert!(global().lock().unwrap().snapshot(4242, "perf-model").is_none());

    send(true, true);
    let snap = global().lock().unwrap().snapshot(4242, "perf-model").expect("流式成功应记样本");
    assert_eq!(snap.samples, 1);
    assert!((snap.ttft_fast_ms.unwrap() - 500.0).abs() < 1e-9);
    // 100 tokens / 1.0s 解码 = 100 tps。
    assert!((snap.tps_fast.unwrap() - 100.0).abs() < 1e-9);
}

// --- 6. 实时性能快照（统计页高频刷新用） ---------------------------------------

#[test]
fn live_snapshot_is_none_without_fresh_samples() {
    let _perf_guard = perf_lock();
    clear_perf();
    let now = Instant::now();
    assert!(global().lock().unwrap().snapshot_all(now).is_empty());

    {
        let mut reg = global().lock().unwrap();
        for i in 0..5 {
            reg.record(777, "live-model", 100.0, Some(50.0), now - Duration::from_secs(5 - i));
        }
    }
    let snap = global().lock().unwrap().snapshot_all(now);
    let node = snap.iter().find(|n| n.account_id == 777 && n.model == "live-model").unwrap();
    assert_eq!(node.samples, 5);
    assert!(node.severity.is_some() && node.severity.unwrap().is_finite());
    assert!(node.ttft_fast_ms.unwrap() > 0.0);
    assert!(node.tps_fast.unwrap() > 0.0);
    assert!(node.ttft_slow_ms.is_some() && node.tps_slow.is_some());
    assert!(node.last_sample_age_secs <= 6);

    // 不足 3 个样本：保留节点，但不给 severity。
    clear_perf();
    global().lock().unwrap().record(778, "thin-model", 100.0, Some(50.0), now);
    let snap = global().lock().unwrap().snapshot_all(now);
    let thin = snap.iter().find(|n| n.account_id == 778).unwrap();
    assert_eq!(thin.samples, 1);
    assert_eq!(thin.severity, None);

    // 过旧：同样显示「无样本」，不能拿旧基线给实时卡片一个健康绿灯。
    clear_perf();
    global().lock().unwrap().record(779, "stale-model", 100.0, Some(50.0), now - STALE_AFTER - Duration::from_secs(1));
    let snap = global().lock().unwrap().snapshot_all(now);
    let stale = snap.iter().find(|n| n.account_id == 779).unwrap();
    assert_eq!(stale.severity, None);
}

#[tokio::test]
async fn perf_endpoint_exposes_live_snapshot_with_filters() {
    let _perf_guard = perf_lock();
    clear_perf();
    let now = Instant::now();
    {
        let mut reg = global().lock().unwrap();
        for i in 0..5 {
            reg.record(777, "live-model", 100.0, Some(50.0), now - Duration::from_secs(5 - i));
            reg.record(778, "other-model", 400.0, Some(10.0), now - Duration::from_secs(5 - i));
        }
    }
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state);
    let cookie = login(&app).await;

    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/stats/perf?account_id=777")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    let resp = llmux_server::test_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    let live = body["live"].as_array().unwrap();
    assert_eq!(live.len(), 1, "body: {body}");
    assert_eq!(live[0]["accountId"], json!(777));
    assert_eq!(live[0]["model"], json!("live-model"));
    assert!(live[0]["severity"].is_number());
}
