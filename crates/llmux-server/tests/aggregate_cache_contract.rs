//! 聚合别名的运行时位置（`active` / `locked`）不能跟着配置一起被 TTL 缓存。
//!
//! 背景（2026-11-21 生产 `os`）：探活的 `record_probe_candidate` 把位置从 1 抬回 0
//! （日志 `🔍 [agg:os] probe V migrated -> 0`），请求侧却继续按缓存里迁移前的
//! `active=1` 分派 —— 紧接着的 3 条派发全是 `🔀 [agg:os V=1]`，一次都没试过刚抬上
//! 去的候选。更糟的是这 3 次在候选 1 上的成功会被 `record_request_outcome` 记成
//! 「迁回 1」的降级确认，把刚抬上去的位置打回，形成每 15 分钟一轮的 ping-pong
//! （配合 `HOT_CACHE_TTL = 60s`，一次抬升最多被钉住一整分钟）。
//!
//! 2026-11-23 起回升只要 1 次确认（降级仍是 3 次），所以本测试的迁移发生在**第一轮**。
//! 判据不变：缓存命中路径必须像 miss 路径那样，用**内存路由**的实时值覆盖
//! `active` / `locked` —— 这两项是运行时状态，不是配置。

use std::time::Instant;

async fn seed_aggregate(state: &llmux_server::app::AppState, alias: &str, active_idx: i64) {
    let candidates = serde_json::json!([
        {"account_id": 1, "model": "m0"},
        {"account_id": 2, "model": "m1"}
    ])
    .to_string();
    sqlx::query("INSERT INTO aggregate_aliases (alias, candidates, active_idx) VALUES (?, ?, ?)")
        .bind(alias)
        .bind(candidates)
        .bind(active_idx)
        .execute(&state.pool)
        .await
        .expect("seed aggregate alias");
}

#[tokio::test]
async fn a_probe_migration_is_visible_to_the_request_path_before_the_ttl_expires() {
    let state = llmux_server::test_state().await;
    seed_aggregate(&state, "agc", 1).await;

    let first = state
        .resolve_aggregate_cached("agc")
        .await
        .unwrap()
        .expect("aggregate exists");
    assert_eq!(first.active, 1, "起点来自库里的 active_idx");
    assert!(!first.locked);

    // 探活判定候选 0 活着 → 回升只要 1 次确认就迁移（生产上的 `probe V migrated`
    // 同一条路）。以前要 3 轮，等于 3×300s。这里传 `fresh_evidence = true`：
    // 回升只认本轮真探过的证据。
    let switched = state
        .aggregate_router
        .lock()
        .unwrap()
        .record_probe_candidate("agc", 0, 2, Instant::now(), true);
    assert!(switched, "回升 1 次确认就迁移");
    assert_eq!(
        state.aggregate_router.lock().unwrap().get_active("agc"),
        0,
        "状态机已抬到候选 0"
    );

    // 缓存命中路径（60s 内）：必须看到新位置，否则下一次请求还从旧起点分派，
    // 而且它在新起点之前的候选上成功会被记成反方向的迁移确认。
    let second = state
        .resolve_aggregate_cached("agc")
        .await
        .unwrap()
        .expect("aggregate exists");
    assert_eq!(
        second.active, 0,
        "缓存命中后 active 要跟内存路由走，不能停留在迁移前"
    );
}

#[tokio::test]
async fn a_lock_set_through_the_router_is_visible_on_the_cache_hit_path() {
    let state = llmux_server::test_state().await;
    seed_aggregate(&state, "agcl", 0).await;

    let first = state
        .resolve_aggregate_cached("agcl")
        .await
        .unwrap()
        .expect("aggregate exists");
    assert!(!first.locked);

    state
        .aggregate_router
        .lock()
        .unwrap()
        .set_locked("agcl", true, 2);

    let second = state
        .resolve_aggregate_cached("agcl")
        .await
        .unwrap()
        .expect("aggregate exists");
    assert!(second.locked, "缓存命中后 locked 也要跟内存路由走");
}
