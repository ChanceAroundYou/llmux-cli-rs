//! 别名同名冲突的合约测试（保存侧）。
//!
//! 钉的行为：`alias` 列上有 UNIQUE，而 UPSERT 永不报冲突 —— 目标名字已被
//! **另一条**别名占用时，原实现会静默改掉那一条的 target / 绑定 / 协议，
//! 调用方只看到「Alias set successfully」。现在必须回 409，且：
//!   - 「同名且是自己」（请求携带的 id == 库里同名行的 id）→ 正常更新
//!   - 普通别名 ↔ 聚合别名的 confirm 流程不变（那是「有意覆盖另一种别名」）
//!   - 保存不换 rowid（否则下一轮编辑带着旧 id 会被自己的规则判成冲突）
//!
//! 每条都会先跑一遍「如果没有这个检查会怎样」：见各测试末尾的反证断言。

use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
};
use serde_json::{json, Value};

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

async fn api(
    app: &axum::Router,
    cookie: &str,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method.clone()).uri(uri);
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    builder = builder.header(header::COOKIE, cookie);
    let req = builder
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

async fn aliases(app: &axum::Router, cookie: &str) -> Vec<Value> {
    let (st, body) = api(app, cookie, Method::GET, "/api/models/aliases", None).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    body.as_array().cloned().unwrap_or_default()
}

fn find_alias(list: &[Value], name: &str) -> Value {
    list.iter()
        .find(|a| a["alias"] == name)
        .cloned()
        .unwrap_or_else(|| panic!("别名 {name} 不在了；当前列表：{list:?}"))
}

async fn create_alias(
    app: &axum::Router,
    cookie: &str,
    alias: &str,
    target: &str,
    extra: Option<Value>,
) -> (StatusCode, Value) {
    let mut body = json!({"alias": alias, "target_model": target, "provider_id": "openai"});
    if let Some(extra) = extra {
        body.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    }
    api(app, cookie, Method::POST, "/api/models/aliases", Some(body)).await
}

/// 名字已被**另一条**别名占用 → 409，而且被占用那条必须原样活着。
#[tokio::test]
async fn saving_an_alias_whose_name_is_taken_by_another_alias_is_rejected() {
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    let cookie = login(&app).await;

    create_alias(&app, &cookie, "keep", "model-keep", None).await;
    let other = create_alias(&app, &cookie, "taken", "model-taken", None).await;
    assert_eq!(other.0, StatusCode::OK, "{:?}", other.1);
    let list = aliases(&app, &cookie).await;
    let taken_id = find_alias(&list, "taken")["id"].as_i64().unwrap();
    let keep_id = find_alias(&list, "keep")["id"].as_i64().unwrap();

    // 拿「keep」这条的身份去写「taken」这个名字 —— UI 改名就是这条路径
    // （编辑 keep、把名字改成已存在的 taken）。
    let (st, body) = create_alias(
        &app,
        &cookie,
        "taken",
        "model-hijacked",
        Some(json!({"id": keep_id})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "同名占用必须 409，实际：{body:?}");
    assert_eq!(
        body["code"].as_str(),
        Some("alias_name_taken"),
        "前端要靠 code 分支：{body:?}"
    );
    assert_eq!(
        body["existing_id"].as_i64(),
        Some(taken_id),
        "错误要说明名字被哪条占用：{body:?}"
    );

    // 反证：静默覆盖的话这两条早就变了。
    let list = aliases(&app, &cookie).await;
    assert_eq!(
        list.iter().filter(|a| a["alias"] == "taken").count(),
        1,
        "同名行不该变成两条：{list:?}"
    );
    let taken = find_alias(&list, "taken");
    assert_eq!(taken["target_model"], "model-taken", "被占用那条被改了");
    assert_eq!(taken["id"], taken_id, "被占用那条换了行 id");
    assert_eq!(find_alias(&list, "keep")["target_model"], "model-keep");
}

/// 「同名且是自己」是合法编辑：带自身 id → 200，且 target 真的更新。
#[tokio::test]
async fn editing_an_alias_with_its_own_id_still_updates() {
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    let cookie = login(&app).await;

    create_alias(&app, &cookie, "dup", "v1", None).await;
    let my_id = find_alias(&aliases(&app, &cookie).await, "dup")["id"].as_i64().unwrap();

    let (st, body) = create_alias(
        &app,
        &cookie,
        "dup",
        "v2",
        Some(json!({"id": my_id, "upstream_api": "responses"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "编辑自身不该冲突：{body:?}");

    let list = aliases(&app, &cookie).await;
    assert_eq!(find_alias(&list, "dup")["target_model"], "v2");
    assert_eq!(find_alias(&list, "dup")["upstream_api"], "responses");
    assert_eq!(
        list.iter().filter(|a| a["alias"] == "dup").count(),
        1,
        "同名行不该变成两条：{list:?}"
    );
}

/// 反复保存同一条别名不能换 rowid。
///
/// 换 id 的实现（`INSERT OR REPLACE`）会让「带着上一次列表里的 id 来编辑」这条
/// 合法路径下一轮被判成同名冲突 —— 用户看到的是「改一次成功、再改就报名字被占」。
#[tokio::test]
async fn repeated_saves_keep_the_same_row_id() {
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    let cookie = login(&app).await;

    create_alias(&app, &cookie, "stable", "a", None).await;
    let first = find_alias(&aliases(&app, &cookie).await, "stable")["id"]
        .as_i64()
        .unwrap();

    for target in ["b", "c", "d"] {
        let (st, body) = create_alias(
            &app,
            &cookie,
            "stable",
            target,
            Some(json!({"id": first})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "保存 {target} 应成功：{body:?}");
        let again = find_alias(&aliases(&app, &cookie).await, "stable");
        assert_eq!(again["id"], first, "保存换了 rowid（{target}）");
        assert_eq!(again["target_model"], target);
    }
}

/// 不带 id 写一个已存在的名字 = 创建语义 → 409（不再是静默覆盖）。
/// 这条钉住「客户端一次性快照失效 / 并发写入」那条路。
#[tokio::test]
async fn creating_a_name_that_already_exists_is_rejected() {
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    let cookie = login(&app).await;

    create_alias(&app, &cookie, "once", "original", None).await;
    let (st, body) = create_alias(&app, &cookie, "once", "overwrite", None).await;
    assert_eq!(st, StatusCode::CONFLICT, "{body:?}");
    assert_eq!(
        find_alias(&aliases(&app, &cookie).await, "once")["target_model"],
        "original",
        "被静默覆盖了"
    );
}

/// 普通别名覆盖聚合别名的 confirm 流程不能被新检查破坏 —— 那是「有意换掉另一种别名」，
/// 与「名字被同类占用」是两回事，必须仍然可以带 confirm 通过。
#[tokio::test]
async fn ordinary_alias_over_aggregate_alias_still_needs_only_confirm() {
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    let cookie = login(&app).await;

    let acct = api(
        &app,
        &cookie,
        Method::POST,
        "/api/accounts",
        Some(json!({
            "alias": "agg-acct",
            "provider_id": "openai",
            "api_key": "sk-mock",
            "base_url": "http://127.0.0.1:1",
            "chat_endpoint": "http://127.0.0.1:1",
            "messages_endpoint": "http://127.0.0.1:1",
            "default_protocol": "chat",
            "skip_validation": true,
        })),
    )
    .await;
    assert_eq!(acct.0, StatusCode::OK, "{:?}", acct.1);
    let account_id = acct.1["id"].as_i64().unwrap();

    let agg = api(
        &app,
        &cookie,
        Method::POST,
        "/api/aggregate-aliases",
        Some(json!({
            "alias": "mix",
            "candidates": [{"account_id": account_id, "model": "m1"}],
            "interval_secs": 300,
            "upstream_api": "default",
        })),
    )
    .await;
    assert_eq!(agg.0, StatusCode::OK, "{:?}", agg.1);

    // 不带 confirm → 409 且 conflict=aggregate（前端据此弹确认框，不是「名字被占」）
    let (st, body) = create_alias(&app, &cookie, "mix", "plain", None).await;
    assert_eq!(st, StatusCode::CONFLICT, "{body:?}");
    assert_eq!(body["conflict"].as_str(), Some("aggregate"), "{body:?}");
    assert_ne!(
        body["code"].as_str(),
        Some("alias_name_taken"),
        "聚合占用不能被报成同类占用：{body:?}"
    );

    // 带 confirm → 覆盖成功，聚合那条被删（既有行为）
    let (st, body) = create_alias(&app, &cookie, "mix", "plain", Some(json!({"confirm": true}))).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    let (_, aggs) = api(&app, &cookie, Method::GET, "/api/aggregate-aliases", None).await;
    assert!(
        aggs.as_array()
            .unwrap()
            .iter()
            .all(|a| a["alias"] != "mix"),
        "确认后聚合别名应被删除：{aggs:?}"
    );
}
