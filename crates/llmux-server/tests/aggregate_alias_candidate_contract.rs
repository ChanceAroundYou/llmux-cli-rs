//! 聚合别名**引用普通别名**（完整转发 / 只允许一层 / 保存校验 + 运行时跳过）的合约测试。
//!
//! 钉住的行为：
//!   1. `{"alias":"d4"}` 这种候选在**保存时**展开成来源别名当前的 (账户集, 具体模型,
//!      协议, 首字节超时)，库里存的永远是具体候选 —— active_idx / locked / 3-confirm
//!      的索引语义因此一点不用改。
//!   2. 只允许一层：候选不能是另一个聚合别名，也不能是自引用。
//!   3. 来源别名改了绑定 → 引用它的聚合别名跟着重新展开（「加进聚合就跟着别名走」）。
//!   4. 来源别名被**删除** → 候选保留，运行时按「配置失效」跳过：回 502 而不是 429，
//!      且**不记** `note_candidate_failure`（否则删一条别名就能把聚合的主候选挤下线）。
//!   5. 候选继承来源别名自己的首字节超时，聚合的值只兜底。
//!   6. 协议不一致是**警告 + 运行时跳过**，不是保存时 400 —— 与 2026-08 那条
//!      「强制协议不拒不支持的绑定」的决定一致（见 docs/superpowers/plans/
//!      2026-08-23-upstream-downstream-split.md）。
//!
//! Mock 上游只有两条路径，形状可区分：`/fast/chat/completions` 立刻回 `A-fast`，
//! `/slow/chat/completions` 睡 2 秒再回 `A-slow` —— 后者让首字节超时的**优先级**
//! 变成可观测的 200/502，而不是只能靠读代码相信它生效了。

use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request, StatusCode},
    response::IntoResponse,
};
use serde_json::{json, Value};

// --- mock 上游 ----------------------------------------------------------------

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

/// 返回 base URL；`chat_endpoint` 拼成 `{base}/fast` 或 `{base}/slow`。
async fn spawn_mock() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = axum::Router::new()
        .route(
            "/fast/chat/completions",
            axum::routing::post(|axum::Json(_body): axum::Json<Value>| async move {
                axum::Json(chat_body("A-fast")).into_response()
            }),
        )
        .route(
            "/slow/chat/completions",
            axum::routing::post(|axum::Json(_body): axum::Json<Value>| async move {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                axum::Json(chat_body("A-slow")).into_response()
            }),
        );
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}")
}

// --- 请求helper --------------------------------------------------------------

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

async fn api_post(
    app: &axum::Router,
    cookie: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    api(app, cookie, Method::POST, uri, Some(body)).await
}

async fn v1_post(
    app: &axum::Router,
    key: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, String) {
    let req = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {key}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    let resp = llmux_server::test_request(app.clone(), req).await;
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

// --- 固定动作 -----------------------------------------------------------------

async fn add_key(state: &llmux_server::AppState, key: &str) {
    sqlx::query("INSERT INTO api_keys (name, key, allowed_models) VALUES (?, ?, ?)")
        .bind(format!("aggref-{key}"))
        .bind(key)
        .bind("*")
        .execute(&state.pool)
        .await
        .unwrap();
}

async fn create_account(app: &axum::Router, cookie: &str, alias: &str, chat_endpoint: &str) -> i64 {
    let (st, body) = api_post(
        app,
        cookie,
        "/api/accounts",
        json!({
            "alias": alias,
            "provider_id": "openai",
            "api_key": "sk-mock",
            "base_url": chat_endpoint,
            "chat_endpoint": chat_endpoint,
            "default_protocol": "chat",
            "skip_validation": true,
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    body["id"].as_i64().expect("account id")
}

async fn create_alias(
    app: &axum::Router,
    cookie: &str,
    alias: &str,
    target_model: &str,
    account_ids: Vec<i64>,
    extra: Option<Value>,
) -> (StatusCode, Value) {
    let mut body = json!({
        "alias": alias,
        "target_model": target_model,
        "account_ids": account_ids,
        "provider_id": "openai",
        "upstream_api": "chat",
    });
    if let Some(extra) = extra {
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
    }
    api_post(app, cookie, "/api/models/aliases", body).await
}

fn alias_id(list: &Value, name: &str) -> i64 {
    list.as_array()
        .unwrap()
        .iter()
        .find(|a| a["alias"] == name)
        .unwrap_or_else(|| panic!("别名 {name} 不在了：{list:?}"))["id"]
        .as_i64()
        .unwrap()
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

fn chat_request(model: &str) -> Value {
    json!({"model": model, "max_tokens": 16, "messages": [{"role": "user", "content": "hi"}]})
}

// --- 1. 完整转发：保存展开 + 运行时真的走到 ------------------------------------

/// `{"alias":"d4"}` 候选：保存时展开成来源别名的账户/模型/超时，运行时请求
/// 经聚合别名落到那个账户的那个模型。
///
/// 反证：如果保存只是把名字存下来、运行时不认这个名字，最后那两条断言会一个
/// 400（库里候选非法）一个 502（走不到 mock）。
#[tokio::test]
async fn an_aggregate_can_forward_an_ordinary_alias_end_to_end() {
    let base = spawn_mock().await;
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    add_key(&state, "sk-ref").await;
    let cookie = login(&app).await;

    let acct = create_account(&app, &cookie, "refacct", &format!("{base}/fast")).await;
    let (st, body) = create_alias(
        &app,
        &cookie,
        "d4ref",
        "m-target",
        vec![acct],
        Some(json!({"first_byte_timeout_secs": 90})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");

    let (st, body) = api_post(
        &app,
        &cookie,
        "/api/aggregate-aliases",
        json!({"alias": "agg-ref", "candidates": [{"alias": "d4ref"}], "upstream_api": "chat"}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    assert_eq!(body["candidates"].as_i64(), Some(1), "别名应展开成 1 个候选：{body:?}");

    let row = aggregate_row(&app, &cookie, "agg-ref").await;
    let cand = &row["candidates"].as_array().unwrap()[0];
    assert_eq!(cand["account_id"], json!(acct), "候选没继承别名绑定的账户：{row:?}");
    assert_eq!(cand["model"], json!("m-target"), "候选没继承别名的具体模型：{row:?}");
    assert_eq!(cand["via_alias"], json!("d4ref"), "候选丢了来源标记：{row:?}");
    assert_eq!(
        cand["first_byte_timeout_secs"],
        json!(90),
        "候选没继承别名自己的首字节超时：{row:?}"
    );
    assert_eq!(
        row["missing_alias_sources"].as_array().unwrap().len(),
        0,
        "来源健在却报了缺失：{row:?}"
    );

    let (st, raw) = v1_post(&app, "sk-ref", "/v1/chat/completions", chat_request("agg-ref")).await;
    assert_eq!(st, StatusCode::OK, "运行时没走到转发的候选：{raw}");
    assert!(raw.contains("A-fast"), "应落在 mock 上：{raw}");
}

// --- 2. 只允许一层 -------------------------------------------------------------

#[tokio::test]
async fn an_aggregate_candidate_may_not_reference_another_aggregate_alias() {
    let base = spawn_mock().await;
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    add_key(&state, "sk-level").await;
    let cookie = login(&app).await;
    let acct = create_account(&app, &cookie, "levelacct", &format!("{base}/fast")).await;

    let (st, body) = api_post(
        &app,
        &cookie,
        "/api/aggregate-aliases",
        json!({"alias": "agg-inner", "candidates": [{"account_id": acct, "model": "m-in"}], "upstream_api": "chat"}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");

    // 写法一：直接引用聚合别名名
    let (st, body) = api_post(
        &app,
        &cookie,
        "/api/aggregate-aliases",
        json!({"alias": "agg-outer", "candidates": [{"alias": "agg-inner"}], "upstream_api": "chat"}),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "二层引用必须 400：{body:?}");
    assert!(
        body["error"].as_str().unwrap_or_default().contains("one level"),
        "错误要说清只允许一层：{body:?}"
    );

    // 写法二：编辑器把已展开的候选原样送回，但 via_alias 指向聚合别名
    let (st, body) = api_post(
        &app,
        &cookie,
        "/api/aggregate-aliases",
        json!({
            "alias": "agg-outer2",
            "candidates": [{"account_id": acct, "model": "m-in", "via_alias": "agg-inner"}],
            "upstream_api": "chat",
        }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body:?}");

    // 写法三：自引用 —— 同名普通别名会在冲突那一步被删掉，候选来源当场死掉
    let (st, body) = api_post(
        &app,
        &cookie,
        "/api/aggregate-aliases",
        json!({"alias": "agg-self", "candidates": [{"alias": "agg-self"}], "upstream_api": "chat"}),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "自引用必须 400：{body:?}");
    assert!(
        body["error"].as_str().unwrap_or_default().contains("itself"),
        "{body:?}"
    );

    let (_, list) = api(&app, &cookie, Method::GET, "/api/aggregate-aliases", None).await;
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["alias"].as_str().unwrap_or_default())
        .collect();
    assert!(names.contains(&"agg-inner"), "合法的聚合别名不该被牵连：{names:?}");
    for bad in ["agg-outer", "agg-outer2", "agg-self"] {
        assert!(!names.contains(&bad), "{bad} 被存进去了：{names:?}");
    }
}

// --- 3. 来源不存在 -------------------------------------------------------------

#[tokio::test]
async fn referencing_a_nonexistent_alias_is_rejected_at_save() {
    let base = spawn_mock().await;
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    let cookie = login(&app).await;
    let acct = create_account(&app, &cookie, "orphanacct", &format!("{base}/fast")).await;

    let (st, body) = api_post(
        &app,
        &cookie,
        "/api/aggregate-aliases",
        json!({"alias": "agg-missing", "candidates": [{"alias": "no-such-alias"}], "upstream_api": "chat"}),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body:?}");
    assert!(
        body["error"].as_str().unwrap_or_default().contains("does not exist"),
        "错误要说清是名字不存在：{body:?}"
    );

    // 已展开的候选带回一个死掉的 via_alias：同样是配置坏了，不能静默存下
    let (st, body) = api_post(
        &app,
        &cookie,
        "/api/aggregate-aliases",
        json!({
            "alias": "agg-orphan",
            "candidates": [{"account_id": acct, "model": "m-ok", "via_alias": "no-such-alias"}],
            "upstream_api": "chat",
        }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body:?}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("no longer exists"),
        "{body:?}"
    );

    let (_, list) = api(&app, &cookie, Method::GET, "/api/aggregate-aliases", None).await;
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .all(|a| a["alias"] != "agg-missing" && a["alias"] != "agg-orphan"),
        "被拒的聚合别名不该落库：{list:?}"
    );
}

// --- 4. 别名改了绑定 → 聚合候选跟着走 ------------------------------------------

/// 「完整转发」的持续部分：别名改了 target / 绑定，引用它的聚合别名必须重新展开。
/// 只存一次快照的话，用户会在聚合里用到早就换掉的账户和模型。
#[tokio::test]
async fn saving_the_referenced_alias_re_expands_the_aggregate() {
    let base = spawn_mock().await;
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    add_key(&state, "sk-sync").await;
    let cookie = login(&app).await;

    let acct_a = create_account(&app, &cookie, "sync-a", &format!("{base}/fast")).await;
    let acct_b = create_account(&app, &cookie, "sync-b", &format!("{base}/fast")).await;

    let (st, body) = create_alias(&app, &cookie, "d4sync", "m-v1", vec![acct_a], None).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    let alias_pk = alias_id(
        &api(&app, &cookie, Method::GET, "/api/models/aliases", None).await.1,
        "d4sync",
    );

    let (st, body) = api_post(
        &app,
        &cookie,
        "/api/aggregate-aliases",
        json!({"alias": "agg-sync", "candidates": [{"alias": "d4sync"}], "upstream_api": "chat"}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    let row = aggregate_row(&app, &cookie, "agg-sync").await;
    assert_eq!(row["candidates"].as_array().unwrap()[0]["account_id"], json!(acct_a));

    // 别名换模型、换账户（带着自己的主键，走正常的编辑路径）
    let (st, body) = create_alias(
        &app,
        &cookie,
        "d4sync",
        "m-v2",
        vec![acct_b],
        Some(json!({"id": alias_pk})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");

    let row = aggregate_row(&app, &cookie, "agg-sync").await;
    let cand = &row["candidates"].as_array().unwrap()[0];
    assert_eq!(
        cand["account_id"],
        json!(acct_b),
        "聚合候选还指着旧账户（别名换了绑定）：{row:?}"
    );
    assert_eq!(cand["model"], json!("m-v2"), "聚合候选还是旧模型：{row:?}");
    assert_eq!(cand["via_alias"], json!("d4sync"), "重展开丢了来源标记：{row:?}");

    // 运行时也要跟着新绑定走（缓存不失效就会仍打 acct_a）
    let (st, raw) = v1_post(&app, "sk-sync", "/v1/chat/completions", chat_request("agg-sync")).await;
    assert_eq!(st, StatusCode::OK, "{raw}");
    assert!(raw.contains("A-fast"), "{raw}");
}

// --- 5. 来源被删：运行时跳过，且不算「确认死」 ----------------------------------

#[tokio::test]
async fn deleting_the_referenced_alias_skips_the_candidate_without_counting_it_as_dead() {
    let base = spawn_mock().await;
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    add_key(&state, "sk-gone").await;
    let cookie = login(&app).await;

    let acct = create_account(&app, &cookie, "goneacct", &format!("{base}/fast")).await;
    create_alias(&app, &cookie, "d4gone", "m-gone", vec![acct], None).await;
    let alias_pk = alias_id(
        &api(&app, &cookie, Method::GET, "/api/models/aliases", None).await.1,
        "d4gone",
    );
    let (st, body) = api_post(
        &app,
        &cookie,
        "/api/aggregate-aliases",
        json!({"alias": "agg-gone", "candidates": [{"alias": "d4gone"}], "upstream_api": "chat"}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");

    // 先打一次健康的：聚合解析缓存（`HOT_CACHE_TTL` = 60s）里留下「来源健在」的快照。
    // 删除别名后不作废这份缓存的话，下面那条请求会在缓存过期前继续走到候选。
    let (st, raw) = v1_post(&app, "sk-gone", "/v1/chat/completions", chat_request("agg-gone")).await;
    assert_eq!(st, StatusCode::OK, "删除前候选应正常可用：{raw}");
    assert!(raw.contains("A-fast"), "{raw}");

    let (st, body) = api(
        &app,
        &cookie,
        Method::DELETE,
        &format!("/api/models/aliases/{alias_pk}"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");

    // 候选保留（删了会让「只剩一个候选」的聚合别名变成非法空列表），但标明来源没了
    let row = aggregate_row(&app, &cookie, "agg-gone").await;
    assert_eq!(
        row["candidates"].as_array().unwrap().len(),
        1,
        "候选被顺手删了：{row:?}"
    );
    assert_eq!(
        row["missing_alias_sources"].as_array().unwrap().as_slice(),
        [json!("d4gone")],
        "UI 要能看出这一候选的来源已删除：{row:?}"
    );

    let (st, raw) = v1_post(&app, "sk-gone", "/v1/chat/completions", chat_request("agg-gone")).await;
    assert_eq!(
        st,
        StatusCode::BAD_GATEWAY,
        "来源已删的候选应被跳过（真去打就会 200 A-fast）：{raw}"
    );
    assert!(
        raw.contains("no longer exists"),
        "错误要说明是配置失效，不是上游坏了：{raw}"
    );

    // 关键：不能记成「确认失败」。否则删一条普通别名就能在 3-confirm 里把
    // 聚合的主候选挤下线 / 触发退避翻倍。
    let router = state.aggregate_router.lock().unwrap();
    if let Some(entry) = router.entries.get("agg-gone") {
        assert!(
            !entry.last_status.iter().any(|s| *s == Some(false)),
            "配置失效被记成候选失败（last_status 里出现了 false）：{entry:?}"
        );
    }
    drop(router);
}

// --- 6. 首字节超时：候选的赢，聚合的兜底 ----------------------------------------

/// 别名把超时抬到 3s 是有原因的（大 prompt 冷前缀 prefill 慢，见 AGENTS.md）；
/// 候选在聚合里不继承它，就会被聚合自己的 1s 掐断 —— 等于把调好的配置改回坏的样子。
/// 对照组证明「聚合的值确实生效」，不是这条机制把超时整个关掉了。
#[tokio::test]
async fn a_referenced_alias_keeps_its_own_first_byte_timeout() {
    let base = spawn_mock().await;
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    add_key(&state, "sk-tmo").await;
    let cookie = login(&app).await;

    let slow = create_account(&app, &cookie, "slowacct", &format!("{base}/slow")).await;
    let (st, body) = create_alias(
        &app,
        &cookie,
        "d4slow",
        "m-slow",
        vec![slow],
        Some(json!({"first_byte_timeout_secs": 3})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");

    // 聚合自己配 1s < mock 的 2s：若按聚合的超时，这条必然 502
    let (st, body) = api_post(
        &app,
        &cookie,
        "/api/aggregate-aliases",
        json!({
            "alias": "agg-tmo",
            "candidates": [{"alias": "d4slow"}],
            "upstream_api": "chat",
            "first_byte_timeout_secs": 1,
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    let (st, raw) = v1_post(&app, "sk-tmo", "/v1/chat/completions", chat_request("agg-tmo")).await;
    assert_eq!(
        st,
        StatusCode::OK,
        "候选没继承别名自己的 3s 超时（被聚合的 1s 掐断）：{raw}"
    );
    assert!(raw.contains("A-slow"), "{raw}");

    // 对照组：候选没有自己的超时 → 聚合的 1s 生效 → 被掐断
    let (st, body) = api_post(
        &app,
        &cookie,
        "/api/aggregate-aliases",
        json!({
            "alias": "agg-tmo-control",
            "candidates": [{"account_id": slow, "model": "m-slow"}],
            "upstream_api": "chat",
            "first_byte_timeout_secs": 1,
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    let (st, raw) = v1_post(
        &app,
        "sk-tmo",
        "/v1/chat/completions",
        chat_request("agg-tmo-control"),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::BAD_GATEWAY,
        "聚合自己的超时没生效，这条对照组无效：{raw}"
    );
}

// --- 7. 协议不一致：警告 + 运行时跳过，不 400 -----------------------------------

/// 与 2026-08 的决定一致（强制协议不在保存侧拒绑定）：拒掉会把「别名后来改了
/// 协议」变成保存不进去的聚合别名。但**必须说出来** —— 用户挑的是别名，看不到
/// 底下的账户，不告诉他就只剩「为什么没走到这个候选」这种查不动的问题。
#[tokio::test]
async fn a_protocol_mismatch_between_alias_and_aggregate_is_a_note_not_a_rejection() {
    let base = spawn_mock().await;
    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());
    let cookie = login(&app).await;
    let acct = create_account(&app, &cookie, "protoacct", &format!("{base}/fast")).await;

    let (st, body) = create_alias(&app, &cookie, "d4proto", "m-proto", vec![acct], None).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");

    let (st, body) = api_post(
        &app,
        &cookie,
        "/api/aggregate-aliases",
        json!({
            "alias": "agg-proto",
            "candidates": [{"alias": "d4proto"}],
            "upstream_api": "responses",
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "协议不一致不该 400：{body:?}");
    let notes = body["notes"].as_array().cloned().unwrap_or_default();
    assert!(
        !notes.is_empty(),
        "不一致必须在响应里说出来（只有后端知道候选底下是什么）：{body:?}"
    );
    assert!(
        notes
            .iter()
            .any(|n| n.as_str().unwrap_or_default().contains("skipped at runtime")),
        "要说清后果是运行时跳过：{body:?}"
    );
}
