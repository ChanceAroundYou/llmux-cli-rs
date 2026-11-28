//! 性能历史端点契约：只有真实流式成功行能喂 TTFT/TPS，成功率分母仍是全部流式行。

use llmux_core::usage::{UsageFilter, UsageService};
use axum::http::Request;
use serde_json::Value;

use std::time::{Duration, Instant};

const FROM: i64 = 1_700_000_000_000;
const TO: i64 = FROM + 3_600_000;

async fn insert(
    pool: &sqlx::SqlitePool,
    timestamp: i64,
    is_stream: bool,
    success: bool,
    is_test: bool,
    latency: i64,
    ttft: Option<i64>,
    output: i64,
) {
    sqlx::query(
        "INSERT INTO usage_logs
         (timestamp, account_id, provider_id, model, input_tokens, output_tokens,
          latency_ms, success, is_stream, is_test, ttft_ms, requested_model)
         VALUES (?, 12, 'openai', 'm', 10, ?, ?, ?, ?, ?, ?, 'X')",
    )
    .bind(timestamp)
    .bind(output)
    .bind(latency)
    .bind(success as i64)
    .bind(is_stream as i64)
    .bind(is_test as i64)
    .bind(ttft)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn only_success_stream_rows_feed_performance() {
    let pool = llmux_server::test_state().await.pool;
    // 当前窗口：2 个流式成功（一个 TPS 有效、一个解码太短），1 个流式失败，
    // 1 个非流式成功，1 个拨测行。
    insert(&pool, FROM + 100, true, true, false, 1100, Some(100), 100).await;
    insert(&pool, FROM + 200, true, true, false, 2000, Some(1900), 10).await;
    insert(&pool, FROM + 300, true, false, false, 500, None, 0).await;
    insert(&pool, FROM + 400, false, true, false, 2000, Some(100), 100).await;
    insert(&pool, FROM + 500, true, true, true, 1100, Some(100), 100).await;
    // 前一等长窗口：一行流式成功，用作 prev* 对比。
    insert(&pool, FROM - 3_600_000 + 100, true, true, false, 2100, Some(100), 100).await;

    let data = UsageService::new(pool)
        .get_performance(
            Some(FROM),
            Some(TO),
            None,
            &UsageFilter::default(),
            None,
            "model",
            3_600_000,
        )
        .await
        .unwrap();

    assert_eq!(data.rows.len(), 1);
    let row = &data.rows[0];
    assert_eq!(row.model.as_deref(), Some("m"));
    assert_eq!(row.account_id, None, "group_by=model 不按账户拆行");
    assert_eq!(row.stream_requests, 3, "流式成功与失败都进分母");
    assert!((row.success_rate - 2.0 / 3.0).abs() < 1e-9);
    assert!((row.avg_ttft - 1.0).abs() < 1e-9, "TTFT 用秒，只有两行成功");
    assert!((row.avg_tps - 100.0).abs() < 1e-9, "短解码行不进 TPS");
    assert_eq!(row.prev_stream_requests, 1);
    assert!((row.prev_avg_tps - 50.0).abs() < 1e-9);
    assert!((row.delta_tps_pct - 100.0).abs() < 1e-9);
    assert!((row.delta_success_rate - (2.0 / 3.0 - 1.0)).abs() < 1e-9);

    // 时序按 bucket 聚合，只有当前窗口两行成功贡献 TTFT/TPS。
    assert_eq!(data.series.len(), 1);
    assert_eq!(data.series[0].stream_requests, 3);
}

#[tokio::test]
async fn performance_endpoint_assembles_live_and_honors_filters() {
    let state = llmux_server::test_state().await;
    insert(&state.pool, FROM + 100, true, true, false, 1100, Some(100), 100).await;
    let now = Instant::now();
    let mut reg = llmux_core::perf::global().lock().unwrap();
    for i in 0..5 {
        reg.record(12, "m", 100.0, Some(100.0), now - Duration::from_secs(5 - i));
    }
    drop(reg);
    let app = llmux_server::app(state);

    // /api 端点需要会话 cookie。
    let req = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(r#"{"username":"admin","password":"admin"}"#))
        .unwrap();
    let resp = llmux_server::test_request(app.clone(), req).await;
    let cookie = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|s| s.split(';').next())
        .find(|s| s.starts_with("llmux_session="))
        .unwrap()
        .to_string();

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/stats/performance?start={FROM}&end={TO}&group_by=account_model"))
        .header("cookie", cookie)
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = llmux_server::test_request(app, req).await;
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert_eq!(status, 200, "body: {}", String::from_utf8_lossy(&bytes));
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["groupBy"], "account_model");
    assert_eq!(body["rows"][0]["accountId"], 12);
    assert_eq!(body["live"][0]["accountId"], 12);
    assert!(body["live"][0]["severity"].is_number());
}
