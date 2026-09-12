use std::sync::{Arc, Mutex};
use std::time::Duration;

use llmux_core::aggregate::{get_account_by_id, AggregateCandidate};

/// Spawn the background aggregate probe loop.
/// Runs every `interval_secs` (default 300) per spec: dual-phase with 3-confirm.
pub fn spawn_aggregate_probe(
    pool: sqlx::SqlitePool,
    master_key: String,
    aggregate_router: Arc<Mutex<llmux_core::aggregate::AggregateRouter>>,
) {
    tokio::spawn(async move {
        loop {
            // Read interval from DB per alias? Spec says per-alias interval_secs but
            // background loop is global. Use min interval among all aliases, default 300.
            let interval_secs = load_min_interval(&pool).await.unwrap_or(300);
            // respect backoff: if any entry has probe_backoff > interval, use that
            let backoff = {
                let guard = aggregate_router.lock().unwrap();
                guard
                    .entries
                    .values()
                    .map(|e| e.probe_backoff_secs)
                    .max()
                    .unwrap_or(interval_secs as u64)
            };
            let sleep_secs = backoff.max(interval_secs as u64);
            tokio::time::sleep(Duration::from_secs(sleep_secs)).await;

            if let Err(e) = run_probe_round(&pool, &master_key, &aggregate_router).await {
                tracing::warn!("aggregate probe round failed: {e}");
            }
        }
    });
}

async fn load_min_interval(pool: &sqlx::SqlitePool) -> anyhow::Result<u64> {
    let v: Option<i64> =
        sqlx::query_scalar("SELECT MIN(interval_secs) FROM aggregate_aliases")
            .fetch_optional(pool)
            .await?;
    Ok(v.unwrap_or(300) as u64)
}

async fn run_probe_round(
    pool: &sqlx::SqlitePool,
    master_key: &str,
    aggregate_router: &Arc<Mutex<llmux_core::aggregate::AggregateRouter>>,
) -> anyhow::Result<()> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT alias, candidates FROM aggregate_aliases",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    for (alias, candidates_json) in rows {
        let candidates = match llmux_core::aggregate::parse_candidates(&candidates_json) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("probe: failed to parse candidates for {}: {e}", alias);
                continue;
            }
        };
        let len = candidates.len();
        if len == 0 {
            continue;
        }
        let active = aggregate_router.lock().unwrap().get_active(&alias);
        let active = active.min(len.saturating_sub(1));

        // Dual-phase probe
        let v_prime = probe_dual_phase(&alias, &candidates, active, pool, master_key).await;

        let switched = if let Some(vp) = v_prime {
            let mut guard = aggregate_router.lock().unwrap();
            // Update per-candidate last_status from the probe round
            // We need to know which candidates were probed — do it via the helper's return
            // For now, we just update the target candidate as success; detailed per-candidate
            // status is maintained by the probe helper via note_candidate_* if needed.
            guard.record_probe_candidate(&alias, vp, len)
        } else {
            // all failed => treat as pending V=0 with 3-confirm
            let mut guard = aggregate_router.lock().unwrap();
            let switched = guard.record_probe_candidate(&alias, 0, len);
            if switched {
                guard.record_probe_all_failed_confirmed(&alias, len);
            }
            switched
        };

        if switched {
            tracing::info!("🔍 [agg:{}] probe V migrated -> {:?}", alias, v_prime);
        }
    }
    Ok(())
}

async fn probe_dual_phase(
    alias: &str,
    candidates: &[AggregateCandidate],
    active: usize,
    pool: &sqlx::SqlitePool,
    master_key: &str,
) -> Option<usize> {
    // Stage 1: 0..=active concurrent (spec); implement as concurrent with join_all for speed
    let stage1_indices: Vec<usize> = (0..=active.min(candidates.len().saturating_sub(1))).collect();
    let mut stage1_alive: Vec<usize> = Vec::new();

    // Concurrent probe for stage1
    let mut futs = Vec::new();
    for &idx in &stage1_indices {
        let cand = candidates[idx].clone();
        let pool = pool.clone();
        let master_key = master_key.to_string();
        futs.push(async move {
            let alive = probe_candidate(&cand, &pool, &master_key).await;
            (idx, alive)
        });
    }
    let results = futures_util::future::join_all(futs).await;
    for (idx, alive) in results {
        if alive {
            stage1_alive.push(idx);
        }
    }

    if !stage1_alive.is_empty() {
        stage1_alive.sort_unstable();
        let best = stage1_alive[0];
        tracing::debug!("🔍 [agg:{}] stage1 best V={}", alias, best);
        return Some(best);
    }

    // Stage 2: active+1..len sequential, first alive
    for idx in (active + 1)..candidates.len() {
        let cand = &candidates[idx];
        if probe_candidate(cand, pool, master_key).await {
            tracing::debug!("🔍 [agg:{}] stage2 hit V={}", alias, idx);
            return Some(idx);
        }
    }

    // All failed
    tracing::warn!("🔍 [agg:{}] all candidates failed", alias);
    None
}

async fn probe_candidate(
    cand: &AggregateCandidate,
    pool: &sqlx::SqlitePool,
    master_key: &str,
) -> bool {
    let account = match get_account_by_id(pool, cand.account_id, master_key).await {
        Ok(Some(a)) => a,
        _ => return false,
    };

    let provider_type = {
        let pt = sqlx::query_scalar::<_, Option<String>>("SELECT type FROM providers WHERE id = ?")
            .bind(&account.provider_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
            .flatten();
        llmux_core::dispatcher::resolve_provider_type(pt.as_deref(), &account.provider_id)
    };

    // 与拨测/别名校验走**同一套**探测（协议缓存起点 + 真实路由同款回退阶梯）。
    // 此前这里只会拼 /chat/completions，对只服务 /v1/responses 的模型
    // （Console Go muse-spark-1.x-contributor）永远判死，聚合候选会被错误降级。
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let outcome = match tokio::time::timeout(
        Duration::from_secs(15),
        llmux_core::probe::run_probe(
            &client,
            &account,
            &cand.model,
            &provider_type,
            llmux_core::protocol::DownstreamMode::Chat,
        ),
    )
    .await
    {
        Ok(o) => o,
        _ => return false,
    };

    llmux_core::probe::store_probed_protocols(
        pool,
        account.id,
        &cand.model,
        &outcome.supported,
        outcome.native,
    )
    .await;

    if outcome.success() {
        if let Some(configured) = outcome.mismatched_config {
            tracing::warn!(
                "🧭 [agg] {} | {} 实际走 /{}，但别名配的是 /{} —— 配置可能写错了",
                cand.model,
                account.alias,
                outcome.via_label(),
                configured.as_str()
            );
        }
    }
    outcome.success()
}
