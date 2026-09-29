use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde_json::{json, Value};
use sqlx::Row;

use llmux_core::crypto::decrypt_api_key;
use llmux_core::dispatcher::{get_active_accounts, resolve_model, resolve_provider_type, ModelResolution};
use llmux_core::probe;
use llmux_core::protocol::DownstreamMode;

use crate::app::AppState;

/// 拨测结果落库。**独立于 usage_logs** —— 真实流量与拨测各存一处，
/// 谁更新都不会抹掉对方（此前共用 usage_logs 按 (account,model) 取最新一条，
/// 真实流量随时把拨测结果冲掉）。展示时由 health 接口合并两边的「最近一次」。
///
/// 协议集合（`supported`）也存这里 —— 原 `model_protocol_cache` 已合并进来
/// （0018 主键漏了 protocol 列，多协议模型永远写不进去）。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn persist_test_result(
    pool: &sqlx::SqlitePool,
    account: &llmux_core::adapters::Account,
    model: &str,
    success: bool,
    latency_ms: i64,
    error: Option<&str>,
    outcome: Option<&llmux_core::probe::ProbeOutcome>,
    source: probe::TestSource,
) {
    let via = outcome
        .and_then(|o| o.preferred())
        .map(|p| p.as_str().to_string());
    // native provider（anthropic/gemini）用自己的端点形式，不是三协议之一，
    // 存了会让 UI 显示一个语义不对的角标。
    let supported = outcome
        .filter(|o| !o.native)
        .map(|o| {
            o.supported
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let _ = sqlx::query(
        "INSERT INTO model_test_results \
         (account_id, model, success, latency_ms, error_message, via, supported, checked_at, source) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(account_id, model) DO UPDATE SET \
           success = excluded.success, latency_ms = excluded.latency_ms, \
           error_message = excluded.error_message, via = excluded.via, \
           supported = excluded.supported, checked_at = excluded.checked_at, \
           source = excluded.source",
    )
    .bind(account.id)
    .bind(model)
    .bind(if success { 1 } else { 0 })
    .bind(latency_ms)
    .bind(error)
    .bind(via)
    .bind(supported)
    .bind(now_ms)
    .bind(source.as_str())
    .execute(pool)
    .await;

    // 请求日志页（`/api/stats/logs`）读的是 usage_logs —— 拨测此前只写
    // model_test_results，页面上一条都看不到（e0fc691 把原先只存在于批量队列里
    // 的那条 INSERT 也一并换掉了）。这里补一条 is_test=1：用量统计、账号排序、
    // 仪表盘活动流全都带 `is_test = 0`，不会污染真实数据，只有请求日志页会展示。
    // 后台聚合探活（每 300s 一轮、近 20 个候选）不写 —— 会把真实请求淹掉，
    // 它的结果在模型卡片角标里已经能看到。
    if !matches!(source, probe::TestSource::Aggregate) {
        // 记**真实**用量，别再写死 0：拨测是真发上游生成请求的（每协议一次，
        // 配齐三协议就是三次），记 0 等于把这笔开销在本地账上抹掉，用户既无法
        // 核对也无法判断拨测划不划算。上游没回 usage 时解析为 0 —— 宁可不记也
        // 不能瞎猜，与 `ProtocolProbe::usage` 的口径一致。
        let (input_tokens, output_tokens) = outcome
            .map(|o| o.total_usage())
            .unwrap_or((0, 0));
        let _ = sqlx::query(
            "INSERT INTO usage_logs \
             (timestamp, account_id, provider_id, model, input_tokens, output_tokens, \
              latency_ms, success, error_message, is_stream, is_test) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 0, 1)",
        )
        .bind(now_ms)
        .bind(account.id)
        .bind(&account.provider_id)
        .bind(model)
        .bind(input_tokens)
        .bind(output_tokens)
        .bind(latency_ms)
        .bind(if success { 1 } else { 0 })
        .bind(error)
        .execute(pool)
        .await;
    }

    // 连续失败 → 暂停自动拨测；成功 → 解除暂停。所有探测路径都经这里，
    // 所以「显式调用成功也能救回模型」是自动成立的（真实流量走 proxy，成功时
    // 另行调用 probe::clear_suspension —— 见 v1/helpers.rs）。
    //
    // `FailureKind::Probe`：拨测失败**不**冷却生产流量。上游把这个模型下架了
    // （却仍留在 /v1/models），探活每轮都失败很正常，但账户配额可能好得很 ——
    // 早前这里和真实配额 429 共用一个计数器，结果下架一个模型就把真实流量
    // 挡 30 分钟还回 429。真实配额 429 走 v1/helpers.rs 的 note_rate_limit。
    if success {
        probe::clear_suspension(pool, account.id, model).await;
    } else {
        let newly = probe::note_failure(
            pool,
            account.id,
            model,
            error,
            probe::FailureKind::Probe,
        )
        .await;
        if newly {
            tracing::warn!(
                "⏸️  {} | {} 连续失败，暂停自动拨测 {} 分钟",
                model,
                account.alias,
                probe::SUSPEND_SECS / 60
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use llmux_core::probe::{ProtocolProbe, TestSource};

    async fn pool() -> sqlx::SqlitePool {
        let pool = llmux_core::db::connect_sqlite("sqlite::memory:").await.unwrap();
        llmux_core::db::init_db(&pool).await.unwrap();
        pool
    }

    fn account() -> llmux_core::adapters::Account {
        llmux_core::adapters::Account {
            id: 7,
            alias: "acc".into(),
            provider_id: "prov".into(),
            api_key: "k".into(),
            base_url: None,
            anthropic_base_url: None,
            is_active: 1,
            weight: 1,
            openai_compatible: 0,
            chat_endpoint: None,
            responses_endpoint: None,
            messages_endpoint: None,
            default_protocol: None,
            balance_provider: String::new(),
            balance_auth: String::new(),
        }
    }

    /// 拨测必须同时落 usage_logs（is_test=1），否则请求日志页看不到；
    /// 后台聚合探活必须不落，否则每轮二十条把真实请求淹掉。
    #[tokio::test]
    async fn probe_writes_request_log_but_background_aggregate_does_not() {
        let pool = pool().await;

        persist_test_result(
            &pool, &account(), "m1", false, 1200, Some("boom"), None, TestSource::Manual,
        )
        .await;
        persist_test_result(
            &pool, &account(), "m2", true, 900, None, None, TestSource::Verify,
        )
        .await;
        persist_test_result(
            &pool, &account(), "m3", true, 800, None, None, TestSource::Aggregate,
        )
        .await;

        let rows: Vec<(String, i64, i64, Option<String>)> = sqlx::query_as(
            "SELECT model, success, is_test, error_message FROM usage_logs ORDER BY model",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![
                ("m1".to_string(), 0, 1, Some("boom".to_string())),
                ("m2".to_string(), 1, 1, None),
            ],
            "manual/verify 拨测要进请求日志，aggregate 不进"
        );

        // 三者的结果都仍然进 model_test_results（卡片/健康角标的数据源）。
        let results: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM model_test_results")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(results, 3);
    }

    /// 拨测行必须记**真实** token，不能写死 0 —— 拨测是真发上游生成请求的，
    /// 记 0 等于把这笔开销在本地账上抹掉，用户既无法核对也无法判断是否值得。
    /// 三协议全通时是三次真实生成的合计。
    #[tokio::test]
    async fn probe_log_rows_carry_real_token_usage() {
        let pool = pool().await;
        let outcome = llmux_core::probe::ProbeOutcome {
            native: false,
            protocols: vec![
                ProtocolProbe {
                    protocol: llmux_core::protocol::Protocol::Chat,
                    ok: true,
                    status: 200,
                    error: String::new(),
                    body: r#"{"usage":{"prompt_tokens":5,"completion_tokens":2}}"#.into(),
                    latency_ms: 100,
                },
                ProtocolProbe {
                    protocol: llmux_core::protocol::Protocol::Messages,
                    ok: true,
                    status: 200,
                    error: String::new(),
                    body: r#"{"usage":{"input_tokens":5,"output_tokens":3}}"#.into(),
                    latency_ms: 120,
                },
                ProtocolProbe {
                    protocol: llmux_core::protocol::Protocol::Responses,
                    ok: false,
                    status: 500,
                    error: "boom".into(),
                    body: r#"{"error":{"message":"boom"}}"#.into(),
                    latency_ms: 90,
                },
            ],
            supported: vec![
                llmux_core::protocol::Protocol::Chat,
                llmux_core::protocol::Protocol::Messages,
            ],
            mismatched_config: None,
        };

        persist_test_result(
            &pool, &account(), "m1", true, 120, None, Some(&outcome), TestSource::Manual,
        )
        .await;

        let (input, output): (i64, i64) = sqlx::query_as(
            "SELECT input_tokens, output_tokens FROM usage_logs WHERE model = 'm1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((input, output), (10, 5), "应记成功协议的合计用量，失败协议不计");
    }

    /// 反向对照：上游没回 usage 时记 0，不能瞎猜。
    #[tokio::test]
    async fn probe_log_rows_fall_back_to_zero_without_upstream_usage() {
        let pool = pool().await;
        let outcome = llmux_core::probe::ProbeOutcome {
            native: false,
            protocols: vec![ProtocolProbe {
                protocol: llmux_core::protocol::Protocol::Chat,
                ok: true,
                status: 200,
                error: String::new(),
                body: r#"{"choices":[]}"#.into(),
                latency_ms: 100,
            }],
            supported: vec![llmux_core::protocol::Protocol::Chat],
            mismatched_config: None,
        };

        persist_test_result(
            &pool, &account(), "m1", true, 100, None, Some(&outcome), TestSource::Manual,
        )
        .await;

        let (input, output): (i64, i64) = sqlx::query_as(
            "SELECT input_tokens, output_tokens FROM usage_logs WHERE model = 'm1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((input, output), (0, 0));
    }

    /// 方案 A 的状态机：单次失败不停、连续失败才停、成功即解除。
    #[tokio::test]
    async fn consecutive_failures_suspend_then_success_clears() {
        let pool = pool().await;
        let acc = account();
        let m = "dead-model";
        let a = |pool: &sqlx::SqlitePool| {
            let pool = pool.clone();
            async move { llmux_core::probe::is_suspended(&pool, 7, m).await }
        };

        // 第 1 次失败：还没到阈值，不该停 —— 上游抖动很常见。
        persist_test_result(&pool, &acc, m, false, 100, Some("e1"), None, TestSource::Manual).await;
        assert!(!a(&pool).await, "首次失败不应暂停");

        // 第 2 次：达到阈值，暂停。
        persist_test_result(&pool, &acc, m, false, 100, Some("e2"), None, TestSource::Aggregate).await;
        assert!(a(&pool).await, "连续两次失败应暂停");
        let failures: i64 =
            sqlx::query_scalar("SELECT consecutive_failures FROM model_probe_suspensions")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(failures, 2);

        // 成功一次（这里用真实流量那条路径）→ 解除。
        // 单模型拨测走 persist_test_result，等价于显式拨测成功。
        persist_test_result(&pool, &acc, m, true, 50, None, None, TestSource::Manual).await;
        assert!(!a(&pool).await, "成功应解除暂停");
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM model_probe_suspensions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 0, "成功应把暂停记录整条清掉");
    }

    /// 真实流量成功也要解除暂停（不等冷却到期由自动拨测发现）。
    #[tokio::test]
    async fn clear_suspension_is_callable_from_traffic_path() {
        let pool = pool().await;
        let acc = account();
        for _ in 0..2 {
            persist_test_result(&pool, &acc, "m", false, 100, Some("x"), None, TestSource::Aggregate)
                .await;
        }
        assert!(llmux_core::probe::is_suspended(&pool, 7, "m").await);
        // v1/helpers.rs 的成功分支就是这么调的
        llmux_core::probe::clear_suspension(&pool, 7, "m").await;
        assert!(!llmux_core::probe::is_suspended(&pool, 7, "m").await);
    }

    /// 冷却到期后，再失败一次要重新起算 30 分钟（而不是叠加）。
    #[tokio::test]
    async fn expired_cooldown_restarts_from_now() {
        let pool = pool().await;
        let acc = account();
        for _ in 0..2 {
            persist_test_result(&pool, &acc, "m", false, 100, Some("x"), None, TestSource::Aggregate)
                .await;
        }
        // 手工把到期时间拨到过去，模拟「冷却结束，自动拨测又来试一次」。
        sqlx::query("UPDATE model_probe_suspensions SET suspended_until = 1 WHERE account_id = 7")
            .execute(&pool)
            .await
            .unwrap();
        assert!(!llmux_core::probe::is_suspended(&pool, 7, "m").await, "已到期应视为未暂停");

        // 再失败 → 从现在重新暂停，且不应把 past 的旧值带进来。
        persist_test_result(&pool, &acc, "m", false, 100, Some("x"), None, TestSource::Aggregate).await;
        let (until, first): (i64, i64) =
            sqlx::query_as("SELECT suspended_until, first_suspended_at FROM model_probe_suspensions")
                .fetch_one(&pool)
                .await
                .unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        assert!(until > now, "到期后再失败应重新暂停");
        assert!(until <= now + llmux_core::probe::SUSPEND_SECS * 1000 + 5_000);
        assert!(first > now - 60_000, "首次暂停时间应重置为本次，而不是沿用过期值");
    }

    /// 回归：探活失败曾与真实配额 429 共用一个 `suspended_until`，于是上游
    /// 下架某模型（仍留在 /v1/models）→ 探活连败 2 次 → **生产流量**被挡
    /// 30 分钟，客户端拿到 429 + Retry-After:1800，而账户配额其实充足。
    /// 探活侧冷却照旧（它正是为省掉死模型的探活请求而存在的），流量侧必须干净。
    #[tokio::test]
    async fn probe_failures_suspend_probing_but_not_production_traffic() {
        let pool = pool().await;
        let acc = account();
        let m = "delisted-model";
        for _ in 0..2 {
            persist_test_result(
                &pool,
                &acc,
                m,
                false,
                100,
                Some("model not found"),
                None,
                TestSource::Aggregate,
            )
            .await;
        }

        // 探活侧：仍然暂停 —— 这正是它该干的事，别让死模型每轮再刷一次红点。
        assert!(
            llmux_core::probe::is_suspended(&pool, 7, m).await,
            "探活连败两次后应暂停自动拨测"
        );
        // 流量侧：必须放行。真实流量不查探活侧那一列了。
        assert!(
            !crate::routes::v1::helpers::rate_limit_suspended(&pool, 7, m).await,
            "探活失败不得冷却生产流量（账户配额可能完好）"
        );
    }

    /// 反向回归：真配额 429 该挡的还是要挡。两侧都冷却，且互不覆盖 ——
    /// 配额冷却不该被后来的探活失败清掉。
    #[tokio::test]
    async fn quota_429_cools_down_traffic_too_and_survives_later_probe_failures() {
        let pool = pool().await;
        let m = "ling-free";
        for _ in 0..2 {
            crate::routes::v1::helpers::note_rate_limit(
                &pool,
                7,
                m,
                r#"{"error":{"message":"You've used all 100 free Ling requests for today. Your quota resets at 2026-09-30T00:00:00.000Z."}}"#,
            )
            .await;
        }
        assert!(
            crate::routes::v1::helpers::rate_limit_suspended(&pool, 7, m).await,
            "配额耗尽必须冷却生产流量，否则每个请求都白吃一个 429"
        );

        // 探活侧同样冷却（配额真没了，再探也是白花钱），且不影响流量侧。
        assert!(llmux_core::probe::is_suspended(&pool, 7, m).await);
        for _ in 0..2 {
            llmux_core::probe::note_failure(
                &pool,
                7,
                m,
                Some("upstream probe failed"),
                llmux_core::probe::FailureKind::Probe,
            )
            .await;
        }
        assert!(
            crate::routes::v1::helpers::rate_limit_suspended(&pool, 7, m).await,
            "探活失败不得把已有的配额冷却清掉"
        );

        // 一次成功（真实流量）整条清掉，两侧同时放行。
        llmux_core::probe::clear_suspension(&pool, 7, m).await;
        assert!(!crate::routes::v1::helpers::rate_limit_suspended(&pool, 7, m).await);
        assert!(!llmux_core::probe::is_suspended(&pool, 7, m).await);
    }

    /// 单次失败不立冷却 —— 两侧都不该因为一次偶发 429 就开始挡流量。
    #[tokio::test]
    async fn a_single_quota_429_does_not_start_the_traffic_cooldown() {
        let pool = pool().await;
        crate::routes::v1::helpers::note_rate_limit(
            &pool,
            7,
            "m",
            r#"{"error":{"message":"You have reached your 5-hour usage limit."}}"#,
        )
        .await;
        assert!(
            !crate::routes::v1::helpers::rate_limit_suspended(&pool, 7, "m").await,
            "阈值是连续 2 次，单次不应冷却"
        );
    }

    /// 回归：只拆冷却列而**共用计数器**时，「1 次探活失败 + 1 次配额 429」
    /// 就能凑够阈值 —— 单次配额 429 照样开挡，要修的 bug 从计数器后门回来。
    /// 两侧必须各数各的。
    #[tokio::test]
    async fn probe_and_quota_failures_count_separately() {
        let pool = pool().await;
        let acc = account();
        let m = "mixed";

        // 探活失败 1 次：只涨探活侧。
        persist_test_result(&pool, &acc, m, false, 100, Some("probe err"), None, TestSource::Aggregate)
            .await;

        // 配额 429 只有 1 次 —— 配着上面那 1 次探活失败，共同计数器会到 2。
        crate::routes::v1::helpers::note_rate_limit(
            &pool,
            7,
            m,
            r#"{"error":{"message":"You've used all 100 free Ling requests for today. Your quota resets at 2026-09-30T00:00:00.000Z."}}"#,
        )
        .await;
        assert!(
            !crate::routes::v1::helpers::rate_limit_suspended(&pool, 7, m).await,
            "1 次配额 429 配 1 次探活失败不得开挡：探活的失败不该替配额数到阈值"
        );

        // 配额侧自己的第 2 次才开挡。
        crate::routes::v1::helpers::note_rate_limit(
            &pool,
            7,
            m,
            r#"{"error":{"message":"You've used all 100 free Ling requests for today. Your quota resets at 2026-09-30T00:00:00.000Z."}}"#,
        )
        .await;
        assert!(
            crate::routes::v1::helpers::rate_limit_suspended(&pool, 7, m).await,
            "连续 2 次配额 429 应开挡"
        );
    }

    /// `is_traffic_suspended` 在**查询出错**时按「冷却中」处理（fail-closed）。
    /// 若放行，0023 的列没建出来时配额冷却整个失效且日志无痕；误挡的代价只是
    /// 一个账户在故障期间少接流量。探活侧相反 —— 那里的 fail-closed 会让一场
    /// DB 抖动停掉全部后台探活。
    #[tokio::test]
    async fn traffic_cooldown_read_fails_closed_on_query_error() {
        let pool = pool().await;
        // 建一个缺列的表来模拟 schema 缺失 / DB 故障
        let bad = llmux_core::db::connect_sqlite("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE model_probe_suspensions (account_id INTEGER, model TEXT)")
            .execute(&bad)
            .await
            .unwrap();
        assert!(
            llmux_core::probe::is_traffic_suspended(&bad, 7, "m").await,
            "查询出错必须按冷却处理，否则 0023 没生效时保护整个消失"
        );
        // 正常库上无行 = 不冷却，确认这不是「永远 true」
        assert!(!llmux_core::probe::is_traffic_suspended(&pool, 7, "m").await);
    }

    /// 回归：计数曾是「读出来 +1 再写回」。真实流量下同一 (账户,模型) 的并发
    /// 429 很常见，两个请求都读到 n、都写 n+1 就丢一次失败 —— 配额冷却迟迟
    /// 不开挡，而开挡正是这个函数存在的理由。改成 SQL 原子自增后必须两个都算数。
    #[tokio::test]
    async fn concurrent_quota_429s_do_not_lose_a_count() {
        let pool = pool().await;
        let err = r#"{"error":{"message":"Daily quota exhausted. Your quota resets at 00:00."}}"#;
        // 两条「同时」到达的配额 429
        let (a, b) = tokio::join!(
            crate::routes::v1::helpers::note_rate_limit(&pool, 7, "m", err),
            crate::routes::v1::helpers::note_rate_limit(&pool, 7, "m", err),
        );
        let _ = (a, b);
        assert!(
            crate::routes::v1::helpers::rate_limit_suspended(&pool, 7, "m").await,
            "两次并发配额 429 必须都计数并开挡，丢一次就等于白挨一个 429"
        );
        let n: i64 = sqlx::query_scalar(
            "SELECT consecutive_quota_failures FROM model_probe_suspensions",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(n, 2, "计数应精确为 2（不多不少）");
    }

    /// 配额 429 也会累计**探活侧**的冷却（配额真没了，再探是白花钱），但探活
    /// 侧只由自己的次数决定 —— 2 次配额 429 后探活侧就该停探，不必等探活也失败。
    #[tokio::test]
    async fn quota_failures_also_arm_the_probe_side_but_keep_counters_apart() {
        let pool = pool().await;
        for _ in 0..2 {
            crate::routes::v1::helpers::note_rate_limit(
                &pool,
                7,
                "m",
                r#"{"error":{"message":"Daily quota exhausted."}}"#,
            )
            .await;
        }
        assert!(
            llmux_core::probe::is_suspended(&pool, 7, "m").await,
            "配额耗尽时探活侧也该停 —— 再探只是白花钱"
        );
        let (probe_f, quota_f): (i64, i64) = sqlx::query_as(
            "SELECT consecutive_failures, consecutive_quota_failures FROM model_probe_suspensions",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((probe_f, quota_f), (2, 2), "两侧各自计数到 2");
    }

    /// 探活侧的计数也必须是原子的。后台探活对同一别名的候选**并发**跑，
    /// 手动拨测队列又会与后台轮次重叠 —— 读-改-写下两个失败都读到 n、都写 n+1，
    /// 丢一次意味着「已下架模型」在计数到阈值前还会多挨若干轮 300s 的真实
    /// 生成请求，而那每一轮都在烧上游配额（本模块存在的理由正是省掉它）。
    #[tokio::test]
    async fn concurrent_probe_failures_do_not_lose_a_count() {
        let pool = pool().await;
        let acc = account();
        let (a, b) = tokio::join!(
            persist_test_result(&pool, &acc, "m", false, 100, Some("x"), None, TestSource::Aggregate),
            persist_test_result(&pool, &acc, "m", false, 100, Some("x"), None, TestSource::Aggregate),
        );
        let _ = (a, b);
        assert!(
            llmux_core::probe::is_suspended(&pool, 7, "m").await,
            "两次并发探活失败必须都计数并开挡"
        );
        let n: i64 = sqlx::query_scalar(
            "SELECT consecutive_failures FROM model_probe_suspensions",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(n, 2, "计数应精确为 2（不多不少）");
    }
}

pub async fn get_test_queue_status(Extension(state): Extension<AppState>) -> Response {
    let queue = state.test_queue.lock().unwrap();
    Json(json!({
        "isRunning": queue.is_running,
        "total": queue.total,
        "current": queue.current,
        "progress": queue.progress,
        // 让前端知道这条队列该归给哪个入口（刷新/换标签页后靠它恢复进度显示）
        "scope": queue.scope,
    }))
    .into_response()
}

pub async fn start_test_queue(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let Some(models) = body.get("models").and_then(Value::as_array) else {
        return crate::error::simple_error("Invalid models array", StatusCode::BAD_REQUEST);
    };

    {
        let mut queue = state.test_queue.lock().unwrap();
        if queue.is_running {
            return (
                StatusCode::CONFLICT,
                Json(json!({ "error": "A test queue is already running." })),
            )
                .into_response();
        }
        queue.is_running = true;
        queue.total = models.len();
        queue.current = 0;
        queue.progress = 0;
        queue.scope = body
            .get("scope")
            .and_then(Value::as_str)
            .filter(|s| matches!(*s, "aliases" | "aggregates" | "models"))
            .unwrap_or("models")
            .to_string();
    }

    tracing::info!("🧪 Starting test for {} models", models.len());
    let pool = state.pool.clone();
    let master_key = state.master_key.clone();
    let queue_state = state.test_queue.clone();
    let models_owned: Vec<Value> = models.to_vec();

    tokio::spawn(async move {
        for (i, model_entry) in models_owned.iter().enumerate() {
            let model_name = model_entry
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let account_id_override = model_entry.get("accountId").and_then(|v| v.as_i64());
            let provider_id_override = model_entry
                .get("providerId")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());

            // 批量队列是「自动拨测」的一种，冷却中的 (账户, 模型) 跳过 ——
            // 否则每一轮都要为已被上游下架的模型付一次必然失败的请求。
            // 定向到具体账户的条目才能精确判断；未指定账户的等下面解析出账户后再判。
            if let Some(acc_id) = account_id_override {
                if probe::is_suspended(&pool, acc_id, model_name).await {
                    tracing::debug!("⏸️  跳过 {} | 账户 {}：冷却中", model_name, acc_id);
                    let mut queue = queue_state.lock().unwrap();
                    queue.current = i + 1;
                    queue.progress = if queue.total > 0 { ((i + 1) * 100) / queue.total } else { 0 };
                    continue;
                }
            }

            // 若前端已指定 accountId，直接定向到该账户，避免同名模型串到 provider 的首账户
            let targeted_accounts: Option<Vec<llmux_core::adapters::Account>> = if let Some(acc_id) = account_id_override {
                match sqlx::query(
                    "SELECT id, alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight, openai_compatible, chat_endpoint, responses_endpoint, messages_endpoint, default_protocol, balance_provider, balance_auth FROM accounts WHERE id = ? AND is_active = 1",
                )
                .bind(acc_id)
                .fetch_optional(&pool)
                .await
                {
                    Ok(Some(row)) => {
                        let enc: String = row.try_get("api_key").unwrap_or_default();
                        match decrypt_api_key(&enc, &master_key) {
                            Ok(api_key) => Some(vec![llmux_core::adapters::Account {
                                id: row.try_get("id").unwrap_or_default(),
                                alias: row.try_get("alias").unwrap_or_default(),
                                provider_id: row.try_get("provider_id").unwrap_or_default(),
                                api_key,
                                base_url: row.try_get("base_url").ok(),
                                anthropic_base_url: row.try_get("anthropic_base_url").ok(),
                                is_active: row.try_get::<i64, _>("is_active").unwrap_or(1),
                                weight: row.try_get("weight").unwrap_or(1),
                                openai_compatible: row.try_get("openai_compatible").unwrap_or(0),
                                chat_endpoint: row.try_get("chat_endpoint").ok(),
                                responses_endpoint: row.try_get("responses_endpoint").ok(),
                                messages_endpoint: row.try_get("messages_endpoint").ok(),
                                  default_protocol: row.try_get("default_protocol").ok(),
                                balance_provider: row.try_get::<Option<String>, _>("balance_provider").ok().flatten().unwrap_or_default(),
                balance_auth: row.try_get::<Option<String>, _>("balance_auth").ok().flatten().unwrap_or_default(),
                            }]),
                            Err(_) => Some(vec![]),
                        }
                    }
                    Ok(None) => Some(vec![]),
                    Err(_) => Some(vec![]),
                }
            } else { None };

            // Resolve provider and get accounts
            // Try resolve_model first (by alias), fall back to providerId, then prefix guess
            let resolution = resolve_model(&pool, model_name).await.unwrap_or_else(|_| {
                ModelResolution {
                    provider_id: provider_id_override.unwrap_or("openai").to_string(),
                    target_model: model_name.to_string(),
                    account_ids: vec![],
                    preferred_account_id: None,
                    alias_name: None,
                    upstream_api: Default::default(),
                }
            });

            // Override resolved provider with explicit providerId when resolution guessed wrong
            let effective_provider = if resolution.provider_id == "openai" || resolution.provider_id == "gemini" || resolution.provider_id == "anthropic" {
                provider_id_override.unwrap_or(&resolution.provider_id)
            } else {
                &resolution.provider_id
            };
            // 拨测的首选协议必须与真实路由同源：alias 的 upstream_api 决定 mode。
            let probe_mode = DownstreamMode::from_str(resolution.upstream_api.as_str());

            let accounts_for_test: Vec<llmux_core::adapters::Account> = if let Some(v) = targeted_accounts {
                v
            } else if let Ok(acs) = get_active_accounts(&pool, Some(effective_provider), &master_key).await {
                acs
            } else { vec![] };
            if let Some(account) = accounts_for_test.first() {
                        // 未定向条目在这里才解析出账户 —— 补一次冷却判断
                        // （定向的那批已在循环开头拦过）。
                        if account_id_override.is_none()
                            && probe::is_suspended(&pool, account.id, model_name).await
                        {
                            tracing::debug!("⏸️  跳过 {} | {}：冷却中", model_name, account.alias);
                            let mut queue = queue_state.lock().unwrap();
                            queue.current = i + 1;
                            queue.progress = if queue.total > 0 { ((i + 1) * 100) / queue.total } else { 0 };
                            continue;
                        }
                        let provider_type = {
                            let pt = sqlx::query_scalar::<_, Option<String>>(
                                "SELECT type FROM providers WHERE id = ?",
                            )
                            .bind(&account.provider_id)
                            .fetch_optional(&pool)
                            .await
                            .ok()
                            .flatten()
                            .flatten();
                            resolve_provider_type(pt.as_deref(), &account.provider_id)
                        };

                        // 统一探测：并行探 chat/messages/responses，可用的全部记入库。
                        let outcome = match reqwest::Client::builder()
                            .timeout(std::time::Duration::from_secs(30))
                            .build()
                        {
                            Ok(client) => Some(
                                probe::run_probe(
                                    &client,
                                    account,
                                    model_name,
                                    &provider_type,
                                    probe_mode,
                                )
                                .await,
                            ),
                            Err(_) => None,
                        };
                        if let Some(o) = &outcome {
                            if let Some(configured) = o.mismatched_config {
                                tracing::warn!(
                                    "🧭 {} | {} 实际可用 [{}]，但别名配的是 /{} —— 配置可能写错了",
                                    model_name,
                                    account.alias,
                                    o.via_label(),
                                    configured.as_str()
                                );
                            }
                        }
                        let test_success = outcome.as_ref().map(|o| o.success()).unwrap_or(false);
                        let latency_ms = outcome.as_ref().map(|o| o.latency_ms()).unwrap_or(0);
                        // 批量队列此前不写日志，失败只能去 UI 看；补一行便于事后 grep。
                        match &outcome {
                            Some(o) if o.success() => tracing::info!(
                                "🧪 {} | {} | {}ms | OK [{}]",
                                model_name,
                                account.alias,
                                o.latency_ms(),
                                o.via_label()
                            ),
                            Some(o) => tracing::warn!(
                                "🧪 {} | {} | FAILED: {}",
                                model_name,
                                account.alias,
                                o.error_summary().chars().take(160).collect::<String>()
                            ),
                            None => tracing::warn!(
                                "🧪 {} | {} | FAILED: 无法创建 HTTP client",
                                model_name,
                                account.alias
                            ),
                        }

                        // 拨测结果落库（与单模型拨测共用同一实现）
                        persist_test_result(
                            &pool,
                            account,
                            model_name,
                            test_success,
                            latency_ms,
                            outcome
                                .as_ref()
                                .filter(|o| !o.success())
                                .map(|o| o.error_summary())
                                .as_deref(),
                            outcome.as_ref(),
                            probe::TestSource::Manual,
                        )
                        .await;
                    }

            {
                let mut queue = queue_state.lock().unwrap();
                queue.current = i + 1;
                queue.progress = if queue.total > 0 {
                    ((i + 1) * 100) / queue.total
                } else {
                    0
                };
            }
        }

        {
            let mut queue = queue_state.lock().unwrap();
            queue.is_running = false;
        }
    });

    Json(json!({
        "success": true,
        "message": "Queue started",
        "total": models.len()
    }))
    .into_response()
}

pub async fn test_model(
    Extension(state): Extension<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let Some(model_name) = body.get("model").and_then(Value::as_str) else {
        return crate::error::simple_error("No model provided", StatusCode::BAD_REQUEST);
    };

    let provider_id_override = body
        .get("providerId")
        .and_then(Value::as_str)
        .map(String::from);
    let account_id_override = body.get("accountId").and_then(|v| v.as_i64());

    // Resolve model to provider
    let resolution = match resolve_model(&state.pool, model_name).await {
        Ok(r) => r,
        Err(e) => {
            return crate::error::simple_error(
                format!("Failed to resolve model: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Use providerId override if provided (matching Bun behavior)
    let effective_provider = provider_id_override
        .as_deref()
        .unwrap_or(&resolution.provider_id);
    // 拨测的首选协议必须与真实路由同源：alias 的 upstream_api 决定 mode。
    let probe_mode = DownstreamMode::from_str(resolution.upstream_api.as_str());

    let accounts = if let Some(acc_id) = account_id_override {
        // Directly fetch the specified account
        match sqlx::query(
            "SELECT id, alias, provider_id, api_key, base_url, anthropic_base_url, is_active, weight, openai_compatible, chat_endpoint, responses_endpoint, messages_endpoint, default_protocol, balance_provider, balance_auth \
             FROM accounts WHERE id = ? AND is_active = 1",
        )
        .bind(acc_id)
        .fetch_optional(&state.pool)
        .await
        {
            Ok(Some(row)) => {
                let encrypted: String = row.try_get("api_key").unwrap_or_default();
                match decrypt_api_key(&encrypted, &state.master_key) {
                    Ok(api_key) => vec![llmux_core::adapters::Account {
                        id: row.try_get("id").unwrap_or_default(),
                        alias: row.try_get("alias").unwrap_or_default(),
                        provider_id: row.try_get("provider_id").unwrap_or_default(),
                        api_key,
                        base_url: row.try_get("base_url").ok(),
                        anthropic_base_url: row.try_get("anthropic_base_url").ok(),
                        is_active: row
                            .try_get::<i64, _>("is_active")
                            .unwrap_or(1),
                        weight: row.try_get("weight").unwrap_or(1),
                        openai_compatible: row.try_get("openai_compatible").unwrap_or(0),
                        chat_endpoint: row.try_get("chat_endpoint").ok(),
                        responses_endpoint: row.try_get("responses_endpoint").ok(),
                        messages_endpoint: row.try_get("messages_endpoint").ok(),
                          default_protocol: row.try_get("default_protocol").ok(),
                                balance_provider: row.try_get::<Option<String>, _>("balance_provider").ok().flatten().unwrap_or_default(),
                balance_auth: row.try_get::<Option<String>, _>("balance_auth").ok().flatten().unwrap_or_default(),
                    }],
                    Err(_) => vec![],
                }
            }
            Ok(None) => vec![],
            Err(_) => vec![],
        }
    } else {
        match get_active_accounts(
            &state.pool,
            Some(effective_provider),
            &state.master_key,
        )
        .await
        {
            Ok(a) => a,
            Err(e) => {
                return crate::error::simple_error(
                    format!("Failed to get accounts: {e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        }
    };

    let Some(account) = accounts.first() else {
        return Json(json!({
            "success": false,
            "error": format!("No active account found for provider {}", effective_provider)
        }))
        .into_response();
    };

    let provider_type = {
        let pt =
            sqlx::query_scalar::<_, Option<String>>("SELECT type FROM providers WHERE id = ?")
                .bind(&account.provider_id)
                .fetch_optional(&state.pool)
                .await
                .ok()
                .flatten()
                .flatten();
        resolve_provider_type(pt.as_deref(), &account.provider_id)
    };

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return Json(json!({
                "success": false,
                "error": format!("Failed to create HTTP client: {e}")
            }))
            .into_response();
        }
    };

    // 统一探测：并行探 chat/messages/responses，可用的全部记入库。
    let outcome = probe::run_probe(
        &client,
        account,
        model_name,
        &provider_type,
        probe_mode,
    )
    .await;
    if let Some(configured) = outcome.mismatched_config {
        tracing::warn!(
            "🧭 {} | {} 实际可用 [{}]，但别名配的是 /{} —— 配置可能写错了",
            model_name,
            account.alias,
            outcome.via_label(),
            configured.as_str()
        );
    }

    let latency_ms = outcome.latency_ms();
    let success = outcome.success();
    // 回显首选协议那次探测的原始应答（成功时是模型回答，失败时是上游错误体）。
    let body_text = outcome
        .preferred()
        .and_then(|p| outcome.protocols.iter().find(|x| x.protocol == p))
        .map(|p| p.body.clone())
        .unwrap_or_else(|| outcome.error_summary());
    let response_json: Value = serde_json::from_str(&body_text).unwrap_or(Value::Null);
    let error_msg = if success {
        None
    } else {
        response_json
            .pointer("/error/message")
            .and_then(Value::as_str)
            .map(String::from)
            .or_else(|| Some(outcome.error_summary()))
    };

    // 落库 —— 此前单模型拨测不写 usage_logs，刷新后结果就没了。
    persist_test_result(
        &state.pool,
        account,
        model_name,
        success,
        latency_ms,
        error_msg.as_deref(),
        Some(&outcome),
        probe::TestSource::Manual,
    )
    .await;

    if success {
        tracing::info!(
            "🧪 {} | {} | {} | {}ms | OK [{}]",
            model_name,
            account.alias,
            effective_provider,
            latency_ms,
            outcome.via_label(),
        );
    } else {
        tracing::warn!(
            "🧪 {} | {} | {} | {}ms | FAILED: {}",
            model_name,
            account.alias,
            effective_provider,
            latency_ms,
            outcome.error_summary()
        );
    }

    Json(json!({
        "success": success,
        "latency": latency_ms,
        "status": outcome.status(),
        // 支持的**全部**协议（按 chat > messages > responses 排序）+ 首选，
        // 前端据此显示多协议角标。
        "supported": outcome.supported.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
        "via": outcome.preferred().map(|p| p.as_str()),
        "mismatchedConfig": outcome.mismatched_config.map(|p| p.as_str()),
        "response": if success { response_json } else { Value::Null },
        "error": error_msg,
    }))
    .into_response()
}
