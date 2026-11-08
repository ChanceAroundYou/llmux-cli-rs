//! 聚合别名候选的**纯逻辑**契约：解析、引用名提取、active/locked 保位、别名展开与排序。
//!
//! 这些判据此前只有 server 层的 HTTP 契约测试间接覆盖（`aggregate_alias_candidate_contract.rs`）。
//! 间接覆盖挡不住两类回归：
//!   1. 库里**已有**的旧 candidates JSON（建于 `via_alias` 之前）被改坏 —— 线上真实数据；
//!   2. 排序 / 保位规则被改 —— 它决定「哪个候选是 active、锁没锁」，错了不会报错，只会静默换线路。
use llmux_core::aggregate::{
    expand_alias_candidate, parse_candidates, preserve_active_state, referenced_aliases,
    AggregateCandidate,
};
use llmux_core::db::{connect_sqlite, init_db};

async fn memory_db() -> sqlx::SqlitePool {
    let pool = connect_sqlite("sqlite::memory:")
        .await
        .expect("connect memory sqlite");
    init_db(&pool).await.expect("initialize schema");
    pool
}

async fn insert_account(pool: &sqlx::SqlitePool, provider_id: &str, weight: i64, active: bool) -> i64 {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO accounts (alias, provider_id, api_key, is_active, weight) VALUES (?, ?, 'k', ?, ?) RETURNING id",
    )
    .bind(format!("acct-{provider_id}-{weight}"))
    .bind(provider_id)
    .bind(i64::from(active))
    .bind(weight)
    .fetch_one(pool)
    .await
    .expect("insert account");
    id
}

async fn insert_alias(
    pool: &sqlx::SqlitePool,
    alias: &str,
    target_model: &str,
    provider_id: Option<&str>,
    account_ids: Option<&[i64]>,
    preferred: Option<i64>,
    upstream_api: Option<&str>,
    timeout_secs: Option<i64>,
) {
    let ids_json = account_ids.map(|ids| serde_json::to_string(ids).expect("encode ids"));
    sqlx::query(
        "INSERT INTO model_aliases (alias, target_model, provider_id, account_ids, preferred_account_id, upstream_api, first_byte_timeout_secs) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(alias)
    .bind(target_model)
    .bind(provider_id)
    .bind(ids_json)
    .bind(preferred)
    .bind(upstream_api)
    .bind(timeout_secs)
    .execute(pool)
    .await
    .expect("insert alias");
}

async fn insert_aggregate(pool: &sqlx::SqlitePool, alias: &str, candidates: &str) {
    sqlx::query("INSERT INTO aggregate_aliases (alias, candidates, interval_secs) VALUES (?, ?, 300)")
        .bind(alias)
        .bind(candidates)
        .execute(pool)
        .await
        .expect("insert aggregate");
}

// ---------------------------------------------------------------------------
// 解析：向后兼容是硬要求，线上库里的行没有新字段
// ---------------------------------------------------------------------------

#[test]
fn a_candidate_json_written_before_the_new_fields_still_parses() {
    // 生产 aggregate_aliases 里现存的行就是这个形状（2026-11-07 之前建的）。
    let list = parse_candidates(r#"[{"account_id":5,"model":"deepseek-v4.1"}]"#)
        .expect("legacy candidate json must parse");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].account_id, 5);
    assert_eq!(list[0].model, "deepseek-v4.1");
    assert_eq!(list[0].via_alias, None);
    assert_eq!(list[0].upstream_api, None);
    assert_eq!(list[0].first_byte_timeout_secs, None);
    // stale_source 是运行时字段，绝不能因为解析而莫名变成 true
    assert!(!list[0].stale_source);

    // 重新序列化时不把 null 写回去：与旧行逐字节同形
    let round = serde_json::to_string(&list).expect("serialize");
    assert_eq!(round, r#"[{"account_id":5,"model":"deepseek-v4.1"}]"#);
}

#[test]
fn candidate_parsing_rejects_every_shape_the_save_route_relies_on() {
    // 空候选集：dispatch 拿不到任何线路，且「删掉最后一个候选」会造出非法行
    assert!(parse_candidates("[]").is_err());
    // account_id <= 0 不是候选，是脏数据（0 在 SQL 里谁都匹配不上）
    assert!(parse_candidates(r#"[{"account_id":0,"model":"m"}]"#).is_err());
    assert!(parse_candidates(r#"[{"account_id":-3,"model":"m"}]"#).is_err());
    // 空模型名会原样发给上游，换来一个查不动的 400
    assert!(parse_candidates(r#"[{"account_id":1,"model":"   "}]"#).is_err());
    assert!(parse_candidates(r#"[{"account_id":1,"model":""}]"#).is_err());
    // 不是 JSON
    assert!(parse_candidates("not json").is_err());
    // 合法 JSON 但不是数组
    assert!(parse_candidates(r#"{"account_id":1,"model":"m"}"#).is_err());
    // 缺必填字段
    assert!(parse_candidates(r#"[{"model":"m"}]"#).is_err());

    // 合法形状必须过，并且字段顺序不影响解析
    let list = parse_candidates(
        r#"[{"model":"m","account_id":1,"via_alias":"d4","upstream_api":"responses","first_byte_timeout_secs":90}]"#,
    )
    .expect("valid candidate json must parse");
    assert_eq!(list[0].via_alias.as_deref(), Some("d4"));
    assert_eq!(list[0].upstream_api.as_deref(), Some("responses"));
    assert_eq!(list[0].first_byte_timeout_secs, Some(90));
}

#[test]
fn stale_source_is_never_loaded_from_the_database() {
    // 运行时判定，`serde(skip, default)`：库里被人塞了值也不该认，
    // 认了就会把一条健在的候选当配置失效跳过（静默少一条线路）。
    let list = parse_candidates(r#"[{"account_id":1,"model":"m","stale_source":true}]"#)
        .expect("unknown fields must be ignored");
    assert!(!list[0].stale_source);
}

// ---------------------------------------------------------------------------
// 引用名提取（保存校验、重展开、运行时陈旧判定共用这一份）
// ---------------------------------------------------------------------------

#[test]
fn referenced_aliases_deduplicate_and_keep_declaration_order() {
    let cands = vec![
        AggregateCandidate { account_id: 1, model: "m1".into(), via_alias: Some("d4".into()), ..Default::default() },
        AggregateCandidate { account_id: 2, model: "m2".into(), via_alias: Some("gl".into()), ..Default::default() },
        // 同一条别名展开出来的多个候选：只算一次引用
        AggregateCandidate { account_id: 3, model: "m3".into(), via_alias: Some("d4".into()), ..Default::default() },
        // 手工挑的 (账户, 模型) 对不是引用
        AggregateCandidate { account_id: 4, model: "m4".into(), ..Default::default() },
        AggregateCandidate { account_id: 5, model: "m5".into(), via_alias: Some("".into()), ..Default::default() },
    ];
    assert_eq!(referenced_aliases(&cands), vec!["d4".to_string(), "gl".to_string()]);
    assert!(referenced_aliases(&[]).is_empty());
}

// ---------------------------------------------------------------------------
// active / locked 保位：按候选身份（account_id + model）搬，不按下标搬
// ---------------------------------------------------------------------------

#[test]
fn the_pinned_active_candidate_follows_itself_when_the_list_is_reordered() {
    let old = r#"[{"account_id":10,"model":"a"},{"account_id":11,"model":"b"},{"account_id":12,"model":"c"}]"#;
    // 旧 active 是 idx 1（11/b）。重展开后它排到 idx 2 —— 必须跟着身份走，
    // 否则「锁定的那一格」会变成另一条线路（锁定语义就是钉住某条线路）。
    let new = vec![
        AggregateCandidate { account_id: 12, model: "c".into(), ..Default::default() },
        AggregateCandidate { account_id: 10, model: "a".into(), ..Default::default() },
        AggregateCandidate { account_id: 11, model: "b".into(), ..Default::default() },
    ];
    assert_eq!(preserve_active_state(old, Some(1), Some(1), &new), (2, 1));
    // 没锁的保位后仍然是没锁
    assert_eq!(preserve_active_state(old, Some(1), Some(0), &new), (2, 0));
    // active_idx 未设 = 第 0 格
    assert_eq!(preserve_active_state(old, None, None, &new), (1, 0));
}

#[test]
fn a_pinned_candidate_that_no_longer_exists_clears_the_lock_too() {
    let old = r#"[{"account_id":10,"model":"a"},{"account_id":11,"model":"b"}]"#;
    let new = vec![
        AggregateCandidate { account_id: 10, model: "a".into(), ..Default::default() },
        AggregateCandidate { account_id: 99, model: "z".into(), ..Default::default() },
    ];
    // 身份没了 → 退回第 0 格；**锁也必须清掉**：留着锁就把一个不存在的线路锁成主候选，
    // 之后所有失败都发生在一条用户以为被锁保护的线路上。
    assert_eq!(preserve_active_state(old, Some(1), Some(1), &new), (0, 0));
}

#[test]
fn preserving_active_state_tolerates_the_shapes_real_rows_have() {
    let new = vec![AggregateCandidate { account_id: 1, model: "m".into(), ..Default::default() }];
    // 旧 JSON 坏了不能 panic（重展开循环里 panic 会让整批聚合别名停更）
    assert_eq!(preserve_active_state("garbage", Some(3), Some(1), &new), (0, 0));
    // active_idx 越界（候选被手工删过）同样退回 (0, 0)，不能 panic
    assert_eq!(
        preserve_active_state(r#"[{"account_id":1,"model":"m"}]"#, Some(7), Some(1), &new),
        (0, 0)
    );
    // 空候选集是非法状态，保位函数不该替调用方造一个 idx
    assert_eq!(preserve_active_state(r#"[{"account_id":1,"model":"m"}]"#, Some(0), Some(1), &[]), (0, 0));
    // locked 为 NULL（列可空）= 没锁
    assert_eq!(
        preserve_active_state(r#"[{"account_id":1,"model":"m"}]"#, Some(0), None, &new),
        (0, 0)
    );
    // locked 存的是 2 这种脏值也不能被当成「锁着」
    assert_eq!(
        preserve_active_state(r#"[{"account_id":1,"model":"m"}]"#, Some(0), Some(2), &new),
        (0, 0)
    );
}

// ---------------------------------------------------------------------------
// 展开：排序、 dropped、错误形状
// ---------------------------------------------------------------------------

#[tokio::test]
async fn expansion_puts_the_preferred_account_first_and_reports_which_accounts_were_dropped() {
    let pool = memory_db().await;
    // 插入顺序刻意与 weight 顺序不同：否则「按 weight DESC, id ASC 排」和
    // 「按建表顺序排」在这个测试里长得一样，去掉 ORDER BY 也不会红。
    let light = insert_account(&pool, "p-ord", 1, true).await;
    let heavy = insert_account(&pool, "p-ord", 10, true).await;
    let mid = insert_account(&pool, "p-ord", 5, true).await;
    let dead = insert_account(&pool, "p-ord", 50, false).await; // 停用：不进候选集

    // 偏好故意选成中间权重的那个 —— 排序必须是「偏好优先」，不是 weight 优先
    insert_alias(
        &pool,
        "d4ord",
        "deepseek-v4.1",
        None,
        Some(&[heavy, mid, light, dead]),
        Some(mid),
        None,
        None,
    )
    .await;

    let (cands, dropped) = expand_alias_candidate(&pool, "d4ord")
        .await
        .expect("expand referenced alias");
    let ids: Vec<i64> = cands.iter().map(|c| c.account_id).collect();
    assert_eq!(ids, vec![mid, heavy, light]);
    assert_eq!(dropped, vec![dead]);
    // 候选里存的必须是**具体模型**：dispatch 直接把候选的 model 发给上游，不会再解析一次
    assert!(cands.iter().all(|c| c.model == "deepseek-v4.1"));
    assert!(cands.iter().all(|c| c.via_alias.as_deref() == Some("d4ord")));
}

#[tokio::test]
async fn expansion_falls_back_to_every_active_account_of_the_provider_when_none_are_declared() {
    let pool = memory_db().await;
    let a = insert_account(&pool, "p-prov", 3, true).await;
    let b = insert_account(&pool, "p-prov", 9, true).await;
    insert_account(&pool, "p-prov", 100, false).await; // 停用
    insert_account(&pool, "p-other", 5, true).await; // 别的 provider，不能混进来

    insert_alias(&pool, "provalias", "some-model", Some("p-prov"), None, None, None, None).await;
    let (cands, dropped) = expand_alias_candidate(&pool, "provalias")
        .await
        .expect("expand provider-scoped alias");
    let ids: Vec<i64> = cands.iter().map(|c| c.account_id).collect();
    // 未声明账户集时按 weight DESC, id ASC（与 dispatch 侧一致）
    assert_eq!(ids, vec![b, a]);
    assert!(dropped.is_empty());
}

#[tokio::test]
async fn expansion_carries_the_source_alias_protocol_and_timeout_onto_every_candidate() {
    let pool = memory_db().await;
    let one = insert_account(&pool, "p-proto", 1, true).await;
    let two = insert_account(&pool, "p-proto", 1, true).await;
    insert_alias(
        &pool,
        "d4proto",
        "muse-spark-1.3-contributor",
        None,
        Some(&[one, two]),
        None,
        Some("responses"),
        Some(90),
    )
    .await;

    let (cands, _) = expand_alias_candidate(&pool, "d4proto").await.expect("expand");
    // 「完整转发」的含义：协议与超时都要带过来，保存侧才拦得住协议冲突、
    // 运行时才有候选级超时可用（候选值优先于聚合值）。
    assert!(cands.iter().all(|c| c.upstream_api.as_deref() == Some("responses")));
    assert!(cands.iter().all(|c| c.first_byte_timeout_secs == Some(90)));
}

#[tokio::test]
async fn expansion_resolves_an_alias_chain_down_to_a_concrete_model() {
    let pool = memory_db().await;
    let acct = insert_account(&pool, "p-chain", 1, true).await;
    // d4chain 自己不绑账户，target_model 指向另一条别名
    insert_alias(&pool, "d4chain", "d4real", None, None, None, Some("chat"), Some(45)).await;
    insert_alias(&pool, "d4real", "real-model", None, Some(&[acct]), Some(acct), Some("responses"), Some(75)).await;

    let (cands, _) = expand_alias_candidate(&pool, "d4chain").await.expect("expand chain");
    assert_eq!(cands.len(), 1);
    assert_eq!(cands[0].model, "real-model");
    // 链上每一跳的协议/超时按 dispatcher 的解析结果（末跳赢），来源名记的是**被引用的那条**
    assert_eq!(cands[0].upstream_api.as_deref(), Some("responses"));
    assert_eq!(cands[0].first_byte_timeout_secs, Some(75));
    assert_eq!(cands[0].via_alias.as_deref(), Some("d4chain"));
}

#[tokio::test]
async fn expansion_refuses_the_three_shapes_that_cannot_be_routed() {
    let pool = memory_db().await;
    let acct = insert_account(&pool, "p-bad", 1, true).await;

    // ① 引用聚合别名：只允许一层
    insert_aggregate(&pool, "aggx", &serde_json::to_string(&vec![AggregateCandidate {
        account_id: acct,
        model: "m".into(),
        ..Default::default()
    }])
    .expect("encode")).await;
    let err = expand_alias_candidate(&pool, "aggx").await.expect_err("must refuse an aggregate name");
    assert!(format!("{err}").contains("aggregate"));

    // ② 引用不存在的别名
    let err = expand_alias_candidate(&pool, "no-such-alias").await.expect_err("must refuse a missing alias");
    assert!(format!("{err}").contains("does not exist"));

    // ③ 声明的账户全都停用 → 没有可用线路
    let dead = insert_account(&pool, "p-bad", 1, false).await;
    insert_alias(&pool, "emptyalias", "m", None, Some(&[dead]), None, None, None).await;
    let err = expand_alias_candidate(&pool, "emptyalias").await.expect_err("must refuse no usable account");
    assert!(format!("{err}").contains("no usable account"));

    // 空名字不是别名
    assert!(expand_alias_candidate(&pool, "   ").await.is_err());
}

// ---------------------------------------------------------------------------
// 反查：普通别名被删时要立刻作废引用它的聚合的解析缓存
// ---------------------------------------------------------------------------

#[tokio::test]
async fn aggregates_referencing_returns_only_the_aggregates_that_declare_the_alias() {
    let pool = memory_db().await;
    let acct = insert_account(&pool, "p-ref", 1, true).await;
    let manual = serde_json::to_string(&vec![AggregateCandidate {
        account_id: acct,
        model: "m".into(),
        ..Default::default()
    }])
    .expect("encode");
    let referencing = serde_json::to_string(&vec![AggregateCandidate {
        account_id: acct,
        model: "m".into(),
        via_alias: Some("d4".into()),
        ..Default::default()
    }])
    .expect("encode");
    insert_aggregate(&pool, "manual-agg", &manual).await;
    insert_aggregate(&pool, "ref-agg", &referencing).await;
    // 脏行不能把整条反查带崩（返回「没有引用」比 panic 好）
    sqlx::query("INSERT INTO aggregate_aliases (alias, candidates, interval_secs) VALUES ('broken-agg', 'garbage', 300)")
        .execute(&pool)
        .await
        .expect("insert broken aggregate");

    let hits = llmux_core::aggregate::aggregates_referencing(&pool, "d4")
        .await
        .expect("reverse lookup must not fail");
    assert_eq!(hits, vec!["ref-agg".to_string()]);

    // 没有任何聚合引用时回空，不是 None / 不是错误
    assert!(llmux_core::aggregate::aggregates_referencing(&pool, "unused-alias")
        .await
        .expect("reverse lookup must not fail")
        .is_empty());
}

// ---------------------------------------------------------------------------
// 聚合别名引用聚合别名：保存时拍平、沿链跟动
// ---------------------------------------------------------------------------

/// 带协议 / 超时 / active 的聚合别名（普通 insert_aggregate 只写 alias+candidates）。
async fn insert_aggregate_full(
    pool: &sqlx::SqlitePool,
    alias: &str,
    candidates: &[AggregateCandidate],
    upstream_api: Option<&str>,
    timeout_secs: Option<i64>,
    active_idx: i64,
) {
    let json = serde_json::to_string(candidates).expect("encode candidates");
    sqlx::query(
        "INSERT INTO aggregate_aliases (alias, candidates, interval_secs, upstream_api, first_byte_timeout_secs, active_idx) VALUES (?, ?, 300, ?, ?, ?)",
    )
    .bind(alias)
    .bind(json)
    .bind(upstream_api)
    .bind(timeout_secs)
    .bind(active_idx)
    .execute(pool)
    .await
    .expect("insert aggregate");
}

async fn read_candidates(pool: &sqlx::SqlitePool, alias: &str) -> Vec<AggregateCandidate> {
    let json: String = sqlx::query_scalar("SELECT candidates FROM aggregate_aliases WHERE alias = ?")
        .bind(alias)
        .fetch_one(pool)
        .await
        .expect("read candidates");
    parse_candidates(&json).expect("parse stored candidates")
}

#[tokio::test]
async fn expanding_an_aggregate_starts_at_its_active_candidate_and_stamps_provenance() {
    let pool = memory_db().await;
    let first = insert_account(&pool, "p-agg", 3, true).await;
    let second = insert_account(&pool, "p-agg", 2, true).await;
    let third = insert_account(&pool, "p-agg", 1, true).await;
    insert_aggregate_full(
        &pool,
        "src",
        &[
            AggregateCandidate { account_id: first, model: "m1".into(), ..Default::default() },
            AggregateCandidate { account_id: second, model: "m2".into(), via_alias: Some("d4".into()), ..Default::default() },
            AggregateCandidate { account_id: third, model: "m3".into(), ..Default::default() },
        ],
        None,
        None,
        2,
    )
    .await;

    let expanded = llmux_core::aggregate::expand_aggregate_candidate(&pool, "src")
        .await
        .expect("expand referenced aggregate");
    // 顺序从被引用聚合的 active 起点轮转：用户引用它是想要「它现在先打谁」。
    let models: Vec<&str> = expanded.iter().map(|c| c.model.as_str()).collect();
    assert_eq!(models, vec!["m3", "m1", "m2"]);
    // 来源记成那条聚合；里面原本来自普通别名的 via_alias 原样保留（删掉那条普通别名时
    // 这批候选同样要在运行时被判成配置失效）。
    assert!(expanded.iter().all(|c| c.via_aggregate.as_deref() == Some("src")));
    assert_eq!(expanded[2].via_alias.as_deref(), Some("d4"));
}

#[tokio::test]
async fn expanding_an_aggregate_propagates_its_forced_protocol_and_only_fills_missing_timeouts() {
    let pool = memory_db().await;
    let a = insert_account(&pool, "p-agg-proto", 1, true).await;
    let b = insert_account(&pool, "p-agg-proto", 1, true).await;
    insert_aggregate_full(
        &pool,
        "forced",
        &[
            // 候选自带超时：候选值优先，不被聚合级覆盖
            AggregateCandidate { account_id: a, model: "m".into(), first_byte_timeout_secs: Some(120), ..Default::default() },
            AggregateCandidate { account_id: b, model: "m".into(), ..Default::default() },
        ],
        Some("responses"),
        Some(45),
        0,
    )
    .await;

    let expanded = llmux_core::aggregate::expand_aggregate_candidate(&pool, "forced")
        .await
        .expect("expand");
    // 协议被强制成 responses 时必须落到候选上：否则引用方按自己的协议发，
    // 保存侧的「候选强制 X、本聚合强制 Y」检查也就拦不住。
    assert!(expanded.iter().all(|c| c.upstream_api.as_deref() == Some("responses")));
    assert_eq!(expanded[0].first_byte_timeout_secs, Some(120));
    assert_eq!(expanded[1].first_byte_timeout_secs, Some(45));
}

#[tokio::test]
async fn resyncing_an_ordinary_alias_cascades_through_aggregate_references() {
    let pool = memory_db().await;
    let old_acct = insert_account(&pool, "p-cas", 1, true).await;
    let new_acct = insert_account(&pool, "p-cas", 2, true).await;
    insert_alias(&pool, "d4", "m-old", None, Some(&[old_acct]), None, Some("chat"), Some(30)).await;
    // G 引用普通别名 d4；A 引用聚合别名 G（引用发生在保存时，库里存的是展开后的快照）。
    insert_aggregate_full(
        &pool,
        "G",
        &[AggregateCandidate { account_id: old_acct, model: "m-old".into(), via_alias: Some("d4".into()), upstream_api: Some("chat".into()), first_byte_timeout_secs: Some(30), ..Default::default() }],
        Some("chat"),
        None,
        0,
    )
    .await;
    insert_aggregate_full(
        &pool,
        "A",
        &[AggregateCandidate { account_id: old_acct, model: "m-old".into(), via_alias: Some("d4".into()), via_aggregate: Some("G".into()), upstream_api: Some("chat".into()), first_byte_timeout_secs: Some(30), ..Default::default() }],
        Some("chat"),
        None,
        0,
    )
    .await;

    // 改 d4 的绑定。只刷 G 是不够的：A 里存的是 G 当时那份快照。
    sqlx::query("UPDATE model_aliases SET target_model = 'm-new', account_ids = ? WHERE alias = 'd4'")
        .bind(serde_json::to_string(&vec![new_acct]).expect("encode"))
        .execute(&pool)
        .await
        .expect("rebind alias");

    let touched = llmux_core::aggregate::resync_alias_candidates(&pool, "d4")
        .await
        .expect("resync");
    // 级联：G 与引用 G 的 A 都必须被刷新，否则 A 会一直打在 d4 早就换掉的账户上。
    assert!(touched.contains(&"G".to_string()), "touched={touched:?}");
    assert!(touched.contains(&"A".to_string()), "touched={touched:?}");

    let g = read_candidates(&pool, "G").await;
    assert_eq!(g[0].account_id, new_acct);
    assert_eq!(g[0].model, "m-new");
    let a = read_candidates(&pool, "A").await;
    assert_eq!(a[0].account_id, new_acct);
    assert_eq!(a[0].model, "m-new");
    // A 的来源标记仍是被引用的那条聚合（不是被 G 的 via_alias 顶掉）。
    assert_eq!(a[0].via_aggregate.as_deref(), Some("G"));
}

#[tokio::test]
async fn resyncing_an_aggregate_refreshes_its_referrers_without_overwriting_itself() {
    let pool = memory_db().await;
    let acct = insert_account(&pool, "p-cyc", 1, true).await;
    // A ↔ B 互相引用：保存侧每次都能展开对方当前的快照（因此是合法配置），
    // 但跟动时必须停下来 —— 否则两圈之后 B 的旧快照会把 A 刚保存的内容盖回去。
    insert_aggregate_full(
        &pool,
        "A",
        &[AggregateCandidate { account_id: acct, model: "from-A".into(), via_aggregate: Some("B".into()), ..Default::default() }],
        None,
        None,
        0,
    )
    .await;
    insert_aggregate_full(
        &pool,
        "B",
        &[AggregateCandidate { account_id: acct, model: "from-B".into(), via_aggregate: Some("A".into()), ..Default::default() }],
        None,
        None,
        0,
    )
    .await;

    let touched = llmux_core::aggregate::resync_aggregate_candidates(&pool, "A")
        .await
        .expect("resync must terminate");
    // 只有 B 被刷新（A 是本轮来源方，不能被反向覆盖）。
    assert_eq!(touched, vec!["B".to_string()]);
    let a = read_candidates(&pool, "A").await;
    assert_eq!(a[0].model, "from-A");
    // B 的候选换成 A 当前的快照，来源记作 A。
    let b = read_candidates(&pool, "B").await;
    assert_eq!(b[0].model, "from-A");
    assert_eq!(b[0].via_aggregate.as_deref(), Some("A"));
}

#[tokio::test]
async fn an_ordinary_alias_that_redirects_to_an_aggregate_is_refused_as_a_candidate() {
    let pool = memory_db().await;
    let acct = insert_account(&pool, "p-redir", 1, true).await;
    insert_aggregate_full(
        &pool,
        "G",
        &[AggregateCandidate { account_id: acct, model: "m".into(), ..Default::default() }],
        None,
        None,
        0,
    )
    .await;
    // 普通别名写成「重定向到聚合别名」的形状：account_ids / provider 都空，target 落到 G 上。
    insert_alias(&pool, "redir", "G", None, None, None, Some("default"), None).await;

    // 候选展开不能跟着这条重定向走：dispatch 不会把候选再当聚合解析一次，
    // 存进去只会变成「打一个叫 G 的模型」。必须当场说清楚。
    let err = expand_alias_candidate(&pool, "redir")
        .await
        .expect_err("a redirect-to-aggregate alias cannot be a candidate source");
    assert!(format!("{err}").contains("redirects to aggregate alias"), "err={err}");
}
