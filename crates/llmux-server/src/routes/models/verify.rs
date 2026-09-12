//! 别名保存后的自动验证。
//!
//! 保存别名时顺带探一次该(账户, 模型)，把走通的协议记进 `model_test_results`
//! 并回显给 UI。**只记事实、只提示，不改任何配置** —— 线上走哪个协议由用户配的
//! `upstream_api` 决定，探出不一致时提示「配置可能写错」，绝不静默覆盖。
//!
//! 写入标 `source=verify`：角标会用这次的协议集合，但 health 的「最近一次状态」
//! 不采信 —— 保存别名这个动作不该把用户手工拨测的结果顶掉。
//!
//! 走的是和拨测/聚合探活完全同一个 `llmux_core::probe`，没有第二套逻辑。

use serde_json::{json, Value};

use llmux_core::probe;

use crate::app::AppState;

/// 保存别名后调用：对每个(账户, 模型)探测一次，返回给 UI 的摘要。
///
/// 探测失败不阻塞保存 —— 保存本身已经成功，这里只是附加信息。
pub async fn verify_targets(
    state: &AppState,
    model: &str,
    provider_id: Option<&str>,
    account_ids: &[i64],
) -> Vec<Value> {
    let accounts = if !account_ids.is_empty() {
        let mut out = Vec::new();
        for id in account_ids {
            if let Ok(Some(a)) =
                llmux_core::aggregate::get_account_by_id(&state.pool, *id, &state.master_key).await
            {
                out.push(a);
            }
        }
        out
    } else {
        match provider_id {
            Some(p) => llmux_core::dispatcher::get_active_accounts(&state.pool, Some(p), &state.master_key)
                .await
                .unwrap_or_default(),
            None => Vec::new(),
        }
    };

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut results = Vec::new();
    for account in accounts {
        let provider_type = {
            let pt =
                sqlx::query_scalar::<_, Option<String>>("SELECT type FROM providers WHERE id = ?")
                    .bind(&account.provider_id)
                    .fetch_optional(&state.pool)
                    .await
                    .ok()
                    .flatten()
                    .flatten();
            llmux_core::dispatcher::resolve_provider_type(pt.as_deref(), &account.provider_id)
        };
        let outcome = probe::run_probe(
            &client,
            &account,
            model,
            &provider_type,
            llmux_core::protocol::DownstreamMode::Default,
        )
        .await;
        let error = (!outcome.success()).then(|| outcome.error_summary());
        crate::routes::models::testing::persist_test_result(
            &state.pool,
            &account,
            model,
            outcome.success(),
            outcome.latency_ms(),
            error.as_deref(),
            Some(&outcome),
            llmux_core::probe::TestSource::Verify,
        )
        .await;
        // 别名校验也要留痕：否则用户保存别名后探测失败，只能去 UI 卡片里找原因。
        if outcome.success() {
            tracing::info!(
                "🧪 [verify] {} | {} | {}ms | OK [{}]",
                model, account.alias, outcome.latency_ms(), outcome.via_label()
            );
        } else {
            tracing::warn!(
                "🧪 [verify] {} | {} | FAILED: {}",
                model, account.alias, outcome.error_summary()
            );
        }

        results.push(json!({
            "account_id": account.id,
            "account": account.alias,
            "success": outcome.success(),
            "supported": outcome.supported.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
            "via": outcome.preferred().map(|p| p.as_str()),
            "latency": outcome.latency_ms(),
            "error": if outcome.success() { None } else { Some(outcome.error_summary()) },
        }));
    }
    results
}

/// 探测结果挂到保存响应里；UI 可据此对「该别名配的协议 vs 实际可用协议」发提示。
pub fn attach(results: Vec<Value>) -> Value {
    json!({ "verified": results })
}
