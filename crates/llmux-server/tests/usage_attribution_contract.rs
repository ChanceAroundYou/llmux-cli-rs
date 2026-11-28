//! usage_logs 的别名归属列契约：客户端请求名、聚合别名、候选下标各自独立落库。

use std::cell::RefCell;

use llmux_server::routes::v1::helpers::{spawn_log_usage_ip, UsageAttribution, USAGE_ATTRIBUTION};

#[tokio::test]
async fn attribution_follows_request_scope_and_aggregate_candidate_updates() {
    assert_eq!(UsageAttribution::current(), UsageAttribution::default());

    USAGE_ATTRIBUTION
        .scope(
            RefCell::new(UsageAttribution {
                requested_model: Some("X".into()),
                ..Default::default()
            }),
            async {
                UsageAttribution::note_aggregate_candidate("G", 2);
                assert_eq!(
                    UsageAttribution::current(),
                    UsageAttribution {
                        requested_model: Some("X".into()),
                        aggregate_alias: Some("G".into()),
                        candidate_idx: Some(2),
                    }
                );
            },
        )
        .await;

    assert_eq!(UsageAttribution::current(), UsageAttribution::default());
}

fn account() -> llmux_core::adapters::Account {
    llmux_core::adapters::Account {
        id: 9911,
        alias: "attr-account".into(),
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
    }
}

async fn wait_for_rows(state: &llmux_server::app::AppState, n: i64) {
    for _ in 0..100 {
        let got: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_logs WHERE account_id = 9911")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        if got >= n {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("usage_logs 归属行没在 5s 内落库");
}

#[tokio::test]
async fn attribution_columns_record_requested_model_aggregate_and_candidate_index() {
    let state = llmux_server::test_state().await;
    let send = |attr: UsageAttribution| {
        spawn_log_usage_ip(
            state.pool.clone(),
            account(),
            "candidate-model".into(),
            "openai".into(),
            10,
            20,
            0,
            0,
            1500,
            true,
            None,
            None,
            None,
            Some(500),
            true,
            None,
            None,
            attr,
        );
    };

    send(UsageAttribution {
        requested_model: Some("X".into()),
        aggregate_alias: Some("G".into()),
        candidate_idx: Some(2),
    });
    send(UsageAttribution {
        requested_model: Some("X".into()),
        ..Default::default()
    });
    send(UsageAttribution::default());

    wait_for_rows(&state, 3).await;
    let mut rows: Vec<(Option<String>, Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT requested_model, aggregate_alias, candidate_idx FROM usage_logs WHERE account_id = 9911",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap();
    // 三个 spawn 并发写库，id 顺序不等于调用顺序；断言按内容排序。
    rows.sort();

    assert_eq!(
        rows,
        vec![
            (None, None, None),
            (Some("X".into()), None, None),
            (Some("X".into()), Some("G".into()), Some(2)),
        ]
    );
}
